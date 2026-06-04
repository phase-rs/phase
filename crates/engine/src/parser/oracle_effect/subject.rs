use crate::parser::oracle_nom::error::OracleError;
use nom::branch::alt;
use nom::bytes::complete::{tag, take_till, take_until};
use nom::combinator::{all_consuming, map, opt, value, verify};
use nom::sequence::{preceded, terminated};
use nom::Parser;

use super::animation::{
    animation_modifications_with_replacement, has_in_addition_to_other_types, parse_animation_spec,
};
use super::{resolve_it_pronoun, ParseContext};
use crate::parser::oracle_ir::ast::*;
use crate::types::ability::{
    AbilityDefinition, AbilityKind, ChosenSubtypeKind, ContinuousModification, ControllerRef,
    Duration, Effect, FilterProp, MultiTargetSpec, PlayerFilter, PlayerScope, PtValue,
    QuantityExpr, QuantityRef, StaticDefinition, TargetFilter, TypedFilter,
};
use crate::types::game_state::DayNight;
use crate::types::keywords::Keyword;
use crate::types::phase::Phase;
use crate::types::statics::{ProhibitionScope, StaticMode};

use super::super::oracle_keyword::parse_keyword_from_oracle;
use super::super::oracle_nom::error::OracleResult;
use super::super::oracle_nom::primitives as nom_primitives;
use super::super::oracle_nom::quantity as nom_quantity;
use super::super::oracle_nom::target::parse_event_context_ref;
use super::super::oracle_quantity;
use super::super::oracle_static::{
    classify_block_exception, parse_additive_type_clause_modifications,
    parse_cant_be_activated_exemption_in_text, parse_chosen_qualifier_subject,
    parse_continuous_modifications, parse_static_line_multi,
};
use super::super::oracle_target::{parse_target, parse_target_with_ctx, parse_type_phrase};
use super::super::oracle_util::{
    parse_number, TextPair, SELF_REF_PARSE_ONLY_PHRASES, SELF_REF_TYPE_PHRASES,
};

pub(super) fn try_parse_subject_predicate_ast(
    text: &str,
    ctx: &mut ParseContext,
) -> Option<ClauseAst> {
    if try_parse_targeted_controller_gain_life(text).is_some() {
        return None;
    }

    // CR 702.3b: "can attack [this turn] as though it didn't have defender" —
    // must intercept before continuous clause parsing which would incorrectly
    // extract "defender" as an AddKeyword from "didn't have defender".
    if let Some(clause) = try_parse_can_attack_with_defender(text, ctx) {
        return Some(subject_predicate_ast_from_clause(
            text,
            clause,
            |effect, duration, _sub_ability| PredicateAst::Restriction { effect, duration },
            ctx,
        ));
    }

    // CR 509.1a + CR 509.1b: "can block an additional creature [this turn]" —
    // must intercept before continuous clause parsing which cannot produce the
    // ExtraBlockers static mode from the predicate text.
    if let Some(clause) = try_parse_can_block_additional(text, ctx) {
        return Some(subject_predicate_ast_from_clause(
            text,
            clause,
            |effect, duration, _sub_ability| PredicateAst::Restriction { effect, duration },
            ctx,
        ));
    }

    if let Some(clause) = try_parse_subject_additive_type_clause(text, ctx) {
        return Some(clause);
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
                multi_target: None,
                inherits_parent: false,
                is_optional: false,
            });
        return Some(ClauseAst::SubjectPredicate {
            subject: Box::new(SubjectPhraseAst {
                affected: application.affected,
                target: application.target,
                multi_target: application.multi_target,
                inherits_parent: application.inherits_parent,
                is_optional: application.is_optional,
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
    ctx: &mut ParseContext,
) -> ClauseAst
where
    F: FnOnce(Effect, Option<Duration>, Option<Box<AbilityDefinition>>) -> PredicateAst,
{
    let subject_text = extract_subject_text(text).unwrap_or_default();
    let application = parse_subject_application(&subject_text, ctx).unwrap_or(SubjectApplication {
        affected: TargetFilter::Any,
        target: None,
        multi_target: None,
        inherits_parent: false,
        is_optional: false,
    });

    ClauseAst::SubjectPredicate {
        subject: Box::new(SubjectPhraseAst {
            affected: application.affected,
            target: application.target,
            multi_target: application.multi_target,
            inherits_parent: application.inherits_parent,
            is_optional: application.is_optional,
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

fn try_parse_subject_additive_type_clause(text: &str, ctx: &mut ParseContext) -> Option<ClauseAst> {
    type VE<'a> = OracleError<'a>;

    if let Some(clause) = try_parse_contracted_subject_additive_type_clause(text, ctx) {
        return Some(clause);
    }

    let lower = text.to_lowercase();
    let (subject_lower, predicate_lower) = nom_primitives::scan_split_at_phrase(&lower, |i| {
        alt((tag::<_, _, VE>("are "), tag::<_, _, VE>("is "))).parse(i)
    })?;
    let subject_text = text[..subject_lower.len()].trim();
    if subject_text.eq_ignore_ascii_case("you") {
        return None;
    }
    let predicate = &text[text.len() - predicate_lower.len()..];
    let application = additive_type_subject_application(subject_text, ctx)?;
    let clause = build_additive_type_continuous_clause(&application, predicate)?;

    Some(ClauseAst::SubjectPredicate {
        subject: Box::new(SubjectPhraseAst {
            affected: application.affected,
            target: application.target,
            multi_target: application.multi_target,
            inherits_parent: application.inherits_parent,
            is_optional: application.is_optional,
        }),
        predicate: Box::new(PredicateAst::Continuous {
            effect: clause.effect,
            duration: clause.duration,
            sub_ability: clause.sub_ability,
        }),
    })
}

fn try_parse_contracted_subject_additive_type_clause(
    text: &str,
    ctx: &mut ParseContext,
) -> Option<ClauseAst> {
    type VE<'a> = OracleError<'a>;

    let lower = text.to_lowercase();
    let (_, (subject_text, prefix_len)) = alt((
        value(("it", "it's ".len()), tag::<_, _, VE>("it's ")),
        value(("it", "it’s ".len()), tag::<_, _, VE>("it’s ")),
    ))
    .parse(lower.as_str())
    .ok()?;
    let rest_original = &text[prefix_len..];
    let predicate = format!("is {rest_original}");
    let application = additive_type_subject_application(subject_text, ctx)?;
    let clause = build_additive_type_continuous_clause(&application, &predicate)?;

    Some(ClauseAst::SubjectPredicate {
        subject: Box::new(SubjectPhraseAst {
            affected: application.affected,
            target: application.target,
            multi_target: application.multi_target,
            inherits_parent: application.inherits_parent,
            is_optional: application.is_optional,
        }),
        predicate: Box::new(PredicateAst::Continuous {
            effect: clause.effect,
            duration: clause.duration,
            sub_ability: clause.sub_ability,
        }),
    })
}

fn try_parse_subject_continuous_clause(
    text: &str,
    ctx: &mut ParseContext,
) -> Option<ParsedEffectClause> {
    let verb_start = find_predicate_start(text)?;
    let subject = text[..verb_start].trim();
    let predicate = text[verb_start..].trim();
    // CR 109.5: "you" as a player subject never participates in continuous-
    // clause parsing — the predicate is always an imperative effect (draw,
    // gain life, get an emblem with, phase out, …). Routing "you" through
    // the continuous arm misclassifies imperatives like "you get an emblem
    // with \"…\"" as `get +X/+X`-style P/T modifications.
    if subject.eq_ignore_ascii_case("you") {
        return None;
    }
    if let Some(clause) = try_parse_additive_type_continuous_clause(subject, predicate, ctx) {
        return Some(clause);
    }
    let application = parse_subject_application(subject, ctx)?;
    build_continuous_clause(application, predicate)
}

fn additive_type_subject_application(
    subject: &str,
    ctx: &mut ParseContext,
) -> Option<SubjectApplication> {
    let (parsed_subject, rest) = parse_target(subject);
    if rest.trim().is_empty()
        && matches!(
            parsed_subject,
            TargetFilter::TrackedSet { .. } | TargetFilter::TrackedSetFiltered { .. }
        )
    {
        return subject_filter_application(parsed_subject, false);
    }

    parse_subject_application(subject, ctx)
}

fn try_parse_additive_type_continuous_clause(
    subject: &str,
    predicate: &str,
    ctx: &mut ParseContext,
) -> Option<ParsedEffectClause> {
    let application = additive_type_subject_application(subject, ctx)?;
    build_additive_type_continuous_clause(&application, predicate)
}

fn build_additive_type_continuous_clause(
    application: &SubjectApplication,
    predicate: &str,
) -> Option<ParsedEffectClause> {
    let modifications = parse_additive_type_clause_modifications(predicate)?;
    let affected = static_affected_for_application(application);

    Some(ParsedEffectClause {
        effect: Effect::GenericEffect {
            static_abilities: vec![StaticDefinition::continuous()
                .affected(affected)
                .modifications(modifications)
                .description(predicate.to_string())],
            duration: Some(Duration::Permanent),
            target: application.target.clone(),
        },
        duration: Some(Duration::Permanent),
        sub_ability: None,
        distribute: None,
        multi_target: None,
        condition: None,
        optional: false,
        unless_pay: None,
    })
}

fn try_parse_subject_become_clause(
    text: &str,
    ctx: &mut ParseContext,
) -> Option<ParsedEffectClause> {
    let verb_start = find_predicate_start(text)?;
    let subject = text[..verb_start].trim();
    let predicate = deconjugate_verb(text[verb_start..].trim());
    let predicate_lower = predicate.to_lowercase();
    tag::<_, _, OracleError<'_>>("become ")
        .parse(predicate_lower.as_str())
        .ok()?;
    let application = parse_subject_application(subject, ctx)?;
    build_become_clause(application, &predicate, ctx)
}

fn try_parse_subject_restriction_clause(
    text: &str,
    ctx: &mut ParseContext,
) -> Option<ParsedEffectClause> {
    let lower = text.to_lowercase();

    // CR 509.1c: "Target creature must be blocked [this turn] [if able]"
    // Handled separately because "must be blocked" isn't a "can't X" restriction pattern
    // and needs AddStaticMode for transient effect propagation through the layer system.
    let tp = TextPair::new(text, &lower);
    if let Some((before, _)) = tp.split_around(" must be blocked") {
        let subject = before.original.trim();
        let application = parse_subject_application(subject, ctx)?;
        let affected = static_affected_for_application(&application);
        return Some(ParsedEffectClause {
            effect: Effect::GenericEffect {
                static_abilities: vec![StaticDefinition::new(StaticMode::MustBeBlocked)
                    .affected(affected)
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
            condition: None,
            optional: false,
            unless_pay: None,
        });
    }

    // CR 602.5 + CR 603.2a: "[subject] activated abilities can't be activated" —
    // the EFFECT/predicate form (Dovin Baan, Xathrid Gorgon, Braided Net), mirror
    // of the static dispatch in `oracle_static/dispatch.rs` (`StaticMode::CantBeActivated`).
    // Splits the same way as the `must be blocked` arm: `before` is the subject
    // ("its", "that creature", "target creature", "~"). Bare possessive/pronoun
    // anaphors ("its"/"it"/"their"/"that creature"/"~") refer back to a previously
    // targeted permanent in the same conjunction (Dovin Baan: "up to one target
    // creature gets -3/-0 and its activated abilities can't be activated"), so they
    // bind to `ParentTarget`; `parse_subject_application` resolves the typed-subject
    // forms ("target creature's", "each creature you control").
    if let Some((before, _)) = tp.split_around(" activated abilities can't be activated") {
        let subject = before.original.trim();
        let application = subject_application_for_cant_be_activated(subject, ctx)?;
        let affected = static_affected_for_application(&application);
        // CR 605.1a: "unless they're mana abilities" exemption rides on the mode.
        let exemption = parse_cant_be_activated_exemption_in_text(&lower);
        let mode = StaticMode::CantBeActivated {
            who: ProhibitionScope::AllPlayers,
            source_filter: TargetFilter::SelfRef,
            exemption,
        };
        return Some(ParsedEffectClause {
            effect: Effect::GenericEffect {
                static_abilities: vec![StaticDefinition::new(mode.clone())
                    .affected(affected)
                    .modifications(vec![ContinuousModification::AddStaticMode { mode }])],
                duration: Some(Duration::UntilEndOfTurn),
                target: application.target,
            },
            distribute: None,
            multi_target: None,
            duration: Some(Duration::UntilEndOfTurn),
            sub_ability: None,
            condition: None,
            optional: false,
            unless_pay: None,
        });
    }

    // CR 701.19c: "[subject] can't be regenerated [this turn]" — the standalone,
    // until-end-of-turn form (Hurr Jackal, Furnace Brood, Lim-Dûl's Cohort).
    // Marks the subject so regeneration shields are not applied the next time it
    // would be destroyed. Splits the same way as the `must be blocked` /
    // `activated abilities can't be activated` arms: `before` is the subject
    // ("target creature", "that creature", "it", "~"). Bare pronoun/anaphor
    // subjects bind to `ParentTarget` via `subject_application_for_cant_be_activated`
    // (Lim-Dûl's Cohort: "Destroy target creature ... That creature can't be
    // regenerated this turn." → "that creature" → ParentTarget), while
    // "target creature" routes through the full subject grammar. The predicate
    // itself is an anchored nom production that absorbs the optional "this turn"
    // suffix; the duration is encoded directly as `UntilEndOfTurn`.
    if let Some((before_lower, (), _)) =
        nom_primitives::scan_preceded(&lower, parse_cant_be_regenerated_predicate)
    {
        let subject = text[..before_lower.len()].trim();
        let application = subject_application_for_cant_be_activated(subject, ctx)?;
        let affected = static_affected_for_application(&application);
        let mode = StaticMode::CantBeRegenerated;
        return Some(ParsedEffectClause {
            effect: Effect::GenericEffect {
                static_abilities: vec![StaticDefinition::new(mode.clone())
                    .affected(affected)
                    .modifications(vec![ContinuousModification::AddStaticMode { mode }])],
                duration: Some(Duration::UntilEndOfTurn),
                target: application.target,
            },
            distribute: None,
            multi_target: None,
            duration: Some(Duration::UntilEndOfTurn),
            sub_ability: None,
            condition: None,
            optional: false,
            unless_pay: None,
        });
    }

    // CR 119.7 + CR 119.8: "[possessor] life total can't change" — bidirectional
    // life-lock for the named player (Teferi's Protection: "your life total can't
    // change"). Distinct from the generic " can't " split below because the
    // subject is a possessive noun phrase ("your") rather than a player subject.
    if let Some((before, _)) = tp.split_around(" life total can't change") {
        let possessor = before.original.trim().to_lowercase();
        let scope_filter = life_lock_scope_from_possessor(&possessor);
        return Some(build_life_lock_clause(scope_filter));
    }
    if let Some((before, _)) = tp.split_around(" life totals can't change") {
        let possessor = before.original.trim().to_lowercase();
        let scope_filter = life_lock_scope_from_possessor(&possessor);
        return Some(build_life_lock_clause(scope_filter));
    }
    if let Some((before, _)) = tp.split_around(" life total cannot change") {
        let possessor = before.original.trim().to_lowercase();
        let scope_filter = life_lock_scope_from_possessor(&possessor);
        return Some(build_life_lock_clause(scope_filter));
    }
    if let Some((before, _)) = tp.split_around(" life totals cannot change") {
        let possessor = before.original.trim().to_lowercase();
        let scope_filter = life_lock_scope_from_possessor(&possessor);
        return Some(build_life_lock_clause(scope_filter));
    }

    // CR 510.1a: "[subject] assigns no combat damage [this turn/this combat]"
    // Transient rule modification that prevents combat damage assignment.
    if let Some((before, after)) = tp.split_around(" assigns no combat damage") {
        let subject = before.original.trim();
        let application = parse_subject_application(subject, ctx)?;
        // CR 514.2: "this combat" → UntilEndOfCombat; default "this turn" → UntilEndOfTurn.
        let after_lower = after.lower.trim_start();
        let duration = if after_lower.starts_with("this combat") {
            Duration::UntilEndOfCombat
        } else {
            Duration::UntilEndOfTurn
        };
        let affected = static_affected_for_application(&application);
        return Some(ParsedEffectClause {
            effect: Effect::GenericEffect {
                static_abilities: vec![StaticDefinition::new(StaticMode::AssignNoCombatDamage)
                    .affected(affected)
                    .modifications(vec![ContinuousModification::AssignNoCombatDamage])],
                duration: Some(duration.clone()),
                target: application.target,
            },
            distribute: None,
            multi_target: None,
            duration: Some(duration),
            sub_ability: None,
            condition: None,
            optional: false,
            unless_pay: None,
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
    } else {
        let pos = tp.find(" don't untap")?;
        let (before, after) = tp.split_at(pos);
        (before.original.trim(), after.original[1..].trim())
    };
    let application = parse_subject_application(subject, ctx)?;
    build_restriction_clause(application, predicate)
}

/// CR 702.3b: "[subject] can attack [this turn] as though it/they didn't have defender"
/// Produces a GenericEffect with CanAttackWithDefender static mode.
fn try_parse_can_attack_with_defender(
    text: &str,
    ctx: &mut ParseContext,
) -> Option<ParsedEffectClause> {
    let lower = text.to_lowercase();
    let tp = TextPair::new(text, &lower);
    let pos = tp.find(" can attack")?;
    if !is_can_attack_despite_defender_predicate(&lower[pos + 1..]) {
        return None;
    }
    let subject = text[..pos].trim();
    let application = parse_subject_application(subject, ctx)?;
    // Determine duration: "this turn" implies UntilEndOfTurn.
    let duration = if lower.contains("this turn") {
        Some(Duration::UntilEndOfTurn)
    } else {
        None
    };
    let affected = static_affected_for_application(&application);
    Some(ParsedEffectClause {
        effect: Effect::GenericEffect {
            static_abilities: vec![StaticDefinition::new(StaticMode::CanAttackWithDefender)
                .affected(affected)
                .description(text.to_string())],
            duration: duration.clone(),
            target: application.target,
        },
        duration,
        sub_ability: None,
        distribute: None,
        multi_target: None,
        condition: None,
        optional: false,
        unless_pay: None,
    })
}

/// CR 509.1a + CR 509.1b: "[subject] can block an additional creature [this turn]"
/// Produces a GenericEffect with ExtraBlockers { count: Some(1) } static mode.
/// Mirrors the static-ability parser in `oracle_static.rs` but for activated/triggered
/// effect text where the grant is transient (until end of turn).
fn try_parse_can_block_additional(
    text: &str,
    ctx: &mut ParseContext,
) -> Option<ParsedEffectClause> {
    let lower = text.to_lowercase();
    let (subject_lower, predicate_lower) =
        nom_primitives::scan_split_at_phrase(&lower, |i| tag("can block ").parse(i))?;
    let subject_text = &text[..subject_lower.len()];
    let application = if subject_text.trim().is_empty() {
        SubjectApplication {
            affected: TargetFilter::ParentTarget,
            target: Some(TargetFilter::ParentTarget),
            multi_target: None,
            inherits_parent: true,
            is_optional: false,
        }
    } else {
        parse_subject_application(subject_text.trim(), ctx)?
    };

    let (_rest, (_, _, _, _, count, duration, _)) = all_consuming((
        tag("can"),
        tag(" "),
        tag("block"),
        tag(" "),
        parse_extra_blockers_count,
        parse_block_grant_duration,
        opt(tag(".")),
    ))
    .parse(predicate_lower)
    .ok()?;
    let duration = if subject_text.trim().is_empty() {
        duration.or(Some(Duration::UntilEndOfTurn))
    } else {
        duration
    };
    let mode = StaticMode::ExtraBlockers { count };
    let affected = static_affected_for_application(&application);
    Some(ParsedEffectClause {
        effect: Effect::GenericEffect {
            static_abilities: vec![StaticDefinition::new(mode.clone())
                .affected(affected)
                .modifications(vec![ContinuousModification::AddStaticMode { mode }])],
            duration: duration.clone(),
            target: application.target,
        },
        duration,
        sub_ability: None,
        distribute: None,
        multi_target: None,
        condition: None,
        optional: false,
        unless_pay: None,
    })
}

pub(super) fn is_can_block_extra_predicate(lower: &str) -> bool {
    all_consuming((
        tag::<_, _, OracleError<'_>>("can"),
        tag(" "),
        tag("block"),
        tag(" "),
        parse_extra_blockers_count,
        parse_block_grant_duration,
        opt(tag(".")),
    ))
    .parse(lower.trim())
    .is_ok()
}

/// CR 702.3b: predicate-only "can attack [this turn] as though [it|they]
/// didn't have defender" — the subjectless conjunct left after the sequence
/// splitter peels it off a "<subject> gets +N/-M ... and ..." compound. Mirrors
/// `is_can_block_extra_predicate`; used by `combat_requirement_conjunct_prepend`
/// to re-attach the subject so `try_parse_can_attack_with_defender` can fire.
pub(super) fn is_can_attack_despite_defender_predicate(lower: &str) -> bool {
    all_consuming((
        tag::<_, _, OracleError<'_>>("can attack"),
        opt(tag(" this turn")),
        tag(" as though "),
        alt((tag("it"), tag("they"))),
        tag(" didn't have defender"),
        opt(tag(".")),
    ))
    .parse(lower.trim())
    .is_ok()
}

fn parse_extra_blockers_count(input: &str) -> OracleResult<'_, Option<u32>> {
    alt((
        map(
            (
                nom_primitives::parse_number,
                tag(" additional creature"),
                opt(tag("s")),
            ),
            |(count, _, _)| Some(count),
        ),
        value(
            None,
            (
                tag("any"),
                tag(" "),
                tag("number"),
                tag(" "),
                tag("of"),
                tag(" "),
                tag("creatures"),
            ),
        ),
    ))
    .parse(input)
}

fn parse_block_grant_duration(input: &str) -> OracleResult<'_, Option<Duration>> {
    opt(preceded(
        tag(" this "),
        alt((
            value(Duration::UntilEndOfTurn, tag("turn")),
            value(Duration::UntilEndOfCombat, tag("combat")),
        )),
    ))
    .parse(input)
}

pub(super) fn parse_subject_application(
    subject: &str,
    ctx: &mut ParseContext,
) -> Option<SubjectApplication> {
    if subject.trim().is_empty() {
        return None;
    }

    let lower = subject.to_lowercase();

    if let Ok((_, _)) = all_consuming((
        tag::<_, _, OracleError<'_>>("you"),
        tag(" and "),
        tag("permanents you control"),
    ))
    .parse(lower.as_str())
    {
        let (permanents, rest) = parse_target("all permanents you control");
        if rest.trim().is_empty() {
            return Some(SubjectApplication {
                affected: TargetFilter::Or {
                    filters: vec![TargetFilter::Controller, permanents],
                },
                target: None,
                multi_target: None,
                inherits_parent: false,
                is_optional: false,
            });
        }
    }

    // CR 115.10a: "another target X" — target with Another filter property,
    // excluding the source object from legal targets.
    if tag::<_, _, OracleError<'_>>("another target ")
        .parse(lower.as_str())
        .is_ok()
    {
        let (filter, _) = parse_target_with_ctx(&subject["another ".len()..], ctx);
        let filter = add_another_property(filter);
        return subject_filter_application(filter, true);
    }
    if tag::<_, _, OracleError<'_>>("target ")
        .parse(lower.as_str())
        .is_ok()
    {
        // CR 109.4 + CR 115.1 + CR 603.2: thread the parse context so that
        // controller-suffix resolution inside `parse_target` (notably the
        // "that player controls" relative reference) can see the enclosing
        // trigger's `relative_player_scope` and emit
        // `ControllerRef::TargetPlayer` for the attacked / damaged player
        // instead of falling back to `You`. Without `ctx`, the subject-form
        // path of "target creature that player controls becomes …" (Gornog,
        // the Red Reaper) silently bound the target to the trigger
        // controller's own creatures.
        let (filter, _) = parse_target_with_ctx(subject, ctx);
        return subject_filter_application(filter, true);
    }
    if tag::<_, _, OracleError<'_>>("up to ")
        .parse(lower.as_str())
        .is_ok()
    {
        let (target_text, multi_target) = super::strip_optional_target_prefix(subject);
        if multi_target.is_some() {
            let (filter, _) = parse_target_with_ctx(target_text, ctx);
            let mut application = subject_filter_application(filter, true)?;
            application.multi_target = multi_target;
            return Some(application);
        }
    }
    if let Some((count, target_text)) = super::strip_exact_target_prefix(lower.as_str()) {
        let consumed = lower.len() - target_text.len();
        let target_text = &subject[consumed..];
        let (filter, _) = parse_target_with_ctx(target_text, ctx);
        let mut application = subject_filter_application(filter, false)?;
        application.multi_target = Some(MultiTargetSpec::exact(count));
        return Some(application);
    }
    // CR 115.1d: "any number of target creatures" — variable-count targeting.
    // Strip "any number of " prefix, delegate to parse_target for the filter,
    // and attach MultiTargetSpec { min: 0, max: None } (unlimited).
    if let Ok((after_prefix, _)) =
        tag::<_, _, OracleError<'_>>("any number of ").parse(lower.as_str())
    {
        let consumed = lower.len() - after_prefix.len();
        let target_text = &subject[consumed..];
        if tag::<_, _, OracleError<'_>>("target ")
            .parse(after_prefix)
            .is_ok()
        {
            let (filter, _) = parse_target_with_ctx(target_text, ctx);
            let mut application = subject_filter_application(filter, true)?;
            application.multi_target = Some(MultiTargetSpec::unlimited(0));
            return Some(application);
        }
    }
    // CR 115.1d: "one or two target X" / "one, two, or three target X" —
    // bounded-count targeting with a minimum of 1 (Scrollboost:
    // "One or two target creatures each get +2/+2 until end of turn"). Mirrors
    // the "any number of target" branch above; the only axis of variation is
    // the min/max pair bound by the phrase.
    for (prefix, min, max) in [
        ("one or two ", 1usize, 2usize),
        ("one, two, or three ", 1, 3),
    ] {
        if let Ok((after_prefix, _)) = tag::<_, _, OracleError<'_>>(prefix).parse(lower.as_str()) {
            if tag::<_, _, OracleError<'_>>("target ")
                .parse(after_prefix)
                .is_ok()
            {
                let consumed = lower.len() - after_prefix.len();
                let target_text = &subject[consumed..];
                let (filter, _) = parse_target_with_ctx(target_text, ctx);
                let mut application = subject_filter_application(filter, true)?;
                application.multi_target = Some(MultiTargetSpec::fixed(min, max));
                return Some(application);
            }
        }
    }
    // "each of your opponents" / "each of those creatures" / "each of them" — variant of
    // "each" with an interposed "of" that parse_target doesn't handle directly.
    // Must check before "each " to avoid the generic "each" path swallowing "each of".
    if let Ok((remainder, _)) = tag::<_, _, OracleError<'_>>("each of ").parse(lower.as_str()) {
        if alt((
            tag::<_, _, OracleError<'_>>("your opponents"),
            tag("your opponent"),
        ))
        .parse(remainder)
        .is_ok()
        {
            return subject_filter_application(
                TargetFilter::Typed(TypedFilter::default().controller(ControllerRef::Opponent)),
                false,
            );
        }
        // "each of those [creatures/players/...]" / "each of them" — anaphoric reference
        // to the targets declared in the parent ability's sub_ability chain.
        if alt((tag::<_, _, OracleError<'_>>("those "), tag("them")))
            .parse(remainder)
            .is_ok()
        {
            return subject_filter_application(TargetFilter::ParentTarget, false);
        }
        // Fallback: strip "of " and re-route through parse_target as "each <remainder>"
        let normalized = format!("each {remainder}");
        let (filter, _) = parse_target(&normalized);
        return subject_filter_application(filter, false);
    }
    if let Ok((rest_lower, _)) =
        alt((tag::<_, _, OracleError<'_>>("all "), tag("each "))).parse(lower.as_str())
    {
        let consumed = lower.len() - rest_lower.len();
        let phrase = &subject[consumed..];
        let (filter, rest) = parse_type_phrase(phrase);
        let filter = merge_partial_type_phrase_filter(filter, rest.trim());
        return subject_filter_application(filter, false);
    }
    if alt((
        tag::<_, _, OracleError<'_>>("enchanted creature"),
        tag("enchanted permanent"),
        tag("equipped creature"),
    ))
    .parse(lower.as_str())
    .is_ok()
    {
        let (filter, _) = parse_target(subject);
        return Some(SubjectApplication {
            affected: filter,
            target: None,
            multi_target: None,
            inherits_parent: false,
            is_optional: false,
        });
    }
    // "those creatures" / "those lands" — anaphoric reference to previous targets.
    // Maps to ParentTarget so the restriction applies to the same objects.
    if let Ok((_, _)) = tag::<_, _, OracleError<'_>>("those ").parse(lower.as_str()) {
        return subject_filter_application(TargetFilter::ParentTarget, false);
    }
    if all_consuming(preceded(
        tag::<_, _, OracleError<'_>>("the chosen "),
        alt((
            tag("artifacts"),
            tag("artifact"),
            tag("cards"),
            tag("card"),
            tag("creatures"),
            tag("creature"),
            tag("enchantments"),
            tag("enchantment"),
            tag("lands"),
            tag("land"),
            tag("permanents"),
            tag("permanent"),
            tag("players"),
            tag("player"),
        )),
    ))
    .parse(lower.as_str())
    .is_ok()
    {
        return subject_filter_application(TargetFilter::ParentTarget, false);
    }

    // Bare plural noun phrase subjects ("creatures you control", "other creatures you control")
    // are implicit "all X" forms — strip any "other " prefix and route through parse_target.
    let (had_other, noun_subject) =
        if let Ok((rest, _)) = tag::<_, _, OracleError<'_>>("other ").parse(lower.as_str()) {
            (true, rest)
        } else {
            (false, lower.as_str())
        };
    if alt((
        tag::<_, _, OracleError<'_>>("target "),
        tag("all "),
        tag("each "),
    ))
    .parse(noun_subject)
    .is_err()
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
            multi_target: None,
            inherits_parent: false,
            is_optional: false,
        });
    }
    // CR 102.1 + CR 103.1: "the player to your right/left" as subject — a
    // seating-relative neighbor (Bucknard's Everfull Purse: "The player to your
    // right gains control of this artifact"). Delegate to `parse_target`, which
    // is the single authority for the `Neighbor` mapping. Must precede the bare
    // "the player" anaphor arm below so the longer seating phrase wins, and the
    // GainControl→GiveControl rewrite receives `recipient: Neighbor` rather than
    // a generic `Any`/`TriggeringPlayer`.
    {
        let (neighbor_filter, rest) = parse_target(subject);
        if rest.trim().is_empty() && matches!(neighbor_filter, TargetFilter::Neighbor { .. }) {
            return subject_filter_application(neighbor_filter, false);
        }
    }
    // CR 608.2c + CR 117.3a: "that player" / "the player" as subject,
    // optionally carrying a "may" modal ("that player may pay {2}").
    // In trigger context (`ctx.subject` is Some — set exclusively by
    // `oracle_trigger.rs::parse_trigger_line` via
    // `extract_trigger_subject_for_context`; non-trigger parse entry points
    // leave it as None), the phrase refers anaphorically to the player from the
    // triggering event (damaged player, casting player, etc.) regardless of
    // whether the trigger subject itself is SelfRef ("~ deals damage to a
    // player") or a typed object. Delegate to the single-authority
    // event-context combinator for the mapping.
    // Outside trigger context, "that player" is the CR 608.2c anaphor to the
    // controller of the object/player target referenced earlier in the same
    // instruction — resolve to TargetFilter::ParentTargetController.
    //
    // Dispatch via the single-authority event-context combinator —
    // `parse_event_context_ref` already recognizes both "that player" and
    // "the player" as TriggeringPlayer. `all_consuming` restricts the match
    // to standalone subject phrases (no trailing text) and restricts the
    // TriggeringPlayer branch here to the two player-referencing forms.
    let player_subject = all_consuming(alt((
        value(
            ("that player", true),
            tag::<_, _, OracleError<'_>>("that player may"),
        ),
        value(("the player", true), tag("the player may")),
        value(("that player", false), tag("that player")),
        value(("the player", false), tag("the player")),
    )))
    .parse(lower.as_str());
    if let Ok((_, (subject_lower, is_optional))) = player_subject {
        let Ok((_, ctx_filter)) = all_consuming(parse_event_context_ref).parse(subject_lower)
        else {
            return None;
        };
        if matches!(ctx_filter, TargetFilter::TriggeringPlayer) {
            // CR 608.2c + CR 109.4 (issue #534): "That player" after a
            // `Choose(Player)`/`Choose(Opponent)` clause binds to the
            // just-chosen player — mirrors the `resolve_they_pronoun`
            // `ChosenPlayer` branch so the "That player <verb>" and "They
            // <verb>" sentence forms produce the same AST (Skullwinder
            // exercises the "That player" form; Gluntch exercises "They").
            let affected = if let Some(scope @ ControllerRef::ChosenPlayer { .. }) =
                &ctx.relative_player_scope
            {
                TargetFilter::Typed(crate::types::ability::TypedFilter {
                    controller: Some(scope.clone()),
                    ..Default::default()
                })
            } else if matches!(ctx.relative_player_scope, Some(ControllerRef::ScopedPlayer)) {
                TargetFilter::ScopedPlayer
            } else if matches!(
                ctx.relative_player_scope,
                Some(ControllerRef::ParentTargetController)
            ) {
                TargetFilter::ParentTargetController
            } else if ctx.subject.is_some() {
                ctx_filter
            } else {
                // CR 608.2c + CR 109.4: Outside trigger context, a bare "that player"
                // subject is an anaphor to the controller of the object/player target
                // referenced earlier in the same instruction (e.g. Volatile Fault's
                // destroyed nonbasic land). Resolve to the parent target's controller,
                // not a generic player. `parent_target_controller` matches
                // TargetRef::Player and TargetRef::Object symmetrically, so
                // player-target cards still resolve to the chosen player.
                TargetFilter::ParentTargetController
            };
            return Some(SubjectApplication {
                affected,
                target: None,
                multi_target: None,
                inherits_parent: false,
                is_optional,
            });
        }
    }
    // CR 109.5 "you" / "your" — the spell or ability's controller. Used as a
    // bare player subject (e.g., "you phase out", "you draw a card"). The
    // imperative resolvers map `TargetFilter::Controller` → the ability's
    // controller player at resolution time.
    if lower == "you" {
        return Some(SubjectApplication {
            affected: TargetFilter::Controller,
            target: None,
            multi_target: None,
            inherits_parent: false,
            is_optional: false,
        });
    }
    // "an opponent" as subject — single opponent (two-player: equivalent to "each opponent").
    if tag::<_, _, OracleError<'_>>("an opponent")
        .parse(lower.as_str())
        .is_ok()
    {
        return subject_filter_application(
            TargetFilter::Typed(TypedFilter::default().controller(ControllerRef::Opponent)),
            false,
        );
    }
    // CR 102.2: In a two-player game, a player's opponent is the other player.
    // Parse both singular/plural bare subject forms via combinators and require
    // full consumption so possessive/modal tails don't get coerced.
    let mut your_opponent_subject = map(
        all_consuming(preceded(
            tag("your "),
            alt((tag("opponents"), tag::<_, _, OracleError<'_>>("opponent"))),
        )),
        |_| TargetFilter::Typed(TypedFilter::default().controller(ControllerRef::Opponent)),
    );
    if let Ok((_, filter)) = your_opponent_subject.parse(lower.as_str()) {
        return subject_filter_application(filter, false);
    }
    // CR 506.3d: "defending player" as subject — resolves from combat state.
    if lower == "defending player" {
        return Some(SubjectApplication {
            affected: TargetFilter::DefendingPlayer,
            target: None,
            multi_target: None,
            inherits_parent: false,
            is_optional: false,
        });
    }
    if lower == "that controller" {
        return Some(SubjectApplication {
            affected: TargetFilter::Controller,
            target: None,
            multi_target: None,
            inherits_parent: false,
            is_optional: false,
        });
    }
    // CR 608.2c + CR 117.3a: "its controller" / "their controller" as anaphoric
    // subject, optionally carrying a "may" modal ("its controller may search
    // their library" — Assassin's Trophy, Path to Exile, Oblation, etc.). When
    // "may" is present, the resulting ability is marked optional so the acting
    // player is offered a yes/no prompt before the effect resolves.
    //
    // Only fires for the subject phrase "its controller may" — bare "its
    // controller" / "their controller" falls through to the RevealUntil-family
    // recognizers in `lower_subject_predicate_ast` (Polymorph, Balustrade Spy,
    // etc.) which already handle the subject-ignorant "reveals cards from the
    // top of their library until …" pattern as RevealUntil.
    if let Ok((after_head, _)) = alt((
        tag::<_, _, OracleError<'_>>("its controller may"),
        tag("their controller may"),
    ))
    .parse(lower.as_str())
    {
        if after_head.trim().is_empty() {
            return Some(SubjectApplication {
                affected: TargetFilter::ParentTargetController,
                target: None,
                multi_target: None,
                inherits_parent: false,
                is_optional: true,
            });
        }
    }
    if lower == "its controller" || lower == "their controller" {
        return Some(SubjectApplication {
            affected: TargetFilter::ParentTargetController,
            target: None,
            multi_target: None,
            inherits_parent: false,
            is_optional: false,
        });
    }
    // CR 608.2c: Definite/anaphoric "[the|that] <noun>'s controller" /
    // "[the|that] <noun>'s owner" — the parent target's controller/owner.
    // Mirrors the generic "the <noun>'s controller" path in `parse_target`
    // (oracle_target.rs) but as a subject-phrase entry-point so subject-shifted
    // clauses like "That creature's controller reveals…" (Proteus Staff,
    // Transmogrify) route to ParentTargetController. Uses nom dispatch on the
    // determiner; the noun-then-suffix structure is verified by a structural
    // `ends_with` check on the remainder (post-tokenization classification, not
    // parsing dispatch).
    if let Ok((after_det, _)) =
        alt((tag::<_, _, OracleError<'_>>("that "), tag("the "))).parse(lower.as_str())
    {
        // structural: not dispatch — the nom `alt(tag(...))` above is the dispatch
        // step that consumes the determiner; this `ends_with` is a post-tokenization
        // structural check that the remaining tail is `<noun>'s controller` /
        // `<noun>'s owner`, mirroring the existing `parse_target` path that uses
        // `find("'s controller")` for the same purpose.
        if after_det.ends_with("'s controller may") // allow-noncombinator: post-tokenized subject suffix classification
            || after_det.ends_with("'s owner may")
        // allow-noncombinator: post-tokenized subject suffix classification
        {
            return Some(SubjectApplication {
                affected: TargetFilter::ParentTargetController,
                target: None,
                multi_target: None,
                inherits_parent: false,
                is_optional: true,
            });
        }
        if after_det.ends_with("'s controller") || after_det.ends_with("'s owner") {
            return Some(SubjectApplication {
                affected: TargetFilter::ParentTargetController,
                target: None,
                multi_target: None,
                inherits_parent: false,
                is_optional: false,
            });
        }
    }
    // Explicit self-reference — always SelfRef.
    // CR 109.3 + CR 201.4b: Gendered pronouns ("he", "she") used as a subject
    // in a card's Oracle text refer to the card itself (modern TMNT/UB cards
    // and legacy flip/legendary cards use humanoid pronouns in place of "it").
    if matches!(lower.as_str(), "~" | "this" | "he" | "she")
        || SELF_REF_PARSE_ONLY_PHRASES.iter().any(|p| lower == *p)
        || SELF_REF_TYPE_PHRASES.iter().any(|p| lower == *p)
    {
        return Some(SubjectApplication {
            affected: TargetFilter::SelfRef,
            target: None,
            multi_target: None,
            inherits_parent: false,
            is_optional: false,
        });
    }
    // CR 608.2k: Bare pronoun "it" — context-dependent. In trigger context,
    // `ctx.subject` identifies the triggering subject. In effect-chain context,
    // `parent_target_available` records that a previous chunk introduced a real
    // typed object referent. Standalone clause parsing leaves it false, so
    // "it connives" remains self-referential instead of inventing ParentTarget.
    if lower == "it" {
        if ctx.subject.is_none() && ctx.parent_target_available {
            return Some(SubjectApplication {
                affected: TargetFilter::ParentTarget,
                target: None,
                multi_target: None,
                inherits_parent: true,
                is_optional: false,
            });
        }
        return Some(SubjectApplication {
            affected: resolve_it_pronoun(ctx),
            target: None,
            multi_target: None,
            inherits_parent: false,
            is_optional: false,
        });
    }
    // CR 608.2k: Bare pronoun "they" — context-dependent.
    // In trigger effects: "they" refers to the triggering player (for player-type
    // subjects like "an opponent") or the triggering source (for object subjects).
    // Outside trigger context: anaphoric reference to previously mentioned objects.
    if lower == "they" {
        return Some(SubjectApplication {
            affected: resolve_they_pronoun(ctx),
            target: None,
            multi_target: None,
            inherits_parent: false,
            is_optional: false,
        });
    }

    // CR 608.2c: "that creature/permanent/land" — anaphoric back-reference to a
    // previously mentioned object in the same effect sequence. Strip "that " and parse
    // the remainder as a type phrase. Covers all "that [type]" patterns generically.
    if let Ok((rest_subject, _)) = tag::<_, _, OracleError<'_>>("that ").parse(lower.as_str()) {
        // CR 608.2c: "that creature/permanent/land" — anaphoric back-reference to a
        // previously mentioned object in the same effect sequence. Strip "that " and parse
        // the remainder as a type phrase. Covers all "that [type]" patterns generically.
        let consumed = lower.len() - rest_subject.len();
        let original_rest = &subject[consumed..];
        let (filter, rem) = parse_type_phrase(original_rest);
        if rem.trim().is_empty() && !matches!(filter, TargetFilter::Any) {
            // CR 603.7c + CR 608.2c: Inside a trigger effect, "that [type]" is an
            // anaphoric back-reference to the triggering event's subject object (the
            // land that was tapped, the creature that was blocked, etc.) — NOT a
            // broadcast over all matching permanents. Set `target: TriggeringSource`
            // so the resolver (extract_event_context_filter in effects/mod.rs) binds
            // the transient effect to the specific triggering object via SpecificObject.
            // Outside triggers, fall back to the type filter (anaphor resolves via
            // `inherits_parent` + ParentTarget at the call site).
            if ctx.subject.is_some() {
                return Some(SubjectApplication {
                    affected: filter,
                    target: Some(TargetFilter::TriggeringSource),
                    multi_target: None,
                    inherits_parent: true,
                    is_optional: false,
                });
            }
            return Some(SubjectApplication {
                affected: filter,
                target: None,
                multi_target: None,
                inherits_parent: true,
                is_optional: false,
            });
        }
    }

    let (filter, rest) = parse_type_phrase(subject);
    if rest.trim().is_empty() {
        return subject_filter_application(filter, false);
    }

    // CR 119.5: Life-total possessive subjects — "your life total",
    // "each player's life total", etc. Map to the player filter so that
    // try_parse_set_life_total can produce the correct SetLifeTotal target.
    if alt((
        tag::<_, _, OracleError<'_>>("your life total"),
        tag("your life totals"),
    ))
    .parse(lower.as_str())
    .is_ok()
    {
        return subject_filter_application(TargetFilter::Controller, false);
    }
    if alt((
        tag::<_, _, OracleError<'_>>("each player's life total"),
        tag("all players' life totals"),
        tag("all players' life total"),
        tag("each player's life totals"),
    ))
    .parse(lower.as_str())
    .is_ok()
    {
        return subject_filter_application(TargetFilter::Any, false);
    }
    if alt((
        tag::<_, _, OracleError<'_>>("that player's life total"),
        tag("the player's life total"),
        tag("their life total"),
    ))
    .parse(lower.as_str())
    .is_ok()
    {
        return subject_filter_application(TargetFilter::ParentTarget, false);
    }

    None
}

pub(super) fn parse_leading_subject_application(
    text: &str,
    ctx: &mut ParseContext,
) -> Option<SubjectApplication> {
    let subject_text = extract_subject_text(text)?;
    parse_subject_application(&subject_text, ctx)
}

/// CR 602.5 + CR 603.2a + CR 608.2c: Resolve the subject of an EFFECT-form
/// "[subject] activated abilities can't be activated" clause.
///
/// The predicate is grammatically a possessive ("its activated abilities"), so
/// the subject is the *possessor* of the abilities, not a standalone noun
/// phrase. `parse_subject_application` does not recognize the bare possessive
/// anaphors "its"/"their" (it handles "it" but not its possessive form). These
/// anaphors back-reference a permanent targeted earlier in the same conjunction
/// (Dovin Baan: "up to one target creature gets -3/-0 and its activated
/// abilities can't be activated") — so they resolve to `ParentTarget`, the same
/// chosen object the sibling pump conjunct targets. This mirrors how the
/// must-be-blocked / extra-blockers conjuncts thread `ParentTarget` onto the
/// trailing combat-requirement clause. Typed subjects ("target creature's",
/// "each creature you control") and the explicit self-reference "~" delegate to
/// `parse_subject_application` for the full grammar.
fn subject_application_for_cant_be_activated(
    subject: &str,
    ctx: &mut ParseContext,
) -> Option<SubjectApplication> {
    let lower = subject.to_lowercase();
    if matches!(
        lower.as_str(),
        "its" | "it" | "their" | "that creature" | "that permanent"
    ) {
        return Some(SubjectApplication {
            affected: TargetFilter::ParentTarget,
            target: Some(TargetFilter::ParentTarget),
            multi_target: None,
            inherits_parent: true,
            is_optional: false,
        });
    }
    // Typed possessor noun phrases carry a trailing "'s" ("target creature's",
    // "~'s", "each creature you control's"). Strip the possessive marker so the
    // remaining noun phrase routes through the full subject grammar.
    let possessor = strip_possessive_subject_suffix(subject);
    parse_subject_application(possessor, ctx)
}

fn strip_possessive_subject_suffix(subject: &str) -> &str {
    type VE<'a> = OracleError<'a>;

    let mut parser = alt((
        all_consuming(terminated(take_until::<_, _, VE>("'s"), tag("'s"))),
        all_consuming(terminated(
            take_until::<_, _, VE>("\u{2019}s"),
            tag("\u{2019}s"),
        )),
    ));

    parser
        .parse(subject)
        .map(|(_, possessor)| possessor.trim())
        .unwrap_or(subject)
}

/// CR 608.2k: Resolve bare pronoun "they" based on parser context.
/// In trigger effects where the subject is a player (e.g., "an opponent"),
/// "they" refers to the triggering player (`TriggeringPlayer`). A player-type
/// trigger subject is identified by having no `type_filters` but a `controller`
/// ref (e.g., `controller: Opponent`). For object-type subjects, "they" refers
/// to the triggering source. Without trigger context, "they" is an anaphoric
/// reference to previously mentioned objects (`ParentTarget`).
fn resolve_they_pronoun(ctx: &mut ParseContext) -> TargetFilter {
    if matches!(ctx.relative_player_scope, Some(ControllerRef::ScopedPlayer)) {
        return TargetFilter::ScopedPlayer;
    }
    // CR 603.7c + CR 120.3 + CR 506.2: A "deals [combat] damage to a player" or
    // "attacks a player" trigger introduces the damaged/attacked player as the
    // event referent (the parser stamps `relative_player_scope = TargetPlayer`).
    // "They" inside such an effect ("they lose half their life") refers to that
    // event player, which auto-resolves from the triggering event
    // (`TriggeringPlayer`) — NOT a chosen target. Without this, "they" fell
    // through to `ParentTarget`, leaving the effect with no player to act on
    // (Unstoppable Slasher's half-life loss silently resolved as "lose 0").
    if matches!(ctx.relative_player_scope, Some(ControllerRef::TargetPlayer)) {
        return TargetFilter::TriggeringPlayer;
    }
    // CR 608.2c + CR 109.4: "They" after a `Choose(Player)` clause refers to
    // the chosen player — a player-only `Typed` filter carrying the chosen
    // scope (Gluntch's "choose a player. They put two +1/+1 counters …").
    if let Some(scope @ ControllerRef::ChosenPlayer { .. }) = &ctx.relative_player_scope {
        return TargetFilter::Typed(crate::types::ability::TypedFilter {
            controller: Some(scope.clone()),
            ..Default::default()
        });
    }
    match &ctx.subject {
        // Player-type trigger subject: no type_filters, has controller ref
        Some(TargetFilter::Typed(tf)) if tf.type_filters.is_empty() && tf.controller.is_some() => {
            TargetFilter::TriggeringPlayer
        }
        Some(TargetFilter::Player) => TargetFilter::TriggeringPlayer,
        // Object-type trigger subject
        Some(subject) if !matches!(subject, TargetFilter::SelfRef | TargetFilter::Any) => {
            TargetFilter::TriggeringSource
        }
        // No trigger context — anaphoric reference to previously mentioned objects
        _ => TargetFilter::ParentTarget,
    }
}

fn subject_filter_application(filter: TargetFilter, targeted: bool) -> Option<SubjectApplication> {
    Some(SubjectApplication {
        target: targeted.then_some(filter.clone()),
        affected: filter,
        multi_target: None,
        inherits_parent: false,
        is_optional: false,
    })
}

/// CR 113.3 + CR 611.2: When a `GenericEffect` carries a target slot
/// (`target: Some(...)`), the embedded static's `affected` filter is the
/// *application* spec, not the *selection* spec. The runtime resolver
/// (`game/effects/effect.rs`) short-circuits on `ability.targets` and binds
/// each transient continuous effect to the chosen object via
/// `SpecificObject`, so the typed selection filter is dead code on that
/// path. Encoding `ParentTarget` here makes the parser output
/// self-documenting and matches the convention used by sibling counter
/// sub_abilities (`PutCounter { target: ParentTarget }`) and the
/// `LastCreated` rewrite for token anaphors.
pub(super) fn static_affected_for_application(application: &SubjectApplication) -> TargetFilter {
    if application.target.is_some() {
        TargetFilter::ParentTarget
    } else {
        application.affected.clone()
    }
}

fn merge_partial_type_phrase_filter(filter: TargetFilter, remainder: &str) -> TargetFilter {
    if remainder.is_empty() {
        return filter;
    }

    let TargetFilter::Typed(mut left) = filter else {
        return filter;
    };
    let (suffix_filter, suffix_remainder) = parse_type_phrase(remainder);
    let TargetFilter::Typed(right) = suffix_filter else {
        return TargetFilter::Typed(left);
    };
    if !suffix_remainder.trim().is_empty() {
        return TargetFilter::Typed(left);
    }

    for type_filter in right.type_filters {
        if !left.type_filters.contains(&type_filter) {
            left.type_filters.push(type_filter);
        }
    }
    if left.controller.is_none() {
        left.controller = right.controller;
    }
    for property in right.properties {
        if !left.properties.contains(&property) {
            left.properties.push(property);
        }
    }
    TargetFilter::Typed(left)
}

/// Build a Pump or PumpAll effect from a subject application and P/T values.
///
/// CR 608.2c: Single-object subject references (`SelfRef`, `TriggeringSource`,
/// `AttachedTo`, `ParentTarget`) identify one specific permanent and must
/// lower to `Effect::Pump`. Only class filters (e.g., `Typed { Creature, You }`)
/// that match multiple permanents lower to `Effect::PumpAll`.
fn build_pump_effect(
    application: &SubjectApplication,
    power: PtValue,
    toughness: PtValue,
) -> Effect {
    if let Some(target) = application.target.clone() {
        return Effect::Pump {
            power,
            toughness,
            target,
        };
    }
    if application.inherits_parent {
        return Effect::Pump {
            power,
            toughness,
            target: TargetFilter::ParentTarget,
        };
    }
    if is_single_object_ref(&application.affected) {
        return Effect::Pump {
            power,
            toughness,
            target: application.affected.clone(),
        };
    }
    Effect::PumpAll {
        power,
        toughness,
        target: application.affected.clone(),
    }
}

/// Returns `true` when a `TargetFilter` refers to exactly one object at
/// resolution time (not a class filter). Used by `build_pump_effect` and other
/// builders that must distinguish single-target from class-targeting effects.
pub(super) fn is_single_object_ref(filter: &TargetFilter) -> bool {
    matches!(
        filter,
        TargetFilter::SelfRef
            | TargetFilter::TriggeringSource
            | TargetFilter::AttachedTo
            | TargetFilter::ParentTarget
    )
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

    // Parse the pump clause first to check whether it carries its own duration.
    let (power, toughness, duration) = super::parse_pump_clause(pump_part)?;

    // Guard: when the pump part has NO duration (e.g., "get +2/+2 and gain flying
    // until end of turn"), the trailing duration is shared across both clauses.
    // Splitting would lose the duration on the pump half, so reject the split and let
    // the continuous-modification fallthrough in build_continuous_clause handle it.
    // When the pump part HAS a duration (e.g., "get +2/+2 until end of turn and gain
    // flying"), the " and " genuinely separates independent clauses, so the split is
    // valid regardless of whether the remainder is a keyword grant.
    if duration.is_none() {
        let (remainder_without_duration, _) = super::strip_trailing_duration(remainder);
        if !parse_continuous_modifications(remainder_without_duration).is_empty() {
            return None;
        }
    }

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
        condition: None,
        optional: false,
        unless_pay: None,
    })
}

fn parse_keyword_choice_grant(predicate: &str) -> Option<(Keyword, Keyword, Option<Duration>)> {
    let lower = predicate.to_lowercase();
    let (choice_text, _) = tag::<_, _, OracleError<'_>>("gain your choice of ")
        .parse(lower.as_str())
        .ok()?;
    let (keyword_text, duration) = super::strip_trailing_duration(choice_text);
    let (_, (left, right)) = nom_primitives::split_once_on(keyword_text.trim(), " or ").ok()?;
    let first = parse_keyword_from_oracle(left.trim())?;
    let second = parse_keyword_from_oracle(right.trim())?;
    Some((first, second, duration.or(Some(Duration::UntilEndOfTurn))))
}

fn keyword_choice_branch(
    keyword: Keyword,
    affected: TargetFilter,
    target: Option<TargetFilter>,
    duration: Option<Duration>,
) -> AbilityDefinition {
    let description = format!("gain {keyword}");
    let mut branch = AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::GenericEffect {
            static_abilities: vec![StaticDefinition::continuous()
                .affected(affected)
                .modifications(vec![ContinuousModification::AddKeyword { keyword }])
                .description(description.clone())],
            duration: duration.clone(),
            target,
        },
    );
    branch.duration = duration;
    branch.description = Some(description);
    branch
}

fn build_keyword_choice_clause(
    application: &SubjectApplication,
    predicate: &str,
) -> Option<ParsedEffectClause> {
    let (first, second, duration) = parse_keyword_choice_grant(predicate)?;
    let affected = static_affected_for_application(application);
    let branches = vec![
        keyword_choice_branch(first, affected.clone(), None, duration.clone()),
        keyword_choice_branch(second, affected, None, duration),
    ];

    let choose_effect = Effect::ChooseOneOf {
        chooser: PlayerFilter::Controller,
        branches,
    };
    let (effect, sub_ability) = if let Some(target) = application.target.clone() {
        let choose = AbilityDefinition::new(AbilityKind::Spell, choose_effect);
        (Effect::TargetOnly { target }, Some(Box::new(choose)))
    } else {
        (choose_effect, None)
    };

    Some(ParsedEffectClause {
        effect,
        duration: None,
        sub_ability,
        distribute: None,
        multi_target: application.multi_target.clone(),
        condition: None,
        optional: false,
        unless_pay: None,
    })
}

fn build_continuous_clause(
    application: SubjectApplication,
    predicate: &str,
) -> Option<ParsedEffectClause> {
    let normalized = deconjugate_verb(predicate);

    // B15: Guard against "becomes" predicates routing through continuous clause parsing.
    // Creature-land animations ("becomes a 3/3 Dinosaur creature with trample") must
    // fall through to try_parse_subject_become_clause for correct animation handling.
    if alt((tag::<_, _, OracleError<'_>>("become "), tag("become\n")))
        .parse(normalized.as_str())
        .is_ok()
    {
        return None;
    }
    if tag::<_, _, OracleError<'_>>("create ")
        .parse(normalized.as_str())
        .is_ok()
    {
        return None;
    }

    // Try the full predicate first (simple pump with no compound).
    if let Some((power, toughness, duration)) = super::parse_pump_clause(&normalized) {
        let effect = build_pump_effect(&application, power, toughness);
        return Some(ParsedEffectClause {
            effect,
            duration,
            sub_ability: None,
            distribute: None,
            multi_target: None,
            condition: None,
            optional: false,
            unless_pay: None,
        });
    }

    // Compound: "get +1/+1 until end of turn and you gain 1 life"
    // Split on " and " that follows a duration marker, producing a pump
    // with a chained sub_ability for the remainder.
    if let Some(clause) = try_split_pump_compound(&normalized, &application) {
        return Some(clause);
    }

    if let Some(clause) = build_keyword_choice_clause(&application, &normalized) {
        return Some(clause);
    }

    // Strip "where X is..." and "for each..." suffixes before extracting duration,
    // so "until end of turn" is found even when followed by these clauses.
    // The full normalized text is still passed to parse_continuous_modifications
    // which handles "where X is" and "for each" internally.
    let norm_lower = normalized.to_lowercase();
    let norm_tp = TextPair::new(&normalized, &norm_lower);
    let (without_where, _) = super::strip_trailing_where_x(norm_tp);
    let duration_source = strip_for_each_for_duration(without_where.original);
    let (_, duration) = super::strip_trailing_duration(duration_source);

    let (predicate_text, fallback_duration) = super::strip_trailing_duration(&normalized);
    let duration = duration.or(fallback_duration);

    let modifications = parse_continuous_modifications(predicate_text);
    if modifications.is_empty() {
        return None;
    }

    // CR 702.62b + CR 611.2a + CR 611.2c: A "gains suspend" grant onto an exiled
    // card has no turn-scoped expiry — a card stays suspended (exiled, has suspend,
    // has a time counter) until its last time counter is removed (CR 702.62b). CR
    // 611.2a: a continuous effect with no stated duration lasts until end of game.
    // Unlike an ordinary "gains <keyword>" combat trick (correctly UntilEndOfTurn
    // via the chain default in effect.rs), the suspend grant's lifetime is owned by
    // the suspend mechanic, so its parsed duration is Permanent. Keyed on the typed
    // Keyword::Suspend variant — never a string. Mirrors the build_become_clause
    // precedent (CR 611.2b default-permanent).
    let duration = if matches!(
        modifications.as_slice(),
        [ContinuousModification::AddKeyword {
            keyword: crate::types::keywords::Keyword::Suspend { .. },
        }]
    ) {
        Some(Duration::Permanent)
    } else {
        duration
    };

    if let Some((power, toughness)) = extract_pump_modifiers(&modifications) {
        let effect = build_pump_effect(&application, power, toughness);
        return Some(ParsedEffectClause {
            effect,
            duration,
            sub_ability: None,
            distribute: None,
            multi_target: None,
            condition: None,
            optional: false,
            unless_pay: None,
        });
    }

    let affected = static_affected_for_application(&application);
    let static_abilities =
        if nom_primitives::scan_contains(&predicate_text.to_lowercase(), "if able") {
            let synthetic_line = format!("Creatures {}.", predicate_text.trim_end_matches('.'));
            let mut split_defs = parse_static_line_multi(&synthetic_line);
            if split_defs.len() > 1 {
                for def in &mut split_defs {
                    def.affected = Some(affected.clone());
                    def.description = Some(predicate_text.to_string());
                }
                split_defs
            } else {
                vec![StaticDefinition::continuous()
                    .affected(affected)
                    .modifications(modifications)
                    .description(predicate_text.to_string())]
            }
        } else {
            vec![StaticDefinition::continuous()
                .affected(affected)
                .modifications(modifications)
                .description(predicate_text.to_string())]
        };

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
        condition: None,
        optional: false,
        unless_pay: None,
    })
}

/// Strip "for each [clause]" suffix from text so that duration extraction can find
/// "until end of turn" that precedes it. Returns the text up to "for each" (or the
/// original text if "for each" is not present). Only used for duration extraction —
/// the full text is still passed to `parse_continuous_modifications` which handles
/// "for each" clauses internally.
fn strip_for_each_for_duration(text: &str) -> &str {
    let lower = text.to_lowercase();
    // Find " for each " — must have space before to avoid matching "before each"
    if let Some(pos) = lower.find(" for each ") {
        text[..pos].trim()
    } else {
        text
    }
}

/// CR 611.2b + CR 707.9: Strip a duration phrase that appears immediately
/// before a `, except` clause (Sarkhan, Soul Aflame:
/// `"a copy of it until end of turn, except its name is ~ ..."`).
///
/// `strip_trailing_duration` only matches end-of-string durations; this helper
/// fills the gap for the BecomeCopy class where the except clause shifts the
/// duration away from the suffix. Returns `(rebuilt_text_without_duration,
/// Some(d))` (head + ", except <body>") when a recognised duration is found
/// between an object phrase and ", except"; otherwise returns
/// `(text.to_string(), None)` so callers can fall back to the prior duration.
fn strip_pre_except_duration(text: &str) -> (String, Option<Duration>) {
    use nom::combinator::eof;
    let lower = text.to_lowercase();
    // Locate the `, except` boundary via the canonical nom-built primitive.
    // Returns `(head_lower, ", except<...>")` with `head_lower` containing
    // everything before the boundary. When no boundary exists the text has
    // no except clause and there's nothing to do.
    let Ok((_, (head_lower, _))) = nom_primitives::split_once_on(&lower, ", except") else {
        return (text.to_string(), None);
    };
    let except_pos = head_lower.len();
    // Each duration phrase is a leaf-level `tag` on the lowercase suffix.
    // The duration "ends at" the comma exactly when the tag, followed by
    // `eof`, consumes the head text from some byte offset. Scan forward at
    // word boundaries inside `head_lower` and try the tag-then-eof
    // combinator at each — the first match wins.
    let duration_alt = |i| -> nom::IResult<&str, Duration, OracleError<'_>> {
        alt((
            value(Duration::UntilEndOfTurn, tag(" until end of turn")),
            value(Duration::UntilEndOfTurn, tag(" this turn")),
            // CR 514.2: "until the end of your next turn" persists through
            // that turn's cleanup step.
            value(
                Duration::UntilEndOfNextTurnOf {
                    player: PlayerScope::Controller,
                },
                tag(" until the end of your next turn"),
            ),
            value(
                Duration::UntilNextTurnOf {
                    player: PlayerScope::Controller,
                },
                tag(" until your next turn"),
            ),
            // CR 514.2: third-person next-turn duration in granted-effect
            // clauses follows the same controller/grantee binding.
            value(
                Duration::UntilEndOfNextTurnOf {
                    player: PlayerScope::Controller,
                },
                tag(" until the end of their next turn"),
            ),
            value(
                Duration::UntilNextTurnOf {
                    player: PlayerScope::Controller,
                },
                tag(" until their next turn"),
            ),
        ))
        .parse(i)
    };
    for (idx, byte) in head_lower.bytes().enumerate() {
        if byte != b' ' {
            continue;
        }
        if let Ok((rest, duration)) = duration_alt(&head_lower[idx..]) {
            if eof::<_, OracleError<'_>>(rest).is_ok() {
                let head = text[..idx].trim_end();
                let tail = &text[except_pos..];
                return (format!("{head}{tail}"), Some(duration));
            }
        }
    }
    (text.to_string(), None)
}

fn build_become_clause(
    application: SubjectApplication,
    predicate: &str,
    ctx: &mut ParseContext,
) -> Option<ParsedEffectClause> {
    let normalized = deconjugate_verb(predicate);
    let (predicate, duration) = super::strip_trailing_duration(&normalized);
    // CR 725.1: "become the monarch" sets the monarch designation, not an animation.
    let predicate_lower = predicate.to_lowercase();
    let (become_rest, _) = tag::<_, _, OracleError<'_>>("become ")
        .parse(predicate_lower.as_str())
        .ok()?;
    let consumed = predicate_lower.len() - become_rest.len();
    let become_text = predicate[consumed..].trim();
    if become_text.eq_ignore_ascii_case("the monarch") {
        return Some(super::parsed_clause(Effect::BecomeMonarch));
    }
    // CR 611.2b: "Becomes" effects without explicit duration are permanent
    let duration = duration.or(Some(Duration::Permanent));

    // CR 119.5: "life total becomes N" — set life total to a specific number.
    // Must intercept before parse_animation_spec which tokenizes each word as a subtype.
    if let Some(clause) = try_parse_set_life_total(become_text, &application) {
        return Some(clause);
    }

    // CR 730.1: "it becomes night" / "it becomes day" — set game day/night designation.
    // Must intercept before parse_animation_spec which produces AddSubtype("Night"/"Day").
    if let Some(clause) = try_parse_set_day_night(become_text) {
        return Some(clause);
    }

    // CR 205.3 / CR 305.7: "become the [type] of your choice" — player chooses a subtype.
    // Must intercept before parse_animation_spec which rejects "of your choice" patterns.
    if let Some(clause) = try_parse_become_choice(become_text, &application, duration.clone()) {
        return Some(clause);
    }

    // CR 205.3e + CR 607.2d: "becomes that type" applies the creature type chosen
    // by the preceding "Choose a creature type" instruction in the same ability
    // (Imagecrafter, Unnatural Selection, Mistform Mutant, Standardize). Unlike
    // the "of your choice" arm above, the choice is already made upstream, so this
    // emits only the apply half — a continuous `AddChosenSubtype` that reads the
    // source's chosen creature type at resolution. Must intercept before
    // parse_animation_spec, which would mis-tokenize "that"/"type" as subtypes.
    if become_text.eq_ignore_ascii_case("that type") {
        let affected = static_affected_for_application(&application);
        let effect = Effect::GenericEffect {
            static_abilities: vec![StaticDefinition::continuous()
                .affected(affected)
                .modifications(vec![ContinuousModification::AddChosenSubtype {
                    kind: ChosenSubtypeKind::CreatureType,
                }])
                .description(become_text.to_string())],
            duration: duration.clone(),
            target: application.target.clone(),
        };
        return Some(ParsedEffectClause {
            effect,
            duration,
            sub_ability: None,
            distribute: None,
            multi_target: None,
            condition: None,
            optional: false,
            unless_pay: None,
        });
    }

    // CR 702.xxx: Prepare (Strixhaven) — "becomes prepared" / "becomes
    // unprepared" toggles the PreparedState on the target creature. Must
    // intercept before parse_animation_spec which would try to classify
    // "prepared" / "unprepared" as a subtype. `all_consuming` enforces that
    // the matched tag covers the full `become_text` trailer; longer-match
    // alternative is listed first so "unprepared" doesn't get shadowed by
    // "prepared". Assign when WotC publishes SOS CR update.
    #[derive(Clone, Copy)]
    enum PreparedKind {
        Prepared,
        Unprepared,
    }
    let become_lower = become_text.trim().to_lowercase();
    if let Ok((_, kind)) = all_consuming(alt((
        value(
            PreparedKind::Unprepared,
            tag::<_, _, OracleError<'_>>("unprepared"),
        ),
        value(PreparedKind::Prepared, tag("prepared")),
    )))
    .parse(become_lower.as_str())
    {
        let target = application
            .target
            .clone()
            .unwrap_or(crate::types::ability::TargetFilter::ParentTarget);
        let effect = match kind {
            PreparedKind::Prepared => Effect::BecomePrepared { target },
            PreparedKind::Unprepared => Effect::BecomeUnprepared { target },
        };
        return Some(super::parsed_clause(effect));
    }

    // CR 707.2 / CR 613.1a: "become a copy of [target]" — copy copiable characteristics.
    // Must intercept before parse_animation_spec which rejects "copy of" patterns.
    //
    // Mirrors `parse_clone_replacement` in `oracle_replacement.rs` but for the
    // triggered / spell-effect form. Both paths produce `Effect::BecomeCopy`
    // with the same `additional_modifications` shape; the only grammatical
    // difference is the trigger frame ("Irma becomes a copy of …") vs the
    // replacement frame ("you may have ~ enter as a copy of …"). The shared
    // `, except <body>` clause parser (CR 707.9) lives in the
    // `become_copy_except` module so the trigger and replacement paths
    // contribute to the same building block.
    if let Ok((after_copy, _)) =
        tag::<_, _, OracleError<'_>>("a copy of ").parse(become_lower.as_str())
    {
        // CR 611.2b + CR 707.9: Sarkhan-class triggers carry a mid-sentence
        // duration directly before the optional ", except <body>" clause
        // ("become a copy of it **until end of turn**, except its name is ~ ...").
        // `strip_trailing_duration` at the start of `build_become_clause`
        // only strips end-of-string durations; here we extract the duration
        // from the position just before `, except`. Any duration found
        // overrides the default `Permanent` so the copy effect expires
        // correctly. Falls through to (text.to_string(), None) when no
        // mid-sentence duration is present (Irma class).
        let (after_copy_owned, mid_sentence_duration) = strip_pre_except_duration(after_copy);
        let duration = mid_sentence_duration.map(Some).unwrap_or(duration);

        // `parse_target` lower-cases internally; pass it the lowercase tail so
        // its returned remainder is also lowercase (we'll feed that to
        // `parse_except_clause` whose tags are lowercase).
        let (target, remainder) = parse_target(&after_copy_owned);
        // CR 707.9: optional `, except <body> [and <body>]*`. The card name
        // for any SetName override comes from the parse context (set by
        // `parse_oracle_text`). When `ctx.card_name` is `None` or empty
        // (e.g. a test calling the chain parser without threading a card
        // name), the body parser's `parse_name_override` arm declines —
        // emitting `SetName { name: "" }` would silently set `obj.name = ""`
        // at Layer 1, strictly worse than dropping the override entirely.
        let card_name = ctx.card_name.as_deref().unwrap_or("");
        let additional_modifications =
            super::become_copy_except::parse_except_clause(remainder, card_name, ctx)
                .map(|(_, mods)| mods)
                .unwrap_or_default();
        return Some(ParsedEffectClause {
            effect: Effect::BecomeCopy {
                target,
                duration: duration.clone(),
                mana_value_limit: None,
                additional_modifications,
            },
            duration,
            sub_ability: None,
            distribute: None,
            multi_target: None,
            condition: None,
            optional: false,
            unless_pay: None,
        });
    }

    if let Some(clause) = try_parse_become_and_attack_if_able(&application, become_text, ctx) {
        return Some(clause);
    }

    let (become_text, name_override) = strip_become_name_override(become_text);
    let animation = parse_animation_spec(&become_text, ctx)?;
    // CR 205.1a vs CR 205.1b: a "becomes a [type]" effect REPLACES the creature's
    // subtypes (so e.g. a Human Soldier that becomes a Frog is only a Frog) unless
    // it says "in addition to its other types", which stays additive. Mirrors the
    // static type-change path's suffix detection.
    let is_additive = has_in_addition_to_other_types(&become_text);
    let mut modifications = animation_modifications_with_replacement(&animation, is_additive);
    for modification in parse_continuous_modifications(predicate) {
        if !modifications.contains(&modification) {
            modifications.push(modification);
        }
    }
    let modifications = if let Some(name) = name_override {
        let mut with_name = Vec::with_capacity(modifications.len() + 1);
        with_name.push(ContinuousModification::SetName { name });
        with_name.extend(modifications);
        with_name
    } else {
        modifications
    };
    if modifications.is_empty() {
        return None;
    }

    let affected = static_affected_for_application(&application);
    Some(ParsedEffectClause {
        effect: Effect::GenericEffect {
            static_abilities: vec![StaticDefinition::continuous()
                .affected(affected)
                .modifications(modifications)
                .description(predicate.to_string())],
            duration: duration.clone(),
            target: application.target,
        },
        duration,
        sub_ability: None,
        distribute: None,
        multi_target: None,
        condition: None,
        optional: false,
        unless_pay: None,
    })
}

fn strip_become_name_override(text: &str) -> (String, Option<String>) {
    let lower = text.to_lowercase();
    let tp = TextPair::new(text, &lower);
    let Some((before, after)) = tp.split_around(" named ") else {
        return (text.to_string(), None);
    };
    let name = after.original.trim().trim_end_matches('.').to_string();
    if name.is_empty() {
        (before.original.trim().to_string(), None)
    } else {
        (before.original.trim().to_string(), Some(name))
    }
}

fn try_parse_become_and_attack_if_able(
    application: &SubjectApplication,
    become_text: &str,
    ctx: &mut ParseContext,
) -> Option<ParsedEffectClause> {
    let lower = become_text.to_lowercase();
    let (before_attack, attack_duration, rest) = nom_primitives::scan_preceded(&lower, |i| {
        preceded(
            tag::<_, _, OracleError<'_>>("and "),
            parse_attack_if_able_duration,
        )
        .parse(i)
    })?;
    if !rest.trim().trim_end_matches('.').is_empty() {
        return None;
    }

    let animation_text = become_text[..before_attack.trim_end().len()].trim();
    let (animation_text, animation_duration) = super::strip_trailing_duration(animation_text);
    let animation_duration = animation_duration?;
    let animation = parse_animation_spec(animation_text, ctx)?;
    // CR 205.1a: non-additive "becomes a [type]" replaces subtypes.
    let is_additive = has_in_addition_to_other_types(animation_text);
    let modifications = animation_modifications_with_replacement(&animation, is_additive);
    if modifications.is_empty() {
        return None;
    }

    let affected = static_affected_for_application(application);
    let attack_effect = Effect::GenericEffect {
        static_abilities: vec![StaticDefinition::new(StaticMode::MustAttack)
            .affected(affected.clone())
            .description("attacks if able".to_string())],
        duration: Some(attack_duration.clone()),
        target: application.target.clone(),
    };

    Some(ParsedEffectClause {
        effect: Effect::GenericEffect {
            static_abilities: vec![StaticDefinition::continuous()
                .affected(affected)
                .modifications(modifications)
                .description(animation_text.to_string())],
            duration: Some(animation_duration.clone()),
            target: application.target.clone(),
        },
        duration: Some(animation_duration),
        sub_ability: Some(Box::new(AbilityDefinition::new(
            AbilityKind::Spell,
            attack_effect,
        ))),
        distribute: None,
        multi_target: application.multi_target.clone(),
        condition: None,
        optional: false,
        unless_pay: None,
    })
}

fn parse_attack_if_able_duration(input: &str) -> OracleResult<'_, Duration> {
    // verb axis × phase axis (PATTERNS.md §8b): factor "attack(s)" out front,
    // then map the phase clause to its duration ("this turn" → end of turn,
    // "this/that combat" → end of combat).
    let (rest, _) = alt((tag("attacks"), tag("attack"))).parse(input)?;
    preceded(
        tag(" "),
        alt((
            value(Duration::UntilEndOfTurn, tag("this turn if able")),
            value(
                Duration::UntilEndOfCombat,
                alt((tag("this combat if able"), tag("that combat if able"))),
            ),
        )),
    )
    .parse(rest)
}

/// CR 119.5: Parse "life total becomes N" into SetLifeTotal effect.
/// Handles: "half that player's starting life total", numeric amounts,
/// "their starting life total", and any other quantity the general quantity
/// parser recognizes (e.g. "the highest/lowest life total among all players").
fn try_parse_set_life_total(
    become_text: &str,
    application: &SubjectApplication,
) -> Option<ParsedEffectClause> {
    let full_lower = become_text.to_lowercase();
    // CR 119.5: "life total becomes equal to <quantity>" — strip the optional
    // "equal to" connector via a nom combinator so the quantity parser below
    // sees the bare quantity ("equal to your starting life total" → "your
    // starting life total"; Oketra's Last Mercy, Resolute Archangel). Forms
    // without the connector ("becomes half ...", "becomes 10") pass through
    // unchanged because `opt` never fails.
    let lower = opt(tag::<_, _, OracleError<'_>>("equal to "))
        .parse(full_lower.as_str())
        .map_or(full_lower.as_str(), |(rest, _)| rest)
        .trim();

    let amount = if nom_primitives::scan_contains(lower, "starting life total") {
        let amount_text = lower.trim().trim_end_matches('.');
        let (rest, amount) = nom_quantity::parse_quantity(amount_text).ok()?;
        if !rest.trim().is_empty() {
            return None;
        }
        amount
    } else if let Some((n, rest)) = parse_number(lower) {
        // Guard: reject if substantial text remains after the number.
        // "a 3/3 red goblin creature" matches "a" as 1 but the rest
        // "3/3 red goblin creature" indicates this is an animation, not
        // a life total. Genuine life total patterns: "10", "1", bare numbers.
        let rest_trimmed = rest.trim().trim_end_matches('.');
        if !rest_trimmed.is_empty() {
            return None;
        }
        QuantityExpr::Fixed { value: n as i32 }
    } else {
        // CR 119.5: the new life total may be a dynamic quantity rather than a
        // fixed number — e.g. "the highest/lowest life total among all players"
        // (Repay in Kind, Arbiter of Knollridge, Mortal Flesh Is Weak). Route
        // the whole RHS through the general quantity parser so every
        // "life total becomes <quantity>" card composes. `parse_cda_quantity`
        // returns `Some` only when it fully consumes the phrase, so an
        // unrecognized trailer yields `None` here — no false positives.
        oracle_quantity::parse_cda_quantity(lower)?
    };

    // CR 119.5: Use the parsed target if targeted ("target player's life total"),
    // otherwise fall back to the subject's affected filter ("each player's life total"
    // → affected=Any which correctly targets all players for a life-setting effect).
    let target = application
        .target
        .clone()
        .unwrap_or_else(|| application.affected.clone());
    Some(ParsedEffectClause {
        effect: Effect::SetLifeTotal { target, amount },
        duration: None,
        sub_ability: None,
        distribute: None,
        multi_target: None,
        condition: None,
        optional: false,
        unless_pay: None,
    })
}

/// CR 730.1: Parse "night" / "day" after "becomes" into SetDayNight effect.
/// Accepts a trailing "as ~ enters" timing qualifier and ignores it.
fn try_parse_set_day_night(become_text: &str) -> Option<ParsedEffectClause> {
    let lower = become_text.to_lowercase();
    let (_, to) = alt((
        value(DayNight::Night, tag::<_, _, OracleError<'_>>("night")),
        value(DayNight::Day, tag::<_, _, OracleError<'_>>("day")),
    ))
    .parse(lower.trim_start())
    .ok()?;

    Some(super::parsed_clause(Effect::SetDayNight { to }))
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
        (ChoiceType::color(), ContinuousModification::AddChosenColor)
    } else {
        return None;
    };

    // Two-step: Choose (prompts player) → GenericEffect (applies chosen subtype).
    let affected = static_affected_for_application(application);
    let apply_effect = Effect::GenericEffect {
        static_abilities: vec![StaticDefinition::continuous()
            .affected(affected)
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
        condition: None,
        optional: false,
        unless_pay: None,
    })
}

/// CR 119.7 + CR 119.8: Map the possessive subject of a "life total can't change"
/// clause to the player-scope filter for the resulting CantGainLife/CantLoseLife
/// statics. Recognizes opponent possessives ("an opponent's", "your opponents'",
/// "each opponent's"), the self possessive ("your"), and falls back to all
/// players for plural-player possessives ("players'", "each player's").
///
/// Opponent forms are checked first so "your opponents'" is not misclassified as
/// "your" (self-scope).
fn life_lock_scope_from_possessor(possessor_lower: &str) -> TargetFilter {
    if nom_primitives::scan_contains(possessor_lower, "opponent's")
        || nom_primitives::scan_contains(possessor_lower, "opponents'")
    {
        return TargetFilter::Typed(TypedFilter::default().controller(ControllerRef::Opponent));
    }
    if nom_primitives::scan_contains(possessor_lower, "your") {
        return TargetFilter::Typed(TypedFilter::default().controller(ControllerRef::You));
    }
    // "Players'" / "each player's" / unrecognized → all players.
    TargetFilter::Typed(TypedFilter::default())
}

/// CR 119.7 + CR 119.8: Build a `GenericEffect` carrying both `CantGainLife`
/// and `CantLoseLife` statics for a "[possessor] life total can't change"
/// clause. The `AddStaticMode` modifications mirror the `CantUntap` pattern
/// in `build_restriction_clause` so duration-scoped life-lock propagates
/// through transient continuous effects (essential for Teferi's Protection,
/// which is an instant rather than a permanent).
fn build_life_lock_clause(scope_filter: TargetFilter) -> ParsedEffectClause {
    let make_static = |mode: StaticMode| -> StaticDefinition {
        StaticDefinition::new(mode.clone())
            .affected(scope_filter.clone())
            .modifications(vec![ContinuousModification::AddStaticMode { mode }])
    };
    ParsedEffectClause {
        effect: Effect::GenericEffect {
            static_abilities: vec![
                make_static(StaticMode::CantGainLife),
                make_static(StaticMode::CantLoseLife),
            ],
            // Duration left unset — the parent chain parser injects the shared
            // "Until your next turn" duration when the clause appears under a
            // leading "Until X, A, B, and C." sentence. Permanents (Platinum
            // Emperion-style) take the bare-static path in `oracle_static.rs`
            // instead and don't reach this function.
            duration: None,
            target: None,
        },
        distribute: None,
        multi_target: None,
        duration: None,
        sub_ability: None,
        condition: None,
        optional: false,
        unless_pay: None,
    }
}

fn build_restriction_clause(
    application: SubjectApplication,
    predicate: &str,
) -> Option<ParsedEffectClause> {
    let normalized = deconjugate_verb(predicate);
    let (predicate, duration) = super::strip_trailing_duration(&normalized);
    let lower = predicate.to_lowercase();

    // CR 702.18a / 702.11a: a duration-scoped "can't be the target [of ...]" grant
    // on a subject/target (Vines of Vastwood: "target creature can't be the target
    // of spells or abilities your opponents control this turn") is Shroud / Hexproof.
    // Emit the keyword grant so the targeting check applies the correct controller
    // scope (Hexproof leaves the controller able to target), reusing the enforced
    // keyword path rather than a scope-less rule static.
    if let Some(scope) = crate::parser::oracle_keyword::classify_cant_be_targeted(&lower) {
        let keyword = match scope {
            crate::parser::oracle_keyword::CantBeTargetedScope::AnyPlayer => {
                crate::types::keywords::Keyword::Shroud
            }
            crate::parser::oracle_keyword::CantBeTargetedScope::OpponentsOnly => {
                crate::types::keywords::Keyword::Hexproof
            }
        };
        let static_def = StaticDefinition::continuous()
            .affected(static_affected_for_application(&application))
            .modifications(vec![ContinuousModification::AddKeyword { keyword }])
            .description(predicate.to_string());
        return Some(ParsedEffectClause {
            effect: Effect::GenericEffect {
                static_abilities: vec![static_def],
                duration: duration.clone(),
                target: application.target,
            },
            duration,
            sub_ability: None,
            distribute: None,
            multi_target: None,
            condition: None,
            optional: false,
            unless_pay: None,
        });
    }

    // CR 508.1d / CR 509.1a: Restriction predicates for attack/block/target.
    // Compound restrictions ("can't attack or block") produce multiple StaticDefinition entries.
    let modes = parse_restriction_modes(&lower)?;

    // CR 502.3: "doesn't untap during its controller's next untap step" —
    // override duration to UntilControllerNextUntapStep when the predicate
    // contains "next untap step". Also inject AddStaticMode modification so
    // the transient continuous effect system can enforce it.
    let has_next_untap = normalized.to_lowercase().contains("next untap step")
        || predicate.to_lowercase().contains("next untap step");
    let duration = if has_next_untap && modes.iter().any(|m| matches!(m, StaticMode::CantUntap)) {
        Some(Duration::UntilNextStepOf {
            step: Phase::Untap,
            player: PlayerScope::Controller,
        })
    } else {
        duration
    };

    let affected = static_affected_for_application(&application);
    // CR 119.7 + CR 119.8 + CR 104.2b + CR 104.3b + CR 305.1: Player-scoped
    // life, game-state, and land-play restriction modes (Everybody Lives!:
    // "Players can't lose life this turn and players can't lose the game or
    // win the game this turn."; Pardic Miner's activated form is target-
    // scoped and routes through the `target.is_some()` branch — but the
    // bare-subject sentence form "Players can't play lands" still needs
    // player-fan-out, so `CantPlayLand` participates here too.) These modes
    // must bind to actual players, not be broadcast over battlefield
    // permanents. Rewrite an unscoped `Typed(empty)` affected filter — the
    // canonical form produced by the bare "Players" subject — to
    // `TargetFilter::Player` so `register_transient_effect` fans the modes
    // out as per-player TCEs.  Controller-scoped subjects ("you") already
    // produce `TargetFilter::Controller`, which the resolver routes to
    // `SpecificPlayer { id: controller }` without further intervention.
    let all_modes_are_player_scoped = !modes.is_empty()
        && modes.iter().all(|m| {
            matches!(
                m,
                StaticMode::CantGainLife
                    | StaticMode::CantLoseLife
                    | StaticMode::CantLoseTheGame
                    | StaticMode::CantWinTheGame
            ) || matches!(m, StaticMode::Other(name) if name == "CantPlayLand")
        });
    let affected = if all_modes_are_player_scoped {
        match &affected {
            TargetFilter::Typed(t) if t.type_filters.is_empty() && t.controller.is_none() => {
                TargetFilter::Player
            }
            _ => affected,
        }
    } else {
        affected
    };
    let static_abilities = modes
        .into_iter()
        .map(|mode| {
            let mut def = StaticDefinition::new(mode.clone())
                .affected(affected.clone())
                .description(predicate.to_string());
            // CR 613.2 layer 6 + CR 509.1b (issue #327): Combat/untap restriction
            // modes granted to a target need AddStaticMode so the layer system
            // propagates them onto the granted creature's `static_definitions`
            // — without it, the transient continuous effect carries empty
            // modifications and the runtime block / attack check never sees
            // the rule. Unconditional on duration: a leading "Until your
            // next turn, ..." clause is duration-stripped by `peel_clause`
            // before `build_restriction_clause` runs, so `duration` here can
            // be `None` even when the restriction is duration-scoped — the
            // peeled duration is reapplied via `with_clause_duration` on the
            // outer clause. The injection is intrinsic to the mode, not the
            // duration: intrinsic statics never reach this grant path
            // (`build_restriction_clause` is the subject-predicate route).
            if static_mode_needs_grant_propagation(&mode) {
                def = def.modifications(vec![ContinuousModification::AddStaticMode {
                    mode: mode.clone(),
                }]);
            }
            def
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
        condition: None,
        optional: false,
        unless_pay: None,
    })
}

// CR 613.2 layer 6 + CR 509.1b: Combat / untap restriction modes granted
// to a target need `AddStaticMode` so the layer system propagates them
// onto the granted creature's `static_definitions`.
// CR 119.7 + CR 119.8 + CR 104.2b + CR 104.3b: Player-scoped life and
// game-state restriction modes (Everybody Lives!, Skullcrack, Teferi's
// Protection-style life locks scoped at the spell layer) must also carry
// `AddStaticMode` so the transient continuous effect system propagates
// them through to runtime queries — without this, the resolver creates a
// TCE with empty modifications and `player_has_cant_lose` /
// `player_has_cant_gain_life` never see it.
pub(crate) fn static_mode_needs_grant_propagation(mode: &StaticMode) -> bool {
    // CR 305.1 + CR 611.1: Player-scoped land-play prohibition (Pardic Miner,
    // Turf Wound, Solfatara, Moonhold: "Target player can't play lands this
    // turn"). Without `AddStaticMode`, the resolver registers a transient
    // continuous effect with empty modifications and
    // `player_has_static_other(..., "CantPlayLand")` never observes it.
    // Mirrors the player-scoped life/game prohibitions below for the
    // named-string ("Other") family. Other `Other(...)` modes (CantBeSacrificed,
    // CantBeEnchanted, etc.) intentionally remain object-scoped and are
    // checked via `object_has_static_other` rather than transient TCEs.
    if matches!(mode, StaticMode::Other(name) if name == "CantPlayLand") {
        return true;
    }
    matches!(
        mode,
        StaticMode::CantBlock
            | StaticMode::CantAttack
            | StaticMode::CantAttackOrBlock
            | StaticMode::CantCrew
            | StaticMode::CantBeBlocked
            | StaticMode::CantBeBlockedBy { .. }
            | StaticMode::CantBeBlockedExceptBy { .. }
            | StaticMode::CantUntap
            | StaticMode::CantGainLife
            | StaticMode::CantLoseLife
            | StaticMode::CantLoseTheGame
            | StaticMode::CantWinTheGame
            // CR 701.19c: CantBeRegenerated is granted to a target/anaphor creature
            // and must propagate onto its `static_definitions` so the regen-shield
            // bypass in replacement.rs::destroy_applier observes it via
            // active_static_definitions.
            | StaticMode::CantBeRegenerated
            // CR 702.18a: CantBeTargeted (the descriptive Shroud form) is granted to
            // a subject/target creature and must propagate onto its
            // `static_definitions` so the targeting check in `targeting.rs::can_target`
            // observes it via active_static_definitions.
            | StaticMode::CantBeTargeted
    )
}

/// Parse restriction predicates into one or more `StaticMode` variants.
/// Handles simple ("can't block") and compound ("can't attack or block") patterns.
pub(crate) fn parse_restriction_modes(lower: &str) -> Option<Vec<StaticMode>> {
    // CR 701.21: "~ can't be sacrificed" — prohibition on sacrifice.
    if lower == "can't be sacrificed" || lower == "cannot be sacrificed" {
        return Some(vec![StaticMode::Other("CantBeSacrificed".to_string())]);
    }
    // CR 702.5: "~ can't be enchanted [by other auras]" — aura attachment prohibition.
    if lower == "can't be enchanted"
        || lower == "cannot be enchanted"
        || lower == "can't be enchanted by other auras"
        || lower == "cannot be enchanted by other auras"
    {
        return Some(vec![StaticMode::Other("CantBeEnchanted".to_string())]);
    }
    // CR 702.6: "~ can't be equipped" — equipment attachment prohibition.
    if lower == "can't be equipped" || lower == "cannot be equipped" {
        return Some(vec![StaticMode::Other("CantBeEquipped".to_string())]);
    }
    // CR 701.3 + CR 702.5 + CR 702.6: "can't be equipped or enchanted" compound —
    // binds to both attach-type prohibitions. Fortifications are excluded by the
    // Oracle wording, so we do NOT emit CantBeAttached (which is a superset).
    if lower == "can't be equipped or enchanted" || lower == "cannot be equipped or enchanted" {
        return Some(vec![
            StaticMode::Other("CantBeEquipped".to_string()),
            StaticMode::Other("CantBeEnchanted".to_string()),
        ]);
    }
    // CR 701.27: "~ can't transform" — prohibition on transform (e.g., Immerwolf).
    if lower == "can't transform" || lower == "cannot transform" {
        return Some(vec![StaticMode::Other("CantTransform".to_string())]);
    }
    // CR 701.19c: "~ can't be regenerated" — marks the subject so regeneration
    // shields are not applied. Backstop for the "cannot" phrasing and any caller
    // that routes through the generic " can't " / " cannot " split before
    // reaching the dedicated arm in `try_parse_subject_restriction_clause`.
    if parse_cant_be_regenerated_predicate(lower.trim()).is_ok() {
        return Some(vec![StaticMode::CantBeRegenerated]);
    }
    // CR 101.2: Spell/ability restriction predicate; the subject path owns
    // the "spells you control" / "green spells you control" grammar.
    if lower == "can't be countered" || lower == "cannot be countered" {
        return Some(vec![StaticMode::CantBeCountered]);
    }
    // Simple restrictions
    if lower == "can't block" || lower == "cannot block" {
        return Some(vec![StaticMode::CantBlock]);
    }
    // "can't block this creature" / "can't block ~" — source-referential variant used in
    // activated abilities; grants CantBlock to the targeted creature (CR 509.1a).
    if let Ok((rest, _)) = alt((
        tag::<_, _, OracleError<'_>>("can't block "),
        tag("cannot block "),
    ))
    .parse(lower)
    {
        let rest = rest.trim();
        if rest == "this creature" || rest == "~" || rest == "it" {
            return Some(vec![StaticMode::CantBlock]);
        }
    }
    if lower == "can't attack" || lower == "cannot attack" {
        return Some(vec![StaticMode::CantAttack]);
    }
    if lower == "can't be blocked"
        || lower == "cannot be blocked"
        || lower == "can't be blocked this turn"
        || lower == "cannot be blocked this turn"
    {
        return Some(vec![StaticMode::CantBeBlocked]);
    }
    // CR 508.1d + CR 509.1a: Compound "can't attack or block"
    if lower == "can't attack or block" || lower == "cannot attack or block" {
        return Some(vec![StaticMode::CantAttack, StaticMode::CantBlock]);
    }
    // CR 702.122c: "~ can't crew [Vehicles]"
    if lower == "can't crew"
        || lower == "cannot crew"
        || lower == "can't crew vehicles"
        || lower == "cannot crew vehicles"
    {
        return Some(vec![StaticMode::CantCrew]);
    }
    // CR 508.1d + CR 509.1a + CR 702.122c: Bound in Gold / Intercessor's Arrest
    if lower == "can't attack, block, or crew vehicles"
        || lower == "cannot attack, block, or crew vehicles"
        || lower == "can't attack, block, or crew"
        || lower == "cannot attack, block, or crew"
    {
        return Some(vec![
            StaticMode::CantAttack,
            StaticMode::CantBlock,
            StaticMode::CantCrew,
        ]);
    }
    // CR 509.1a + "can't be blocked": Compound "can't block or be blocked"
    if lower == "can't block or be blocked" || lower == "cannot block or be blocked" {
        return Some(vec![StaticMode::CantBlock, StaticMode::CantBeBlocked]);
    }
    // CR 509.1b: "can't be blocked except by ..." — evasion restriction
    if let Ok((except_text, _)) = alt((
        tag::<_, _, OracleError<'_>>("can't be blocked except by "),
        tag("cannot be blocked except by "),
    ))
    .parse(lower)
    {
        return Some(vec![StaticMode::CantBeBlockedExceptBy {
            kind: classify_block_exception(except_text),
        }]);
    }
    // CR 509.1b: "can't be blocked by <filter>" — blocker restriction
    if let Ok((by_rest, _)) = alt((
        tag::<_, _, OracleError<'_>>("can't be blocked by "),
        tag("cannot be blocked by "),
    ))
    .parse(lower)
    {
        let filter_text = by_rest.trim_end_matches('.').trim_end_matches(" this turn");
        // CR 105.4 + CR 608.2c (issue #327): Try the "of the chosen / of that"
        // qualifier parser first so "creatures of that color" lowers to a
        // typed filter with `FilterProp::IsChosenColor`. The plain
        // `parse_type_phrase` would silently drop the trailing qualifier and
        // leave the filter as a bare-creature match, making the restriction
        // accept ALL creatures rather than only those of the chosen color.
        let filter_tp = TextPair::new(filter_text, filter_text);
        let filter = parse_chosen_qualifier_subject(&filter_tp).unwrap_or_else(|| {
            let (f, _) = parse_type_phrase(filter_text);
            f
        });
        if !matches!(filter, TargetFilter::Any) {
            return Some(vec![StaticMode::CantBeBlockedBy { filter }]);
        }
    }
    // CR 702.18a: "can't be the target of spells or abilities" is blanket Shroud,
    // modeled as `CantBeTargeted` (propagated onto the subject via `AddStaticMode`
    // and enforced in `can_target`). CR 702.11a: the opponent-scoped variant is
    // Hexproof — a keyword grant this rule-mode parser can't express, so it is
    // handled by the keyword-grant path and deliberately not produced here, lest a
    // bare `CantBeTargeted` over-block the controller.
    if matches!(
        crate::parser::oracle_keyword::classify_cant_be_targeted(lower),
        Some(crate::parser::oracle_keyword::CantBeTargetedScope::AnyPlayer)
    ) {
        return Some(vec![StaticMode::CantBeTargeted]);
    }
    // CR 119.7: "can't gain life" — a player can't make their life total increase.
    if all_consuming(alt((
        tag::<_, _, OracleError<'_>>("can't gain life"),
        tag("cannot gain life"),
    )))
    .parse(lower)
    .is_ok()
    {
        return Some(vec![StaticMode::CantGainLife]);
    }
    // CR 305.1 + CR 611.1: "can't play lands" — a player can't take the land-play
    // special action (CR 305.1). This is the player-scoped prohibition shared by
    // the static form ("Players can't play lands", Worms of the Earth — CR 113.3d)
    // and the one-shot continuous-effect form ("Target player can't play lands
    // this turn", Pardic Miner — CR 611.1 + CR 611.2c, generated by an activated
    // ability's resolution rather than a static). The runtime gate lives in
    // `handle_play_land` via `player_has_static_other(state, pid, "CantPlayLand")`.
    //
    // Decomposed into independent negation × verb-phrase axes (CLAUDE.md
    // "Compose nom combinators, don't enumerate permutations") so future
    // related prohibitions can reuse the same negation prefix without
    // re-enumerating the cross-product.
    if all_consuming((
        alt((tag::<_, _, OracleError<'_>>("can't "), tag("cannot "))),
        alt((tag("play lands"), tag("play land cards"))),
    ))
    .parse(lower)
    .is_ok()
    {
        return Some(vec![StaticMode::Other("CantPlayLand".to_string())]);
    }
    // CR 119.8: "can't lose life" — life-loss events are prevented.
    if all_consuming(alt((
        tag::<_, _, OracleError<'_>>("can't lose life"),
        tag("cannot lose life"),
    )))
    .parse(lower)
    .is_ok()
    {
        return Some(vec![StaticMode::CantLoseLife]);
    }
    // CR 104.2b + CR 104.3e + CR 104.3f: "can't lose the game" / "can't win
    // the game" prohibitions. CR 104.2b ("An effect may state that a player
    // wins the game") and CR 104.3e ("An effect may state that a player loses
    // the game") are the rules these restrictions override; CR 104.3f handles
    // the simultaneous-win-and-lose case that Everybody Lives! creates by
    // blocking both outcomes at once. Compound "can't lose the game or win
    // the game" (and the symmetric "win or lose") must be checked before the
    // bare forms — Everybody Lives! prints the compound shape with the
    // negation elided over the conjunction ("can't (lose the game or win the
    // game)"), so the second leg is a bare verb phrase without its own
    // "can't" prefix. The bare "can't lose the game" tag would otherwise
    // short-circuit before the win-leg is recognized.
    {
        let negation = || alt((tag::<_, _, OracleError<'_>>("can't "), tag("cannot ")));
        let lose_the_game = || tag::<_, _, OracleError<'_>>("lose the game");
        let win_the_game = || tag::<_, _, OracleError<'_>>("win the game");
        // Compound: "{neg} lose the game or win the game" or the symmetric
        // "{neg} win the game or lose the game". The negation applies once
        // and distributes over both verbs (English ellipsis).
        if all_consuming(alt((
            (negation(), lose_the_game(), tag(" or "), win_the_game()),
            (negation(), win_the_game(), tag(" or "), lose_the_game()),
        )))
        .parse(lower)
        .is_ok()
        {
            return Some(vec![
                StaticMode::CantLoseTheGame,
                StaticMode::CantWinTheGame,
            ]);
        }
        if all_consuming((negation(), lose_the_game()))
            .parse(lower)
            .is_ok()
        {
            return Some(vec![StaticMode::CantLoseTheGame]);
        }
        if all_consuming((negation(), win_the_game()))
            .parse(lower)
            .is_ok()
        {
            return Some(vec![StaticMode::CantWinTheGame]);
        }
    }
    // CR 302.6: "doesn't untap during [controller's] untap step"
    if alt((
        tag::<_, _, OracleError<'_>>("doesn't untap"),
        tag("don't untap"),
    ))
    .parse(lower)
    .is_ok()
    {
        return Some(vec![StaticMode::CantUntap]);
    }

    None
}

fn parse_cant_be_regenerated_predicate(input: &str) -> OracleResult<'_, ()> {
    all_consuming(value(
        (),
        (
            alt((
                tag::<_, _, OracleError<'_>>("can't"),
                tag::<_, _, OracleError<'_>>("cannot"),
            )),
            tag(" be regenerated"),
            opt(tag(" this turn")),
            opt(tag(".")),
        ),
    ))
    .parse(input)
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
    let (after_prefix, _) = opt(tag::<_, _, OracleError<'_>>("then "))
        .parse(lower.as_str())
        .ok()?;
    let (after_subject, _) = tag::<_, _, OracleError<'_>>("its controller ")
        .parse(after_prefix)
        .ok()?;
    if !nom_primitives::scan_contains(&lower, "gain")
        || !nom_primitives::scan_contains(&lower, "life")
    {
        return None;
    }
    let amount = if nom_primitives::scan_contains(&lower, "equal to its power")
        || nom_primitives::scan_contains(&lower, "its power")
    {
        QuantityExpr::Ref {
            qty: QuantityRef::Power {
                scope: crate::types::ability::ObjectScope::Target,
            },
        }
    } else if nom_primitives::scan_contains(&lower, "equal to its toughness")
        || nom_primitives::scan_contains(&lower, "its toughness")
    {
        QuantityExpr::Ref {
            qty: QuantityRef::Toughness {
                scope: crate::types::ability::ObjectScope::Target,
            },
        }
    } else if nom_primitives::scan_contains(&lower, "equal to its mana value")
        || nom_primitives::scan_contains(&lower, "its mana value")
    {
        QuantityExpr::Ref {
            qty: QuantityRef::ObjectManaValue {
                scope: crate::types::ability::ObjectScope::Target,
            },
        }
    } else {
        // Try to parse a fixed amount: "its controller gains 3 life"
        let after = alt((tag::<_, _, OracleError<'_>>("gains "), tag("gain ")))
            .parse(after_subject)
            .map(|(rest, _)| rest)
            .unwrap_or(after_subject);
        QuantityExpr::Fixed {
            value: parse_number(after).map(|(n, _)| n as i32).unwrap_or(1),
        }
    };
    Some(parsed_clause(Effect::GainLife {
        amount,
        player: TargetFilter::ParentTargetController,
    }))
}

/// Parse `~ <predicate-verb>` at the start of input, succeeding only when the
/// first word after `~ ` deconjugates to a registered [`PREDICATE_VERBS`]
/// entry. Used as the single authority for validating the tilde-subject form
/// from both `starts_with_subject_prefix` (dispatch guard) and
/// `strip_subject_clause` (the same check is subsumed by `starts_with_*`).
///
/// CR 201.4b: after `parse_oracle_text` normalizes self-references, lines
/// like `~ phases out` / `~ gains haste` reach subject-stripping with `~` as
/// the subject token. Without the predicate-verb guard, `find_predicate_start`
/// would scan past non-predicate tokens (e.g. `~ enters with a token copy of
/// Pacifism attached to it.`) and match a later PREDICATE_VERB, stripping the
/// wrong clause.
fn parse_tilde_subject_with_predicate(input: &str) -> nom::IResult<&str, (), OracleError<'_>> {
    verify(
        preceded(tag("~ "), take_till(|c: char| c == ' ')),
        |first_word: &str| {
            let normalized = super::normalize_verb_token(first_word);
            PREDICATE_VERBS.contains(&normalized.as_str())
        },
    )
    .parse(input)
    .map(|(rest, _)| (rest, ()))
}

pub(super) fn strip_subject_clause(text: &str) -> Option<String> {
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
    alt((
        alt((
            value((), tag::<_, _, OracleError<'_>>("all ")),
            value((), tag("an opponent ")),
            value((), tag("your opponent ")),
            value((), tag("your opponents ")),
            value((), tag("any number of ")),
            value((), tag("defending player ")),
            value((), tag("each of ")),
            value((), tag("each opponent ")),
            value((), tag("each player ")),
            value((), tag("each ")),
            value((), tag("enchanted ")),
            value((), tag("equipped ")),
            value((), tag("it ")),
            value((), tag("its controller ")),
        )),
        alt((
            value((), tag::<_, _, OracleError<'_>>("its owner ")),
            value((), tag("~'s owner ")),
            value((), tag("target ")),
            value((), tag("that ")),
            value((), tag("the chosen ")),
            value((), tag("the player ")),
            // CR 609.7 + CR 615.5: "the source's controller" / "the source's
            // owner" as a subject in a damage-prevention follow-up (Swans of
            // Bryn Argoll, Eye for an Eye class). The "that source's …" form
            // is already covered by the bare `tag("that ")` arm above.
            // `parse_subject_application` recognizes the full phrase via the
            // generic "[the|that] <noun>'s controller" path and emits
            // `TargetFilter::ParentTargetController`; the prevention call site
            // then rewrites that to `PostReplacementSourceController`.
            value((), tag("the source's controller ")),
            value((), tag("the source's owner ")),
            value((), tag("they ")),
            value((), tag("this ")),
            value((), tag("those ")),
            value((), tag("up to ")),
            value((), tag("you ")),
            // CR 109.3: Gendered self-ref pronouns (e.g., Metalhead's
            // "He gains menace and haste"). Always resolve to SelfRef in
            // `parse_subject_application`.
            value((), tag("he ")),
            value((), tag("she ")),
            // CR 201.4b: After `parse_oracle_text` normalizes self-references
            // to `~`, predicates like "~ phases out" / "~ gains haste" reach
            // here with `~` as the subject token. Only dispatch as a subject
            // prefix when the next word is a recognized predicate verb —
            // otherwise lines like "~ enters with a token copy of Pacifism..."
            // would be falsely subject-stripped, scanning forward to an
            // unrelated verb and mis-matching the clause.
            parse_tilde_subject_with_predicate,
        )),
    ))
    .parse(lower)
    .is_ok()
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
    "assign",
    // NOTE: "counter" intentionally omitted from this list. The verb "counter"
    // (as in counter-a-spell, CR 701.5) only appears at the absolute start of
    // an imperative sentence, where first-word dispatch in
    // `parse_counter_ast` handles it. Every occurrence of "counter" / "counters"
    // *after* a subject is the noun form (CR 122.1) — "a +1/+1 counter on it",
    // "page counter on this artifact", "hit counters on them". Including it
    // here caused subject-stripped clauses to be misparsed as counter-spell
    // effects (e.g., Diary of Dreams' cost-reduction sentence, Wildgrowth
    // Archaic's "that creature enters with X additional +1/+1 counters on it",
    // Retto's "that creature enters with two +1/+1 counters on it").
    "create",
    "deal",
    "discard",
    "draw",
    // CR 701.63: Endure — "it endures N" / "this creature endures N" /
    // "~ endures N" / "<cardname> endures N". The self-referential subject is
    // stripped here so the deconjugated predicate ("endure N") re-dispatches
    // through the imperative path to `Effect::Endure`. The endure resolver acts
    // on the ability source, so no subject target injection is required.
    "endure",
    "exile",
    "explore",
    "fight",
    "gain",
    "get",
    "have",
    "look",
    "lose",
    "investigate",
    "learn",
    // CR 701.40a: Manifest — "its controller manifests the top card of their
    // library" (Reality Shift). Subject-shifted manifest clauses route through
    // the PredicateAst::ImperativeFallback arm in `lower_subject_predicate_ast`.
    "manifest",
    "mill",
    "pay",
    "phase",
    "populate",
    "put",
    "proliferate",
    "regenerate",
    "reveal",
    "return",
    "sacrifice",
    "scry",
    "search",
    "shuffle",
    "surveil",
    // CR 726.1: "take the initiative" / CR 500.7: "take an extra turn" — the
    // subject layer must recognize "take" so subject-prefixed forms ("you take
    // the initiative", "they take an extra turn") split correctly; the bare
    // imperative is already handled by first-word dispatch in imperative.rs.
    "take",
    "tap",
    "transform",
    "convert",
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
    use crate::types::ability::{AbilityKind, ContinuousModification, Effect, TypeFilter};
    use crate::types::card_type::Supertype;

    /// CR 702.3b: the subjectless conjunct recognizer accepts every grammatical
    /// shape the sequence splitter can leave behind ("this turn" optional, both
    /// "it"/"they" pronoun forms, optional trailing period) and rejects unrelated
    /// combat predicates so it only re-attaches subjects for genuine
    /// can-attack-despite-defender grants.
    #[test]
    fn is_can_attack_despite_defender_predicate_matches() {
        assert!(is_can_attack_despite_defender_predicate(
            "can attack this turn as though it didn't have defender"
        ));
        assert!(is_can_attack_despite_defender_predicate(
            "can attack as though they didn't have defender"
        ));
        assert!(is_can_attack_despite_defender_predicate(
            "can attack this turn as though it didn't have defender."
        ));
        // Negative: a bare "can attack" with no defender clause must not match.
        assert!(!is_can_attack_despite_defender_predicate("can attack"));
        // Negative: an extra-blocker grant belongs to the can-block predicate.
        assert!(!is_can_attack_despite_defender_predicate(
            "can block an additional creature"
        ));
    }

    /// CR 707.9 + CR 611.2b: Sarkhan, Soul Aflame's "have ~ become a copy of
    /// it until end of turn, except its name is ~ and it's legendary in
    /// addition to its other types" routes through `try_parse_have_redirection`
    /// → `try_parse_subject_become_clause` → `build_become_clause` →
    /// `try_parse_become_copy` block. The mid-sentence "until end of turn"
    /// lives between the target and the except clause; `strip_pre_except_duration`
    /// is the seam that pulls the duration through.
    #[test]
    fn sarkhan_soul_aflame_have_become_copy() {
        let mut ctx = ParseContext {
            card_name: Some("Sarkhan, Soul Aflame".to_string()),
            ..Default::default()
        };
        let ability = crate::parser::oracle_effect::parse_effect_chain_with_context(
            "have ~ become a copy of it until end of turn, except its name is ~ and it's legendary in addition to its other types",
            AbilityKind::Spell,
            &mut ctx,
        );
        match &*ability.effect {
            Effect::BecomeCopy {
                duration,
                additional_modifications,
                ..
            } => {
                assert_eq!(
                    duration,
                    &Some(crate::types::ability::Duration::UntilEndOfTurn),
                    "mid-sentence duration must be extracted"
                );
                assert!(
                    additional_modifications
                        .iter()
                        .any(|m| matches!(m, ContinuousModification::SetName { name } if name == "Sarkhan, Soul Aflame")),
                    "SetName missing in {additional_modifications:?}"
                );
                assert!(
                    additional_modifications.iter().any(|m| matches!(
                        m,
                        ContinuousModification::AddSupertype {
                            supertype: Supertype::Legendary
                        }
                    )),
                    "AddSupertype(Legendary) missing in {additional_modifications:?}"
                );
            }
            other => panic!("expected BecomeCopy, got {other:?}"),
        }
    }

    /// CR 726.1: "you take the initiative" (Seasoned Dungeoneer's ETB). The
    /// "you" subject must split off so the predicate "take the initiative"
    /// reaches the imperative dispatcher — this requires "take" in
    /// PREDICATE_VERBS. Without it, the whole clause falls to Unimplemented.
    #[test]
    fn you_take_the_initiative_subject_prefixed() {
        let mut ctx = ParseContext::default();
        let ability = crate::parser::oracle_effect::parse_effect_chain_with_context(
            "you take the initiative",
            AbilityKind::Spell,
            &mut ctx,
        );
        assert!(
            matches!(&*ability.effect, Effect::TakeTheInitiative),
            "expected TakeTheInitiative, got {:?}",
            ability.effect
        );
    }

    #[test]
    fn set_life_total_becomes_equal_to_starting_life_total() {
        for (text, expected) in [
            (
                // Oketra's Last Mercy, Resolute Archangel.
                "Your life total becomes equal to your starting life total.",
                QuantityExpr::Ref {
                    qty: QuantityRef::StartingLifeTotal,
                },
            ),
            (
                "Your life total becomes equal to 10.",
                QuantityExpr::Fixed { value: 10 },
            ),
        ] {
            let ability =
                crate::parser::oracle_effect::parse_effect_chain(text, AbilityKind::Spell);
            let Effect::SetLifeTotal { amount, .. } = &*ability.effect else {
                panic!(
                    "expected SetLifeTotal for {text:?}, got {:?}",
                    ability.effect
                );
            };
            assert_eq!(amount, &expected, "wrong amount for {text:?}");
        }
    }

    #[test]
    fn life_total_becomes_half_starting_life_total_rounded_up() {
        let mut ctx = ParseContext::default();
        let ability = crate::parser::oracle_effect::parse_effect_chain_with_context(
            "your life total becomes half your starting life total, rounded up",
            AbilityKind::Spell,
            &mut ctx,
        );
        let Effect::SetLifeTotal { amount, .. } = &*ability.effect else {
            panic!("expected SetLifeTotal, got {:?}", ability.effect);
        };
        assert!(matches!(
            amount,
            QuantityExpr::DivideRounded {
                rounding: crate::types::ability::RoundingMode::Up,
                ..
            }
        ));
    }

    /// CR 119.5: "<player>'s life total becomes <dynamic>" routes the RHS through
    /// the general quantity parser, so a cross-player life extremum (CR 119.1 /
    /// CR 102.1, parsed by `parse_cross_player_life_extremum`) resolves to a
    /// dynamic `QuantityExpr::Ref(LifeTotal{..})` rather than collapsing to
    /// `Effect::Unimplemented`. Covers the class shared by Repay in Kind,
    /// Arbiter of Knollridge, and Mortal Flesh Is Weak.
    #[test]
    fn life_total_becomes_cross_player_extremum() {
        use crate::types::ability::AggregateFunction;

        for (text, expected_player) in [
            (
                "each player's life total becomes the highest life total among all players",
                PlayerScope::AllPlayers {
                    aggregate: AggregateFunction::Max,
                    exclude: None,
                },
            ),
            (
                "each player's life total becomes the lowest life total among all players",
                PlayerScope::AllPlayers {
                    aggregate: AggregateFunction::Min,
                    exclude: None,
                },
            ),
            (
                "each opponent's life total becomes the lowest life total among your opponents",
                PlayerScope::Opponent {
                    aggregate: AggregateFunction::Min,
                },
            ),
        ] {
            let mut ctx = ParseContext::default();
            let ability = crate::parser::oracle_effect::parse_effect_chain_with_context(
                text,
                AbilityKind::Spell,
                &mut ctx,
            );
            let Effect::SetLifeTotal { amount, .. } = &*ability.effect else {
                panic!(
                    "expected SetLifeTotal for {text:?}, got {:?}",
                    ability.effect
                );
            };
            assert_eq!(
                amount,
                &QuantityExpr::Ref {
                    qty: QuantityRef::LifeTotal {
                        player: expected_player,
                    },
                },
                "wrong amount for {text:?}",
            );
        }
    }

    #[test]
    fn have_card_name_become_named_equipment_and_lose_other_abilities() {
        let mut ctx = ParseContext {
            card_name: Some("The Irencrag".to_string()),
            ..Default::default()
        };
        let ability = crate::parser::oracle_effect::parse_effect_chain_with_context(
            "have The Irencrag become a legendary Equipment artifact named Everflame, Heroes' Legacy. If you do, it gains equip {3} and \"Equipped creature gets +3/+3\" and loses all other abilities.",
            AbilityKind::Spell,
            &mut ctx,
        );

        let Effect::GenericEffect {
            static_abilities, ..
        } = &*ability.effect
        else {
            panic!("expected GenericEffect, got {:?}", ability.effect);
        };
        let modifications = &static_abilities[0].modifications;
        assert!(
            modifications.iter().any(|modification| matches!(
                modification,
                ContinuousModification::SetName { name } if name == "Everflame, Heroes' Legacy"
            )),
            "expected SetName in {modifications:?}",
        );
        assert!(
            modifications.iter().any(|modification| matches!(
                modification,
                ContinuousModification::AddSubtype { subtype } if subtype == "Equipment"
            )),
            "expected AddSubtype(Equipment) in {modifications:?}",
        );

        let sub_ability = ability.sub_ability.as_ref().expect("If you do sub-ability");
        assert!(sub_ability
            .condition
            .as_ref()
            .is_some_and(crate::types::ability::AbilityCondition::is_optional_effect_performed));
        let Effect::GenericEffect {
            static_abilities, ..
        } = &*sub_ability.effect
        else {
            panic!(
                "expected GenericEffect sub-ability, got {:?}",
                sub_ability.effect
            );
        };
        let sub_modifications = &static_abilities[0].modifications;
        assert!(
            sub_modifications.iter().any(|modification| matches!(
                modification,
                ContinuousModification::RemoveAllAbilities
            )),
            "expected RemoveAllAbilities in {sub_modifications:?}",
        );
    }

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
    fn starts_with_subject_prefix_your_opponents() {
        assert!(starts_with_subject_prefix(
            "your opponents can't gain life this turn"
        ));
        assert!(starts_with_subject_prefix("your opponent discards a card"));
    }

    #[test]
    fn starts_with_subject_prefix_the_player() {
        assert!(starts_with_subject_prefix("the player draws a card"));
    }

    #[test]
    fn parse_subject_each_of_your_opponents() {
        let mut ctx = ParseContext::default();
        let result = parse_subject_application("each of your opponents", &mut ctx);
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
        let mut ctx = ParseContext::default();
        let result = parse_subject_application("each of them", &mut ctx);
        assert!(result.is_some());
        let app = result.unwrap();
        assert_eq!(app.affected, TargetFilter::ParentTarget);
    }

    #[test]
    fn parse_subject_each_of_those_creatures() {
        let mut ctx = ParseContext::default();
        let result = parse_subject_application("each of those creatures", &mut ctx);
        assert!(result.is_some());
        let app = result.unwrap();
        assert_eq!(app.affected, TargetFilter::ParentTarget);
    }

    #[test]
    fn parse_subject_the_chosen_creature() {
        for subject in [
            "the chosen artifact",
            "the chosen card",
            "the chosen creature",
            "the chosen creatures",
            "the chosen land",
            "the chosen permanent",
            "the chosen player",
        ] {
            let mut ctx = ParseContext::default();
            let result = parse_subject_application(subject, &mut ctx);
            let app = result.expect("should recognize selected subject");
            assert_eq!(app.affected, TargetFilter::ParentTarget);
            assert!(
                app.target.is_none(),
                "chosen object is an anaphoric parent target, not a new target"
            );
        }
    }

    #[test]
    fn chosen_creature_doesnt_untap_builds_cant_untap_restriction() {
        let mut ctx = ParseContext::default();
        let clause = try_parse_subject_restriction_clause(
            "The chosen creature doesn't untap during its controller's next untap step.",
            &mut ctx,
        )
        .expect("chosen object untap restriction should parse");

        let Effect::GenericEffect {
            static_abilities,
            duration,
            target,
        } = clause.effect
        else {
            panic!(
                "expected GenericEffect restriction, got {:?}",
                clause.effect
            );
        };

        assert_eq!(target, None);
        assert_eq!(
            duration,
            Some(Duration::UntilNextStepOf {
                step: Phase::Untap,
                player: PlayerScope::Controller,
            })
        );
        assert_eq!(static_abilities.len(), 1);
        assert_eq!(static_abilities[0].mode, StaticMode::CantUntap);
        assert_eq!(
            static_abilities[0].affected,
            Some(TargetFilter::ParentTarget)
        );
        assert!(static_abilities[0].modifications.iter().any(|m| matches!(
            m,
            ContinuousModification::AddStaticMode {
                mode: StaticMode::CantUntap
            }
        )));
    }

    /// CR 102.1 + CR 103.1: "the player to your right" as a subject resolves to
    /// the seating-relative `Neighbor` filter (untargeted), so the
    /// GainControl→GiveControl rewrite gets `recipient: Neighbor { Right }`
    /// rather than a generic `Any`. Regression for Bucknard's Everfull Purse.
    #[test]
    fn parse_subject_the_player_to_your_right_is_neighbor() {
        use crate::types::ability::SeatDirection;
        let mut ctx = ParseContext::default();
        let app = parse_subject_application("the player to your right", &mut ctx)
            .expect("seating-neighbor subject should parse");
        assert_eq!(
            app.affected,
            TargetFilter::Neighbor {
                direction: SeatDirection::Right
            }
        );
        assert!(
            app.target.is_none(),
            "neighbor recipient is computed, not a chosen target slot"
        );
    }

    #[test]
    fn parse_subject_an_opponent() {
        let mut ctx = ParseContext::default();
        let result = parse_subject_application("an opponent", &mut ctx);
        assert!(result.is_some());
        let app = result.unwrap();
        assert_eq!(
            app.affected,
            TargetFilter::Typed(TypedFilter::default().controller(ControllerRef::Opponent))
        );
    }

    #[test]
    fn parse_subject_your_opponents() {
        let mut ctx = ParseContext::default();
        let result = parse_subject_application("your opponents", &mut ctx);
        assert!(result.is_some());
        let app = result.unwrap();
        assert_eq!(
            app.affected,
            TargetFilter::Typed(TypedFilter::default().controller(ControllerRef::Opponent))
        );
        assert!(app.target.is_none());
    }

    #[test]
    fn parse_subject_your_opponents_possessive_is_not_bare_opponent_scope() {
        let mut ctx = ParseContext::default();
        let result = parse_subject_application("your opponents' creatures", &mut ctx);
        if let Some(app) = result {
            assert_ne!(
                app.affected,
                TargetFilter::Typed(TypedFilter::default().controller(ControllerRef::Opponent))
            );
        }
    }

    #[test]
    fn parse_subject_your_opponent_may_is_not_treated_as_bare_subject() {
        let mut ctx = ParseContext::default();
        let result = parse_subject_application("your opponent may", &mut ctx);
        assert!(result.is_none());
    }

    #[test]
    fn your_opponents_cant_gain_life_builds_restriction() {
        let mut ctx = ParseContext::default();
        let clause = try_parse_subject_restriction_clause(
            "Your opponents can't gain life this turn",
            &mut ctx,
        )
        .expect("your opponents life-lock should parse");

        let Effect::GenericEffect {
            static_abilities,
            duration,
            target,
        } = clause.effect
        else {
            panic!(
                "expected GenericEffect restriction, got {:?}",
                clause.effect
            );
        };

        assert_eq!(target, None);
        assert_eq!(duration, Some(Duration::UntilEndOfTurn));
        assert_eq!(static_abilities.len(), 1);
        let def = &static_abilities[0];
        assert_eq!(def.mode, StaticMode::CantGainLife);
        assert_eq!(
            def.affected,
            Some(TargetFilter::Typed(
                TypedFilter::default().controller(ControllerRef::Opponent)
            ))
        );
        assert!(def.modifications.iter().any(|m| matches!(
            m,
            ContinuousModification::AddStaticMode {
                mode: StaticMode::CantGainLife
            }
        )));
    }

    #[test]
    fn parse_subject_the_player() {
        // CR 608.2c: a bare non-trigger "the player" subject is the same anaphor
        // class as "that player" — it resolves to the controller of the target
        // referenced earlier in the same instruction.
        let mut ctx = ParseContext::default();
        let result = parse_subject_application("the player", &mut ctx);
        assert!(result.is_some());
        let app = result.unwrap();
        assert_eq!(app.affected, TargetFilter::ParentTargetController);
    }

    // CR 608.2c + CR 117.3a: "its/their controller [may]" anaphoric player subject.
    #[test]
    fn parse_subject_its_controller_bare() {
        let mut ctx = ParseContext::default();
        let result = parse_subject_application("its controller", &mut ctx);
        let app = result.expect("should recognize 'its controller'");
        assert_eq!(app.affected, TargetFilter::ParentTargetController);
        assert!(!app.is_optional, "no 'may' modal → not optional");
    }

    #[test]
    fn parse_subject_their_controller_bare() {
        let mut ctx = ParseContext::default();
        let result = parse_subject_application("their controller", &mut ctx);
        let app = result.expect("should recognize 'their controller'");
        assert_eq!(app.affected, TargetFilter::ParentTargetController);
        assert!(!app.is_optional);
    }

    #[test]
    fn parse_subject_its_controller_may() {
        let mut ctx = ParseContext::default();
        let result = parse_subject_application("its controller may", &mut ctx);
        let app = result.expect("should recognize 'its controller may'");
        assert_eq!(app.affected, TargetFilter::ParentTargetController);
        assert!(
            app.is_optional,
            "'may' modal must mark the subject as optional"
        );
    }

    #[test]
    fn targeted_controller_gains_life_equal_to_target_toughness() {
        let clause = try_parse_targeted_controller_gain_life(
            "Its controller gains life equal to its toughness.",
        )
        .expect("targeted controller gain life clause");

        assert!(matches!(
            clause.effect,
            Effect::GainLife {
                amount: QuantityExpr::Ref {
                    qty: QuantityRef::Toughness {
                        scope: crate::types::ability::ObjectScope::Target
                    }
                },
                player: TargetFilter::ParentTargetController
            }
        ));
    }

    #[test]
    fn targeted_controller_gains_life_equal_to_target_mana_value() {
        let clause = try_parse_targeted_controller_gain_life(
            "Its controller gains life equal to its mana value.",
        )
        .expect("targeted controller mana value gain life clause");

        assert!(matches!(
            clause.effect,
            Effect::GainLife {
                amount: QuantityExpr::Ref {
                    qty: QuantityRef::ObjectManaValue {
                        scope: crate::types::ability::ObjectScope::Target
                    }
                },
                player: TargetFilter::ParentTargetController
            }
        ));
    }

    #[test]
    fn targeted_controller_gain_life_accepts_then_prefix() {
        let clause = try_parse_targeted_controller_gain_life(
            "Then its controller gains life equal to its mana value.",
        )
        .expect("chained targeted controller mana value gain life clause");

        assert!(matches!(
            clause.effect,
            Effect::GainLife {
                amount: QuantityExpr::Ref {
                    qty: QuantityRef::ObjectManaValue {
                        scope: crate::types::ability::ObjectScope::Target
                    }
                },
                player: TargetFilter::ParentTargetController
            }
        ));
    }

    #[test]
    fn targeted_controller_gains_fixed_life_still_parses() {
        let clause = try_parse_targeted_controller_gain_life("Its controller gains 3 life.")
            .expect("targeted controller fixed gain life clause");

        assert!(matches!(
            clause.effect,
            Effect::GainLife {
                amount: QuantityExpr::Fixed { value: 3 },
                player: TargetFilter::ParentTargetController
            }
        ));
    }

    #[test]
    fn parse_subject_their_controller_may() {
        let mut ctx = ParseContext::default();
        let result = parse_subject_application("their controller may", &mut ctx);
        let app = result.expect("should recognize 'their controller may'");
        assert_eq!(app.affected, TargetFilter::ParentTargetController);
        assert!(app.is_optional);
    }

    // CR 608.2c: "that [type]" anaphoric back-references
    #[test]
    fn parse_subject_that_creature() {
        let mut ctx = ParseContext::default();
        let result = parse_subject_application("That creature", &mut ctx);
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
        let mut ctx = ParseContext::default();
        let result = parse_subject_application("that land", &mut ctx);
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
        let mut ctx = ParseContext::default();
        let result = parse_subject_application("that permanent", &mut ctx);
        assert!(result.is_some(), "should recognize 'that permanent'");
        let app = result.unwrap();
        assert!(
            matches!(app.affected, TargetFilter::Typed(ref t) if t.type_filters.contains(&TypeFilter::Permanent)),
            "affected should be Permanent filter, got {:?}",
            app.affected
        );
    }

    #[test]
    fn parse_subject_that_player_resolves_parent_target_controller() {
        // CR 608.2c: outside trigger context, a bare "that player" subject is an
        // anaphor to the controller of the target referenced earlier in the same
        // instruction (e.g. Volatile Fault's destroyed nonbasic land). It resolves
        // to ParentTargetController, not a generic Player.
        let mut ctx = ParseContext::default();
        assert!(ctx.subject.is_none(), "non-trigger context");
        let result = parse_subject_application("that player", &mut ctx);
        assert!(result.is_some());
        assert_eq!(
            result.unwrap().affected,
            TargetFilter::ParentTargetController
        );
    }

    #[test]
    fn parse_subject_that_player_trigger_context_is_triggering_player() {
        // In trigger context (ctx.subject is Some), "that player" refers
        // anaphorically to the player from the triggering event.
        let mut ctx = ParseContext {
            subject: Some(TargetFilter::SelfRef),
            ..ParseContext::default()
        };
        let result = parse_subject_application("that player", &mut ctx);
        assert!(result.is_some());
        assert_eq!(result.unwrap().affected, TargetFilter::TriggeringPlayer);
    }

    #[test]
    fn parse_subject_that_player_trigger_context_honors_parent_target_controller_scope() {
        let mut ctx = ParseContext {
            subject: Some(TargetFilter::SelfRef),
            relative_player_scope: Some(ControllerRef::ParentTargetController),
            ..ParseContext::default()
        };
        let result = parse_subject_application("that player", &mut ctx);

        assert!(result.is_some());
        assert_eq!(
            result.unwrap().affected,
            TargetFilter::ParentTargetController
        );
    }

    // CR 115.1d: "any number of target" subject prefix tests
    #[test]
    fn parse_subject_any_number_of_target_creatures() {
        let mut ctx = ParseContext::default();
        let result = parse_subject_application("any number of target creatures", &mut ctx);
        assert!(result.is_some());
        let app = result.unwrap();
        assert!(
            matches!(app.affected, TargetFilter::Typed(ref t) if t.type_filters.contains(&TypeFilter::Creature)),
            "should parse creature filter, got {:?}",
            app.affected
        );
        assert!(app.target.is_some(), "should be targeted");
        assert_eq!(
            app.multi_target,
            Some(MultiTargetSpec::unlimited(0)),
            "should have unlimited multi_target"
        );
    }

    #[test]
    fn parse_subject_any_number_of_target_creatures_you_control() {
        let mut ctx = ParseContext::default();
        let result =
            parse_subject_application("any number of target creatures you control", &mut ctx);
        assert!(result.is_some());
        let app = result.unwrap();
        assert!(
            matches!(app.affected, TargetFilter::Typed(ref t)
                if t.type_filters.contains(&TypeFilter::Creature)
                && t.controller == Some(ControllerRef::You)),
            "should parse creature + controller, got {:?}",
            app.affected
        );
        assert_eq!(app.multi_target, Some(MultiTargetSpec::unlimited(0)),);
    }

    #[test]
    fn parse_subject_another_target_honors_relative_player_scope() {
        let mut ctx = ParseContext {
            relative_player_scope: Some(ControllerRef::TargetPlayer),
            ..ParseContext::default()
        };
        let result =
            parse_subject_application("another target creature that player controls", &mut ctx);
        assert!(result.is_some());
        let app = result.unwrap();
        assert!(
            matches!(app.affected, TargetFilter::Typed(ref t)
                if t.type_filters.contains(&TypeFilter::Creature)
                && t.controller == Some(ControllerRef::TargetPlayer)
                && t.properties.iter().any(|prop| matches!(prop, FilterProp::Another))),
            "should parse another creature controlled by target player, got {:?}",
            app.affected
        );
    }

    #[test]
    fn parse_subject_up_to_one_target_honors_relative_player_scope() {
        let mut ctx = ParseContext {
            relative_player_scope: Some(ControllerRef::TargetPlayer),
            ..ParseContext::default()
        };
        let result =
            parse_subject_application("up to one target creature that player controls", &mut ctx);
        assert!(result.is_some());
        let app = result.unwrap();
        assert!(
            matches!(app.affected, TargetFilter::Typed(ref t)
                if t.type_filters.contains(&TypeFilter::Creature)
                && t.controller == Some(ControllerRef::TargetPlayer)),
            "should parse creature controlled by target player, got {:?}",
            app.affected
        );
        assert_eq!(
            app.multi_target,
            Some(MultiTargetSpec::up_to(QuantityExpr::Fixed { value: 1 }))
        );
    }

    #[test]
    fn parse_subject_any_number_of_target_players() {
        let mut ctx = ParseContext::default();
        let result = parse_subject_application("any number of target players", &mut ctx);
        assert!(result.is_some());
        let app = result.unwrap();
        assert_eq!(app.multi_target, Some(MultiTargetSpec::unlimited(0)),);
    }

    #[test]
    fn starts_with_subject_prefix_any_number_of() {
        assert!(starts_with_subject_prefix(
            "any number of target creatures each get +1/+1"
        ));
    }

    // --- Group: prohibition-family restriction predicates ---
    // Each test proves `parse_restriction_modes` emits the canonical
    // `StaticMode::Other("...")` name(s) for the given predicate after
    // subject stripping (e.g., "Creatures you control can't be sacrificed"
    // reduces to the "can't be sacrificed" predicate here).

    #[test]
    fn parse_restriction_modes_cant_be_sacrificed() {
        assert_eq!(
            parse_restriction_modes("can't be sacrificed"),
            Some(vec![StaticMode::Other("CantBeSacrificed".to_string())])
        );
    }

    #[test]
    fn parse_restriction_modes_cant_be_enchanted_variants() {
        assert_eq!(
            parse_restriction_modes("can't be enchanted"),
            Some(vec![StaticMode::Other("CantBeEnchanted".to_string())])
        );
        assert_eq!(
            parse_restriction_modes("can't be enchanted by other auras"),
            Some(vec![StaticMode::Other("CantBeEnchanted".to_string())])
        );
    }

    #[test]
    fn parse_restriction_modes_cant_be_equipped() {
        assert_eq!(
            parse_restriction_modes("can't be equipped"),
            Some(vec![StaticMode::Other("CantBeEquipped".to_string())])
        );
    }

    #[test]
    fn parse_restriction_modes_cant_be_equipped_or_enchanted_compound() {
        // Compound phrase emits BOTH CantBeEquipped and CantBeEnchanted, in that order.
        // CantBeAttached is intentionally NOT emitted (it includes Fortifications).
        assert_eq!(
            parse_restriction_modes("can't be equipped or enchanted"),
            Some(vec![
                StaticMode::Other("CantBeEquipped".to_string()),
                StaticMode::Other("CantBeEnchanted".to_string()),
            ])
        );
    }

    #[test]
    fn parse_restriction_modes_cant_transform() {
        assert_eq!(
            parse_restriction_modes("can't transform"),
            Some(vec![StaticMode::Other("CantTransform".to_string())])
        );
    }

    #[test]
    fn parse_restriction_modes_cant_crew_variants() {
        assert_eq!(
            parse_restriction_modes("can't crew"),
            Some(vec![StaticMode::CantCrew])
        );
        assert_eq!(
            parse_restriction_modes("cannot crew vehicles"),
            Some(vec![StaticMode::CantCrew])
        );
    }

    #[test]
    fn parse_restriction_modes_cant_attack_block_or_crew_vehicles_compound() {
        assert_eq!(
            parse_restriction_modes("can't attack, block, or crew vehicles"),
            Some(vec![
                StaticMode::CantAttack,
                StaticMode::CantBlock,
                StaticMode::CantCrew,
            ])
        );
    }

    #[test]
    fn parse_restriction_modes_cant_be_regenerated_variants() {
        let expected = Some(vec![StaticMode::CantBeRegenerated]);
        assert_eq!(parse_restriction_modes("can't be regenerated"), expected);
        assert_eq!(parse_restriction_modes("cannot be regenerated"), expected);
        assert_eq!(
            parse_restriction_modes("can't be regenerated this turn"),
            expected
        );
        assert_eq!(
            parse_restriction_modes("cannot be regenerated this turn."),
            expected
        );
    }

    // CR 119.8: "can't lose life" predicate emits `CantLoseLife`. Players-subject
    // and you-subject share this same predicate after subject stripping.
    #[test]
    fn parse_restriction_modes_cant_lose_life() {
        assert_eq!(
            parse_restriction_modes("can't lose life"),
            Some(vec![StaticMode::CantLoseLife])
        );
        assert_eq!(
            parse_restriction_modes("cannot lose life"),
            Some(vec![StaticMode::CantLoseLife])
        );
    }

    // CR 305.1: "can't play lands" and "can't play land cards" are the same
    // land-play special-action prohibition after subject stripping.
    #[test]
    fn parse_restriction_modes_cant_play_land_variants() {
        let expected = Some(vec![StaticMode::Other("CantPlayLand".to_string())]);
        assert_eq!(parse_restriction_modes("can't play lands"), expected);
        assert_eq!(parse_restriction_modes("cannot play lands"), expected);
        assert_eq!(parse_restriction_modes("can't play land cards"), expected);
        assert_eq!(parse_restriction_modes("cannot play land cards"), expected);
    }

    // CR 104.3 + CR 704.5: "can't lose the game" predicate emits `CantLoseTheGame`.
    #[test]
    fn parse_restriction_modes_cant_lose_the_game() {
        assert_eq!(
            parse_restriction_modes("can't lose the game"),
            Some(vec![StaticMode::CantLoseTheGame])
        );
        assert_eq!(
            parse_restriction_modes("cannot lose the game"),
            Some(vec![StaticMode::CantLoseTheGame])
        );
    }

    // CR 104.2b: "can't win the game" predicate emits `CantWinTheGame`.
    #[test]
    fn parse_restriction_modes_cant_win_the_game() {
        assert_eq!(
            parse_restriction_modes("can't win the game"),
            Some(vec![StaticMode::CantWinTheGame])
        );
    }

    // CR 104.2b + CR 104.3e + CR 104.3f: Compound "can't lose the game or
    // win the game" (Everybody Lives! prints this shape) emits BOTH
    // `CantLoseTheGame` and `CantWinTheGame`. The compound check fires
    // before the bare-"can't lose the game" arm so we never short-circuit
    // and drop the win-leg.
    #[test]
    fn parse_restriction_modes_cant_lose_or_win_the_game_compound() {
        assert_eq!(
            parse_restriction_modes("can't lose the game or win the game"),
            Some(vec![
                StaticMode::CantLoseTheGame,
                StaticMode::CantWinTheGame
            ])
        );
        assert_eq!(
            parse_restriction_modes("can't win the game or lose the game"),
            Some(vec![
                StaticMode::CantLoseTheGame,
                StaticMode::CantWinTheGame
            ])
        );
    }

    /// CR 509.1a + CR 509.1b: Activated ability "~ can block an additional creature
    /// this turn" produces a transient GenericEffect granting ExtraBlockers { count: Some(1) }
    /// via AddStaticMode. Validates the `try_parse_can_block_additional` handler.
    #[test]
    fn can_block_additional_creature_this_turn_effect() {
        let mut ctx = ParseContext {
            card_name: Some("Luminous Guardian".to_string()),
            ..Default::default()
        };
        let ability = crate::parser::oracle_effect::parse_effect_chain_with_context(
            "~ can block an additional creature this turn.",
            AbilityKind::Activated,
            &mut ctx,
        );
        match &*ability.effect {
            Effect::GenericEffect {
                static_abilities,
                duration,
                ..
            } => {
                assert_eq!(
                    duration,
                    &Some(Duration::UntilEndOfTurn),
                    "duration must be UntilEndOfTurn"
                );
                assert_eq!(static_abilities.len(), 1);
                let sd = &static_abilities[0];
                assert_eq!(
                    sd.mode,
                    StaticMode::ExtraBlockers { count: Some(1) },
                    "mode must be ExtraBlockers(1)"
                );
                assert!(
                    sd.modifications.iter().any(|m| matches!(
                        m,
                        ContinuousModification::AddStaticMode {
                            mode: StaticMode::ExtraBlockers { count: Some(1) }
                        }
                    )),
                    "must have AddStaticMode(ExtraBlockers(1)) modification"
                );
            }
            other => panic!("expected GenericEffect, got {other:?}"),
        }
    }

    /// CR 509.1a: "~ can block any number of creatures this turn" produces
    /// ExtraBlockers { count: None } via the same handler.
    #[test]
    fn can_block_any_number_this_turn_effect() {
        let mut ctx = ParseContext {
            card_name: Some("Test Card".to_string()),
            ..Default::default()
        };
        let ability = crate::parser::oracle_effect::parse_effect_chain_with_context(
            "~ can block any number of creatures this turn.",
            AbilityKind::Activated,
            &mut ctx,
        );
        match &*ability.effect {
            Effect::GenericEffect {
                static_abilities,
                duration,
                ..
            } => {
                assert_eq!(
                    duration,
                    &Some(Duration::UntilEndOfTurn),
                    "duration must be UntilEndOfTurn"
                );
                assert_eq!(static_abilities.len(), 1);
                let sd = &static_abilities[0];
                assert_eq!(
                    sd.mode,
                    StaticMode::ExtraBlockers { count: None },
                    "mode must be ExtraBlockers(None)"
                );
            }
            other => panic!("expected GenericEffect, got {other:?}"),
        }
    }

    /// CR 509.1a + CR 509.1b: combat-scoped blocking permissions expire at
    /// end of combat, and numeric counts are parsed through the shared number
    /// combinator rather than a one-card string branch.
    #[test]
    fn can_block_two_additional_creatures_this_combat_effect() {
        let mut ctx = ParseContext {
            card_name: Some("Test Card".to_string()),
            ..Default::default()
        };
        let ability = crate::parser::oracle_effect::parse_effect_chain_with_context(
            "~ can block two additional creatures this combat.",
            AbilityKind::Activated,
            &mut ctx,
        );
        match &*ability.effect {
            Effect::GenericEffect {
                static_abilities,
                duration,
                ..
            } => {
                assert_eq!(
                    duration,
                    &Some(Duration::UntilEndOfCombat),
                    "duration must be UntilEndOfCombat"
                );
                assert_eq!(static_abilities.len(), 1);
                let sd = &static_abilities[0];
                assert_eq!(
                    sd.mode,
                    StaticMode::ExtraBlockers { count: Some(2) },
                    "mode must be ExtraBlockers(2)"
                );
            }
            other => panic!("expected GenericEffect, got {other:?}"),
        }
    }
}
