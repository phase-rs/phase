use crate::parser::oracle_nom::error::OracleError;
use nom::branch::alt;
use nom::bytes::complete::{tag, take_till, take_until};
use nom::character::complete::multispace0;
use nom::combinator::{all_consuming, map, opt, peek, rest, value, verify};
use nom::multi::separated_list1;
use nom::sequence::{delimited, preceded, terminated};
use nom::Parser;

use super::animation::{
    animation_modifications_with_replacement, has_in_addition_to_other_colors,
    has_in_addition_to_other_types, parse_animation_spec,
};
use super::imperative;
use super::lower::BOUNDED_TARGET_CARDINALITIES;
use super::{resolve_it_pronoun, ParseContext};
use crate::parser::oracle_ir::ast::*;
use crate::types::ability::{
    AbilityDefinition, AbilityKind, ChosenSubtypeKind, ColorChangeMode, ContinuousModification,
    ControllerRef, Duration, EachDamageRecipient, Effect, FilterProp, MultiTargetSpec,
    PlayerFilter, PlayerScope, PtValue, QuantityExpr, QuantityRef, StaticCondition,
    StaticDefinition, TargetFilter, TypedFilter,
};
use crate::types::game_state::DayNight;
use crate::types::keywords::Keyword;
use crate::types::phase::Phase;
use crate::types::statics::{ProhibitionScope, StaticMode};

use super::super::oracle_keyword::parse_granted_keyword_fragment;
use super::super::oracle_nom::bridge::nom_on_lower;
use super::super::oracle_nom::duration::parse_duration;
use super::super::oracle_nom::error::OracleResult;
use super::super::oracle_nom::primitives as nom_primitives;
use super::super::oracle_nom::quantity as nom_quantity;
use super::super::oracle_nom::target::parse_event_context_ref;
use super::super::oracle_quantity;
use super::super::oracle_static::{
    classify_block_exception, parse_additive_type_clause_modifications,
    parse_cant_be_activated_exemption_in_text, parse_chosen_qualifier_subject,
    parse_continuous_modifications, parse_continuous_subject_filter, parse_static_line,
    parse_static_line_multi, peel_compound_all_quantified_conjuncts,
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

    // CR 723.1 / CR 723.2: "you control [target] during [possessive] next turn /
    // next combat phase" (Mindslaver / Secret of Bloodbending) is a
    // `ControlNextTurn` imperative, not a subject-predicate grant. The imperative
    // path owns the window axis; without this deferral the subject grammar
    // mis-parses the trailing "combat phase" into an `Unimplemented` remnant.
    if matches!(
        imperative::parse_targeted_action_ast(
            text,
            &text.to_lowercase(),
            &mut ParseContext::default()
        ),
        Some(crate::parser::oracle_ir::ast::TargetedImperativeAst::ControlNextTurn { .. })
    ) {
        return None;
    }

    // CR 120.1 + CR 115.1d: defer the "up to two target creatures you control each
    // deal damage equal to their power to target creature" shape to the imperative
    // path, which preserves both the targeted source set and the recipient as
    // independent targets. Splitting it into subject + imperative-fallback here
    // would drop the per-source damage semantics. (The team-scope variant fails
    // closed to `Unimplemented` earlier, in `parse_effect_clause_inner`.)
    if try_parse_each_deals_damage_equal_to_power(text).is_some() {
        return None;
    }

    // CR 702.3b: "can attack [this turn] as though it didn't have defender" —
    // must intercept before continuous clause parsing which would incorrectly
    // extract "defender" as an AddKeyword from "didn't have defender".
    if let Some(clause) = try_parse_can_attack_with_defender(text, ctx) {
        return Some(subject_predicate_ast_from_clause(
            text,
            clause,
            |effect, duration, sub_ability| PredicateAst::Restriction {
                effect,
                duration,
                sub_ability,
            },
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
            |effect, duration, sub_ability| PredicateAst::Restriction {
                effect,
                duration,
                sub_ability,
            },
            ctx,
        ));
    }

    // CR 701.15a: "it's goaded [duration]" / "it is goaded [duration]" — copula +
    // past-participle state assignment. The contraction "it's" fuses subject +
    // copula and cannot be split by `find_predicate_start`, so intercept the
    // pattern early and lower it to `Effect::Goad` with the pronoun-resolved
    // target. Covers Jon Irenicus, Vislor Turlough, and any future card that
    // sets the goaded state via copula rather than the imperative "goad it".
    if let Some(clause) = try_parse_copula_goaded_clause(text, ctx) {
        return Some(subject_predicate_ast_from_clause(
            text,
            clause,
            |effect, duration, _sub_ability| PredicateAst::Continuous {
                effect,
                duration,
                sub_ability: None,
            },
            ctx,
        ));
    }

    // CR 611.2 + CR 201.2a: "target <T> and all other <T>s with the same name as
    // that <T> get -N/-M" (Bile Blight, Echoing Decay/Courage). Must run before
    // the generic continuous clause, which resolves only the first conjunct and
    // drops the same-name mass debuff (issue #4727).
    if let Some(clause) = try_parse_target_and_same_name_pump_clause(text, ctx) {
        return Some(clause);
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

    // CR 613.4b + CR 613.1f: "[subject]'s base power and toughness become N/M and
    // (it/they) gain(s) <keywords>" — set-base-P/T + keyword grant on the
    // possessor, no type change. Must run before `try_parse_subject_become_clause`
    // (which assumes the verb "become" acts on the permanent's type, not its P/T).
    // Returns a fully-built `ClauseAst` because the possessive subject ("~'s base
    // power and toughness") cannot be re-derived by `find_predicate_start`.
    if let Some(clause) = try_parse_subject_base_pt_set_clause_ast(text, ctx) {
        return Some(clause);
    }

    // CR 611.3 + CR 105.2 + CR 305.7: "all <X> become <P> and all <Y> become <Q>"
    // — a compound-quantified dual-subject become effect (Nightcreep: "all
    // creatures become black and all lands become Swamps"). Each conjunct carries
    // its own subject and predicate; must run before `try_parse_subject_become_clause`,
    // which would claim only the first conjunct and drop the second into the
    // static description. Sibling of the compound-subject static handlers in
    // `oracle_static/type_change.rs` (#5219 class).
    if let Some(clause) = try_parse_compound_all_subjects_become_clause(text, ctx) {
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

    // CR 509.1b + CR 611.2c: "<source> and up to N other target creature(s) can't
    // be blocked this turn" (Martha Jones) — conjoined-subject evasion grant.
    // Builds its own ClauseAst (with the source as the primary subject and the
    // targeted creature as a sub_ability) so the secondary target/multi_target
    // does not leak onto the source grant. Must run before the generic
    // restriction split, which can't resolve the compound subject.
    if let Some(ast) = try_parse_source_and_other_restriction_clause(text, ctx) {
        return Some(ast);
    }

    if let Some(clause) = try_parse_subject_restriction_clause(text, ctx) {
        return Some(subject_predicate_ast_from_clause(
            text,
            clause,
            |effect, duration, sub_ability| PredicateAst::Restriction {
                effect,
                duration,
                sub_ability,
            },
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
    // CR 608.2c: drop the additive "also" connector (see
    // `strip_trailing_additive_adverb`) so the re-extracted subject phrase used
    // by `subject_predicate_ast_from_clause` matches the one parsed inside
    // `try_parse_subject_continuous_clause`. Without this the AST subject falls
    // back to `TargetFilter::Any` (broadcasting the grant to every permanent).
    let subject = strip_trailing_additive_adverb(text[..verb_start].trim());
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

    // CR 205.1a + CR 205.1b + CR 613.1d + CR 611.2a + CR 400.7 + CR 603.6:
    // type-REPLACEMENT copula — "It's a Treasure artifact with '<ability>', and it
    // loses all other card types" (Vraska, the Silencer). The "loses all other card
    // types" tail (kept attached to the copula by the sequence splitter) routes this
    // to the shared `SetCardTypes` + subtype + granted-ability builder — the same
    // modifications the copy path already emits (Shelob) — reused here on the
    // subject-copula path. Tried BEFORE the additive form because the replacement
    // tail must win over the type-addition reading (mirrors the copy-path ordering
    // in `parse_except_body`). Like the animation arm below, this anaphor names the
    // object the PRECEDING clause returned (a reanimated card), never the source
    // permanent, so gate on a real prior referent (`ParentTarget`, or the
    // `TriggeringSource` that was returned by a dies-trigger reanimate) and decline
    // `SelfRef` so a source-permanent misbind honest-defers instead. Installed as an
    // indefinite continuous effect that ends when the returned object leaves play
    // (CR 611.2a: no stated duration; CR 400.7: a new object on re-entry is not the
    // same object — mirrors `install_aura_continuous_effect`).
    if let Some((_, modifications)) =
        super::become_copy_except::parse_its_a_type_loses_others(&lower)
    {
        let affected = static_affected_for_application(&application);
        if matches!(
            affected,
            TargetFilter::ParentTarget | TargetFilter::TriggeringSource
        ) {
            return Some(ClauseAst::SubjectPredicate {
                subject: Box::new(SubjectPhraseAst {
                    affected: application.affected.clone(),
                    target: application.target.clone(),
                    multi_target: application.multi_target.clone(),
                    inherits_parent: application.inherits_parent,
                    is_optional: application.is_optional,
                }),
                predicate: Box::new(PredicateAst::Become {
                    effect: Effect::GenericEffect {
                        static_abilities: vec![StaticDefinition::continuous()
                            .affected(affected)
                            .modifications(modifications)
                            .description(predicate.clone())],
                        duration: Some(Duration::UntilHostLeavesPlay),
                        target: application.target.clone(),
                    },
                    duration: Some(Duration::UntilHostLeavesPlay),
                    sub_ability: None,
                }),
            });
        }
    }

    // CR 205.1b: additive form first — "it's a [type] in addition to its other
    // types" retains prior types (AddType/AddSubtype only).
    if let Some(clause) = build_additive_type_continuous_clause(&application, &predicate) {
        return Some(ClauseAst::SubjectPredicate {
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
        });
    }

    // CR 205.1a + CR 613.1d: non-additive animation — "it's a 3/3 Robot artifact
    // creature with flying" sets the referenced permanent's base P/T, card types,
    // and keywords. The copula "is" form is equivalent to the verb "become" here,
    // so reuse the shared animation builder rather than re-deriving the spec.
    // Routes through `build_become_clause`, which delegates to
    // `parse_animation_spec`/`animation_modifications`.
    //
    // Honest-bind gate: a non-additive animation joined by "and it's a …" /
    // ". It's a …" is an anaphor to the permanent the *preceding* clause acted
    // on (a returned/created object), never the source permanent itself. Only
    // emit when the subject application resolves to a real prior referent
    // (`ParentTarget` — set when the chain carries a typed referent the
    // `parent_target_available` ctx propagates onto the "it" anaphor). If it
    // would bind to `SelfRef` (no prior typed referent in scope — e.g. the
    // anaphoric "Return it … and it's a 3/3 …" or modal-else branch), decline so
    // the clause honest-defers to `Effect::unimplemented` rather than silently
    // animating the wrong object. The additive "… in addition to its other
    // types" form above is unaffected (it is a type *addition* and stays on the
    // referenced subject regardless).
    if !matches!(
        static_affected_for_application(&application),
        TargetFilter::ParentTarget
    ) {
        return None;
    }
    let become_predicate = format!("becomes {rest_original}");
    let clause = build_become_clause(application.clone(), &become_predicate, ctx)?;
    Some(ClauseAst::SubjectPredicate {
        subject: Box::new(SubjectPhraseAst {
            affected: application.affected,
            target: application.target,
            multi_target: application.multi_target,
            inherits_parent: application.inherits_parent,
            is_optional: application.is_optional,
        }),
        predicate: Box::new(PredicateAst::Become {
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
    // CR 608.2c: An additive "also" sitting between a filter subject and its
    // continuous verb ("Kithkin creatures you control *also* gain first strike
    // until end of turn") is a natural-language connector with no semantic
    // weight — it chains this grant onto a preceding effect (the pump in
    // "creatures you control get +1/+0 ... . <subtype> creatures you control
    // also gain <keyword> ..."). Strip it so the residual filter subject routes
    // through the standard subject grammar; without the strip the trailing
    // "also" leaks into `parse_target`, which rejects the subject and drops the
    // whole grant to `Effect::Unimplemented`. Mirrors the self-ref additive
    // strip in `parse_effect_clause_inner` ("~ also gains ...").
    let subject = strip_trailing_additive_adverb(text[..verb_start].trim());
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
    build_continuous_clause(application, predicate, ctx)
}

/// CR 611.3a + CR 702.16: A multi-clause conditional protection grant
/// ("creatures you control gain protection from white if you control a Plains,
/// from blue if you control an Island, ..., and from green if you control a
/// Forest" — Dominaria's Judgment). Must be tried on the FULL clause text before
/// the generic suffix-condition strip, which would peel only the final clause's
/// condition and collapse the rest. Splits subject from predicate and emits one
/// conditionally-gated grant per color.
pub(super) fn try_parse_conditional_protection_grant_clause(
    text: &str,
    ctx: &mut ParseContext,
) -> Option<ParsedEffectClause> {
    // CR 611.2a: "Until end of turn, creatures you control gain ..." carries a
    // leading duration ahead of the subject; peel it (propagating the duration)
    // so the subject grammar sees a bare "creatures you control" subject.
    let (text, leading_duration) = strip_leading_duration(text);
    let verb_start = find_predicate_start(text)?;
    let subject = text[..verb_start].trim();
    let predicate = text[verb_start..].trim();
    if subject.eq_ignore_ascii_case("you") {
        return None;
    }
    let application = parse_subject_application(subject, ctx)?;
    build_conditional_protection_grant_clause(&application, predicate, leading_duration)
}

/// CR 611.3a + CR 702.16: Build one conditional continuous protection grant per
/// "from `<color>` if you control a `<land>`" clause (Dominaria's Judgment), so
/// each color's protection is gated on its own land condition rather than the
/// whole grant sharing only the final clause's condition.
fn build_conditional_protection_grant_clause(
    application: &SubjectApplication,
    predicate: &str,
    leading_duration: Option<Duration>,
) -> Option<ParsedEffectClause> {
    let (without_duration, trailing_duration) = super::strip_trailing_duration(predicate);
    let grants =
        crate::parser::oracle_static::parse_conditional_protection_grant_list(without_duration)?;
    let duration = leading_duration.or(trailing_duration);
    let affected = static_affected_for_application(application);
    let static_abilities = grants
        .into_iter()
        .map(|(target, condition)| {
            let def = StaticDefinition::continuous()
                .affected(affected.clone())
                .modifications(vec![ContinuousModification::AddKeyword {
                    keyword: crate::types::keywords::Keyword::Protection(target),
                }]);
            // The final grant's condition may have been peeled one layer up and
            // is re-applied to it after this clause returns; emit it unconditioned.
            match condition {
                Some(cond) => def.condition(cond),
                None => def,
            }
        })
        .collect();
    Some(ParsedEffectClause {
        effect: Effect::GenericEffect {
            static_abilities,
            duration: duration.clone(),
            target: application.target.clone(),
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
    // CR 707.9: A copy's `, except <body>` clause (Sarkhan, Soul Aflame — "become
    // a copy of it until end of turn, except its name is ~ and it's legendary in
    // addition to its other types") owns any "in addition to its other types"
    // tail. That supertype belongs in the `BecomeCopy` additional_modifications
    // produced by `become_copy_except`, not a standalone additive-type static —
    // so decline here (letting the copy parser win) rather than let the scanned
    // `is` inside "its name is ~" be read as this clause's copula verb.
    let predicate_lower = predicate.to_lowercase();
    if nom_primitives::split_once_on(&predicate_lower, ", except").is_ok() {
        return None;
    }
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

/// CR 611.3 + CR 105.2 + CR 305.7: "all `<X>` become `<P>` and all `<Y>` become
/// `<Q>`" — compound-quantified dual-subject become. Nightcreep is the exemplar:
/// each conjunct applies its own subject-specific transformation (creatures →
/// black, lands → Swamp). Declines when fewer than two conjuncts resolve, when
/// any conjunct lacks a become verb, or when subjects share one predicate
/// ("all creatures and all lands become Swamps").
fn try_parse_compound_all_subjects_become_clause(
    text: &str,
    ctx: &mut ParseContext,
) -> Option<ParsedEffectClause> {
    let conjuncts = peel_compound_all_quantified_conjuncts(text)?;
    let mut static_abilities = Vec::new();
    let mut merged_duration = None;
    let mut merged_target = None;

    for conjunct in conjuncts {
        let conjunct_lower = conjunct.to_lowercase();
        let tp = TextPair::new(&conjunct, &conjunct_lower);
        let (subject_tp, predicate_tp) = tp
            .split_around(" becomes ")
            .or_else(|| tp.split_around(" become "))?;
        let predicate_with_verb = if nom_primitives::scan_contains(&conjunct_lower, " becomes ") {
            format!("becomes {}", predicate_tp.original.trim())
        } else {
            format!("become {}", predicate_tp.original.trim())
        };
        let predicate_lower = predicate_with_verb.to_lowercase();
        alt((tag::<_, _, OracleError<'_>>("become "), tag("becomes ")))
            .parse(predicate_lower.as_str())
            .ok()?;
        let affected = parse_continuous_subject_filter(subject_tp.original.trim())?;
        let application = SubjectApplication {
            affected,
            target: None,
            multi_target: None,
            inherits_parent: false,
            is_optional: false,
        };
        let clause = build_become_clause(application, &predicate_with_verb, ctx)?;
        let Effect::GenericEffect {
            static_abilities: mut defs,
            duration,
            target,
            ..
        } = clause.effect
        else {
            return None;
        };
        if merged_duration.is_none() {
            merged_duration = duration.or(clause.duration);
        }
        if merged_target.is_none() {
            merged_target = target;
        }
        static_abilities.append(&mut defs);
    }

    if static_abilities.len() < 2 {
        return None;
    }

    Some(ParsedEffectClause {
        effect: Effect::GenericEffect {
            static_abilities,
            duration: merged_duration.clone(),
            target: merged_target,
        },
        duration: merged_duration,
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
    // CR 608.2c: a bare "becomes <descriptor>" conjunct (no leading subject) is
    // the second half of a compound-become instruction whose subject carried over
    // from the prior conjunct — Alacrian Armory's "that permanent becomes saddled
    // if it's a Mount and becomes an artifact creature if it's a Vehicle", where
    // the sequence splitter peels "becomes an artifact creature …" off as its own
    // chunk. Resolve the empty subject through the same context-dependent "it"
    // anaphor the explicit "it becomes …" form uses (parent target / triggering
    // source), so the second animation binds to the same object as the first.
    let application = if subject.is_empty() {
        parse_subject_application("it", ctx)?
    } else {
        parse_subject_application(subject, ctx)?
    };
    build_become_clause(application, &predicate, ctx)
}

/// CR 208.1: which base characteristic(s) a "base power [and toughness] become"
/// clause overwrites. `set_power`/`set_toughness` are independent so the
/// power-only axis (Pupu UFO) and the both-axes axis (Moon Girl, Porcelain
/// Gallery, Sita Varma) share one parser rather than proliferating arms.
struct BasePtSetAxes {
    set_power: bool,
    set_toughness: bool,
}

/// CR 208.1: Parse the "base power [and [base] toughness]" characteristic axes
/// after the possessive marker, returning the axes and the remainder positioned
/// at the "become[s] " copula. Factored per the nom mandate so the optional
/// "base " on the second noun and the optional "and toughness" conjunct are each
/// a single combinator.
fn parse_base_pt_axes(input: &str) -> OracleResult<'_, BasePtSetAxes> {
    // CR 208.1: "base toughness" alone (toughness-only, symmetric with the
    // power-only axis) — Sentinel / Wall of Tombstones set base toughness without
    // touching base power. Tried first; it cannot shadow the both-axes form,
    // which always opens with "base power" ("base power and base toughness").
    if let Ok((rest, _)) = tag::<_, _, OracleError<'_>>("base toughness").parse(input) {
        return Ok((
            rest,
            BasePtSetAxes {
                set_power: false,
                set_toughness: true,
            },
        ));
    }
    let (input, _) = tag("base power").parse(input)?;
    let (input, toughness) =
        opt(alt((tag(" and base toughness"), tag(" and toughness")))).parse(input)?;
    Ok((
        input,
        BasePtSetAxes {
            set_power: true,
            set_toughness: toughness.is_some(),
        },
    ))
}

/// CR 208.1 + CR 613.4b: The dynamic-or-fixed value side of a "base power …
/// become[s] <value>" clause, resolved into per-axis layer-7b modifications.
enum BasePtSetValue {
    /// Fixed "N/M" — `SetPower`/`SetToughness`.
    Fixed { power: i32, toughness: i32 },
    /// Dynamic "[each] equal to <quantity>" — `SetPowerDynamic`/`SetToughnessDynamic`.
    Dynamic(QuantityExpr),
    /// Split dynamic values per axis — Amplifire ("becomes twice that card's power
    /// and its base toughness becomes twice that card's toughness").
    SplitDynamic {
        power: QuantityExpr,
        toughness: QuantityExpr,
    },
}

/// CR 208.1: Parse the value following the "become[s] " copula of a base-P/T-set
/// clause. Tries the fixed "N/M" form first (so "6/6" is not mis-routed through
/// the quantity grammar), then the dynamic "[each] equal to <quantity>" form,
/// which routes through the shared CDA quantity grammar so every recognized
/// count/aggregate/possessive-power phrase composes ("the number of Towns you
/// control", "~'s power", …).
fn parse_base_pt_set_value(remainder: &str) -> Option<(BasePtSetValue, &str)> {
    if let Some((power, toughness, after_pt)) =
        super::animation::parse_fixed_become_pt_prefix(remainder)
    {
        return Some((BasePtSetValue::Fixed { power, toughness }, after_pt));
    }
    // CR 208.1: "[each] equal to <quantity>" dynamic value. "each equal to" and
    // "equal to" are the two surface forms (each/each-not are not independent
    // axes here — the optional "each " is the only variation).
    let lower = remainder.to_lowercase();
    // Return `()` (owned) from the closure so its result does not borrow the
    // temporary `lower`; `nom_on_lower` hands back the post-match remainder in
    // original case.
    let (_, after_copula) = nom_on_lower(remainder, &lower, |i| {
        let (i, _) = opt(tag::<_, _, OracleError<'_>>("each ")).parse(i)?;
        value((), tag::<_, _, OracleError<'_>>("equal to ")).parse(i)
    })?;
    let tail = after_copula.trim().trim_end_matches('.').trim();
    let expr = oracle_quantity::parse_cda_quantity(tail)
        .or_else(|| oracle_quantity::parse_event_context_quantity(tail))?;
    Some((BasePtSetValue::Dynamic(expr), ""))
}

/// CR 208.1 + CR 613.4b: Parse the copula that separates a base-P/T subject from
/// its value. The intransitive "become[s] " form and the transitive
/// "change … to " form (Riptide Mangler, Shape Stealer, Halfdane) share one
/// downstream value/emission path; `is_change` selects the token so the two
/// surface verbs are a single parameterized copula rather than duplicated arms.
fn parse_base_pt_copula(input: &str, is_change: bool) -> OracleResult<'_, ()> {
    if is_change {
        value((), tag(" to ")).parse(input)
    } else {
        value((), (tag(" become"), opt(tag("s")), tag(" "))).parse(input)
    }
}

/// CR 208.1 + CR 613.4b: value side of the transitive "change <subject>'s base
/// power [and toughness] to <value>" frame. Unlike the "become[s] equal to"
/// copula, the "change … to" frame introduces the value with a bare " to ", so
/// the value is a fixed "N/M" (Brine Hag), a paired "<X>'s power and toughness"
/// referent (Shape Stealer, Halfdane), or a bare single-axis quantity (Riptide
/// Mangler). Each form routes to the exact same building block the copula form
/// uses — no value grammar is duplicated.
fn parse_change_base_pt_value(remainder: &str) -> Option<(BasePtSetValue, &str)> {
    if let Some((power, toughness, after)) =
        super::animation::parse_fixed_become_pt_prefix(remainder)
    {
        return Some((BasePtSetValue::Fixed { power, toughness }, after));
    }
    if let Some((power, toughness)) = parse_pt_pair_referent(remainder) {
        return Some((BasePtSetValue::SplitDynamic { power, toughness }, ""));
    }
    let tail = remainder.trim().trim_end_matches('.').trim();
    let expr = parse_base_pt_axis_quantity(tail)?;
    Some((BasePtSetValue::Dynamic(expr), ""))
}

/// CR 208.1: Resolve a paired "<X>'s power and toughness" / "the power and
/// toughness of <X>" referent into its two single-axis quantities, both reading
/// the same object `X` (its power feeds base power, its toughness feeds base
/// toughness). Rather than duplicate the referent-scope grammar (event-context
/// "that creature", "target creature", source), each axis is resolved by feeding
/// the reconstructed single-axis phrase back through `parse_base_pt_axis_quantity`
/// — the same combinator the copula form already uses — so every recognized
/// referent scope composes automatically.
fn parse_pt_pair_referent(tail: &str) -> Option<(QuantityExpr, QuantityExpr)> {
    let trimmed = tail.trim().trim_end_matches('.').trim();
    let lower = trimmed.to_lowercase();

    // Possessive: "<X>'s power and toughness" (ASCII or Unicode apostrophe).
    // Capture the possessor with nom `take_until` up to the "'s power and
    // toughness" tail (prefix-oriented, mirroring the possessive subject
    // grammar), requiring the tail to consume the remainder so only a genuine
    // suffix matches. `apostrophe` re-attaches the possessive marker when
    // reconstructing each single-axis referent.
    for (marker, apostrophe) in [
        ("'s power and toughness", "'s"),
        ("\u{2019}s power and toughness", "\u{2019}s"),
    ] {
        if let Ok((rest_lower, possessor_lower)) =
            (take_until::<_, _, OracleError<'_>>(marker), tag(marker))
                .map(|(possessor, _)| possessor)
                .parse(lower.as_str())
        {
            if !rest_lower.is_empty() {
                continue;
            }
            let possessor = &trimmed[..possessor_lower.len()];
            let power = parse_base_pt_axis_quantity(&format!("{possessor}{apostrophe} power"))?;
            let toughness =
                parse_base_pt_axis_quantity(&format!("{possessor}{apostrophe} toughness"))?;
            return Some((power, toughness));
        }
    }

    // Inverted genitive: "the power and toughness of <X>".
    if let Ok((rest, _)) =
        tag::<_, _, OracleError<'_>>("the power and toughness of ").parse(lower.as_str())
    {
        let object = &trimmed[trimmed.len() - rest.len()..];
        let power = parse_base_pt_axis_quantity(&format!("the power of {object}"))?;
        let toughness = parse_base_pt_axis_quantity(&format!("the toughness of {object}"))?;
        return Some((power, toughness));
    }

    None
}

/// Parse a single-axis base-P/T quantity tail, including "twice that card's power"
/// where the inner referent is event-context scoped (Amplifire).
fn parse_base_pt_axis_quantity(tail: &str) -> Option<QuantityExpr> {
    type VE<'a> = OracleError<'a>;
    if let Some(qty) = oracle_quantity::parse_cda_quantity(tail)
        .or_else(|| oracle_quantity::parse_event_context_quantity(tail))
    {
        return Some(qty);
    }
    let lower = tail.to_lowercase();
    let Ok((rest_lower, factor)) = alt((
        value(2i32, tag::<_, _, VE>("twice ")),
        value(3, tag("three times ")),
    ))
    .parse(lower.as_str()) else {
        return None;
    };
    let inner_text = tail[tail.len() - rest_lower.len()..].trim();
    let inner = oracle_quantity::parse_event_context_quantity(inner_text)?;
    Some(QuantityExpr::Multiply {
        factor,
        inner: Box::new(inner),
    })
}

/// CR 208.1 + CR 608.2c: "<power-expr> and its base toughness becomes <toughness-expr>"
/// when power and toughness each carry independent dynamic quantities (Amplifire).
fn parse_split_base_pt_dynamic_values(
    remainder: &str,
) -> Option<(QuantityExpr, QuantityExpr, &str)> {
    const TOUGHNESS_INTRO: &str = " and its base toughness becomes ";
    let (power_part, rest) = nom_primitives::split_once_on(remainder, TOUGHNESS_INTRO)
        .ok()?
        .1;
    let power_tail = power_part.trim().trim_end_matches('.').trim();
    let tough_tail = rest.trim().trim_end_matches('.').trim();
    let power = parse_base_pt_axis_quantity(power_tail)?;
    let toughness = parse_base_pt_axis_quantity(tough_tail)?;
    Some((power, toughness, ""))
}

/// CR 613.4b + CR 613.1f: "[subject]'s base power [and toughness] become[s]
/// <value> [and (it/they) gain(s)/has/have <keywords>]" — a set-base-P/T plus
/// keyword-grant continuous effect on the possessor, with NO type/subtype
/// change. Also handles the inverted-genitive surface form "the base power and
/// toughness of [subject] become[s] <value>" (Sita Varma).
///
/// This differs grammatically from the `becomes a [type] with base power and
/// toughness N/M` animation form (handled by `parse_animation_spec`): here the
/// grammatical subject is the *possessive* "[subject]'s base power and
/// toughness", and the verb "become" acts on the P/T characteristics, not on the
/// permanent's card type. So it produces a `GenericEffect` carrying
/// `SetPower`/`SetToughness` (fixed) or `SetPowerDynamic`/`SetToughnessDynamic`
/// (dynamic, Layer 7b, CR 613.4b) and `AddKeyword` (Layer 6, CR 613.1f)
/// modifications, without any `AddType`/`AddSubtype`. Covers Moon Girl and Devil
/// Dinosaur ("~'s base power and toughness become 6/6 and they gain trample"),
/// Pupu UFO ("this creature's base power becomes equal to the number of Towns you
/// control"), Sita Varma ("the base power and toughness of each other creature
/// you control become equal to ~'s power"), and the broader class of
/// "<permanent>'s base power [and toughness] become[s] <value> …" effects.
fn try_parse_subject_base_pt_set_clause_ast(
    text: &str,
    ctx: &mut ParseContext,
) -> Option<ClauseAst> {
    type VE<'a> = OracleError<'a>;

    // CR 611.2a: A standalone effect line may carry a *leading* duration
    // ("Until end of turn, ~'s base power and toughness become 6/6 …"). Strip it
    // here and thread it onto the clause; inside a trigger the sequence layer has
    // already stripped it, so this is a no-op in that path.
    let (body, leading_duration) = strip_leading_duration(text);

    let lower = body.to_lowercase();

    // CR 613.4b: two verb surface forms set base P/T with no type change:
    //   (a) the intransitive "become[s] <value>" copula (Moon Girl, Pupu, Sita
    //       Varma), and
    //   (b) the transitive "change <subject>'s base power [and toughness] to
    //       <value>" frame (Riptide Mangler, Shape Stealer, Halfdane, Eldrazi
    //       Mimic).
    // Strip the optional "change"/"you may change" verb so the shared possessive
    // subject/axes grammar below applies to both forms; the copula token is then
    // " to " (transitive) instead of " become[s] ", and the value side is bare
    // (no "equal to" lead-in). Only the possessive subject form takes the
    // transitive verb — the inverted-genitive "the base power and toughness of
    // <subject>" would read the value through " to ", which collides with a
    // " to " inside the subject (Brine Hag: "creatures that dealt damage to it"),
    // so the inverted form stays copula-only.
    let (parse_lower, is_change) = match alt((
        tag::<_, _, VE>("you may change "),
        tag::<_, _, VE>("change "),
    ))
    .parse(lower.as_str())
    {
        Ok((rest, _)) => (rest, true),
        Err(_) => (lower.as_str(), false),
    };
    // Recover the original-case body at the same offset (the verb prefix, if
    // any, is ASCII so the byte length delta is identical in `body`).
    let parse_body = &body[body.len() - parse_lower.len()..];

    // Two surface forms, each yielding `(subject_text, axes, value_remainder)`:
    //   1. Possessive:  "<subject>'s base power [and toughness] <copula> <value>"
    //   2. Inverted:    "the base power and toughness of <subject> become[s] <value>"
    // The possessive marker may be ASCII `'s` or the Unicode right single quote
    // `\u{2019}s`. `to_lowercase` preserves byte length/offsets for this text
    // (ASCII letters, apostrophes, digits, slashes), so a span taken from
    // `parse_lower` indexes the same bytes in `parse_body`.
    let inverted = if is_change {
        // The inverted genitive is copula-only (see the `is_change` note above).
        None
    } else {
        // Inverted genitive: "the base power and toughness of <subject> become[s] "
        (
            preceded(tag::<_, _, VE>("the "), parse_base_pt_axes),
            tag(" of "),
            take_until::<_, _, VE>(" become"),
            tag(" become"),
            opt(tag("s")),
            tag(" "),
        )
            .parse(parse_lower)
            .ok()
    };
    let (subject, axes, remainder) =
        if let Some((rest_lower, (axes, _, subject_lower, _, _, _))) = inverted {
            // `subject_lower` is a sub-slice of `parse_lower`; its byte offset is
            // the pointer delta. Recover original case at the same span.
            let subject_start = subject_lower.as_ptr() as usize - parse_lower.as_ptr() as usize;
            let subject = parse_body
                .get(subject_start..subject_start + subject_lower.len())?
                .trim();
            let remainder = &parse_body[parse_body.len() - rest_lower.len()..];
            (subject, axes, remainder)
        } else {
            // Possessive: "<subject>'s base power [and toughness]" followed by the
            // copula (" to " for the transitive "change" frame, else "become[s] ").
            // Anchor on "'s base " (not "'s base power") so the toughness-only
            // axis ("~'s base toughness") reaches `parse_base_pt_axes`, which
            // classifies the specific characteristic word.
            let (rest_lower, (subject_lower, axes)) = alt((
                (
                    take_until::<_, _, VE>("'s base "),
                    tag("'s "),
                    parse_base_pt_axes,
                )
                    .map(|(subject, _, axes)| (subject, axes)),
                (
                    take_until::<_, _, VE>("\u{2019}s base "),
                    tag("\u{2019}s "),
                    parse_base_pt_axes,
                )
                    .map(|(subject, _, axes)| (subject, axes)),
            ))
            .parse(parse_lower)
            .ok()?;
            let (rest_lower, ()) = parse_base_pt_copula(rest_lower, is_change).ok()?;
            let subject = parse_body[..subject_lower.len()].trim();
            let remainder = &parse_body[parse_body.len() - rest_lower.len()..];
            (subject, axes, remainder)
        };

    // Parse the value side. The transitive "change … to" frame carries a bare
    // value (fixed, paired referent, or single-axis quantity); the copula form
    // carries "[each] equal to <quantity>" or an Amplifire-style per-axis split.
    let (value, after_pt) = if is_change {
        parse_change_base_pt_value(remainder)?
    } else {
        parse_base_pt_set_value(remainder).or_else(|| {
            parse_split_base_pt_dynamic_values(remainder).map(|(power, toughness, after)| {
                (BasePtSetValue::SplitDynamic { power, toughness }, after)
            })
        })?
    };

    // Parse the optional trailing keyword-grant conjunct ("and they gain trample").
    let keywords = parse_base_pt_set_trailing_keywords(after_pt);

    let application = parse_subject_application(subject, ctx)?;
    let affected = static_affected_for_application(&application);

    // CR 208.1 + CR 613.4b: emit per-axis layer-7b set modifications. Fixed
    // values stay `SetPower`/`SetToughness`; dynamic values use the
    // `SetPowerDynamic`/`SetToughnessDynamic` variants the layer system
    // re-evaluates each tick.
    let mut modifications = Vec::new();
    match value {
        BasePtSetValue::Fixed { power, toughness } => {
            if axes.set_power {
                modifications.push(ContinuousModification::SetPower { value: power });
            }
            if axes.set_toughness {
                modifications.push(ContinuousModification::SetToughness { value: toughness });
            }
        }
        BasePtSetValue::Dynamic(expr) => {
            if axes.set_power {
                modifications.push(ContinuousModification::SetPowerDynamic {
                    value: expr.clone(),
                });
            }
            if axes.set_toughness {
                modifications.push(ContinuousModification::SetToughnessDynamic { value: expr });
            }
        }
        BasePtSetValue::SplitDynamic { power, toughness } => {
            modifications.push(ContinuousModification::SetPowerDynamic { value: power });
            modifications.push(ContinuousModification::SetToughnessDynamic { value: toughness });
        }
    }
    if modifications.is_empty() {
        return None;
    }
    modifications.extend(
        keywords
            .into_iter()
            .map(|keyword| ContinuousModification::AddKeyword { keyword }),
    );

    let effect = Effect::GenericEffect {
        static_abilities: vec![StaticDefinition::continuous()
            .affected(affected)
            .modifications(modifications)
            .description(body.trim_end_matches('.').to_string())],
        // CR 611.2a: a leading duration stripped above is threaded onto the
        // GenericEffect; otherwise the sequence layer's wrapping duration (for
        // the trigger-body path, where it is already stripped upstream) applies.
        duration: leading_duration.clone(),
        target: application.target.clone(),
    };

    Some(ClauseAst::SubjectPredicate {
        subject: Box::new(SubjectPhraseAst {
            affected: application.affected,
            target: application.target,
            multi_target: application.multi_target,
            inherits_parent: application.inherits_parent,
            is_optional: application.is_optional,
        }),
        predicate: Box::new(PredicateAst::Continuous {
            effect,
            duration: leading_duration,
            sub_ability: None,
        }),
    })
}

/// Strip a leading duration phrase ("Until end of turn, " / "This turn, ") off a
/// standalone effect line, returning `(remaining_body, duration)`. When no
/// leading duration is present, returns `(text, None)` so the caller is a no-op.
fn strip_leading_duration(text: &str) -> (&str, Option<Duration>) {
    type VE<'a> = OracleError<'a>;
    let lower = text.to_lowercase();

    let parsed = (
        parse_duration,
        opt(tag::<_, _, VE>(",")),
        nom::character::complete::multispace1,
    )
        .parse(lower.as_str());
    let Ok((rest_lower, (duration, _, _))) = parsed else {
        return (text, None);
    };
    let body = &text[text.len() - rest_lower.len()..];
    (body, Some(duration))
}

/// Parse the trailing "[, ] and [it/they] gain(s)/has/have <keyword list>"
/// conjunct after a "base power and toughness become N/M" clause. Returns the
/// recognized keywords (empty when no trailing conjunct or no keywords parse).
fn parse_base_pt_set_trailing_keywords(after_pt: &str) -> Vec<Keyword> {
    type VE<'a> = OracleError<'a>;

    let lower = after_pt.to_lowercase();
    let intro = (
        opt(tag::<_, _, VE>(",")),
        multispace0,
        tag("and "),
        opt(alt((tag("it "), tag("they "), tag("he "), tag("she ")))),
        alt((tag("gains "), tag("gain "), tag("has "), tag("have "))),
    );
    let Ok((rest, _)) = value((), intro).parse(lower.as_str()) else {
        return Vec::new();
    };

    let raw = rest.trim().trim_end_matches('.');
    super::token::split_token_keyword_list(raw)
        .into_iter()
        .filter_map(super::token::map_token_keyword)
        .collect()
}

/// CR 508.1d + CR 508.1h: "[creatures] can't attack [you] unless [player] pays
/// {N} [for each of those creatures]" as a one-shot effect (Summon: Yojimbo
/// chapters II/III). The static-side `parse_combat_tax_static` authority
/// already lowers the UnlessPay payload; wrap it in `GrantStaticAbility` on
/// `SelfRef` so `register_transient_effect` installs the tax on the resolving
/// source for the effect's duration (mirrors quoted-static grant routing in
/// `oracle_static/grammar.rs`).
fn try_parse_combat_tax_effect_clause(text: &str) -> Option<ParsedEffectClause> {
    let static_def = parse_static_line(text)?;
    if !matches!(
        static_def.condition,
        Some(StaticCondition::UnlessPay { .. })
    ) {
        return None;
    }
    Some(ParsedEffectClause {
        effect: Effect::GenericEffect {
            static_abilities: vec![StaticDefinition::continuous().modifications(vec![
                ContinuousModification::GrantStaticAbility {
                    definition: Box::new(static_def),
                },
            ])],
            duration: None,
            target: Some(TargetFilter::SelfRef),
        },
        distribute: None,
        multi_target: None,
        duration: None,
        sub_ability: None,
        condition: None,
        optional: false,
        unless_pay: None,
    })
}

/// CR 509.1b + CR 611.2c: "<source> and up to N other target creature(s) can't
/// be blocked [this turn]" — a conjoined-subject evasion grant (Martha Jones:
/// "Martha Jones and up to one other target creature can't be blocked this
/// turn."). The subject is a conjunction of the source (self-ref) and a
/// separately-targeted creature; the single predicate applies to BOTH conjuncts.
///
/// Each conjunct is granted the SAME restriction through the ordinary
/// single-subject builder ([`build_restriction_clause`]): the source grant is the
/// primary effect, and the targeted grant rides as a `sub_ability` continuation
/// carrying its own `multi_target` so the "up to one" optional creature is
/// selected independently of the source. Scoped to the can't-be-blocked class —
/// other "<X> and <Y> can't …" compounds fall through to the generic split. This
/// builds the class of "<self> and up to N other target creature(s) can't be
/// blocked" riders, not a single card.
///
/// Returns a fully-built [`ClauseAst`] (not routed through
/// `subject_predicate_ast_from_clause`) because that helper re-derives the
/// subject from the full text — it cannot split the compound back into a clean
/// primary subject, and would leak the secondary's `multi_target` onto the
/// primary grant. The primary `SubjectPhraseAst` is the source conjunct alone
/// (no target/multi_target); the secondary rides in the predicate's `sub_ability`.
fn try_parse_source_and_other_restriction_clause(
    text: &str,
    ctx: &mut ParseContext,
) -> Option<ClauseAst> {
    let lower = text.to_lowercase();

    // Use nom `take_until + alt` to locate the " can't " / " cannot " predicate
    // boundary. Per the nom-combinator mandate, parsing recognition is expressed
    // with combinators on first write — `take_until` finds the predicate marker
    // without string-method dispatch.
    let ((), predicate_with_space) = nom_on_lower(text, &lower, |input| {
        alt((
            value((), take_until::<_, _, OracleError<'_>>(" can't ")),
            value((), take_until::<_, _, OracleError<'_>>(" cannot ")),
        ))
        .parse(input)
    })?;
    let subject = text[..text.len() - predicate_with_space.len()].trim();
    let predicate = predicate_with_space.trim_start();

    // Class gate: only the can't-be-blocked evasion grant participates.
    if !is_cant_be_blocked_restriction_predicate(&predicate.to_lowercase()) {
        return None;
    }

    // The subject must be a two-part conjunction "<primary> and <secondary>".
    let subject_lower = subject.to_lowercase();
    let subject_pair = TextPair::new(subject, &subject_lower);
    let (primary_tp, secondary_tp) = subject_pair.split_around(" and ")?;
    let primary_application = parse_subject_application(primary_tp.original.trim(), ctx)?;
    let secondary_application = parse_subject_application(secondary_tp.original.trim(), ctx)?;
    // The secondary conjunct must carry its own target slot ("up to one other
    // target creature"); a bare conjunction with no second target is not this
    // class and is left to the generic subject split. The primary conjunct, by
    // contrast, must NOT introduce its own target slot — its grant rides as a
    // direct (self-ref/anaphor) static, with the predicate's multi_target empty.
    secondary_application.target.as_ref()?;
    if primary_application.target.is_some() || primary_application.multi_target.is_some() {
        return None;
    }
    let secondary_multi = secondary_application.multi_target.clone();

    let primary_clause = build_restriction_clause(primary_application.clone(), predicate)?;
    let mut secondary_clause = build_restriction_clause(secondary_application, predicate)?;
    secondary_clause.multi_target = secondary_multi;
    let secondary_def = super::ability_definition_from_clause(AbilityKind::Spell, secondary_clause);

    Some(ClauseAst::SubjectPredicate {
        subject: Box::new(SubjectPhraseAst {
            affected: primary_application.affected,
            target: primary_application.target,
            multi_target: None,
            inherits_parent: primary_application.inherits_parent,
            is_optional: primary_application.is_optional,
        }),
        predicate: Box::new(PredicateAst::Restriction {
            effect: primary_clause.effect,
            duration: primary_clause.duration,
            sub_ability: Some(Box::new(secondary_def)),
        }),
    })
}

/// CR 611.2 + CR 201.2a + CR 115.1: "target &lt;T&gt; and all other &lt;T&gt;s with the same
/// name as that &lt;T&gt; get -N/-M [until end of turn]" (Bile Blight, Echoing Decay,
/// Echoing Courage's +2/+2). A compound subject sharing one pump predicate: the
/// announced creature is a single `Pump` and the same-name others are a mass
/// `PumpAll`. CR 611.2a — each object receives the continuous P/T change exactly
/// once, so the `PumpAll` must EXCLUDE the announced target (which the primary
/// `Pump` already covers); the `Not{ParentTarget}` conjunct is concretized
/// against the inherited target at resolution (`pump::resolve_all`), mirroring
/// Maelstrom Pulse's `Destroy`+`DestroyAll`. Runs before the generic continuous
/// clause, which resolves only the first conjunct and silently drops the mass
/// debuff (issue #4727).
fn try_parse_target_and_same_name_pump_clause(
    text: &str,
    ctx: &mut ParseContext,
) -> Option<ClauseAst> {
    let lower = text.to_lowercase();

    // Locate the pump predicate boundary (" gets " / " get ") with nom take_until
    // (nom-combinator dispatch, not string methods).
    let ((), predicate_with_space) = nom_on_lower(text, &lower, |input| {
        alt((
            value((), take_until::<_, _, OracleError<'_>>(" gets ")),
            value((), take_until::<_, _, OracleError<'_>>(" get ")),
        ))
        .parse(input)
    })?;
    let subject = text[..text.len() - predicate_with_space.len()].trim();
    let predicate = predicate_with_space.trim_start();

    // Class gate: the predicate must be a P/T pump ("get -3/-3 until end of turn").
    // Non-pump predicates fall through to the generic subject grammar.
    let (power, toughness, duration) =
        super::lower::parse_pump_clause_with_context(predicate, ctx)?;

    // The subject must be a two-part conjunction "&lt;target&gt; and &lt;same-name mass&gt;".
    let subject_lower = subject.to_lowercase();
    let subject_pair = TextPair::new(subject, &subject_lower);
    let (primary_tp, secondary_tp) = subject_pair.split_around(" and ")?;

    // Primary conjunct must announce a target ("target creature") — this is what
    // carries the CR 115.1 target slot the mass sub-ability inherits.
    let primary = parse_subject_application(primary_tp.original.trim(), ctx)?;
    primary.target.as_ref()?;

    // Secondary conjunct must be the same-name mass ("all other creatures with
    // the same name as that creature") — a class filter, not a target slot. The
    // `SameNameAsParentTarget` gate is the load-bearing discriminator: a plain
    // "target creature and each creature you control get +1/+1" secondary lacks
    // it and returns `None`, leaving the generic path untouched.
    let (secondary_filter, rest) = parse_target(secondary_tp.original.trim());
    if !rest.trim().is_empty() || !filter_carries_same_name_as_parent_target(&secondary_filter) {
        return None;
    }

    // Primary: single-target Pump on the announced creature.
    let primary_effect = build_pump_effect(&primary, power.clone(), toughness.clone());

    // Mass: PumpAll over the same-name others, EXCLUDING the announced target.
    let mass_target = TargetFilter::And {
        filters: vec![
            secondary_filter,
            TargetFilter::Not {
                filter: Box::new(TargetFilter::ParentTarget),
            },
        ],
    };
    let mass_clause = ParsedEffectClause {
        effect: Effect::PumpAll {
            power,
            toughness,
            target: mass_target,
        },
        duration: duration.clone().or(Some(Duration::UntilEndOfTurn)),
        sub_ability: None,
        distribute: None,
        multi_target: None,
        condition: None,
        optional: false,
        unless_pay: None,
    };
    let mass_def = super::ability_definition_from_clause(AbilityKind::Spell, mass_clause);

    Some(ClauseAst::SubjectPredicate {
        subject: Box::new(SubjectPhraseAst {
            affected: primary.affected,
            target: primary.target,
            multi_target: None,
            inherits_parent: primary.inherits_parent,
            is_optional: primary.is_optional,
        }),
        predicate: Box::new(PredicateAst::Continuous {
            effect: primary_effect,
            duration: duration.or(Some(Duration::UntilEndOfTurn)),
            sub_ability: Some(Box::new(mass_def)),
        }),
    })
}

/// Walks a `TargetFilter` for `FilterProp::SameNameAsParentTarget` (parser-side
/// mirror of the runtime `filter_refs_same_name_as_parent_target`).
fn filter_carries_same_name_as_parent_target(filter: &TargetFilter) -> bool {
    match filter {
        TargetFilter::Typed(typed) => typed
            .properties
            .iter()
            .any(|p| matches!(p, FilterProp::SameNameAsParentTarget)),
        TargetFilter::And { filters } | TargetFilter::Or { filters } => filters
            .iter()
            .any(filter_carries_same_name_as_parent_target),
        TargetFilter::Not { filter } => filter_carries_same_name_as_parent_target(filter),
        _ => false,
    }
}

/// CR 611.2b + CR 110.5: Recognize a trailing "for as long as &lt;target
/// anaphor&gt; remains tapped" duration on a `CantBeActivated` grant (Braided
/// Net: "Its activated abilities can't be activated for as long as it remains
/// tapped."). The anaphoric subject ("it" / "that creature" / …) co-refers with
/// the grant's target, which the arm binds to `ParentTarget`, so the tapped
/// gate is `IsTapped { scope: Target }` — NOT the standalone duration
/// combinator's source-scoped `SourceIsTapped`
/// (`oracle_nom::duration::parse_remains_tapped`), which has no clause subject
/// to disambiguate and defaults to the source. Returns the tapped-bound
/// `Duration` when the clause is present, `None` otherwise (caller keeps the
/// default `UntilEndOfTurn`, so suffix-free prohibitions are unchanged).
fn tapped_bound_prohibition_duration(lower: &str) -> Option<Duration> {
    nom_primitives::scan_at_word_boundaries(lower, |input: &str| {
        let (input, _) = tag::<_, _, OracleError<'_>>("for as long as ").parse(input)?;
        let (input, _) = alt((
            tag("it"),
            tag("that creature"),
            tag("that permanent"),
            tag("that artifact"),
            tag("~"),
        ))
        .parse(input)?;
        let (input, _) = tag(" remains tapped").parse(input)?;
        Ok((input, ()))
    })
    .map(|()| Duration::ForAsLongAs {
        condition: StaticCondition::IsTapped {
            scope: crate::types::ability::ObjectScope::Target,
        },
    })
}

fn try_parse_subject_restriction_clause(
    text: &str,
    ctx: &mut ParseContext,
) -> Option<ParsedEffectClause> {
    if let Some(clause) = try_parse_combat_tax_effect_clause(text) {
        return Some(clause);
    }

    let lower = text.to_lowercase();

    // CR 509.1c: "Target creature must be blocked [this turn] [if able]"
    // Handled separately because "must be blocked" isn't a "can't X" restriction pattern
    // and needs AddStaticMode for transient effect propagation through the layer system.
    let tp = TextPair::new(text, &lower);

    // CR 119.7 + CR 608.2c + CR 104.1: Screaming Nemesis's rider — "If a player
    // is dealt damage this way, they can't gain life for the rest of the game."
    // This sentence chains after the redirect sub-ability ("it deals that much
    // damage to any other target"); its anaphor ("a player ... this way" /
    // "they") refers to that redirect's TARGET, but CR 119.7 governs only
    // players, not creatures/planeswalkers. Bind the restriction's `affected`
    // to `ParentTarget`: at resolution `register_transient_effect` maps a
    // parent `TargetRef::Player` to a `SpecificPlayer` TCE (locking that
    // player) and a `TargetRef::Object` to a `SpecificObject` TCE — which the
    // player-scoped `player_has_cant_gain_life` query never reads — so the lock
    // correctly no-ops when the redirect struck a creature or planeswalker.
    // The recognizer consumes the anaphoric head; the residual "can't gain
    // life for the rest of the game" predicate (CR 104.1 permanence via "for
    // the rest of the game") flows into the shared restriction builder.
    if let Some(rest) = strip_dealt_damage_this_way_player_anaphor(&lower) {
        let offset = lower.len() - rest.len();
        let predicate = text[offset..].trim();
        let application = SubjectApplication {
            affected: TargetFilter::ParentTarget,
            target: None,
            multi_target: None,
            inherits_parent: false,
            is_optional: false,
        };
        return build_restriction_clause(application, predicate);
    }

    if let Some((before, _)) = tp.split_around(" must be blocked") {
        let subject = before.original.trim();
        let application = parse_subject_application(subject, ctx)?;
        let affected = static_affected_for_application(&application);
        return Some(ParsedEffectClause {
            effect: Effect::GenericEffect {
                static_abilities: vec![StaticDefinition::new(StaticMode::MustBeBlocked {
                    by: None,
                })
                .affected(affected)
                .modifications(vec![ContinuousModification::AddStaticMode {
                    mode: StaticMode::MustBeBlocked { by: None },
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

    // CR 508.1d (must-attack declaration) + CR 608.2c (one-shot anaphora binding)
    // + CR 611.2c (continuous effect affected-set) — mirrors the " must be blocked"
    // subject form (CR 509.1c). "[subject] attacks/attack this turn/combat if able"
    // for a targeted or set subject (Boiling Blood, Heckling Fiends, Incite, …):
    // the bare imperative recognizer drops the subject (target: None, affected:
    // None), silently un-binding the MustAttack requirement. Split off the subject
    // here and re-bind: target = the typed/targeted subject, static affected =
    // ParentTarget so `register_transient_effect` produces a per-target
    // SpecificObject TCE. Use `find_predicate_start`/`deconjugate_verb` (NOT
    // `split_around`, which consumes the "attack" needle and leaves a tail the
    // recognizer rejects) to yield subject + deconjugated "attack … if able"
    // predicate, exactly as `try_parse_subject_become_clause` does.
    if let Some(verb_start) = find_predicate_start(text) {
        let subject = text[..verb_start].trim();
        let predicate = deconjugate_verb(text[verb_start..].trim());
        // CR 508.1d + CR 509.1c: "[subject] attacks or blocks this turn/combat if
        // able" (Hustle) — the combined requirement re-binds BOTH MustAttack and
        // MustBlock statics to the subject. Tried before the plain attack
        // recognizer since both share the "attacks" verb prefix; the combined
        // form is the strict superset and must win.
        if let Some(ImperativeFamilyAst::GainKeyword(Effect::GenericEffect { duration, .. })) =
            imperative::try_parse_attack_or_block_if_able(&predicate)
        {
            let application = parse_subject_application(subject, ctx)?;
            let affected = static_affected_for_application(&application);
            let static_abilities = imperative::must_attack_or_block_static_definitions()
                .into_iter()
                .map(|def| def.affected(affected.clone()))
                .collect();
            return Some(ParsedEffectClause {
                effect: Effect::GenericEffect {
                    static_abilities,
                    duration: duration.clone(),
                    target: application.target,
                },
                distribute: None,
                multi_target: application.multi_target,
                duration,
                sub_ability: None,
                condition: None,
                optional: application.is_optional,
                unless_pay: None,
            });
        }
        // CR 701.15a + CR 701.15b: "[subject] attacks each combat if able and
        // attacks a player other than you if able" is the printed goad definition
        // (Maximum Carnage chapter I). Map it to `Effect::GoadAll` over the subject
        // population so the goad mechanic (goaded_by mark, "attack a player other
        // than the goading player", goading-player next-turn cleanup) handles it.
        // Tried before the plain attack recognizer since the goad compound is the
        // strict superset and must win. The subject is a population ("each
        // creature"), so the GoadAll target is `application.affected`.
        if imperative::try_parse_goad_equivalent(&predicate) {
            let application = parse_subject_application(subject, ctx)?;
            let goad_target = application
                .target
                .clone()
                .unwrap_or_else(|| application.affected.clone());
            return Some(ParsedEffectClause {
                effect: Effect::GoadAll {
                    target: goad_target,
                },
                distribute: None,
                multi_target: application.multi_target,
                duration: None,
                sub_ability: None,
                condition: None,
                optional: application.is_optional,
                unless_pay: None,
            });
        }
        // Classify via the existing recognizer. Only the bare GenericEffect form
        // (MustAttack) is re-bound here; the player-bound `ForceAttack` form
        // ("attacks you/that player …") has its own targeted handling and must
        // NOT be captured.
        if let Some(ImperativeFamilyAst::GainKeyword(Effect::GenericEffect { duration, .. })) =
            imperative::try_parse_attack_if_able(&predicate)
        {
            // `?` here makes a bare/source-granted "attacks this turn if able"
            // (empty subject, granted ability) fall through to None, preserving
            // the existing target:None behavior for that class.
            let application = parse_subject_application(subject, ctx)?;
            let affected = static_affected_for_application(&application);
            return Some(ParsedEffectClause {
                effect: Effect::GenericEffect {
                    static_abilities: vec![
                        imperative::must_attack_static_definition().affected(affected)
                    ],
                    duration: duration.clone(),
                    target: application.target,
                },
                distribute: None,
                multi_target: application.multi_target,
                duration,
                sub_ability: None,
                condition: None,
                optional: application.is_optional,
                unless_pay: None,
            });
        }
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
    // CR 605.1a: split on the predicate with either apostrophe glyph so a U+2019
    // effect clause ("target creature's activated abilities can't be activated
    // unless they're mana abilities") still reaches the shared exemption scan.
    if let Some((before, _)) = tp
        .split_around(" activated abilities can't be activated")
        .or_else(|| tp.split_around(" activated abilities can\u{2019}t be activated"))
    {
        let subject = before.original.trim();
        let application = subject_application_for_cant_be_activated(subject, ctx)?;
        let affected = static_affected_for_application(&application);
        // CR 605.1a: "unless they're mana abilities" exemption rides on the mode.
        let exemption = parse_cant_be_activated_exemption_in_text(&lower);
        // CR 611.2b + CR 110.5: "for as long as it remains tapped" (Braided Net)
        // ties the prohibition to the target's tap state; without the suffix
        // (Dovin Baan, Xathrid Gorgon) it keeps the default end-of-turn duration.
        let duration = tapped_bound_prohibition_duration(&lower).or(Some(Duration::UntilEndOfTurn));
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
                duration: duration.clone(),
                target: application.target,
            },
            distribute: None,
            multi_target: None,
            duration,
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
        // CR 608.2c + CR 701.19c: the DAMAGE-form anaphor ("a creature/creatures/
        // a permanent dealt damage this way") binds to the preceding damage
        // clause's published set (`TrackedSet`) rather than a fresh target.
        // Tried first so it pre-empts the generic subject resolution; non-damage
        // subjects (Hurr Jackal, Lim-Dûl's Cohort) return None here and fall
        // through to `subject_application_for_cant_be_activated`.
        let application = subject_application_for_cant_be_regenerated(subject)
            .or_else(|| subject_application_for_cant_be_activated(subject, ctx))?;
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
                .modifications(vec![ContinuousModification::AddStaticMode {
                    mode: StaticMode::CanAttackWithDefender,
                }])
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

    let (_rest, (_, _, _, _, _, count, duration, _)) = all_consuming((
        tag("can"),
        tag(" "),
        tag("block"),
        tag(" "),
        opt(tag("up to ")),
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
        opt(tag("up to ")),
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

/// CR 509.1b: predicate-only "can't be blocked [this turn] [except by … | by …]"
/// conjunct left after the sequence splitter peels a trailing evasion restriction
/// off a keyword/P/T grant ("gain haste until end of turn and can't be blocked
/// this turn except by creatures with haste"). Used by
/// `combat_requirement_conjunct_prepend` to re-attach the subject.
pub(super) fn is_cant_be_blocked_restriction_predicate(lower: &str) -> bool {
    let trimmed = lower.trim().trim_end_matches('.').trim();
    parse_cant_be_blocked_restriction_predicate(trimmed).is_ok()
        || parse_restriction_modes(trimmed).is_some_and(|modes| {
            modes.iter().any(|mode| {
                matches!(
                    mode,
                    StaticMode::CantBeBlocked
                        | StaticMode::CantBeBlockedBy { .. }
                        | StaticMode::CantBeBlockedExceptBy { .. }
                )
            })
        })
}

fn parse_cant_be_blocked_restriction_predicate(input: &str) -> OracleResult<'_, ()> {
    let (input, _) = alt((
        tag::<_, _, OracleError<'_>>("can't be blocked"),
        tag("cannot be blocked"),
    ))
    .parse(input)?;
    let (input, _) = opt(alt((tag(" this turn"), tag(" this combat")))).parse(input)?;
    if input.is_empty() {
        return Ok((input, ()));
    }
    let (input, _) = (tag(" "), alt((tag("except by "), tag("by "))), rest).parse(input)?;
    Ok((input, ()))
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
    // The phrase→`Duration` mapping is owned by the single duration grammar
    // (`oracle_nom/duration.rs`); this adapter owns only the slot's leading
    // space and optionality.
    opt(preceded(tag(" "), parse_duration)).parse(input)
}

pub(super) fn parse_subject_application(
    subject: &str,
    ctx: &mut ParseContext,
) -> Option<SubjectApplication> {
    if subject.trim().is_empty() {
        return None;
    }

    // CR 608.2c: A trailing "also" adverb is a natural-language additive
    // connector ("it also has …", "that creature also gains …") with no
    // semantic weight on the subject — it modifies the verb, not the
    // referent. `find_predicate_start` leaves it on the subject side because
    // it is not a predicate verb, so strip it here so the bare anaphor/typed
    // subject resolves identically to its non-"also" form. Mirrors the
    // self-ref "also" strip in `oracle_effect/mod.rs` (Expressive Firedancer),
    // generalized to every subject the subject-predicate parser accepts.
    let subject = subject
        .trim()
        // allow-noncombinator: structural adverb cleanup on already-isolated subject text (not parsing dispatch); mirrors the self-ref "also" strip in oracle_effect/mod.rs (PATTERNS.md §9)
        .strip_suffix(" also")
        .map(str::trim_end)
        .unwrap_or(subject);
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
        let (filter, rest) = parse_target_with_ctx(subject, ctx);
        // CR 608.2c + CR 109.4: "target <filter>'s controller/owner <verb>s it"
        // (Arcum Dagsson, Mercy Killing) — the ability TARGETS <filter>; its
        // controller/owner performs the verb on it ("it" = that target). Preserve
        // <filter> as the ability's object target while shifting the acting
        // subject to the target's controller/owner, so the imperative lowering
        // declares a `TargetOnly{<filter>}` slot (see `lower_subject_predicate_ast`)
        // against which `ParentTarget`/`ParentTargetController` resolve. Only the
        // exact possessive-controller/owner suffix (nothing else) qualifies.
        if let Ok((_, actor)) = all_consuming(alt((
            value(
                TargetFilter::ParentTargetController,
                alt((
                    tag::<_, _, OracleError<'_>>("'s controller"),
                    tag("\u{2019}s controller"),
                )),
            ),
            value(
                TargetFilter::ParentTargetOwner,
                alt((tag("'s owner"), tag("\u{2019}s owner"))),
            ),
        )))
        .parse(rest.trim())
        {
            return Some(SubjectApplication {
                affected: actor,
                target: Some(filter),
                multi_target: None,
                inherits_parent: false,
                is_optional: false,
            });
        }
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
        // CR 115.1d: Accept "any number of target X" and "any number of other
        // target X". consumed is kept at the end of "any number of " so that
        // target_text starts with "other target..." or "target..." and
        // parse_target_with_ctx can add FilterProp::Another for the "other" form
        // (Guardian of Faith: "any number of other target creatures you control").
        let consumed = lower.len() - after_prefix.len();
        let target_text = &subject[consumed..];
        if alt((
            tag::<_, _, OracleError<'_>>("target "),
            tag("other target "),
        ))
        .parse(after_prefix)
        .is_ok()
        {
            let (filter, _) = parse_target_with_ctx(target_text, ctx);
            let mut application = subject_filter_application(filter, true)?;
            application.multi_target = Some(MultiTargetSpec::unlimited(0));
            return Some(application);
        }
    }
    // CR 115.1 + CR 115.1d: "one or more target X" — variable-count targeting
    // with a minimum of 1 and no upper bound (Dwarven Song / Heaven's Gate /
    // Sea Kings' Blessing / Sylvan Paradise / Touch of Darkness:
    // "One or more target creatures become <color> until end of turn"). Mirrors
    // the unbounded "any number of target" branch above; the only axis of
    // variation is the minimum count (1 here vs. 0 there).
    if let Ok((after_prefix, _)) =
        tag::<_, _, OracleError<'_>>("one or more ").parse(lower.as_str())
    {
        if tag::<_, _, OracleError<'_>>("target ")
            .parse(after_prefix)
            .is_ok()
        {
            let consumed = lower.len() - after_prefix.len();
            let target_text = &subject[consumed..];
            let (filter, _) = parse_target_with_ctx(target_text, ctx);
            let mut application = subject_filter_application(filter, true)?;
            application.multi_target = Some(MultiTargetSpec::unlimited(1));
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
        // CR 115.1d: "each of one or two targets" — bounded multi-target selection
        // where the effect applies to each chosen target (Prismari Charm).
        for &(stem, min, max) in BOUNDED_TARGET_CARDINALITIES {
            if (tag::<_, _, OracleError<'_>>(stem), tag(" targets"))
                .parse(remainder)
                .is_ok()
            {
                let mut application = subject_filter_application(TargetFilter::Any, true)?;
                application.multi_target = Some(MultiTargetSpec::fixed(min, max));
                return Some(application);
            }
        }
        // Fallback: strip "of " and re-route through parse_target as "each <remainder>"
        let normalized = format!("each {remainder}");
        let (filter, _) = parse_target(&normalized);
        return subject_filter_application(filter, false);
    }
    // CR 119.5: "each player's life total" / "all players' life
    // total(s)" is a non-targeted ALL-players scope (Worldfire — issue #2882).
    // This must precede the generic "each "/"all " branch below: that branch
    // strips the quantifier and routes "player's life total" through
    // `parse_target`, yielding an empty (targetable) filter that wrongly
    // prompts the controller to pick a single player.
    if alt((
        tag::<_, _, OracleError<'_>>("each player's life totals"),
        tag("each player's life total"),
        tag("all players' life totals"),
        tag("all players' life total"),
    ))
    .parse(lower.as_str())
    .is_ok()
    {
        return subject_filter_application(TargetFilter::AllPlayers, false);
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
    // "those creatures" / "those lands" — anaphoric reference to previous
    // targets. Maps to ParentTarget so the restriction applies to the same
    // objects.
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
            ("that attacking player", true),
            tag::<_, _, OracleError<'_>>("that attacking player may"),
        ),
        // CR 603.7c: "the attacking player" on a DamageReceived trigger — the
        // controller of the creature that dealt combat damage (Contested Game
        // Ball). Longest-match before "the player".
        value(
            ("the attacking player", true),
            tag("the attacking player may"),
        ),
        value(
            ("that player", true),
            tag::<_, _, OracleError<'_>>("that player may"),
        ),
        value(("the player", true), tag("the player may")),
        value(
            ("that attacking player", false),
            tag("that attacking player"),
        ),
        value(("the attacking player", false), tag("the attacking player")),
        value(("that player", false), tag("that player")),
        value(("the player", false), tag("the player")),
    )))
    .parse(lower.as_str());
    if let Ok((_, (subject_lower, is_optional))) = player_subject {
        let Ok((_, ctx_filter)) = all_consuming(parse_event_context_ref).parse(subject_lower)
        else {
            return None;
        };
        if matches!(
            ctx_filter,
            TargetFilter::TriggeringPlayer
                | TargetFilter::DefendingPlayer
                | TargetFilter::TriggeringSourceController
        ) {
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
                Some(ControllerRef::SourceChosenPlayer)
            ) {
                TargetFilter::SourceChosenPlayer
            } else if matches!(
                ctx.relative_player_scope,
                Some(ControllerRef::ParentTargetController)
            ) {
                TargetFilter::ParentTargetController
            } else if matches!(
                ctx.relative_player_scope,
                Some(ControllerRef::TriggeringPlayer | ControllerRef::DefendingPlayer)
            ) {
                // CR 608.2c + CR 106.12a: An explicit triggering/defending player
                // scope established by `relative_player_scope_for_condition`
                // (e.g. the instant/sorcery taps-for-mana delayed trigger split:
                // "whenever a player taps <type> for mana, that player adds …" —
                // High Tide, Bubbling Muck) makes "that player" the triggering
                // player, NOT the parent target's controller. Without this arm the
                // scope is resolved but silently discarded, defaulting to
                // `ParentTargetController` below. `ctx_filter` is the matching
                // event-context ref for the parsed subject phrase.
                ctx_filter
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
    // CR 608.2c + CR 608.2d: "its controller" / "their controller" as anaphoric
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
    // CR 608.2c + CR 608.2d: "its owner may" / "their owner may" — owner-of-target
    // subject carrying a "may" modal (mirrors the "its controller may" arm above).
    if let Ok((after_head, _)) = alt((
        tag::<_, _, OracleError<'_>>("its owner may"),
        tag("their owner may"),
    ))
    .parse(lower.as_str())
    {
        if after_head.trim().is_empty() {
            return Some(SubjectApplication {
                affected: TargetFilter::ParentTargetOwner,
                target: None,
                multi_target: None,
                inherits_parent: false,
                is_optional: true,
            });
        }
    }
    // CR 108.3 + CR 608.2c: bare "its owner" / "their owner" — owner of the parent
    // target (distinct from its controller; "destroy target creature, its owner
    // gains 4 life" pays the OWNER, not the controller of the destroy ability).
    if lower == "its owner" || lower == "their owner" {
        return Some(SubjectApplication {
            affected: TargetFilter::ParentTargetOwner,
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
        // allow-noncombinator: post-tokenized subject suffix classification
        if after_det.ends_with("'s controller may") {
            return Some(SubjectApplication {
                affected: TargetFilter::ParentTargetController,
                target: None,
                multi_target: None,
                inherits_parent: false,
                is_optional: true,
            });
        }
        // CR 108.3: "[the|that] <noun>'s owner may" — owner of the parent target.
        // allow-noncombinator: post-tokenized subject suffix classification
        if after_det.ends_with("'s owner may") {
            return Some(SubjectApplication {
                affected: TargetFilter::ParentTargetOwner,
                target: None,
                multi_target: None,
                inherits_parent: false,
                is_optional: true,
            });
        }
        // allow-noncombinator: post-tokenized subject suffix classification
        if after_det.ends_with("'s controller") {
            return Some(SubjectApplication {
                affected: TargetFilter::ParentTargetController,
                target: None,
                multi_target: None,
                inherits_parent: false,
                is_optional: false,
            });
        }
        // CR 108.3: "[the|that] <noun>'s owner" — owner of the parent target
        // (The Matrix of Time "that card's owner loses 3 life", Thieving Amalgam).
        // allow-noncombinator: post-tokenized subject suffix classification
        if after_det.ends_with("'s owner") {
            return Some(SubjectApplication {
                affected: TargetFilter::ParentTargetOwner,
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

    // CR 608.2k + CR 509.1/509.3d: "the other creature" — the creature on the
    // opposite side of a compound blocks-or-becomes-blocked pairing (Mammoth
    // Harness, Venom). Unconditionally ParentTarget (unlike "that creature"):
    // the antecedent flips per-firing-orientation, which
    // `blocked_attacker_from_event` already disambiguates from the resolved
    // event shape, regardless of ctx.subject.
    if let Ok((rest_subject, _)) = tag::<_, _, OracleError<'_>>("the other ").parse(lower.as_str())
    {
        let consumed = lower.len() - rest_subject.len();
        let original_rest = &subject[consumed..];
        let (filter, rem) = parse_type_phrase(original_rest);
        if rem.trim().is_empty() && !matches!(filter, TargetFilter::Any) {
            return Some(SubjectApplication {
                affected: TargetFilter::ParentTarget,
                target: Some(TargetFilter::ParentTarget),
                multi_target: None,
                inherits_parent: true,
                is_optional: false,
            });
        }
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

/// CR 608.2c + CR 701.19c: Resolve the subject of a DAMAGE-form
/// "[noun] dealt damage this way can't be regenerated [this turn]" clause.
///
/// The three clean cards (Incinerate, Flamebreak, Jaya Ballard, Task Mage)
/// print this regen prohibition as a separate sentence following a damage
/// clause. The subject is the anaphor "a creature/creatures/a permanent dealt
/// damage this way" — a back-reference (CR 608.2c "this way") to the set of
/// objects the preceding damage effect struck, NOT a fresh target. It resolves
/// to the most recently published tracked set (`TrackedSetId(0)` sentinel),
/// which the parent damage clause publishes once this rider is attached as its
/// sub-ability (the `target: Some(TrackedSet)` trips
/// `next_sub_needs_tracked_set`, and `affected_objects_from_events`' DealDamage
/// arm collects the damaged object ids). The anaphor tag set mirrors the
/// die-exile rider's (`oracle_effect/mod.rs::try_parse_die_exile_rider`) plus
/// the plural "creatures dealt damage this way" for the DamageAll form
/// (Flamebreak). Returns `None` for every non-damage subject so the other
/// "can't be regenerated" clauses (Hurr Jackal, Lim-Dûl's Cohort) fall through
/// to `subject_application_for_cant_be_activated` unchanged.
fn subject_application_for_cant_be_regenerated(subject: &str) -> Option<SubjectApplication> {
    let lower = subject.to_lowercase();
    let matched = all_consuming(alt((
        tag::<_, _, OracleError<'_>>("a creature dealt damage this way"),
        tag("creatures dealt damage this way"),
        tag("a permanent dealt damage this way"),
    )))
    .parse(lower.as_str())
    .is_ok();
    if !matched {
        return None;
    }
    Some(cant_be_regenerated_tracked_set_application())
}

/// CR 608.2c + CR 701.19c: The `SubjectApplication` for a regen rider that binds
/// to the preceding damage clause's published set via the `TrackedSetId(0)`
/// sentinel. `target: Some(TrackedSet)` trips `next_sub_needs_tracked_set` on the
/// parent damage clause so it publishes the struck-object ids; the rider does not
/// inherit the parent's chosen targets. Shared by the unconditional anaphor form
/// and the conditional ("if it's a creature, it") damage-form so both bind the
/// CantBeRegenerated static to exactly the same set.
pub(super) fn cant_be_regenerated_tracked_set_application() -> SubjectApplication {
    let tracked = TargetFilter::TrackedSet {
        id: crate::types::identifiers::TrackedSetId(0),
    };
    SubjectApplication {
        affected: tracked.clone(),
        target: Some(tracked),
        multi_target: None,
        inherits_parent: false,
        is_optional: false,
    }
}

/// CR 608.2c + CR 701.19c: Build the separate-sentence regen rider attached to a
/// preceding damage clause. Recognizes the full "[noun] dealt damage this way
/// can't be regenerated [this turn]" sentence (the three clean cards print it as
/// its own sentence) and returns an `AbilityDefinition` carrying the
/// `GenericEffect{CantBeRegenerated}` whose `target: TrackedSet(0)` binds to the
/// damage clause's published set. Mirrors `static_affected_for_application`'s
/// `target.is_some() → ParentTarget` convention so the static's `affected` is the
/// runtime-bound `ParentTarget` (which the GenericEffect resolver reads against
/// `chain_tracked_set_id`). Returns `None` for any other "can't be regenerated"
/// subject (the targeted/anaphor forms keep their existing in-chain dispatch).
pub(super) fn try_parse_cant_be_regenerated_damage_rider(
    text: &str,
    kind: AbilityKind,
) -> Option<AbilityDefinition> {
    let lower = text.to_lowercase();
    let (before_lower, (), _) =
        nom_primitives::scan_preceded(&lower, parse_cant_be_regenerated_predicate)?;
    let subject = text[..before_lower.len()].trim();
    let application = subject_application_for_cant_be_regenerated(subject)?;
    Some(build_cant_be_regenerated_rider(kind, &application))
}

/// CR 701.19c + CR 614.8: Build the `CantBeRegenerated` rider `AbilityDefinition`
/// shared by the unconditional damage-anaphor form ("a creature dealt damage this
/// way can't be regenerated") and the conditional damage-form (Disintegrate /
/// Carbonize, gated on "if it's a creature, it"). The rider is a
/// `GenericEffect{CantBeRegenerated}` whose `target`/`affected` bind to the
/// preceding damage clause's published set via `SubjectApplication`
/// (`static_affected_for_application` maps `target.is_some()` → `ParentTarget`,
/// the runtime-bound back-reference the GenericEffect resolver reads against the
/// chain's tracked set). Factored out so both call sites construct the identical
/// def; the conditional caller additionally stamps `def.condition` to gate it.
pub(super) fn build_cant_be_regenerated_rider(
    kind: AbilityKind,
    application: &SubjectApplication,
) -> AbilityDefinition {
    let affected = static_affected_for_application(application);
    let mode = StaticMode::CantBeRegenerated;
    AbilityDefinition::new(
        kind,
        Effect::GenericEffect {
            static_abilities: vec![StaticDefinition::new(mode.clone())
                .affected(affected)
                .modifications(vec![ContinuousModification::AddStaticMode { mode }])],
            duration: Some(Duration::UntilEndOfTurn),
            target: application.target.clone(),
        },
    )
    .duration(Duration::UntilEndOfTurn)
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
    // CR 608.2c + CR 109.4 (issue #1670, #3659): "they" after body "its
    // controller may … if they do, they draw" refers to that creature's
    // controller, not the damaged opponent from the trigger condition.
    if matches!(
        ctx.relative_player_scope,
        Some(ControllerRef::ParentTargetController)
    ) {
        return TargetFilter::ParentTargetController;
    }
    if matches!(
        ctx.relative_player_scope,
        Some(ControllerRef::ParentTargetOwner)
    ) {
        return TargetFilter::ParentTargetOwner;
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
///
/// CR 608.2c + CR 502.3: also bind to the inherited target when the subject is
/// an anaphor to a previously-mentioned single object (`inherits_parent`,
/// e.g. spell-form "Tap target land. That land doesn't untap" — Chandra's
/// Revolution, Glacial Grasp). Without this, the static's `affected` would
/// broadcast the CantUntap lock over every matching permanent. The
/// transient-effect resolver already binds `ParentTarget` to the inherited
/// (immediately-preceding) object target, so this resolves to exactly the one
/// tapped object. Mirrors `build_pump_effect`, which honors `inherits_parent`
/// the same way for the Pump family.
pub(super) fn static_affected_for_application(application: &SubjectApplication) -> TargetFilter {
    if application.target.is_some() || application.inherits_parent {
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
    ctx: &ParseContext,
) -> Option<ParsedEffectClause> {
    let lower = normalized.to_lowercase();
    // Find " and " that separates two independent clauses after a pump+duration.
    let tp = TextPair::new(normalized, &lower);
    let (pump_tp, remainder_tp) = tp.split_around(" and ")?;
    let pump_part = pump_tp.original;
    let remainder = remainder_tp.original.trim();

    // Parse the pump clause first to check whether it carries its own duration.
    let (power, toughness, mut duration) =
        super::lower::parse_pump_clause_with_context(pump_part, ctx)?;

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

    // CR 608.2d: a pump compounded with a modal keyword grant --
    // "gets +1/+1 and gains your choice of deathtouch or lifelink" (Alchemist's
    // Gift) -- has a grant half that is a two-branch player choice, so it cannot
    // collapse into a single `ContinuousModification` the way a fixed "and gains
    // trample" does (which the guard above routes to `build_continuous_clause`'s
    // coalescing path). Route the choice through the same
    // `parse_keyword_choice_grant` / `keyword_choice_branch` builders the
    // standalone modal grant uses (`build_keyword_choice_clause`), riding the
    // pump as a `ChooseOneOf` sub_ability keyed to the pumped creature
    // (`ParentTarget`). Non-modal remainders ("you gain 1 life") fall back to
    // the general effect-chain parse.
    let sub_ability = if remainder.is_empty() {
        None
    } else if let Some((choice, choice_duration)) =
        build_keyword_choice_sub_ability(application, remainder)
    {
        duration = duration.or(choice_duration);
        Some(Box::new(choice))
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

/// CR 608.2d: build the modal keyword-grant half of a pump
/// compound ("gets +1/+1 AND gains your choice of X or Y") as a `ChooseOneOf`
/// sub_ability. Reuses the same `parse_keyword_choice_grant` /
/// `keyword_choice_branch` builders as the standalone `build_keyword_choice_clause`,
/// keyed to the pumped creature via `static_affected_for_application`
/// (`ParentTarget` for a targeted application). Returns `None` for a non-modal
/// remainder so the caller falls back to the general effect-chain parse.
fn build_keyword_choice_sub_ability(
    application: &SubjectApplication,
    remainder: &str,
) -> Option<(AbilityDefinition, Option<Duration>)> {
    // The split remainder keeps its conjugated verb ("gains ..."):
    // `deconjugate_verb` in `build_continuous_clause` only normalizes the
    // compound's leading verb ("gets"), so the second clause arrives as
    // "gains your choice of ...". `parse_keyword_choice_grant` anchors on the
    // bare "gain ..." form, so deconjugate the remainder here first.
    let normalized = deconjugate_verb(remainder);
    let (first, second, duration) = parse_keyword_choice_grant(&normalized)?;
    let affected = static_affected_for_application(application);
    let choice_duration = duration.clone();
    let branches = vec![
        keyword_choice_branch(first, affected.clone(), None, duration.clone()),
        keyword_choice_branch(second, affected, None, duration),
    ];
    Some((
        AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::ChooseOneOf {
                chooser: PlayerFilter::Controller,
                branches,
            },
        ),
        choice_duration,
    ))
}

fn parse_keyword_choice_grant(predicate: &str) -> Option<(Keyword, Keyword, Option<Duration>)> {
    let lower = predicate.to_lowercase();

    // Shape 1: "gain your choice of X or Y" — an explicit keyword-grant menu.
    if let Ok((choice_text, _)) =
        tag::<_, _, OracleError<'_>>("gain your choice of ").parse(lower.as_str())
    {
        let (keyword_text, duration) = super::strip_trailing_duration(choice_text);
        let (_, (left, right)) = nom_primitives::split_once_on(keyword_text.trim(), " or ").ok()?;
        let first = parse_granted_keyword_fragment(left.trim())?;
        let second = parse_granted_keyword_fragment(right.trim())?;
        return Some((first, second, duration.or(Some(Duration::UntilEndOfTurn))));
    }

    // Shape 2: "gain/have protection from X or from the color of your choice"
    // (Angelic Intervention, Apostle's Blessing, Giver of Runes, Jeweled Spirit,
    // Razor Barrier). The predicate arrives DECONJUGATED (gains→gain, has→have),
    // so anchor on the bare forms — never "gains"/"has".
    // CR 608.2d: a choice offered by a resolving ability is announced as the
    // effect is applied (choose one protection).
    // CR 702.16a: protection from [quality] (color, colorless, or card type).
    let (remainder, _) = alt((
        tag::<_, _, OracleError<'_>>("gain protection from "),
        tag("have protection from "),
    ))
    .parse(lower.as_str())
    .ok()?;
    let (quality_text, duration) = super::strip_trailing_duration(remainder);
    // GUARDRAIL: split on the literal " or from " (NOT " or "). Splitting on
    // " or " would leave the right half as "from the color of your choice",
    // which parse_protection_target's `from `-prefix arm routes to Quality —
    // silently killing the color choice. When there is no " or from " this is a
    // single protection grant, so return None and fall through to the existing
    // continuous-clause behavior.
    let (_, (left, right)) =
        nom_primitives::split_once_on(quality_text.trim(), " or from ").ok()?;
    // The halves are bare qualities (the "protection from " prefix is already
    // stripped), so map each with parse_protection_target — NOT
    // parse_granted_keyword_fragment, which expects the full "protection from …" form.
    let first = Keyword::Protection(crate::types::keywords::parse_protection_target(left.trim()));
    let second = Keyword::Protection(crate::types::keywords::parse_protection_target(
        right.trim(),
    ));
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
    ctx: &ParseContext,
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
    if let Some((power, toughness, duration)) =
        super::lower::parse_pump_clause_with_context(&normalized, ctx)
    {
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
    if let Some(clause) = try_split_pump_compound(&normalized, &application, ctx) {
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

    if let Some(static_abilities) =
        build_defender_attack_continuous_compound(&application, predicate_text)
    {
        return Some(ParsedEffectClause {
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
        });
    }

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

/// CR 702.62a + CR 702.62b + CR 611.2a + CR 608.2c: Recognize the plural,
/// set-referencing "cards exiled this way gain <kw>" continuous keyword grant.
/// This is the plural / relative-clause sibling of the singular "If it doesn't
/// have suspend, it gains suspend" grant (Jhoira of the Ghitu, The Tenth
/// Doctor): the subject "cards exiled this way" is a back-reference (CR 608.2c
/// "this way") to the chain's tracked set, which the `GenericEffect` resolver
/// broadcasts the grant to via `ParentTarget`.
///
/// Parameterized on the keyword (never Suspend-hardcoded) so it covers the whole
/// "cards exiled this way gain <kw>" class (only the "exiled this way" head is
/// matched today — no "affected this way" arm). The produced clause is
/// byte-for-byte the Jhoira/Tenth suspend-grant shape.
///
/// The optional "that don't have <kw>" restrictive clause (CR 702.62a) is
/// recognised by the parser but results in a strict-failure (`None`) because
/// `evaluate_condition` resolves `SourceLacksKeyword` against the ability's
/// `source_id` (the spell, which never carries the keyword), not each individual
/// exiled card. Attaching the condition therefore produces an unconditional
/// overgrant — already-<kw> cards would still receive a redundant grant. A
/// correct per-card exclusion requires an object-scoped condition variant (e.g.
/// `CostPaidObjectLacksKeyword`) that does not yet exist in the engine. Until
/// that building block is added, "cards exiled this way that don't have <kw>
/// gain <kw>" is a documented strict-failure deferred to `Unimplemented`.
///
/// Returns `None` (strict-failure to `Unimplemented`) when the restrictive
/// clause is present or when the predicate is not a recognised "gain <kw>"
/// keyword grant.
pub(super) fn try_parse_exiled_this_way_keyword_grant(
    text: &str,
    ctx: &ParseContext,
) -> Option<ParsedEffectClause> {
    let lower = text.to_lowercase();
    // Strip the set-referencing subject head ("cards exiled this way") — a
    // back-reference (CR 608.2c) to the chain's exiled-card tracked set.
    let (_, after_head) = nom_on_lower(text, &lower, |i| {
        value(
            (),
            alt((
                tag("the cards exiled this way"),
                tag("a card exiled this way"),
                tag("cards exiled this way"),
            )),
        )
        .parse(i)
    })?;

    // Detect the restrictive "that don't have <kw>" clause (CR 702.62a).
    // When present, strict-fail: the correct object-scoped condition
    // (`evaluate_condition` per exiled card, not per spell source) is not
    // yet implemented. Attaching `SourceLacksKeyword` here would silently
    // overgrant — see the fn doc for the full explanation.
    let after_head_lower = after_head.to_lowercase();
    let has_restrictive = nom_on_lower(after_head, &after_head_lower, |i| {
        let (i, _) = tag(" that do").parse(i)?;
        let (i, _) = opt(tag("es")).parse(i)?;
        let (i, _) = alt((tag("n't"), tag(" not"))).parse(i)?;
        let (i, _) = tag(" have ").parse(i)?;
        let (i, _) = take_until(" gain").parse(i)?;
        Ok((i, ()))
    });
    if has_restrictive.is_some() {
        return None;
    }

    // The predicate must be a "gain <kw>" continuous keyword grant; reuse the
    // shared `build_continuous_clause` machinery (which applies the keyword-driven
    // `Suspend → Permanent` duration rule per CR 611.2a). `target.is_some()` maps
    // `affected` to the runtime-bound `ParentTarget` back-reference.
    let application = SubjectApplication {
        affected: TargetFilter::ParentTarget,
        target: Some(TargetFilter::ParentTarget),
        multi_target: None,
        inherits_parent: false,
        is_optional: false,
    };
    build_continuous_clause(application, after_head.trim(), ctx)
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

/// CR 305.7 + CR 305.6 + CR 205.1b (Layer 4): Lower a "become[s] [a[n]] `<basic
/// land type>`" predicate to its land-subtype modification(s). Non-additive
/// replaces the object's land subtypes (`SetBasicLandType`, CR 305.7 — the land
/// gains only the named type's intrinsic mana ability); the "in addition to
/// {their|its} other types" form retains existing land types and adds the subtype
/// (`AddSubtype`, CR 205.1b). Composed along its axes — optional article, the
/// basic-land-type word (singular or plural via `parse_basic_land_type_plural`),
/// and the optional additive marker. Declines any predicate that does not name
/// exactly a basic land type (optionally with the additive marker), so non-land
/// becomes fall through to `parse_animation_spec`.
fn try_parse_become_basic_land_type_modifications(
    become_text: &str,
) -> Option<Vec<ContinuousModification>> {
    type VE<'a> = OracleError<'a>;
    let lower = become_text
        .trim()
        .trim_end_matches('.')
        .trim()
        .to_lowercase();
    let (rest, _) = opt(alt((tag::<_, _, VE>("a "), tag::<_, _, VE>("an "))))
        .parse(lower.as_str())
        .ok()?;
    // Extract the candidate basic-land word with a nom combinator (not manual
    // scanning), then classify it via the shared `parse_basic_land_type_plural`.
    let (after, word) = nom::character::complete::alpha1::<_, VE>(rest).ok()?;
    let land_type = crate::parser::oracle_static::parse_basic_land_type_plural(word)?;
    let after = after.trim();
    // CR 205.1b: only a bare type word (replacement) or the "in addition to their/
    // its other types" additive marker may follow; anything else is a mixed
    // predicate that must fall through to the animation parser.
    let additive = if after.is_empty() {
        false
    } else if nom_primitives::scan_contains(after, "in addition to") {
        true
    } else {
        return None;
    };
    Some(if additive {
        vec![ContinuousModification::AddSubtype {
            subtype: land_type.as_subtype_str().to_string(),
        }]
    } else {
        vec![ContinuousModification::SetBasicLandType { land_type }]
    })
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
    if let Some(clause) = try_parse_set_life_total(become_text, &application, ctx) {
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

    // CR 105.2 + CR 105.3 + CR 613.1e (Layer 5): "becomes all colors" (Tam,
    // Mindful First-Year) and "becomes the chosen color" (Puca's Eye) set the
    // affected object's color. "All colors" maps to the full WUBRG set
    // (CR 105.2: a multicolored object can be each of the five colors); "the
    // chosen color" reads the source's `ChosenAttribute::Color` chosen upstream
    // (preceding `Effect::Choose { ChoiceType::Color }`) via `AddChosenColor`.
    // Both are non-additive (CR 105.3: a new color replaces all previous
    // colors) — the additive "in addition to its other colors" form is handled
    // by the animation path below. Must intercept before `parse_animation_spec`,
    // which bails on " all colors" and would mis-tokenize "chosen"/"color" as a
    // subtype.
    if let Some(modification) = try_parse_become_color_modification(become_text) {
        let affected = static_affected_for_application(&application);
        let effect = Effect::GenericEffect {
            static_abilities: vec![StaticDefinition::continuous()
                .affected(affected)
                .modifications(vec![modification])
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

    // CR 305.7 + CR 305.6 + CR 205.1b (Layer 4): "become[s] [a] <basic land type>"
    // (Nightcreep's "all lands become Swamps") is a LAND-subtype change, not a
    // creature subtype. `parse_animation_spec` below mis-tokenizes the basic-land
    // word — notably the plural "Swamps" — as a creature subtype (`AddSubtype` +
    // `RemoveAllSubtypes{Creature}`), so the land never gains the type's intrinsic
    // mana ability. Intercept it here and emit the correct land-type modification.
    if let Some(modifications) = try_parse_become_basic_land_type_modifications(become_text) {
        let affected = static_affected_for_application(&application);
        let effect = Effect::GenericEffect {
            static_abilities: vec![StaticDefinition::continuous()
                .affected(affected)
                .modifications(modifications)
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
        // CR 722.3a: Resolve the prepare/unprepare target from the subject.
        // A targeted subject ("target creature becomes prepared", Biblioplex)
        // binds to the chosen object via `ParentTarget` at resolution; a
        // self-referential or anaphoric subject ("this creature becomes
        // prepared" — Stensian Sanguinist, normalized to `~` → `SelfRef`) uses
        // the subject's own `affected` filter. Mirrors
        // `static_affected_for_application`'s targeted-vs-subject split so the
        // self-reference is preserved instead of collapsing to `ParentTarget`.
        let target = if application.target.is_some() || application.inherits_parent {
            crate::types::ability::TargetFilter::ParentTarget
        } else {
            application.affected.clone()
        };
        let effect = match kind {
            PreparedKind::Prepared => Effect::BecomePrepared { target },
            PreparedKind::Unprepared => Effect::BecomeUnprepared { target },
        };
        return Some(super::parsed_clause(effect));
    }

    // CR 702.171b: "becomes saddled" toggles the saddled designation on the
    // target permanent. Must intercept before parse_animation_spec which would
    // mis-classify "saddled" as a subtype. The designation always clears at end
    // of turn / when the permanent leaves the battlefield (handled by the
    // engine's cleanup pass), so the trailing "until end of turn" duration that
    // `strip_trailing_duration` already peeled is not re-attached — the effect
    // carries no duration. Unlike a `GenericEffect` animation (whose typed
    // selection filter lives on the effect's `target` field and whose static's
    // `affected` is `ParentTarget`), `BecomeSaddled` is a single targeted effect
    // whose `target` IS the selection slot: a "Target Mount you control"
    // subject (Guidelight Matrix) carries its real `Typed(Mount, You)` filter so
    // `build_target_slots` surfaces a target slot; an anaphoric subject ("it
    // becomes saddled" — Kolodin's Mount-enters trigger lowers "it" to a
    // `TriggeringSource` context ref) carries `affected`, which the resolver
    // resolves from event context.
    if all_consuming(tag::<_, _, OracleError<'_>>("saddled"))
        .parse(become_lower.as_str())
        .is_ok()
    {
        let target = application
            .target
            .clone()
            .unwrap_or_else(|| application.affected.clone());
        return Some(super::parsed_clause(Effect::BecomeSaddled { target }));
    }

    // CR 509.1h: "becomes blocked" makes the target attacking creature a blocked
    // creature with no blockers assigned (Dazzling Beauty: "Target unblocked
    // attacking creature becomes blocked."). Mirrors the saddled idiom: a real
    // "Target ... creature" subject carries its `Typed` filter as the target slot;
    // an anaphoric subject falls back to `affected`.
    if all_consuming(tag::<_, _, OracleError<'_>>("blocked"))
        .parse(become_lower.as_str())
        .is_ok()
    {
        let target = application
            .target
            .clone()
            .unwrap_or_else(|| application.affected.clone());
        return Some(super::parsed_clause(Effect::BecomeBlocked { target }));
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
                recipient: TargetFilter::SelfRef,
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

    // CR 707.2 + CR 611.2c: "become copies of [donor]" — a MASS become-copy
    // (Niko, Light of Hope: "Shards you control become copies of it"). Every
    // member of the subject's recipient set becomes a copy of the single donor,
    // locked to the members present at resolution. Mirrors the singular "a copy
    // of" arm; the recipient set is the subject filter
    // (`static_affected_for_application`) and the donor is the single "of" object.
    // Additive: "copies of it" fell through to Unimplemented before this arm.
    if let Ok((after_copies, _)) =
        tag::<_, _, OracleError<'_>>("copies of ").parse(become_lower.as_str())
    {
        let (after_copies_owned, mid_sentence_duration) = strip_pre_except_duration(after_copies);
        let duration = mid_sentence_duration.map(Some).unwrap_or(duration);
        let (target, remainder) = parse_target(&after_copies_owned);
        let card_name = ctx.card_name.as_deref().unwrap_or("");
        let additional_modifications =
            super::become_copy_except::parse_except_clause(remainder, card_name, ctx)
                .map(|(_, mods)| mods)
                .unwrap_or_default();
        return Some(ParsedEffectClause {
            effect: Effect::BecomeCopy {
                target,
                recipient: static_affected_for_application(&application),
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

    // CR 205.1a + CR 613.1d + CR 613.1f + CR 613.8a: "becomes a <type> [with
    // \"<ability>\"] and loses all other card types and abilities" — full
    // card-type replacement plus ability wipe plus optional ability grant
    // (Vraska, Betrayal's Sting [-2]). Must intercept before parse_animation_spec,
    // which bails on the " loses all other card types " tail and would drop both
    // the type replacement and the granted ability.
    if let Some(modifications) =
        super::become_copy_except::parse_becomes_type_loses_all(become_text)
    {
        let affected = static_affected_for_application(&application);
        let effect = Effect::GenericEffect {
            static_abilities: vec![StaticDefinition::continuous()
                .affected(affected)
                .modifications(modifications)
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

    let (become_text, name_override) = strip_become_name_override(become_text);
    let animation = parse_animation_spec(&become_text, ctx)?;
    // CR 205.1a vs CR 205.1b: a "becomes a [type]" effect REPLACES the creature's
    // subtypes (so e.g. a Human Soldier that becomes a Frog is only a Frog) unless
    // it says "in addition to its other types", which stays additive. Mirrors the
    // static type-change path's suffix detection.
    let is_additive = has_in_addition_to_other_types(&become_text);
    let mut modifications = animation_modifications_with_replacement(&animation, is_additive);
    // CR 105.3: "in addition to its other colors" makes the granted color
    // ADDITIVE — Possessed Goat "becomes a black Demon in addition to its other
    // colors and types". The animation path emits `SetColor` (the CR 105.3
    // replacement default); convert it to one `AddColor` per color so the
    // existing colors are preserved.
    if has_in_addition_to_other_colors(&become_text) {
        modifications = modifications
            .into_iter()
            .flat_map(|m| match m {
                ContinuousModification::SetColor { colors } => colors
                    .into_iter()
                    .map(|color| ContinuousModification::AddColor { color })
                    .collect::<Vec<_>>(),
                other => vec![other],
            })
            .collect();
    }
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

/// Capture a card name following `" named "`, terminating at a trailing
/// conjunction or sentence break. "becomes … named Fenric and loses all
/// abilities" yields the name `"Fenric"` (not `"Fenric and loses all
/// abilities"`); the residual `"and loses all abilities"` is recovered
/// independently by `parse_continuous_modifications` on the full predicate.
/// CR 201.4: an effect-assigned name is a single token-or-phrase, not the rest
/// of the clause.
fn strip_become_name_override(text: &str) -> (String, Option<String>) {
    let lower = text.to_lowercase();
    let tp = TextPair::new(text, &lower);
    let Some((before, after)) = tp.split_around(" named ") else {
        return (text.to_string(), None);
    };
    // The name extends up to a trailing " and <clause-verb>" conjunction that
    // introduces a FURTHER modification ("named Fenric and loses all abilities").
    // Commas are NOT terminators — names legitimately contain them ("Everflame,
    // Heroes' Legacy"). The trailing period is handled by `trim_end_matches('.')`.
    let after_lower = after.lower;
    let name_len = take_until_name_terminator(after_lower)
        .map(|(_, captured)| captured.len())
        .unwrap_or(after_lower.len());
    let name = after.original[..name_len]
        .trim()
        .trim_end_matches('.')
        .to_string();
    if name.is_empty() {
        (before.original.trim().to_string(), None)
    } else {
        (before.original.trim().to_string(), Some(name))
    }
}

/// Capture the name text up to (but not including) a trailing `" and <verb>"`
/// conjunction that begins a further modification clause (`" and loses all
/// abilities"`, `" and has flying"`). Commas and other punctuation are NOT
/// terminators — a card name may legitimately contain a comma ("Everflame,
/// Heroes' Legacy"). Returns the captured name slice; on no terminator the
/// caller falls back to the full remainder (trailing period stripped there).
fn take_until_name_terminator(input: &str) -> OracleResult<'_, &str> {
    // The terminator is the word "and" introducing a continuous-modification
    // clause verb ("and loses all abilities", "and has flying"). A bare "and"
    // inside a name ("Trial and Error") is NOT followed by a clause verb and is
    // kept. The scan starts each candidate at a word boundary, so "and" is the
    // first token of the slice (no leading space).
    fn and_clause_verb(i: &str) -> OracleResult<'_, ()> {
        let (i, _) = tag("and ").parse(i)?;
        value(
            (),
            alt((
                tag("loses "),
                tag("lose "),
                tag("gains "),
                tag("gain "),
                tag("has "),
                tag("have "),
                tag("is "),
                tag("are "),
                tag("becomes "),
                tag("can't "),
                tag("gets "),
            )),
        )
        .parse(i)
    }
    // UTF-8-safe word-boundary scan: never indexes mid-codepoint, unlike a raw
    // byte-increment loop. The captured prefix includes the trailing space
    // before "and" (e.g. "Fenric "); the caller trims it.
    nom_primitives::scan_split_at_phrase(input, and_clause_verb)
        .map(|(name, rest)| (rest, name))
        .ok_or_else(|| nom::Err::Error(OracleError::new(input, nom::error::ErrorKind::TakeUntil)))
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
    // verb axis × phase axis (PATTERNS.md §8b): factor "attack(s)" out front;
    // the phase clause maps through the single duration grammar
    // (`oracle_nom/duration.rs`: "this turn" → end of turn, "this/that
    // combat" → end of combat).
    let (rest, _) = alt((tag("attacks"), tag("attack"))).parse(input)?;
    delimited(tag(" "), parse_duration, tag(" if able")).parse(rest)
}

/// CR 119.5: Parse "life total becomes N" into SetLifeTotal effect.
/// Handles: "half that player's starting life total", numeric amounts,
/// "their starting life total", and any other quantity the general quantity
/// parser recognizes (e.g. "the highest/lowest life total among all players").
fn try_parse_set_life_total(
    become_text: &str,
    application: &SubjectApplication,
    ctx: &mut ParseContext,
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
        //
        // CR 119.5 + CR 109.5: the untargeted "each player's life total becomes
        // the number of [X] THEY control" form (Biorhythm, Shaman of Forgotten
        // Ways) resolves per player — the third-person "they" binds to the
        // iterating player, not the caster. Thread `ScopedPlayer` so the count's
        // controller resolves per-recipient. Gate strictly to the AllPlayers
        // each-player form: the targeted form ("target player's life total",
        // `application.target = Some`), the cross-player extremum (Repay in Kind
        // → `LifeTotal{AllPlayers{Min/Max}}`, which carries no "they control"
        // count to rebind), "your life total" (Controller), and the numeric arm
        // (Worldfire) are all left at the default controller scope.
        if application.target.is_none() && matches!(application.affected, TargetFilter::AllPlayers)
        {
            ctx.with_player_scope(ControllerRef::ScopedPlayer, |c| {
                oracle_quantity::parse_cda_quantity_with_context(lower, c)
            })?
        } else {
            oracle_quantity::parse_cda_quantity_with_context(lower, ctx)?
        }
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

/// CR 105.2 + CR 105.3 + CR 613.1e (Layer 5): map a "becomes [color]" predicate
/// to its color-setting `ContinuousModification`, for the forms that do NOT
/// prompt the controller for a choice. Two cases:
///
/// - "all colors" / "every color" → `SetColor(WUBRG)` (CR 105.2: a multicolored
///   object can be each of the five colors). The new color set replaces all
///   previous colors (CR 105.3).
/// - "the chosen color" / "that color" → `AddChosenColor`, reading the source's
///   `ChosenAttribute::Color`. For "the chosen color" the color is bound by a
///   preceding `Effect::Choose` in the same ability (Puca's Eye: "draw a card,
///   then choose a color. This artifact becomes the chosen color"). For "that
///   color" (CR 106.1a + CR 202.2) the color is bound by the mana produced
///   earlier in the SAME activated mana ability ("Add one mana of any color.
///   This creature becomes that color", Foraging Wickermaw) — the mana producer
///   records it as `ChosenAttribute::Color` on the source
///   (`produce_mana_from_ability`), and this `AddChosenColor` reads it live at
///   Layer 5 with [`ColorChangeMode::Set`] (CR 105.3 / CR 613.1e) — "becomes
///   that color" replaces prior colors unless an "in addition" retain-suffix
///   selected [`ColorChangeMode::Add`].
///
/// Returns `None` for any other predicate so the caller falls through to the
/// fixed-color animation path (which already handles single named colors) and to
/// `try_parse_become_choice` (the prompting "of your choice" form).
fn try_parse_become_color_modification(become_text: &str) -> Option<ContinuousModification> {
    let lower = become_text.trim().to_lowercase();
    if let Ok((rest, _)) = all_consuming(alt((
        tag::<_, _, OracleError<'_>>("all colors"),
        tag("every color"),
    )))
    .parse(lower.as_str())
    {
        let _ = rest;
        return Some(ContinuousModification::SetColor {
            colors: crate::types::mana::ManaColor::ALL.to_vec(),
        });
    }
    if all_consuming(alt((
        tag::<_, _, OracleError<'_>>("the chosen color"),
        tag("the color chosen this way"),
        tag("that color"),
    )))
    .parse(lower.as_str())
    .is_ok()
    {
        return Some(ContinuousModification::AddChosenColor {
            mode: ColorChangeMode::Set,
        });
    }
    None
}

/// True when `lower` ends with the "of your choice" anchor. Pattern 2 (whole
/// input parsed, trailing fixed phrase consumed last): `take_until` skips to the
/// final occurrence and `all_consuming` requires the suffix to terminate the
/// input. Replaces a bare `ends_with` so the dispatch stays combinator-driven.
fn ends_with_of_your_choice(lower: &str) -> bool {
    all_consuming(terminated(
        take_until::<_, _, OracleError<'_>>("of your choice"),
        tag("of your choice"),
    ))
    .parse(lower)
    .is_ok()
}

/// CR 205.3 / CR 305.7 / CR 105.3: Parse "become the [creature type / basic land
/// type / color] of your choice [and <keyword grant>]" into a Choose →
/// GenericEffect(apply) chain.
///
/// The optional trailing "and <keyword grant>" clause (Mondo Gecko: "becomes the
/// color of your choice and gains hexproof from that color") composes the chosen
/// attribute with one or more keyword grants on the same recipient. The keyword
/// clause is parsed by the shared `parse_continuous_modifications` building block,
/// which resolves "hexproof from that color" to `HexproofFrom(ChosenColor)`
/// (CR 702.11d) — the same `ChosenAttribute::Color` the `AddChosenColor`
/// modification reads, so the protection tracks the chosen color.
fn try_parse_become_choice(
    become_text: &str,
    application: &SubjectApplication,
    duration: Option<Duration>,
) -> Option<ParsedEffectClause> {
    use crate::types::ability::{ChoiceType, ChosenSubtypeKind, ContinuousModification};

    // CR 608.2c: split off a trailing "and <continuous-modification clause>" so
    // the "of your choice" anchor below sees only the choice phrase. The residual
    // (e.g. "gains hexproof from that color") is reparsed as continuous
    // modifications and appended to the apply-half. A subject like "the color of
    // your choice and gains hexproof from that color" splits at " and " into the
    // choice phrase and the grant phrase; absent the conjunction the whole text
    // is the choice phrase and there is no grant. The choice phrase is anchored
    // by the "of your choice" suffix combinator (Pattern 2: whole input parsed,
    // fixed suffix consumed last).
    let lower_full = become_text.to_lowercase();
    let tp = TextPair::new(become_text, &lower_full);
    let (choice_text, grant_text) = match tp.split_around(" and ") {
        Some((before, after)) if ends_with_of_your_choice(before.lower) => {
            (before.original.trim(), Some(after.original.trim()))
        }
        _ => (become_text.trim(), None),
    };

    let lower = choice_text.to_lowercase();
    if !ends_with_of_your_choice(lower.as_str()) {
        return None;
    }

    let (choice_type, modification) = if lower.contains("creature type") {
        (
            ChoiceType::creature_type(),
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
        (
            ChoiceType::color(),
            ContinuousModification::AddChosenColor {
                mode: ColorChangeMode::Set,
            },
        )
    } else {
        return None;
    };

    // CR 608.2c + CR 702.11d: append any trailing keyword grant ("and gains
    // hexproof from that color") onto the apply-half. `parse_continuous_modifications`
    // is the shared keyword-grant building block; it maps "gains hexproof from
    // that color" → `AddKeyword(HexproofFrom(ChosenColor))`.
    let mut modifications = vec![modification];
    if let Some(grant) = grant_text {
        modifications.extend(parse_continuous_modifications(grant));
    }

    // Two-step: Choose (prompts player) → GenericEffect (applies chosen subtype).
    let affected = static_affected_for_application(application);
    let apply_effect = Effect::GenericEffect {
        static_abilities: vec![StaticDefinition::continuous()
            .affected(affected)
            .modifications(modifications)
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
            selection: crate::types::ability::TargetSelectionMode::Chosen,
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

/// CR 611.2 + CR 514.2: Recover a duration phrase embedded mid-predicate (not at
/// the trailing edge `strip_trailing_duration` scans). Granted combat
/// restrictions place the timing phrase before the restriction body —
/// "can't be blocked this turn except by <filter>" — so the marker is interior.
/// Scanned at word boundaries via a nom combinator so "this turn"/"this combat"
/// matches a complete phrase, never an arbitrary substring. Returns `None` when
/// no recognized interior duration phrase is present.
fn embedded_restriction_duration(lower: &str) -> Option<Duration> {
    // The phrase→`Duration` mapping is owned by the single duration grammar
    // (`oracle_nom/duration.rs`); this helper owns only the interior
    // word-boundary scan position.
    let (_, duration, _) = nom_primitives::scan_preceded(lower, parse_duration)?;
    Some(duration)
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

    // CR 611.2 + CR 509.1b: A duration phrase can sit mid-predicate rather than
    // trailing — "can't be blocked this turn except by <filter>" (Fast //
    // Furious) — so `strip_trailing_duration` (which only matches a suffix) left
    // `duration` as None. Recover the embedded "this turn"/"this combat" marker
    // so the granted restriction is correctly scoped; without it the static
    // would persist indefinitely. Only fills an unset duration, so a trailing
    // phrase the strip already captured is never overridden.
    let duration = duration.or_else(|| embedded_restriction_duration(&lower));

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

fn build_defender_attack_continuous_compound(
    application: &SubjectApplication,
    predicate_text: &str,
) -> Option<Vec<StaticDefinition>> {
    // CR 702.3b + CR 510.1c + CR 611.2c: A resolved ability can grant one
    // targeted creature multiple continuous pieces at once: characteristics
    // (haste), a defender attack rule exception, and toughness-based damage
    // assignment. Split only the grammar that contains the defender exception so
    // ordinary keyword lists continue through the shared continuous parser.
    let segments = split_continuous_compound_segments(predicate_text);
    if segments.len() < 2
        || !segments
            .iter()
            .any(|segment| is_can_attack_despite_defender_predicate(&segment.to_lowercase()))
    {
        return None;
    }

    let affected = static_affected_for_application(application);
    let mut static_abilities = Vec::new();

    for segment in segments {
        let segment = segment.trim();
        if segment.is_empty() {
            continue;
        }
        let lower = segment.to_lowercase();
        if is_can_attack_despite_defender_predicate(&lower) {
            static_abilities.push(
                StaticDefinition::new(StaticMode::CanAttackWithDefender)
                    .affected(affected.clone())
                    .modifications(vec![ContinuousModification::AddStaticMode {
                        mode: StaticMode::CanAttackWithDefender,
                    }])
                    .description(segment.to_string()),
            );
            continue;
        }

        let modifications = parse_continuous_modifications(segment);
        if modifications.is_empty() {
            return None;
        }
        static_abilities.push(
            StaticDefinition::continuous()
                .affected(affected.clone())
                .modifications(modifications)
                .description(segment.to_string()),
        );
    }

    (!static_abilities.is_empty()).then_some(static_abilities)
}

fn split_continuous_compound_segments(predicate_text: &str) -> Vec<&str> {
    predicate_text
        .trim()
        .trim_end_matches('.')
        .split(',')
        .map(|segment| strip_leading_conjunction(segment.trim()))
        .collect()
}

fn strip_leading_conjunction(segment: &str) -> &str {
    let lower = segment.to_lowercase();
    if let Ok((rest, _)) = tag::<_, _, OracleError<'_>>("and ").parse(lower.as_str()) {
        let consumed = lower.len() - rest.len();
        return segment[consumed..].trim_start();
    }
    segment
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
            // CR 702.122a / CR 702.171a / CR 702.184c: a granted crew/saddle/station power modifier (e.g. Stoic
            // Star-Captain's "Each creature you control crews … as though its power
            // were 2 greater") must propagate onto the affected creatures so the
            // crew/saddle power summation observes it via active_static_definitions.
            | StaticMode::CrewContribution { .. }
            | StaticMode::CantBeBlocked
            | StaticMode::CantBeBlockedBy { .. }
            | StaticMode::CantBeBlockedExceptBy { .. }
            | StaticMode::CantUntap
            // CR 702.26a + CR 101.2: the phase-in lock is granted to the parent
            // ability's chosen target ("It can't phase in …"), so it must
            // propagate onto that permanent's `static_definitions` as a
            // `SpecificObject` transient grant — without `AddStaticMode` the TCE
            // carries empty modifications and the phase-in gate never sees it.
            | StaticMode::CantPhaseIn
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

/// One verb-phrase atom of a "can't …" restriction list, mapped to the
/// `StaticMode`(s) it denies.
///
/// The negation prefix and list separators are owned by
/// [`parse_restriction_modes`]; atoms never re-encode "can't". Compound
/// Oracle wordings ("can't attack, block, or crew Vehicles" — Bound in Gold;
/// "can't block or be blocked"; "can't be equipped or enchanted") are list
/// compositions of these atoms, never enumerated permutations — each
/// compound emits exactly its members' modes (so "equipped or enchanted"
/// does NOT collapse to a CantBeAttached superset; Fortifications are
/// excluded by the Oracle wording).
fn parse_restriction_list_atom(input: &str) -> OracleResult<'_, Vec<StaticMode>> {
    alt((
        // CR 508.1d: attack restriction.
        value(vec![StaticMode::CantAttack], tag("attack")),
        // CR 509.1a: "block this creature" / "block ~" / "block it" —
        // source-referential variant used by activated abilities; the mode
        // applies to the subject (the would-be blocker), so the object is
        // not encoded. Must precede the bare "block" atom.
        value(
            vec![StaticMode::CantBlock],
            (
                tag("block "),
                alt((tag("this creature"), tag("~"), tag("it"))),
            ),
        ),
        // CR 509.1a: block restriction.
        value(vec![StaticMode::CantBlock], tag("block")),
        // CR 509.1a: "be blocked [this turn]". Followed-by-filter forms
        // ("… except by <filter>", "… by <filter>") fail the outer
        // all_consuming list and fall through to their dedicated arms.
        value(
            vec![StaticMode::CantBeBlocked],
            (tag("be blocked"), opt(tag(" this turn"))),
        ),
        // CR 701.21: sacrifice prohibition.
        value(
            vec![StaticMode::Other("CantBeSacrificed".to_string())],
            tag("be sacrificed"),
        ),
        // CR 702.5: aura attachment prohibition. The bare "enchanted"
        // alternate covers the elided-"be" second leg of "can't be equipped
        // or enchanted" (the negation and "be" distribute over the list).
        value(
            vec![StaticMode::Other("CantBeEnchanted".to_string())],
            (
                alt((tag("be enchanted"), tag("enchanted"))),
                opt(tag(" by other auras")),
            ),
        ),
        // CR 702.6: equipment attachment prohibition.
        value(
            vec![StaticMode::Other("CantBeEquipped".to_string())],
            tag("be equipped"),
        ),
        // CR 101.2: "be countered" overrides counterspell effects; the
        // subject path owns the "spells you control" / "green spells you
        // control" grammar.
        value(vec![StaticMode::CantBeCountered], tag("be countered")),
        // CR 701.27: transform prohibition (e.g., Immerwolf).
        value(
            vec![StaticMode::Other("CantTransform".to_string())],
            tag("transform"),
        ),
        // CR 702.122d: "crew [Vehicles]".
        value(
            vec![StaticMode::CantCrew],
            (tag("crew"), opt(tag(" vehicles"))),
        ),
        // CR 702.26a + CR 101.2: "phase in" prohibition (The Pandorica: "It
        // can't phase in for as long as ~ remains tapped"). The negation prefix
        // and any trailing "for as long as …" duration are owned by the caller
        // (`parse_restriction_modes` / `strip_trailing_duration`); this atom
        // matches only the bare verb phrase.
        value(vec![StaticMode::CantPhaseIn], tag("phase in")),
    ))
    .parse(input)
}

/// Parse restriction predicates into one or more `StaticMode` variants.
/// Handles simple ("can't block") and compound ("can't attack or block") patterns.
pub(crate) fn parse_restriction_modes(lower: &str) -> Option<Vec<StaticMode>> {
    // Negation prefix × verb-phrase-list grammar (CLAUDE.md "Compose nom
    // combinators, don't enumerate permutations"): "can't"/"cannot" applies
    // once and distributes over a comma/or-separated list of
    // [`parse_restriction_list_atom`]s, covering every compound wording
    // without enumerating the cross-product. Parameterized forms that carry
    // a trailing filter ("… except by <filter>", "… by <filter>") fail the
    // all_consuming list and fall through to their dedicated arms below.
    if let Ok((_, atom_modes)) = all_consuming(preceded(
        (
            alt((tag::<_, _, OracleError<'_>>("can't"), tag("cannot"))),
            tag(" "),
        ),
        // A static line's terminal period can reach here (the predicate keeps it
        // when no trailing duration strips it), so absorb an optional trailing
        // "." in the combinator before `all_consuming`'s eof rather than trimming
        // the input — mirroring the dedicated `can't be regenerated` arm below.
        terminated(
            separated_list1(
                alt((tag(", or "), tag(", "), tag(" or "))),
                parse_restriction_list_atom,
            ),
            opt(tag(".")),
        ),
    ))
    .parse(lower)
    {
        return Some(atom_modes.concat());
    }
    // CR 701.19c: "~ can't be regenerated" — marks the subject so regeneration
    // shields are not applied. Backstop for the "cannot" phrasing and any caller
    // that routes through the generic " can't " / " cannot " split before
    // reaching the dedicated arm in `try_parse_subject_restriction_clause`.
    // Kept outside the atom list: it tolerates a trailing period.
    if parse_cant_be_regenerated_predicate(lower.trim()).is_ok() {
        return Some(vec![StaticMode::CantBeRegenerated]);
    }
    // CR 509.1b + CR 611.2: "can't be blocked [this turn] except by <filter>" —
    // granted evasion restriction (Fast // Furious: "It can't be blocked this turn
    // except by Vehicles or by creatures with haste."). The duration phrase can
    // sit mid-predicate ("blocked this turn except by …"), so it is not removed by
    // the trailing-duration strip; absorb the optional " this turn" here between
    // "blocked" and "except by". The filter is classified by the same
    // `classify_block_exception` authority the printed/static evasion path uses, so
    // "Vehicles or by creatures with haste" lowers to the full quality `Or`.
    if let Ok((except_text, _)) = (
        alt((
            tag::<_, _, OracleError<'_>>("can't be blocked"),
            tag("cannot be blocked"),
        )),
        opt(tag::<_, _, OracleError<'_>>(" this turn")),
        tag(" except by "),
    )
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

pub(super) fn parse_cant_be_regenerated_predicate(input: &str) -> OracleResult<'_, ()> {
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

/// CR 608.2c + CR 119.7: Recognize the anaphoric head of Screaming Nemesis's
/// life-lock rider — "if a player is dealt damage this way, they " — and
/// return the residual predicate ("can't gain life for the rest of the game")
/// for the shared restriction builder. Decomposed into independent pieces per
/// the combinator rule: the leading "if" glue, the "a/any player ... dealt
/// damage this way" anaphor (CR 608.2c "this way" back-reference to the
/// redirect's damage event), and the trailing "they " pronoun. Returns `None`
/// when the head is absent, so the caller falls through to the generic
/// subject/predicate split. The returned slice borrows from `lower`.
fn strip_dealt_damage_this_way_player_anaphor(lower: &str) -> Option<&str> {
    let (rest, _) = (
        tag::<_, _, OracleError<'_>>("if "),
        alt((tag("a player"), tag("any player"))),
        tag(" is dealt damage this way, "),
        tag("they "),
    )
        .parse(lower)
        .ok()?;
    Some(rest)
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

/// CR 701.15a: Parse "it's goaded [duration]" / "it is goaded [duration]" copula
/// state-setting clauses. The contraction "it's" fuses the subject pronoun with
/// the copula "is", so `find_predicate_start` cannot split subject from predicate.
/// This helper catches the pattern early and lowers it to `Effect::Goad` with the
/// pronoun-resolved target and an optional trailing duration.
///
/// Covers: Jon Irenicus, Shattered One ("it's goaded for the rest of the game"),
/// Vislor Turlough ("it's goaded for as long as they control it"), and the
/// non-contracted form ("it is goaded").
fn try_parse_copula_goaded_clause(
    text: &str,
    ctx: &mut ParseContext,
) -> Option<ParsedEffectClause> {
    let lower = text.to_lowercase();
    // Strip subject + copula: "it's goaded ..." / "it is goaded ..."
    let after_subject = alt((
        preceded(
            tag::<_, _, OracleError<'_>>("it's "),
            tag::<_, _, OracleError<'_>>("goaded"),
        ),
        preceded(tag::<_, _, OracleError<'_>>("it is "), tag("goaded")),
    ))
    .parse(lower.as_str())
    .ok()?;
    let remainder = after_subject.0.trim_start().trim_end_matches('.').trim();
    // Parse optional trailing duration ("for the rest of the game", "for as long as ...").
    let duration = if remainder.is_empty() {
        // CR 701.15a: Default goad duration is until the goading player's next turn.
        None
    } else {
        // The trailing text must be a *complete*, clause-final duration with
        // nothing left over. A clause like "it's goaded for the rest of the
        // game and draws a card" carries a further conjunct that this helper
        // does not lower — declining (rather than silently dropping the
        // remainder) avoids dishonest coverage and lets the compound fall
        // through to the chained-clause parser.
        let (rest, d) = parse_duration(remainder).ok()?;
        if !rest.trim().is_empty() {
            return None;
        }
        Some(d)
    };
    let target = resolve_it_pronoun(ctx);
    Some(ParsedEffectClause {
        effect: Effect::Goad { target },
        duration,
        sub_ability: None,
        distribute: None,
        multi_target: None,
        condition: None,
        optional: false,
        unless_pay: None,
    })
}

/// Detect "its controller gains life equal to its power" and similar patterns where
/// the targeted permanent's controller (or owner) gains life based on the permanent's stats.
///
/// Despite the historical name, this also handles the owner-of-target phrasing
/// ("its owner gains 4 life" — Misfortune's Gain, Path of Peace). The subject
/// alt yields the resolved player `TargetFilter` (controller vs. owner) which is
/// threaded into the emitted `GainLife.player`. CR 108.3 distinguishes owner
/// from controller (CR 109.4); they differ when the spell controller doesn't own
/// the targeted permanent.
pub(super) fn try_parse_targeted_controller_gain_life(text: &str) -> Option<ParsedEffectClause> {
    let lower = text.to_lowercase();
    let (after_prefix, _) = opt(tag::<_, _, OracleError<'_>>("then "))
        .parse(lower.as_str())
        .ok()?;
    // "That creature's controller gains life" (Solitude) and "its controller
    // gains life" are both controller-of-target phrasing — route to
    // ParentTargetController.
    fn parse_det_noun_ctrl(i: &str) -> OracleResult<'_, ()> {
        let (i, _) = alt((tag("that "), tag("the "))).parse(i)?;
        let (i, _) = take_until("'s controller ").parse(i)?;
        let (i, _) = tag("'s controller ").parse(i)?;
        Ok((i, ()))
    }
    // CR 108.3: "[that|the] <noun>'s owner gains life" — owner-of-target phrasing.
    fn parse_det_noun_owner(i: &str) -> OracleResult<'_, ()> {
        let (i, _) = alt((tag("that "), tag("the "))).parse(i)?;
        let (i, _) = take_until("'s owner ").parse(i)?;
        let (i, _) = tag("'s owner ").parse(i)?;
        Ok((i, ()))
    }
    let (after_subject, player_filter) = alt((
        map(tag::<_, _, OracleError<'_>>("its controller "), |_| {
            TargetFilter::ParentTargetController
        }),
        map(parse_det_noun_ctrl, |_| {
            TargetFilter::ParentTargetController
        }),
        map(tag("its owner "), |_| TargetFilter::ParentTargetOwner),
        map(parse_det_noun_owner, |_| TargetFilter::ParentTargetOwner),
    ))
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
        player: player_filter,
    }))
}

/// CR 120.1 + CR 120.3 + CR 115.1d: "Up to two target creatures you control each
/// deal damage equal to their power to target creature [restriction]." (Band
/// Together, Allies at Last, Combo Attack, Friendly Rivalry, Graceful Takedown.)
///
/// The subject ("[up to two|two|one or two] target creatures you control" /
/// "two target creatures your team controls") is a TARGETED source set — its
/// count bound becomes the ability's `multi_target` spec and its per-object
/// legality the `sources` filter. The "to <target creature>" tail is the single
/// recipient. Produces `Effect::EachDealsDamageEqualToPower { sources, recipient }`
/// with the count spec carried on the returned clause's `multi_target`.
///
/// Subject preservation matters: the sources are not the ability source (the
/// spell), and the recipient is a second, independent target — so this must be
/// recognized before generic subject stripping flattens the sentence.
pub(super) fn try_parse_each_deals_damage_equal_to_power(text: &str) -> Option<ParsedEffectClause> {
    let lower = text.to_lowercase();
    // CR 115.1d: the source-count quantifier. "two" → exactly 2; "up to two" →
    // 0..=2; "one or two" → 1..=2. Each axis is one `tag()` alternative.
    let (after_count, multi_target) = parse_each_deals_source_count(lower.as_str())?;

    // CR 115.1: the source set, as a TARGETED creature filter. "your team
    // controls" (Two-Headed Giant team scope, CR 810) is intentionally NOT
    // collapsed to "you control": a team is not the caster's controller, so a
    // card that uses it (Combo Attack) must fail closed rather than mis-target a
    // single player's creatures. `parse_target` does not consume the non-standard
    // "your team controls" controller phrase, so it stays in `after_sources` and
    // the verb-phrase `tag` below rejects the line (-> Unimplemented).
    let (sources, after_sources) = parse_target(after_count);
    if !target_filter_is_targeted_creature(&sources) {
        return None;
    }

    // CR 115.4 + CR 601.2c: optional group B — "and up to one other target
    // creature you control" (Graceful Takedown). Parsed BEFORE the verb tag; a
    // single-group card's remainder starts "each …", so `tag("and ")` fails and
    // `extra_source` stays None (byte-identical to the pre-group-B behavior).
    let after_sources_trimmed = after_sources.trim_start();
    let (extra_source, after_group_b) = match parse_extra_other_source(after_sources_trimmed) {
        Some((f, rest)) => (Some(f), rest.trim_start()),
        None => (None, after_sources_trimmed),
    };

    // CR 120.1: the verb phrase. Each chosen source deals damage equal to its own
    // power. `parse()` returns `(remaining, output)` — bind the remaining text.
    let after_verb = match tag::<_, _, OracleError<'_>>("each deal damage equal to their power to ")
        .parse(after_group_b)
    {
        Ok((rest, _)) => rest,
        // CR 810: the team-up shape carrying an unmodelable "your team controls"
        // source scope (Combo Attack). `parse_target` leaves "your team controls
        // …" in the remainder, so the verb tag above misses. Fail CLOSED to a
        // DETERMINISTIC `Unimplemented` rather than returning `None`: `None` would
        // let a generic target-only fallback non-deterministically accept the
        // leading "two target creatures …" as a bare `TargetOnly` clause (the
        // fallback order is HashMap-seed dependent).
        Err(_)
            if tag::<_, _, OracleError<'_>>(
                "your team controls each deal damage equal to their power to ",
            )
            .parse(after_sources_trimmed)
            .is_ok() =>
        {
            return Some(super::parsed_clause(Effect::unimplemented("deal", text)));
        }
        Err(_) => return None,
    };

    // CR 115.1: the single recipient creature ("target creature", "target
    // creature an opponent controls", "another target creature", "target
    // creature you don't control").
    let (recipient, _rest) = parse_target(after_verb);
    if !target_filter_is_targeted_creature(&recipient) {
        return None;
    }

    // SINGLE-CONSTRUCTION-SITE INVARIANT: this is the sole place
    // `EachDealsDamageEqualToPower` is built. `extra_source` is a parse-time
    // filter carried by `#[derive(Clone)]`, never a resolved target, so no
    // copy/retarget/control-change arm can drop it (retarget's `target_filter()`
    // returns None for this effect → no-op) and the resolver reads only the flat
    // `ability.targets` vector, never `extra_source`.
    Some(ParsedEffectClause {
        multi_target: Some(multi_target),
        ..super::parsed_clause(Effect::EachDealsDamageEqualToPower {
            sources,
            recipient,
            extra_source,
        })
    })
}

/// CR 115.4 + CR 601.2c: optional second source group of a compound each-deals
/// line — "and up to one OTHER target creature you control". The "other" marker
/// (`FilterProp::Another`) is REQUIRED: "and up to one target ..." (no "other")
/// returns None → the caller's verb tag then misses → the whole line falls
/// through (no false green). Returns the group-B `TargetFilter` and the
/// remaining text after it.
///
// ponytail: "up to one" is the only attested cardinality; a future "up to N
// other" extends here via `parse_number`, not proliferation.
fn parse_extra_other_source(input: &str) -> Option<(TargetFilter, &str)> {
    let (after_and, _) = tag::<_, _, OracleError<'_>>("and ").parse(input).ok()?;
    let (after_qty, _) = tag::<_, _, OracleError<'_>>("up to one ")
        .parse(after_and)
        .ok()?;
    let (filter, rest) = parse_target(after_qty);
    let has_other = matches!(&filter, TargetFilter::Typed(tf)
        if tf.properties.iter().any(|p| matches!(p, FilterProp::Another)));
    if !target_filter_is_targeted_creature(&filter) || !has_other {
        return None;
    }
    Some((filter, rest))
}

/// CR 120.1 + CR 608.2c: "each <object-class filter> [you control] deals N damage
/// to <recipient>" — every matching object is its OWN damage source. Produces
/// `Effect::EachSourceDealsDamage` so per-source iteration survives, instead of the
/// generic `strip_subject_clause` path which flattens the source class into a
/// single ability-sourced `DealDamage`.
///
/// Dispatched in `parse_effect_clause_inner` immediately after the
/// `EachDealsDamageEqualToPower` intercept, ahead of every `strip_subject_clause`
/// site, so all nested contexts (trigger bodies, `ChooseOneOf` branches,
/// `sub_ability` chains) are caught.
pub(super) fn try_parse_each_source_deals_damage(
    text: &str,
    ctx: &mut ParseContext,
) -> Option<ParsedEffectClause> {
    let lower = text.to_lowercase();
    // CR 120.1: require a genuine UNIVERSAL/class quantifier subject ("each <X>").
    // This is what makes every match its own damage source. It deliberately
    // EXCLUDES singular anaphoric/targeted subjects that denote ONE source — "that
    // creature" (TriggeringSource), "it"/"this creature" (SelfRef), "enchanted
    // creature" (AttachedTo), "target creature you control" (a chosen target) — all
    // of which `parse_subject_application` would otherwise hand back as a bare
    // `Typed` class filter, wrongly broadcasting one source's damage across the
    // whole class.
    if tag::<_, _, OracleError<'_>>("each ")
        .parse(lower.as_str())
        .is_err()
    {
        return None;
    }
    // CR 608.2c: split subject vs predicate at the "deals"/"deal" verb. "attach"
    // is not a PREDICATE_VERB, so "each Aura attached to a creature deals …" splits
    // at "deals", not at "attached".
    let verb_start = find_predicate_start(text)?;
    let subject = text[..verb_start].trim();
    let predicate = text[verb_start..].trim();
    let predicate_lower = predicate.to_lowercase();

    // CR 120.1: the damage verb must be the clause's MAIN verb — the predicate
    // begins with "deals"/"deal". `find_predicate_start` splits at the FIRST
    // predicate verb, so a granted ability ("each creature you control gains
    // \"{T}: This creature deals 1 damage…\"" — Stensia) splits at "gains" and is
    // declined here, never mistaking the quoted "deals" for the main action.
    if alt((
        tag::<_, _, OracleError<'_>>("deals "),
        tag::<_, _, OracleError<'_>>("deal "),
    ))
    .parse(predicate_lower.as_str())
    .is_err()
    {
        return None;
    }

    // The recipient phrase: everything after the "deals N damage to " marker.
    let recipient_phrase = damage_recipient_phrase(&predicate_lower);

    // CR 303.4 (DEFERRED §9): "...to the creature/permanent it's attached to" — a
    // per-attachment host recipient not yet modeled. Fail CLOSED to an honest
    // `Unimplemented` BEFORE the subject-parse requirement, so the clause never
    // mis-deals regardless of how the attachment subject parses (Aura Barbs clause
    // 2). Clause 1 ("its controller") does not match this phrase.
    if recipient_phrase.is_some_and(is_attached_host_recipient) {
        return Some(super::parsed_clause(Effect::unimplemented(
            "each_source_attached_damage",
            text,
        )));
    }

    // Require a usable non-player object-class source. Player-shaped subjects
    // ("each player", "each opponent") and anaphoric "each of those …"
    // (`ParentTarget`) fall through to their own handling.
    let sources = parse_subject_application(subject, ctx)?.affected;
    if !is_object_class_source(&sources) {
        return None;
    }

    // Delegate the predicate to the shared damage parser so the amount and the
    // recipient anaphora (`ParentTarget`, `TriggeringSource`, `Any`) resolve
    // identically to the `DealDamage` the misparse produced — no re-implementation.
    let (amount, target, damage_source) =
        match super::lower::try_parse_damage(&predicate_lower, predicate, ctx)? {
            Effect::DealDamage {
                amount,
                target,
                damage_source,
                ..
            } => (amount, target, damage_source),
            _ => return None,
        };
    // The source set comes from the filter, never a `Target` damage source.
    if damage_source.is_some() {
        return None;
    }
    // CR 120.1: only a FIXED, source-INDEPENDENT amount. The amount is resolved
    // ONCE (uniform across the batch), so a per-source dynamic amount ("equal to
    // its power", "equal to its mana value") would be wrong — that filter-source
    // own-power class is deferred (see §1/§9).
    if !matches!(amount, QuantityExpr::Fixed { .. }) {
        return None;
    }

    // CR 109.4 + CR 120.3a: "its controller" is a per-source recipient. Every other
    // recipient is the shared announced/context target produced above.
    let recipient = if recipient_phrase.is_some_and(is_its_controller_recipient) {
        EachDamageRecipient::EachController
    } else {
        EachDamageRecipient::Shared(target)
    };

    Some(super::parsed_clause(Effect::EachSourceDealsDamage {
        sources,
        amount,
        recipient,
    }))
}

/// Return the recipient phrase of a "deals N damage to <recipient>" predicate —
/// the slice after the `" damage to "` marker, trimmed of a trailing period.
fn damage_recipient_phrase(predicate_lower: &str) -> Option<&str> {
    let (_rest, (_before, after)) =
        nom_primitives::split_once_on(predicate_lower, " damage to ").ok()?;
    Some(after.trim_end_matches('.').trim())
}

/// CR 109.4 + CR 120.3a: the recipient phrase is exactly "its controller".
fn is_its_controller_recipient(recipient_phrase: &str) -> bool {
    all_consuming(tag::<_, _, OracleError<'_>>("its controller"))
        .parse(recipient_phrase)
        .is_ok()
}

/// CR 303.4 (DEFERRED §9): the recipient phrase is "the creature/permanent it's
/// attached to" (straight + typographic apostrophe), an unmodeled per-attachment
/// host recipient.
fn is_attached_host_recipient(recipient_phrase: &str) -> bool {
    all_consuming(alt((
        tag::<_, _, OracleError<'_>>("the creature it's attached to"),
        tag("the creature it\u{2019}s attached to"),
        tag("the permanent it's attached to"),
        tag("the permanent it\u{2019}s attached to"),
    )))
    .parse(recipient_phrase)
    .is_ok()
}

/// CR 120.1: True when `filter` selects game OBJECTS by type/subtype (a valid
/// `EachSourceDealsDamage` source class), not players.
fn is_object_class_source(filter: &TargetFilter) -> bool {
    match filter {
        TargetFilter::Typed(_) => true,
        TargetFilter::Or { filters } | TargetFilter::And { filters } => {
            !filters.is_empty() && filters.iter().all(is_object_class_source)
        }
        TargetFilter::Not { filter } => is_object_class_source(filter),
        _ => false,
    }
}

/// CR 115.1d: Parse the leading source-count quantifier of the team-up damage
/// line and return the remaining text plus the `MultiTargetSpec` it encodes.
/// "up to two" → 0..=2, "one or two" → 1..=2, "two" → exactly 2, "any number
/// of" → 0..unbounded. A bare singular "target …" (no quantifier) → exactly 1.
fn parse_each_deals_source_count(lower: &str) -> Option<(&str, MultiTargetSpec)> {
    alt((
        map(tag::<_, _, OracleError<'_>>("up to two "), |_| {
            MultiTargetSpec::fixed(0, 2)
        }),
        map(tag("one or two "), |_| MultiTargetSpec::fixed(1, 2)),
        map(tag("two "), |_| MultiTargetSpec::fixed(2, 2)),
        // CR 115.1d: unbounded group-A quantifier ("any number of target
        // enchanted creatures you control", Graceful Takedown) → 0..unbounded.
        map(tag("any number of "), |_| MultiTargetSpec::unlimited(0)),
        // CR 115.1: singular group A with NO quantifier ("Target creature you
        // control and up to one other target legendary creature …", Friendly
        // Rivalry) → exactly one. `peek` leaves "target " unconsumed so
        // `parse_target` reads the keyword exactly as the quantified arms do.
        // Must be LAST — the quantified prefixes above never start with "target".
        map(peek(tag::<_, _, OracleError<'_>>("target ")), |_| {
            MultiTargetSpec::fixed(1, 1)
        }),
    ))
    .parse(lower)
    .ok()
}

/// CR 115.1: True when `filter` is a "target creature" object filter (the
/// `target` keyword present, restricted to creatures). Guards both the source
/// set and the recipient of the team-up damage line so non-creature or
/// non-targeted phrases fall through to the generic parser.
fn target_filter_is_targeted_creature(filter: &TargetFilter) -> bool {
    matches!(filter, TargetFilter::Typed(tf)
    if tf.type_filters.iter().any(|t| matches!(
        t,
        crate::types::ability::TypeFilter::Creature
    )))
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

/// Strip a leading *controller* subject ("you may " / "you ") from `text`,
/// returning the imperative remainder in original case; `None` for every other
/// leading subject.
///
/// This is the controller-only counterpart to [`strip_subject_clause`]. It
/// exists for the token "create … for each X" fallback in
/// `oracle_effect/mod.rs`, which delegates to `token::try_parse_token`. That
/// path defaults `Effect::Token.owner` to `TargetFilter::Controller`
/// (CR 109.5: an unqualified "you" is the controller of the source) and — via
/// the early return in `try_parse_for_each_effect` — does NOT run the
/// subject→owner rebinding that the numeric/targeted for-each arms use. So
/// stripping a *non-controller* subject there ("each player", "each opponent",
/// "target player/opponent", "its controller", "that player", …) would
/// silently mis-own the created token to the source controller
/// (CR 111.11: a token is created under a specific player's control). Only the
/// controller subject may be stripped in that fallback; any other leading
/// subject returns `None` so the clause honestly falls through to unsupported.
pub(super) fn strip_controller_subject_clause(text: &str) -> Option<String> {
    let lower = text.to_lowercase();
    // Longest-match first: "you may " before the bare "you " so the optional
    // permission word is consumed rather than stranded on the remainder.
    let ((), remainder) = nom_on_lower(text, &lower, |i| {
        value(
            (),
            alt((
                tag::<_, _, OracleError<'_>>("you may "),
                tag::<_, _, OracleError<'_>>("you "),
            )),
        )
        .parse(i)
    })?;
    let remainder = remainder.trim();
    if remainder.is_empty() {
        return None;
    }
    Some(remainder.to_string())
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
            // CR 115.1 + CR 115.1d: "one or more target X" variable-count
            // subject (Dwarven Song et al.). Dispatched to the multi-target
            // branch in `parse_subject_application`.
            value((), tag("one or more ")),
        )),
        alt((
            value((), tag::<_, _, OracleError<'_>>("its owner ")),
            value((), tag("~'s owner ")),
            value((), tag("target ")),
            value((), tag("that ")),
            value((), tag("the chosen ")),
            // CR 506.2 + CR 603.7c: "the attacking player" as a control-handoff
            // subject on a DamageReceived trigger (Contested Game Ball) — the
            // controller of the creature that dealt combat damage. Longest-match
            // before the bare "the player " arm.
            value((), tag("the attacking player ")),
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
    // CR 705.1: Coin flips — "you flip a coin" / "that player flips a coin" /
    // "each player flips a coin". The self/player subject is stripped here so the
    // deconjugated predicate ("flip a coin") re-dispatches through the imperative
    // path to `Effect::FlipCoin`. The flip arm in `imperative.rs` requires the
    // literal "a coin", so the Kamigawa "flip <permanent>" flip-card mechanic
    // ("flip ~" / "flip it", CR 710.4) is never mis-routed to a coin flip.
    "flip",
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

/// CR 608.2c: Strip a trailing additive "also" connector from a filter-subject
/// phrase. In a chained grant ("<subject> also gain <keyword> ...") the "also"
/// is the additive adverb linking this continuous effect to a sibling clause and
/// carries no selection semantics. `find_predicate_start` lands the verb split
/// after the "also", so the residual subject is "<filter> also"; left intact the
/// trailing word leaks into `parse_target` and fails the subject match. Returns
/// the subject unchanged when no additive "also" is present, or when the head is
/// empty (a bare "also" has no filter to grant against).
///
/// Pattern 2a (`oracle_nom/PATTERNS.md`): the whole subject is parsed and the
/// fixed suffix is consumed last. `all_consuming` anchors " also" to the END, so
/// a non-terminal "also" (e.g. "also creatures you control") is not matched. The
/// lowercase head's byte length maps 1:1 onto `subject` for case preservation.
fn strip_trailing_additive_adverb(subject: &str) -> &str {
    let lower = subject.to_lowercase();
    // Return the head's byte length (not a borrow of the temporary `lower`) so the
    // closure result is owned; map it back onto `subject` for case preservation.
    let parsed = nom_on_lower(subject, &lower, |input| {
        all_consuming(terminated(
            map(take_until::<_, _, OracleError<'_>>(" also"), str::len),
            tag(" also"),
        ))
        .parse(input)
    });
    match parsed {
        Some((head_len, _)) if !subject[..head_len].trim_end().is_empty() => {
            subject[..head_len].trim_end()
        }
        _ => subject,
    }
}

fn is_restriction_predicate_verb(token: &str) -> bool {
    // CR 613.1d: "isn't"/"aren't" head a layer-4 type-removal predicate ("~ isn't
    // a creature until end of turn", Blink's Alien Angel token). Recognizing the
    // copula-negation here lets `find_predicate_start` split subject from
    // predicate so the continuous-clause path produces a `RemoveType`
    // modification (via `parse_continuous_modifications`).
    matches!(token, "can't" | "cannot" | "isn't" | "aren't")
}

fn token_starts_predicate(token: &str) -> bool {
    is_restriction_predicate_verb(token)
        || PREDICATE_VERBS.contains(&super::normalize_verb_token(token).as_str())
}

pub(super) fn find_predicate_start(text: &str) -> Option<usize> {
    let lower = text.to_lowercase();
    let mut word_start = None;

    for (idx, ch) in lower.char_indices() {
        if ch.is_whitespace() {
            if let Some(start) = word_start.take() {
                let token = &lower[start..idx];
                if token_starts_predicate(token) {
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
        if token_starts_predicate(token) {
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
    use crate::types::ability::{
        AbilityKind, BasicLandType, ContinuousModification, ControllerRef, Effect, TypeFilter,
    };
    use crate::types::card_type::Supertype;
    use crate::types::statics::BlockExceptionKind;

    /// CR 105.3 + CR 106.1a: "becomes that color" (Foraging Wickermaw) maps to the
    /// same `AddChosenColor` reader as "the chosen color" (Puca's Eye) — only the
    /// upstream writer differs (mana production vs `Effect::Choose`). Regression-
    /// guards the sibling arms so a future edit can't drop them.
    #[test]
    fn become_that_color_maps_to_add_chosen_color() {
        assert!(matches!(
            try_parse_become_color_modification("that color"),
            Some(ContinuousModification::AddChosenColor {
                mode: ColorChangeMode::Set
            })
        ));
        assert!(matches!(
            try_parse_become_color_modification("the chosen color"),
            Some(ContinuousModification::AddChosenColor {
                mode: ColorChangeMode::Set
            })
        ));
        assert!(matches!(
            try_parse_become_color_modification("all colors"),
            Some(ContinuousModification::SetColor { .. })
        ));
        // Unrelated predicates still fall through (the animation path handles them).
        assert!(try_parse_become_color_modification("a giant lizard").is_none());
        assert_eq!(
            try_parse_become_basic_land_type_modifications("Swamps"),
            Some(vec![ContinuousModification::SetBasicLandType {
                land_type: BasicLandType::Swamp,
            }])
        );
        assert!(try_parse_become_basic_land_type_modifications("black").is_none());
    }

    // CR 702.62a + CR 702.62b + CR 611.2a: "Cards exiled this way gain suspend"
    // (unconditional form — no "that don't have" clause) must produce the
    // GenericEffect{AddKeyword(Suspend), ParentTarget, Permanent} shape that
    // matches the singular Jhoira/Tenth suspend-grant, with no condition set.
    #[test]
    fn exiled_this_way_suspend_grant_matches_singular_shape() {
        use crate::types::keywords::Keyword;
        let ctx = ParseContext::default();
        let clause =
            try_parse_exiled_this_way_keyword_grant("Cards exiled this way gain suspend", &ctx)
                .expect("should recognize the unconditional set-referencing suspend grant");
        assert_eq!(clause.duration, Some(Duration::Permanent));
        // No condition on the unconditional form.
        assert_eq!(clause.condition, None);
        let Effect::GenericEffect {
            static_abilities,
            duration,
            target,
        } = &clause.effect
        else {
            panic!("expected GenericEffect, got {:?}", clause.effect);
        };
        assert_eq!(*duration, Some(Duration::Permanent));
        assert_eq!(*target, Some(TargetFilter::ParentTarget));
        assert_eq!(static_abilities.len(), 1);
        assert!(static_abilities[0].modifications.iter().any(|m| matches!(
            m,
            ContinuousModification::AddKeyword {
                keyword: Keyword::Suspend { .. }
            }
        )));
    }

    // Building-block proof: the recognizer is parameterized on the keyword, not
    // Suspend-hardcoded. A synthetic "cards exiled this way gain flying" must
    // produce an AddKeyword(Flying) grant with no condition.
    #[test]
    fn exiled_this_way_grant_is_keyword_parameterized() {
        use crate::types::keywords::Keyword;
        let ctx = ParseContext::default();
        let clause =
            try_parse_exiled_this_way_keyword_grant("Cards exiled this way gain flying", &ctx)
                .expect("should recognize a keyword-parameterized set grant");
        assert_eq!(clause.condition, None);
        let Effect::GenericEffect {
            static_abilities, ..
        } = &clause.effect
        else {
            panic!("expected GenericEffect, got {:?}", clause.effect);
        };
        assert!(static_abilities[0].modifications.iter().any(|m| matches!(
            m,
            ContinuousModification::AddKeyword {
                keyword: Keyword::Flying
            }
        )));
    }

    // Strict-failure guard: the "that don't have <kw>" restrictive clause
    // produces a documented strict-failure (None) because the correct per-card
    // object-scoped condition is not yet implemented. Both keyword variants must
    // be rejected so the chunk falls through to Unimplemented.
    #[test]
    fn exiled_this_way_with_restrictive_clause_is_strict_failure() {
        let ctx = ParseContext::default();
        assert!(
            try_parse_exiled_this_way_keyword_grant(
                "Cards exiled this way that don't have suspend gain suspend",
                &ctx,
            )
            .is_none(),
            "suspend restrictive clause must strict-fail"
        );
        assert!(
            try_parse_exiled_this_way_keyword_grant(
                "Cards exiled this way that don't have flying gain flying",
                &ctx,
            )
            .is_none(),
            "flying restrictive clause must strict-fail"
        );
    }

    // Strict-failure guard: a non-"gain <kw>" predicate after the subject head
    // must decline (return None) so the chunk falls through to normal dispatch
    // rather than producing a wrong continuous grant.
    #[test]
    fn exiled_this_way_grant_declines_non_keyword_predicate() {
        let ctx = ParseContext::default();
        assert!(try_parse_exiled_this_way_keyword_grant(
            "Cards exiled this way are put into their owner's graveyard",
            &ctx,
        )
        .is_none());
    }

    // CR 608.2c: The Wedding of River Song — full spell chain. Both Defect B
    // ("then target opponent does the same") and Defect C ("cards exiled this
    // way that don't have suspend gain suspend") are documented strict-failures
    // (`Unimplemented`): Defect B pending cross-cutting opponent-choice routing,
    // Defect C pending an object-scoped condition variant that applies per
    // exiled card rather than per spell source (see
    // try_parse_exiled_this_way_keyword_grant). Neither should degenerate into
    // the prior silent `ChangeZone{empty, Opponent}` misparse.
    #[test]
    fn wedding_of_river_song_chain_strict_failures_are_documented() {
        let def = super::super::parse_effect_chain(
            "Draw two cards, then you may exile a nonland card from your hand with a \
             number of time counters on it equal to its mana value. Then target \
             opponent does the same. Cards exiled this way that don't have suspend \
             gain suspend.\nTime travel.",
            AbilityKind::Spell,
        );

        // Walk the whole def + sub_ability tree collecting every effect.
        fn collect<'a>(def: &'a AbilityDefinition, out: &mut Vec<&'a Effect>) {
            out.push(&def.effect);
            if let Some(sub) = &def.sub_ability {
                collect(sub, out);
            }
            if let Some(els) = &def.else_ability {
                collect(els, out);
            }
        }
        let mut effects = Vec::new();
        collect(&def, &mut effects);

        // Defect B: "does the same" lowers to a DOCUMENTED strict-failure keyed on
        // the typed subject — never the degenerate `ChangeZone{empty, Opponent}`.
        assert!(
            effects.iter().any(|e| matches!(
                e,
                // allow-noncombinator: test assertion on the stable snake_case Unimplemented pattern-class key, not parser dispatch
                Effect::Unimplemented { name, .. } if name == "target_opponent_does_the_same"
            )),
            "expected the documented 'does the same' strict-failure, got {effects:#?}"
        );
        assert!(
            !effects.iter().any(|e| matches!(
                e,
                Effect::ChangeZone {
                    destination: crate::types::zones::Zone::Exile,
                    target: TargetFilter::Typed(tf),
                    ..
                } if tf.controller == Some(ControllerRef::Opponent) && tf.type_filters.is_empty()
            )),
            "the degenerate empty-Opponent exile misparse must be gone"
        );

        // Defect C: "cards exiled this way that don't have suspend gain suspend"
        // must NOT produce a GenericEffect suspend grant. The "that don't have"
        // restrictive clause strict-fails until an object-scoped condition exists.
        fn chain_has_suspend_grant(def: &AbilityDefinition) -> bool {
            use crate::types::keywords::Keyword;
            let here = matches!(
                &*def.effect,
                Effect::GenericEffect { static_abilities, .. }
                    if static_abilities.iter().any(|s| s.modifications.iter().any(|m| matches!(
                        m,
                        ContinuousModification::AddKeyword { keyword: Keyword::Suspend { .. } }
                    )))
            );
            here || def
                .sub_ability
                .as_ref()
                .is_some_and(|s| chain_has_suspend_grant(s))
        }
        assert!(
            !chain_has_suspend_grant(&def),
            "Defect C must not produce a GenericEffect suspend grant (strict-failure expected)"
        );
    }

    // CR 611.3a + CR 702.16: Dominaria's Judgment — "creatures you control gain
    // protection from white if you control a Plains, from blue if you control an
    // Island, ..., and from green if you control a Forest" must emit one
    // conditionally-gated grant PER color, each gated on its own land. The prior
    // generic path collapsed the list into a single static keeping only the final
    // (Forest) condition and left the other colors as raw `CardType` strings.
    #[test]
    fn conditional_protection_grant_list_gates_each_color_on_its_own_land() {
        use crate::types::ability::StaticCondition;
        use crate::types::keywords::{Keyword, ProtectionTarget};
        use crate::types::mana::ManaColor;

        let def = super::super::parse_effect_chain(
            "Until end of turn, creatures you control gain protection from white if \
             you control a Plains, from blue if you control an Island, from black if \
             you control a Swamp, from red if you control a Mountain, and from green \
             if you control a Forest.",
            AbilityKind::Spell,
        );
        let Effect::GenericEffect {
            static_abilities, ..
        } = &*def.effect
        else {
            panic!("expected GenericEffect, got {:?}", def.effect);
        };
        assert_eq!(
            static_abilities.len(),
            5,
            "one conditional grant per color, got {static_abilities:?}"
        );

        // Each color must be gated on its matching basic land type.
        let expected = [
            (ManaColor::White, "Plains"),
            (ManaColor::Blue, "Island"),
            (ManaColor::Black, "Swamp"),
            (ManaColor::Red, "Mountain"),
            (ManaColor::Green, "Forest"),
        ];
        for (color, land) in expected {
            let found = static_abilities.iter().any(|sd| {
                let grants_color = sd
                    .modifications
                    .contains(&ContinuousModification::AddKeyword {
                        keyword: Keyword::Protection(ProtectionTarget::Color(color)),
                    });
                let gated_on_land = matches!(
                    &sd.condition,
                    Some(StaticCondition::IsPresent { filter })
                        if format!("{filter:?}").contains(land)
                );
                grants_color && gated_on_land
            });
            assert!(
                found,
                "expected protection from {color:?} gated on controlling a {land}, \
                 got {static_abilities:?}"
            );
        }
    }

    // CR 613.1d: "~ isn't a <core type>" must lower to a one-shot continuous
    // effect that REMOVES the type (RemoveType modification on SelfRef). Building
    // block for Blink's Alien Angel token ("this token isn't a creature until end
    // of turn"); exercised here on the normalized self-ref form.
    #[test]
    fn self_ref_isnt_a_creature_removes_type() {
        let effect = super::super::parse_effect("~ isn't a creature");
        let Effect::GenericEffect {
            static_abilities, ..
        } = effect
        else {
            panic!("expected GenericEffect, got {effect:?}");
        };
        assert!(
            static_abilities[0].modifications.iter().any(|m| matches!(
                m,
                ContinuousModification::RemoveType {
                    core_type: crate::types::card_type::CoreType::Creature
                }
            )),
            "expected RemoveType(Creature), got {:?}",
            static_abilities[0].modifications
        );
    }

    // CR 120.1 + CR 608.2c: "each <object class> [you control] deals N damage to
    // <recipient>" parses to `EachSourceDealsDamage` (per-source iteration), NOT a
    // single ability-sourced `DealDamage`. Building-block tests, one per cluster
    // recipient shape, fed the full clause text via `parse_effect`.
    #[test]
    fn each_source_deals_damage_parent_target_recipient() {
        // Missy branch[0] / Case of the Gateway Express: "that <recipient>" bound to
        // a parent target → Shared(ParentTarget).
        let effect = super::super::parse_effect(
            "each artifact creature you control deals 1 damage to that opponent",
        );
        let Effect::EachSourceDealsDamage {
            sources,
            amount,
            recipient,
        } = effect
        else {
            panic!("expected EachSourceDealsDamage, got {effect:?}");
        };
        assert!(matches!(sources, TargetFilter::Typed(_)), "got {sources:?}");
        assert_eq!(amount, QuantityExpr::Fixed { value: 1 });
        assert_eq!(
            recipient,
            EachDamageRecipient::Shared(TargetFilter::ParentTarget)
        );
    }

    #[test]
    fn each_source_deals_damage_triggering_source_recipient() {
        // Sarkhan the Masterless: "that creature" = the attacker that triggered the
        // ability → Shared(TriggeringSource).
        let effect =
            super::super::parse_effect("each Dragon you control deals 1 damage to that creature");
        let Effect::EachSourceDealsDamage { recipient, .. } = effect else {
            panic!("expected EachSourceDealsDamage, got {effect:?}");
        };
        assert_eq!(
            recipient,
            EachDamageRecipient::Shared(TargetFilter::TriggeringSource)
        );
    }

    #[test]
    fn each_source_deals_damage_any_target_recipient() {
        // Princess Snowfall: "any target" → Shared(Any).
        let effect =
            super::super::parse_effect("each Dwarf you control deals 1 damage to any target");
        let Effect::EachSourceDealsDamage { recipient, .. } = effect else {
            panic!("expected EachSourceDealsDamage, got {effect:?}");
        };
        assert_eq!(recipient, EachDamageRecipient::Shared(TargetFilter::Any));
    }

    #[test]
    fn each_source_deals_damage_each_controller_recipient() {
        // Rakdos Charm mode 3: "its controller" → EachController (per-source).
        let effect = super::super::parse_effect("each creature deals 1 damage to its controller");
        let Effect::EachSourceDealsDamage {
            sources,
            amount,
            recipient,
        } = effect
        else {
            panic!("expected EachSourceDealsDamage, got {effect:?}");
        };
        assert!(matches!(sources, TargetFilter::Typed(_)), "got {sources:?}");
        assert_eq!(amount, QuantityExpr::Fixed { value: 1 });
        assert_eq!(recipient, EachDamageRecipient::EachController);
    }

    #[test]
    fn each_enchantment_deals_to_its_controller_each_controller() {
        // Aura Barbs clause 1, exact text → EachController with amount 2.
        let effect =
            super::super::parse_effect("each enchantment deals 2 damage to its controller");
        let Effect::EachSourceDealsDamage {
            amount, recipient, ..
        } = effect
        else {
            panic!("expected EachSourceDealsDamage, got {effect:?}");
        };
        assert_eq!(amount, QuantityExpr::Fixed { value: 2 });
        assert_eq!(recipient, EachDamageRecipient::EachController);
    }

    // CR 303.4 (DEFERRED §9): the per-attachment host recipient is not yet modeled —
    // the clause must fail CLOSED to `Unimplemented` (replacing today's live
    // `DealDamage{Fixed 2, ParentTarget}` misparse). Asserts the §6 step-1 reorder
    // fires BEFORE the subject-parse requirement (Aura Barbs clause 2).
    #[test]
    fn each_aura_attached_damage_defers_to_unimplemented() {
        let effect = super::super::parse_effect(
            "each Aura attached to a creature deals 2 damage to the creature it's attached to",
        );
        assert!(
            matches!(effect, Effect::Unimplemented { .. }),
            "expected Unimplemented, got {effect:?}"
        );
    }

    // Negative: a player-shaped subject must NOT be captured — it falls through so
    // "each player ..." keeps its existing per-player handling.
    #[test]
    fn each_player_subject_is_not_each_source_deals_damage() {
        let effect = super::super::parse_effect("each player draws a card");
        assert!(
            !matches!(effect, Effect::EachSourceDealsDamage { .. }),
            "player subject wrongly captured: {effect:?}"
        );
    }

    // Negative: a self-source broadcast ("~ deals N damage to each creature",
    // DamageAll) is NOT a per-source class — it must fall through.
    #[test]
    fn self_source_damage_each_creature_is_not_each_source_deals_damage() {
        let effect = super::super::parse_effect("~ deals 1 damage to each creature");
        assert!(
            !matches!(effect, Effect::EachSourceDealsDamage { .. }),
            "self-source broadcast wrongly captured: {effect:?}"
        );
    }

    // Negative: a SINGULAR anaphoric source ("that creature" = TriggeringSource,
    // Flametongue Kavu Avatar) denotes ONE source, not a class — must NOT be
    // captured (it would wrongly broadcast across all creatures).
    #[test]
    fn singular_that_creature_source_is_not_each_source_deals_damage() {
        let effect = super::super::parse_effect("that creature deals 1 damage to target creature");
        assert!(
            !matches!(effect, Effect::EachSourceDealsDamage { .. }),
            "singular anaphoric source wrongly captured: {effect:?}"
        );
    }

    // Negative: an Aura's "enchanted creature deals 1 damage to its owner" (Enslave)
    // is a single AttachedTo source — must NOT be captured as a class.
    #[test]
    fn enchanted_creature_source_is_not_each_source_deals_damage() {
        let effect = super::super::parse_effect("enchanted creature deals 1 damage to its owner");
        assert!(
            !matches!(effect, Effect::EachSourceDealsDamage { .. }),
            "enchanted-creature source wrongly captured: {effect:?}"
        );
    }

    // Negative: damage nested inside a GRANTED ability ("each creature you control
    // gains '{T}: This creature deals 1 damage to target player...'", Stensia) is
    // not a direct per-source damage — the predicate's main verb is "gains", so it
    // must NOT be captured.
    #[test]
    fn granted_ability_damage_is_not_each_source_deals_damage() {
        let effect = super::super::parse_effect(
            "each creature you control gains \"{T}: This creature deals 1 damage to target player or planeswalker\" until end of turn",
        );
        assert!(
            !matches!(effect, Effect::EachSourceDealsDamage { .. }),
            "granted-ability damage wrongly captured: {effect:?}"
        );
    }

    // Negative: a per-source DYNAMIC amount ("each creature you control deals damage
    // equal to its power") is the deferred filter-source own-power class — the
    // single uniform resolve would be wrong, so it must NOT be captured.
    #[test]
    fn each_source_own_power_amount_is_not_each_source_deals_damage() {
        let effect = super::super::parse_effect(
            "each creature you control deals damage equal to its power to any target",
        );
        assert!(
            !matches!(effect, Effect::EachSourceDealsDamage { .. }),
            "per-source dynamic amount wrongly captured: {effect:?}"
        );
    }

    // Negative: the targeted own-power team-up shape still routes to
    // `EachDealsDamageEqualToPower`, never `EachSourceDealsDamage`.
    #[test]
    fn each_power_team_up_is_not_each_source_deals_damage() {
        let effect = super::super::parse_effect(
            "up to two target creatures you control each deal damage equal to their power to another target creature",
        );
        assert!(
            !matches!(effect, Effect::EachSourceDealsDamage { .. }),
            "own-power team-up wrongly captured: {effect:?}"
        );
    }

    // CR 201.4: a "named X" effect-assigned name terminates at the first
    // conjunction — "becomes … named Fenric and loses all abilities" yields
    // name "Fenric", not "Fenric and loses all abilities". The residual "loses
    // all abilities" is recovered independently as RemoveAllAbilities. Building
    // block for The Curse of Fenric II.
    #[test]
    fn become_named_terminates_at_conjunction() {
        let (before, name) = strip_become_name_override(
            "becomes a 6/6 Horror creature named Fenric and loses all abilities",
        );
        assert_eq!(name.as_deref(), Some("Fenric"));
        assert_eq!(
            before, "becomes a 6/6 Horror creature",
            "the ' named ...' clause must be removed from the residual"
        );
    }

    // Regression: a plain "named X" with no trailing conjunction still captures
    // the full name token.
    #[test]
    fn become_named_plain_captures_full_name() {
        let (_, name) = strip_become_name_override("becomes a creature named Serra Angel");
        assert_eq!(name.as_deref(), Some("Serra Angel"));
    }

    /// CR 608.2c: the additive-"also" strip is a building block — it removes the
    /// trailing connector for any filter subject, is case-insensitive, leaves
    /// non-additive subjects untouched, and refuses to strip a bare "also" that
    /// would leave no filter.
    #[test]
    fn strip_trailing_additive_adverb_building_block() {
        // Trailing additive "also" is stripped, original case preserved.
        assert_eq!(
            strip_trailing_additive_adverb("Kithkin creatures you control also"),
            "Kithkin creatures you control"
        );
        // Case-insensitive on the connector.
        assert_eq!(
            strip_trailing_additive_adverb("Goblins you control ALSO"),
            "Goblins you control"
        );
        // No trailing "also" → unchanged.
        assert_eq!(
            strip_trailing_additive_adverb("creatures you control"),
            "creatures you control"
        );
        // A non-terminal "also" is not a trailing connector → unchanged.
        assert_eq!(
            strip_trailing_additive_adverb("also creatures you control"),
            "also creatures you control"
        );
        // Bare "also" has no filter to grant against → not stripped to empty.
        assert_eq!(strip_trailing_additive_adverb("also"), "also");
    }

    /// CR 509.1c (issue #4233): "Each creature your opponents control blocks this
    /// turn if able" (Predatory Rampage) is a non-targeted mass requirement — it
    /// must lower to a `ForceBlock` whose `target` filter selects every opponent
    /// creature, with NO target prompt (so it does not ask the caster to pick one
    /// creature). The resolver then expands the filter at resolution.
    #[test]
    fn each_opponent_creature_blocks_if_able_is_non_targeted_mass_force_block() {
        let def = crate::parser::oracle_effect::parse_effect_chain(
            "Each creature your opponents control blocks this turn if able.",
            AbilityKind::Spell,
        );
        assert!(
            def.target_prompt.is_none(),
            "mass force-block must not request a target, got {:?}",
            def.target_prompt
        );
        let Effect::ForceBlock { target } = &*def.effect else {
            panic!("expected ForceBlock, got {:?}", def.effect);
        };
        let TargetFilter::Typed(filter) = target else {
            panic!("expected a typed mass filter, got {target:?}");
        };
        assert_eq!(filter.controller, Some(ControllerRef::Opponent));
        assert!(filter.type_filters.contains(&TypeFilter::Creature));
    }

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

    /// CR 707.2 + CR 611.2a: Shifting Woodland's Delirium activated ability —
    /// "becomes a copy of target permanent card in your graveyard until end of
    /// turn" must extract `UntilEndOfTurn`, not default to `Permanent`.
    #[test]
    fn parse_effect_chain_ir_woodland_become_copy() {
        let mut ctx = ParseContext {
            card_name: Some("Shifting Woodland".to_string()),
            ..Default::default()
        };
        let ir = crate::parser::oracle_effect::parse_effect_chain_ir(
            "This land becomes a copy of target permanent card in your graveyard until end of turn.",
            AbilityKind::Activated,
            &mut ctx,
        );
        let def = crate::parser::oracle_effect::lower_effect_chain_ir(&ir);
        match &*def.effect {
            Effect::BecomeCopy { duration, .. } => {
                assert_eq!(
                    duration,
                    &Some(crate::types::ability::Duration::UntilEndOfTurn),
                    "effect-chain IR must preserve until-end-of-turn duration"
                );
            }
            other => panic!("expected BecomeCopy, got {other:?}"),
        }
    }

    #[test]
    fn shifting_woodland_become_copy_until_end_of_turn() {
        let mut ctx = ParseContext {
            card_name: Some("Shifting Woodland".to_string()),
            ..Default::default()
        };
        let ability = crate::parser::oracle_effect::parse_effect_chain_with_context(
            "This land becomes a copy of target permanent card in your graveyard until end of turn.",
            AbilityKind::Activated,
            &mut ctx,
        );
        match &*ability.effect {
            Effect::BecomeCopy { duration, .. } => {
                assert_eq!(
                    duration,
                    &Some(crate::types::ability::Duration::UntilEndOfTurn),
                    "graveyard-target copy must expire at end of turn"
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
    fn each_players_life_total_becomes_n_targets_all_players() {
        // CR 119.5 + issue #2882: Worldfire — "Each player's life total becomes 1"
        // must lower to an all-players (non-targeted) SetLifeTotal, not `Any`
        // (which prompts the controller to pick one player).
        // Worldfire's exact wording.
        let text = "Each player's life total becomes 1.";
        let ability = crate::parser::oracle_effect::parse_effect_chain(text, AbilityKind::Spell);
        let Effect::SetLifeTotal { target, amount } = &*ability.effect else {
            panic!(
                "expected SetLifeTotal for {text:?}, got {:?}",
                ability.effect
            );
        };
        assert_eq!(
            target,
            &TargetFilter::AllPlayers,
            "expected AllPlayers target, got {target:?}"
        );
        assert_eq!(amount, &QuantityExpr::Fixed { value: 1 });
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
    fn combat_tax_effect_clause_yojimbo_chapter() {
        use crate::types::ability::{ContinuousModification, StaticCondition};
        use crate::types::statics::StaticMode;

        let mut ctx = ParseContext::default();
        let clause = try_parse_subject_restriction_clause(
            "Creatures can't attack you unless their controller pays {2} for each of those creatures.",
            &mut ctx,
        )
        .expect("Yojimbo chapter combat tax should parse");

        let Effect::GenericEffect {
            static_abilities,
            target,
            ..
        } = clause.effect
        else {
            panic!("expected GenericEffect combat tax grant");
        };
        assert_eq!(target, Some(TargetFilter::SelfRef));
        assert_eq!(static_abilities.len(), 1);
        let mods = &static_abilities[0].modifications;
        let ContinuousModification::GrantStaticAbility { definition } = &mods[0] else {
            panic!("combat tax effect must grant static onto source");
        };
        assert!(matches!(definition.mode, StaticMode::CantAttack));
        assert!(matches!(
            definition.condition,
            Some(StaticCondition::UnlessPay { .. })
        ));
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

    /// CR 702.26a + CR 101.2 + CR 611.2b: "It can't phase in for as long as ~
    /// remains tapped" (The Pandorica) lowers to a `CantPhaseIn` continuous
    /// restriction — NOT an `Effect::PhaseIn` (locks the dispatch-priority
    /// finding: the ` can't ` subject split wins before imperative "phase in").
    /// The `ForAsLongAs { SourceIsTapped }` duration is preserved and the
    /// restriction propagates via `AddStaticMode` so it registers as a
    /// `SpecificObject` transient grant on the parent target.
    #[test]
    fn cant_phase_in_builds_restriction_not_phase_in() {
        // Mark a prior typed object referent so the "It" anaphor binds to the
        // parent target, mirroring the activated-ability chain.
        let mut ctx = ParseContext {
            parent_target_available: true,
            ..ParseContext::default()
        };
        let clause = try_parse_subject_restriction_clause(
            "It can't phase in for as long as ~ remains tapped",
            &mut ctx,
        )
        .expect("phase-in restriction should parse");

        let Effect::GenericEffect {
            static_abilities,
            duration,
            ..
        } = &clause.effect
        else {
            panic!(
                "expected GenericEffect restriction, not PhaseIn, got {:?}",
                clause.effect
            );
        };
        assert!(
            !matches!(clause.effect, Effect::PhaseIn { .. }),
            "must not be an Effect::PhaseIn"
        );
        assert_eq!(static_abilities.len(), 1);
        assert_eq!(static_abilities[0].mode, StaticMode::CantPhaseIn);
        assert_eq!(
            duration,
            &Some(Duration::ForAsLongAs {
                condition: crate::types::ability::StaticCondition::SourceIsTapped,
            })
        );
        assert!(static_abilities[0].modifications.iter().any(|m| matches!(
            m,
            ContinuousModification::AddStaticMode {
                mode: StaticMode::CantPhaseIn
            }
        )));
    }

    /// CR 702.26a + CR 101.2: the restriction grammar maps the "phase in" atom
    /// to `CantPhaseIn` for any subject, proving the building block covers the
    /// class (not one card). The negation/duration are owned by the caller.
    #[test]
    fn parse_restriction_modes_phase_in_atom() {
        assert_eq!(
            parse_restriction_modes("can't phase in"),
            Some(vec![StaticMode::CantPhaseIn])
        );
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

    /// CR 119.7 + CR 608.2c + CR 104.1: Screaming Nemesis's rider. The
    /// anaphoric head ("If a player is dealt damage this way, they") binds the
    /// `can't gain life for the rest of the game` restriction to the redirect's
    /// parent target via `ParentTarget` (so it no-ops for non-player targets),
    /// with permanent duration and the `AddStaticMode` grant propagation that
    /// the runtime `player_has_cant_gain_life` query relies on.
    #[test]
    fn dealt_damage_this_way_player_cant_gain_life_builds_permanent_restriction() {
        let mut ctx = ParseContext::default();
        let clause = try_parse_subject_restriction_clause(
            "If a player is dealt damage this way, they can't gain life for the rest of the game",
            &mut ctx,
        )
        .expect("dealt-damage-this-way life-lock rider should parse");

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

        // No new target slot: the rider reuses the redirect's target anaphorically.
        assert_eq!(target, None);
        // CR 104.1: "for the rest of the game" -> Permanent.
        assert_eq!(duration, Some(Duration::Permanent));
        assert_eq!(static_abilities.len(), 1);
        let def = &static_abilities[0];
        assert_eq!(def.mode, StaticMode::CantGainLife);
        // CR 119.7 player-gating: ParentTarget binds Player->SpecificPlayer and
        // Object->SpecificObject at resolution, so a creature/planeswalker hit
        // never locks a player.
        assert_eq!(def.affected, Some(TargetFilter::ParentTarget));
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

    // CR 108.3 + CR 608.2c: "its owner"/"<noun>'s owner" life-change subject must
    // route to the OBJECT'S OWNER (ParentTargetOwner), NOT the spell controller
    // (ParentTargetController). Issue #3351 — Misfortune's Gain / Path of Peace /
    // Thieving Amalgam / The Matrix of Time.
    #[test]
    fn parse_subject_its_owner_bare_routes_to_owner() {
        let mut ctx = ParseContext::default();
        let app =
            parse_subject_application("its owner", &mut ctx).expect("should recognize 'its owner'");
        assert_eq!(app.affected, TargetFilter::ParentTargetOwner);
        assert!(!app.is_optional, "no 'may' modal → not optional");
    }

    #[test]
    fn parse_subject_their_owner_bare_routes_to_owner() {
        let mut ctx = ParseContext::default();
        let app = parse_subject_application("their owner", &mut ctx)
            .expect("should recognize 'their owner'");
        assert_eq!(app.affected, TargetFilter::ParentTargetOwner);
        assert!(!app.is_optional);
    }

    #[test]
    fn parse_subject_its_owner_may_is_optional() {
        let mut ctx = ParseContext::default();
        let app = parse_subject_application("its owner may", &mut ctx)
            .expect("should recognize 'its owner may'");
        assert_eq!(app.affected, TargetFilter::ParentTargetOwner);
        assert!(
            app.is_optional,
            "'may' modal must mark the subject optional"
        );
    }

    #[test]
    fn parse_subject_that_card_owner_routes_to_owner() {
        // The Matrix of Time: "that card's owner loses 3 life" — the det-suffix
        // owner arm must route to ParentTargetOwner, not ParentTargetController.
        let mut ctx = ParseContext::default();
        let app = parse_subject_application("that card's owner", &mut ctx)
            .expect("should recognize \"that card's owner\"");
        assert_eq!(app.affected, TargetFilter::ParentTargetOwner);
        assert!(!app.is_optional);
    }

    #[test]
    fn parse_subject_that_noun_controller_still_routes_to_controller() {
        // No-regression: the controller det-suffix arm is unchanged by the owner
        // split (literals are mutually exclusive).
        let mut ctx = ParseContext::default();
        let app = parse_subject_application("that creature's controller", &mut ctx)
            .expect("should recognize \"that creature's controller\"");
        assert_eq!(app.affected, TargetFilter::ParentTargetController);
    }

    #[test]
    fn targeted_owner_gains_fixed_life_routes_to_owner() {
        // Misfortune's Gain / Path of Peace: "Its owner gains 4 life." The
        // GainLife player slot must be ParentTargetOwner, not the default
        // Controller. Reverting the fix makes this ParentTargetController.
        let clause = try_parse_targeted_controller_gain_life("Its owner gains 4 life.")
            .expect("targeted owner fixed gain life clause");

        assert!(matches!(
            clause.effect,
            Effect::GainLife {
                amount: QuantityExpr::Fixed { value: 4 },
                player: TargetFilter::ParentTargetOwner
            }
        ));
    }

    #[test]
    fn targeted_owner_gains_life_that_noun_phrasing_routes_to_owner() {
        // "That creature's owner gains life equal to its power." — det-noun owner
        // combinator (parse_det_noun_owner) yields ParentTargetOwner.
        let clause = try_parse_targeted_controller_gain_life(
            "That creature's owner gains life equal to its power.",
        )
        .expect("'that noun's owner' phrasing should route to ParentTargetOwner");

        assert!(matches!(
            clause.effect,
            Effect::GainLife {
                amount: QuantityExpr::Ref {
                    qty: QuantityRef::Power {
                        scope: crate::types::ability::ObjectScope::Target
                    }
                },
                player: TargetFilter::ParentTargetOwner
            }
        ));
    }

    #[test]
    fn targeted_controller_gains_fixed_life_still_routes_to_controller_after_owner_split() {
        // No-regression: the controller arm of the same alt is unaffected.
        let clause = try_parse_targeted_controller_gain_life("Its controller gains 4 life.")
            .expect("controller gain life still parses");
        assert!(matches!(
            clause.effect,
            Effect::GainLife {
                amount: QuantityExpr::Fixed { value: 4 },
                player: TargetFilter::ParentTargetController
            }
        ));
    }

    #[test]
    fn targeted_controller_gains_life_that_noun_phrasing() {
        // Solitude: "That creature's controller gains life equal to its power."
        let clause = try_parse_targeted_controller_gain_life(
            "That creature's controller gains life equal to its power.",
        )
        .expect("'that noun's controller' phrasing should route to ParentTargetController");

        assert!(matches!(
            clause.effect,
            Effect::GainLife {
                amount: QuantityExpr::Ref {
                    qty: QuantityRef::Power {
                        scope: crate::types::ability::ObjectScope::Target
                    }
                },
                player: TargetFilter::ParentTargetController
            }
        ));
    }

    #[test]
    fn targeted_controller_gains_life_the_noun_phrasing() {
        // "The permanent's controller gains life equal to its toughness."
        let clause = try_parse_targeted_controller_gain_life(
            "The permanent's controller gains life equal to its toughness.",
        )
        .expect("'the noun's controller' phrasing should route to ParentTargetController");

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
    fn parse_subject_that_attacking_player_trigger_context_is_triggering_player() {
        // Issue #1325: "that attacking player" is synonymous with the attack
        // event's declaring player (CR 506.2 + CR 603.7c).
        let mut ctx = ParseContext {
            subject: Some(TargetFilter::Player),
            relative_player_scope: Some(ControllerRef::DefendingPlayer),
            card_name: Some("Ellie, Brick Master".to_string()),
            ..ParseContext::default()
        };
        let result = parse_subject_application("that attacking player", &mut ctx);
        assert!(result.is_some());
        assert_eq!(
            result.unwrap().affected,
            TargetFilter::TriggeringPlayer,
            "that attacking player must bind to TriggeringPlayer in trigger context"
        );
    }

    #[test]
    fn parse_subject_predicate_that_attacking_player_creates_token() {
        use crate::parser::oracle_effect::parse_effect_clause;
        use crate::types::ability::Effect;

        let mut ctx = ParseContext {
            subject: Some(TargetFilter::Player),
            relative_player_scope: Some(ControllerRef::DefendingPlayer),
            card_name: Some("Ellie, Brick Master".to_string()),
            ..ParseContext::default()
        };
        let clause = parse_effect_clause(
            "that attacking player creates a tapped 1/1 black Fungus Zombie creature token named Cordyceps Infected that's attacking that opponent",
            &mut ctx,
        );
        let Effect::Token {
            owner,
            name,
            tapped,
            enters_attacking,
            ..
        } = &clause.effect
        else {
            panic!("expected Token effect, got {:?}", clause.effect);
        };
        assert_eq!(*owner, TargetFilter::TriggeringPlayer);
        assert_eq!(name, "Cordyceps Infected");
        assert!(*tapped);
        assert!(*enters_attacking);
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

    #[test]
    fn any_number_of_other_target_produces_multi_target() {
        // CR 115.1d: Guardian of Faith — "any number of other target creatures"
        // must produce multi_target and a filter with FilterProp::Another.
        let mut ctx = ParseContext::default();
        let app =
            parse_subject_application("any number of other target creatures you control", &mut ctx)
                .expect("should parse");
        assert!(app.multi_target.is_some(), "multi_target must be set");
        assert!(app.target.is_some(), "must be a targeted form");
        if let Some(TargetFilter::Typed(ref tf)) = app.target {
            assert!(
                tf.properties
                    .iter()
                    .any(|p| matches!(p, FilterProp::Another)),
                "filter must have FilterProp::Another for 'other'"
            );
        }
    }

    #[test]
    fn any_number_of_target_without_other_still_works() {
        // Regression: "any number of target creatures" (no "other") still parses.
        let mut ctx = ParseContext::default();
        let app = parse_subject_application("any number of target creatures", &mut ctx)
            .expect("should parse");
        assert!(app.multi_target.is_some(), "multi_target must be set");
    }

    // CR 115.1 + CR 115.1d: "one or more target X" variable-count subject tests.
    // The minimum is 1 (unlike "any number of", min 0); the maximum is unbounded.
    #[test]
    fn parse_subject_one_or_more_target_creatures() {
        let mut ctx = ParseContext::default();
        let result = parse_subject_application("one or more target creatures", &mut ctx);
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
            Some(MultiTargetSpec::unlimited(1)),
            "should have unlimited multi_target with min 1"
        );
    }

    #[test]
    fn parse_subject_one_or_more_target_creatures_you_control() {
        let mut ctx = ParseContext::default();
        let result =
            parse_subject_application("one or more target creatures you control", &mut ctx);
        assert!(result.is_some());
        let app = result.unwrap();
        assert!(
            matches!(app.affected, TargetFilter::Typed(ref t)
                if t.type_filters.contains(&TypeFilter::Creature)
                && t.controller == Some(ControllerRef::You)),
            "should parse creature + controller, got {:?}",
            app.affected
        );
        assert_eq!(app.multi_target, Some(MultiTargetSpec::unlimited(1)));
    }

    #[test]
    fn starts_with_subject_prefix_one_or_more() {
        assert!(starts_with_subject_prefix(
            "one or more target creatures become red until end of turn"
        ));
    }

    // CR 105.1 + CR 115.1 + CR 613.1e: end-to-end — the "one or more target
    // creatures become <color>" class (Dwarven Song / Heaven's Gate / Sea Kings'
    // Blessing / Sylvan Paradise / Touch of Darkness) parses to a multi-target
    // (min 1, unbounded) Layer-5 SetColor continuous modification, NOT
    // Effect::Unimplemented.
    #[test]
    fn one_or_more_target_become_color_parses_to_multi_target_setcolor() {
        use crate::types::mana::ManaColor;

        let cases = [
            ("Dwarven Song", "red", ManaColor::Red),
            ("Heaven's Gate", "white", ManaColor::White),
            ("Sea Kings' Blessing", "blue", ManaColor::Blue),
            ("Sylvan Paradise", "green", ManaColor::Green),
            ("Touch of Darkness", "black", ManaColor::Black),
        ];

        // All five cards are Sorceries; the priority-10 imperative effect-chain
        // path is gated on the Instant/Sorcery card type in `parse_oracle_ir`.
        let types = vec!["Sorcery".to_string()];
        for (card_name, color_word, color) in cases {
            let text =
                format!("One or more target creatures become {color_word} until end of turn.");
            let parsed =
                crate::parser::oracle::parse_oracle_text(&text, card_name, &[], &types, &[]);
            let ability = parsed
                .abilities
                .iter()
                .find(|a| {
                    matches!(
                        &*a.effect,
                        Effect::GenericEffect { static_abilities, .. }
                            if static_abilities.iter().any(|s| s
                                .modifications
                                .contains(&ContinuousModification::SetColor {
                                    colors: vec![color],
                                }))
                    )
                })
                .unwrap_or_else(|| {
                    panic!("{card_name}: expected SetColor GenericEffect, got {parsed:?}")
                });
            assert!(
                !matches!(&*ability.effect, Effect::Unimplemented { .. }),
                "{card_name}: must not be Unimplemented"
            );
            assert_eq!(
                ability.multi_target,
                Some(MultiTargetSpec::unlimited(1)),
                "{card_name}: must carry unbounded min-1 multi-target"
            );
        }
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
    fn parse_restriction_modes_tolerates_trailing_period() {
        // A static line's terminal period can reach `parse_restriction_modes`
        // (e.g. via `try_parse_subject_restriction_clause`, whose predicate keeps
        // the period when no trailing duration strips it). The compound atom-list
        // grammar must tolerate it, matching the dedicated `can't be regenerated`
        // arm which already does.
        assert_eq!(
            parse_restriction_modes("can't attack or block."),
            Some(vec![StaticMode::CantAttack, StaticMode::CantBlock])
        );
    }

    #[test]
    fn cant_attack_or_block_with_trailing_period_builds_both_modes() {
        let mut ctx = ParseContext::default();
        let clause = try_parse_subject_restriction_clause(
            "Creatures you control can't attack or block.",
            &mut ctx,
        )
        .expect("compound restriction with a trailing period should parse");
        let Effect::GenericEffect {
            static_abilities, ..
        } = clause.effect
        else {
            panic!(
                "expected GenericEffect restriction, got {:?}",
                clause.effect
            );
        };
        let modes: Vec<_> = static_abilities.iter().map(|s| s.mode.clone()).collect();
        assert_eq!(modes, vec![StaticMode::CantAttack, StaticMode::CantBlock]);
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

    /// Yare — "That creature can block up to two additional creatures this turn."
    /// The optional "up to" prefix must not swallow the extra-block grant.
    #[test]
    fn yare_spell_extra_blockers_up_to_two_this_turn() {
        let def = crate::parser::oracle_effect::parse_effect_chain(
            "Target creature defending player controls gets +3/+0 until end of turn. That creature can block up to two additional creatures this turn.",
            AbilityKind::Spell,
        );
        let sub = def
            .sub_ability
            .as_ref()
            .expect("Yare extra-block clause must be a sub-ability");
        assert!(
            !matches!(*sub.effect, Effect::Unimplemented { .. }),
            "Yare extra-block clause must parse, got {:?}",
            sub.effect
        );
        let Effect::GenericEffect {
            static_abilities,
            duration,
            ..
        } = &*sub.effect
        else {
            panic!("expected GenericEffect, got {:?}", sub.effect);
        };
        assert_eq!(duration, &Some(Duration::UntilEndOfTurn));
        assert_eq!(
            static_abilities[0].mode,
            StaticMode::ExtraBlockers { count: Some(2) }
        );
    }

    /// CR 509.1b + CR 611.2: A granted "can't be blocked [this turn] except by
    /// <filter>" clause (Fast // Furious's second sentence) must lower to a real
    /// `CantBeBlockedExceptBy` evasion static on the anaphoric "It" subject —
    /// previously the whole clause fell through to `Effect::Unimplemented`,
    /// flipping the card unsupported and inflating the swallowed-clause gate.
    ///
    /// Asserts the building-block shape: the granted static carries
    /// `CantBeBlockedExceptBy { kind: Quality(<filter>) }` propagated via
    /// `AddStaticMode`, the duration is `UntilEndOfTurn` (the mid-predicate "this
    /// turn"), and the quality filter is an `Or` whose disjuncts cover the Vehicle
    /// subtype and a has-haste creature.
    #[test]
    fn granted_cant_be_blocked_except_by_filter_is_supported() {
        use crate::parser::oracle_effect::parse_effect_chain;

        let def = parse_effect_chain(
            "Target creature gains haste until end of turn. It can't be blocked this turn except by Vehicles or by creatures with haste.",
            AbilityKind::Spell,
        );

        let sub = def
            .sub_ability
            .expect("the can't-be-blocked clause must be a supported sub-ability");
        assert!(
            !matches!(*sub.effect, Effect::Unimplemented { .. }),
            "the evasion clause must not be swallowed as Unimplemented, got {:?}",
            sub.effect
        );

        let Effect::GenericEffect {
            static_abilities,
            duration,
            target,
        } = &*sub.effect
        else {
            panic!("expected GenericEffect, got {:?}", sub.effect);
        };

        // The anaphoric "It" binds to the previously-targeted creature.
        assert_eq!(*target, Some(TargetFilter::ParentTarget));
        // CR 611.2: the mid-predicate "this turn" sets the granted duration.
        assert_eq!(*duration, Some(Duration::UntilEndOfTurn));

        let def = static_abilities
            .iter()
            .find_map(|sd| match &sd.modifications[..] {
                [ContinuousModification::AddStaticMode {
                    mode: StaticMode::CantBeBlockedExceptBy { kind },
                }] => Some(kind.clone()),
                _ => None,
            })
            .expect("granted static must carry AddStaticMode(CantBeBlockedExceptBy)");

        let BlockExceptionKind::Quality(filter) = def else {
            panic!("expected a quality block-exception filter, got {def:?}");
        };
        let TargetFilter::Or { filters } = filter else {
            panic!("expected an Or of Vehicle/has-haste disjuncts, got {filter:?}");
        };
        // The union must cover the Vehicle subtype and a has-haste creature; the
        // repeated "by" ("Vehicles or by creatures with haste") must not truncate
        // the second disjunct.
        assert!(
            filters.iter().any(|f| matches!(
                f,
                TargetFilter::Typed(t)
                    if t.type_filters.contains(&TypeFilter::Subtype("Vehicle".into()))
            )),
            "filter union must include the Vehicle subtype, got {filters:?}"
        );
        assert!(
            filters.iter().any(|f| matches!(
                f,
                TargetFilter::Typed(t)
                    if t.properties.contains(&FilterProp::WithKeyword { value: Keyword::Haste })
            )),
            "filter union must include a has-haste creature, got {filters:?}"
        );
    }

    /// CR 509.1b: `classify_block_exception` is the single authority for the
    /// "except by <filter>" grammar shared by the printed/static and granted
    /// evasion paths. The evasion wording repeats the "by" preposition before
    /// each disjunct ("Vehicles or by creatures with haste"); the redundant "by"
    /// must be stripped so the full union parses, not just its first disjunct.
    #[test]
    fn classify_block_exception_strips_redundant_by() {
        let kind = classify_block_exception("vehicles or by creatures with haste");
        let BlockExceptionKind::Quality(TargetFilter::Or { filters }) = kind else {
            panic!("expected a quality Or filter, got {kind:?}");
        };
        assert!(
            filters.iter().any(|f| matches!(
                f,
                TargetFilter::Typed(t)
                    if t.type_filters.contains(&TypeFilter::Subtype("Vehicle".into()))
            )),
            "first disjunct (Vehicle) missing: {filters:?}"
        );
        assert!(
            filters.iter().any(|f| matches!(
                f,
                TargetFilter::Typed(t)
                    if t.properties.contains(&FilterProp::WithKeyword { value: Keyword::Haste })
            )),
            "second disjunct (has-haste) dropped by repeated 'by': {filters:?}"
        );
    }

    /// CR 613.4b + CR 613.1f: the possessive base-P/T-set + keyword-grant clause
    /// builds a `GenericEffect` with `SetPower`/`SetToughness` and `AddKeyword`,
    /// across the trigger-body form ("~'s …, they gain …"), the singular pronoun
    /// ("it gains"), the multi-keyword conjunct, and the standalone form with a
    /// leading "Until end of turn," duration (which exercises
    /// `strip_leading_duration`).
    fn base_pt_set_mods(text: &str) -> (Vec<ContinuousModification>, Option<Duration>) {
        let mut ctx = ParseContext::default();
        let ast = try_parse_subject_base_pt_set_clause_ast(text, &mut ctx)
            .unwrap_or_else(|| panic!("clause did not parse: {text:?}"));
        let ClauseAst::SubjectPredicate { predicate, .. } = ast else {
            panic!("expected SubjectPredicate");
        };
        let PredicateAst::Continuous {
            effect, duration, ..
        } = *predicate
        else {
            panic!("expected Continuous predicate");
        };
        let Effect::GenericEffect {
            static_abilities, ..
        } = effect
        else {
            panic!("expected GenericEffect");
        };
        (static_abilities[0].modifications.clone(), duration)
    }

    #[test]
    fn base_pt_set_clause_trigger_body_form() {
        // Moon Girl's trigger-body form (leading duration already stripped
        // upstream). "they gain trample" plural pronoun.
        let (mods, duration) =
            base_pt_set_mods("~'s base power and toughness become 6/6 and they gain trample");
        assert!(mods.contains(&ContinuousModification::SetPower { value: 6 }));
        assert!(mods.contains(&ContinuousModification::SetToughness { value: 6 }));
        assert!(mods.contains(&ContinuousModification::AddKeyword {
            keyword: Keyword::Trample
        }));
        // No leading duration in the trigger-body form.
        assert_eq!(duration, None);
    }

    #[test]
    fn base_pt_set_clause_leading_duration_and_singular_pronoun() {
        // Standalone form: leading "Until end of turn," + singular "it gains".
        // The leading duration must be stripped and threaded onto the clause.
        let (mods, duration) = base_pt_set_mods(
            "Until end of turn, ~'s base power and toughness become 4/4 and it gains flying",
        );
        assert!(mods.contains(&ContinuousModification::SetPower { value: 4 }));
        assert!(mods.contains(&ContinuousModification::SetToughness { value: 4 }));
        assert!(mods.contains(&ContinuousModification::AddKeyword {
            keyword: Keyword::Flying
        }));
        assert_eq!(duration, Some(Duration::UntilEndOfTurn));
    }

    #[test]
    fn parse_split_base_pt_dynamic_values_smoke() {
        let (power, toughness, _) = parse_split_base_pt_dynamic_values(
            "twice that card's power and its base toughness becomes twice that card's toughness",
        )
        .expect("split dynamic values");
        assert!(matches!(power, QuantityExpr::Multiply { factor: 2, .. }));
        assert!(matches!(
            toughness,
            QuantityExpr::Multiply { factor: 2, .. }
        ));
    }

    #[test]
    fn base_pt_set_clause_split_dynamic_revealed_card_referent() {
        let (mods, duration) = base_pt_set_mods(
            "Until your next turn, this creature's base power becomes twice that card's power and its base toughness becomes twice that card's toughness",
        );
        assert!(
            mods.iter().any(|m| matches!(
                m,
                ContinuousModification::SetPowerDynamic { value }
                    if matches!(value, QuantityExpr::Multiply { factor: 2, .. })
            )),
            "expected SetPowerDynamic(Multiply x2), got {mods:?}"
        );
        assert!(
            mods.iter().any(|m| matches!(
                m,
                ContinuousModification::SetToughnessDynamic { value }
                    if matches!(value, QuantityExpr::Multiply { factor: 2, .. })
            )),
            "expected SetToughnessDynamic(Multiply x2), got {mods:?}"
        );
        assert!(matches!(
            duration,
            Some(Duration::UntilNextTurnOf {
                player: PlayerScope::Controller
            })
        ));
    }

    #[test]
    fn base_pt_set_clause_no_keyword_conjunct() {
        // Bare "become N/M" with no trailing keyword grant is still a valid
        // set-base-P/T clause.
        let (mods, _) = base_pt_set_mods("~'s base power and toughness become 2/3");
        assert!(mods.contains(&ContinuousModification::SetPower { value: 2 }));
        assert!(mods.contains(&ContinuousModification::SetToughness { value: 3 }));
        assert!(!mods
            .iter()
            .any(|m| matches!(m, ContinuousModification::AddKeyword { .. })));
    }

    // -----------------------------------------------------------------------
    // CR 208.1 + CR 613.4b: the transitive "change <subject>'s base power [and
    // toughness] to <value>" surface form. Same layer-7b set-base-P/T primitives
    // as the "become[s]" copula, reached through the "change … to" verb frame.
    // -----------------------------------------------------------------------

    #[test]
    fn change_base_power_to_target_power_single_axis() {
        // Riptide Mangler: "{1}{U}: Change ~'s base power to target creature's
        // power." Only the power axis is set; the value reads the object target.
        let (mods, _) = base_pt_set_mods("Change ~'s base power to target creature's power");
        assert_eq!(
            mods,
            vec![ContinuousModification::SetPowerDynamic {
                value: QuantityExpr::Ref {
                    qty: QuantityRef::Power {
                        scope: crate::types::ability::ObjectScope::Target,
                    },
                },
            }],
            "power-only change clause should emit exactly one SetPowerDynamic",
        );
    }

    #[test]
    fn change_base_power_to_offset_aggregate() {
        // Arni Brokenbrow: "you may change ~'s base power to 1 plus the greatest
        // power among other creatures you control …". Exercises the "you may
        // change" verb variant and an offset-aggregate value. The trailing
        // duration is stripped by the effect pipeline before this function runs,
        // so it is absent here.
        let (mods, _) = base_pt_set_mods(
            "you may change ~'s base power to 1 plus the greatest power among other creatures you control",
        );
        assert!(
            matches!(
                mods.as_slice(),
                [ContinuousModification::SetPowerDynamic {
                    value: QuantityExpr::Offset { offset: 1, .. }
                }]
            ),
            "expected a single SetPowerDynamic(Offset +1), got {mods:?}",
        );
    }

    #[test]
    fn change_base_pt_to_paired_referent_dual_axis() {
        // Shape Stealer / Eldrazi Mimic: "change ~'s base power and toughness to
        // that creature's power and toughness". The paired referent splits into a
        // per-axis SetPowerDynamic / SetToughnessDynamic reading the same object.
        let (mods, _) = base_pt_set_mods(
            "change ~'s base power and toughness to that creature's power and toughness",
        );
        assert!(
            mods.iter()
                .any(|m| matches!(m, ContinuousModification::SetPowerDynamic { .. })),
            "expected SetPowerDynamic, got {mods:?}",
        );
        assert!(
            mods.iter()
                .any(|m| matches!(m, ContinuousModification::SetToughnessDynamic { .. })),
            "expected SetToughnessDynamic, got {mods:?}",
        );
    }

    #[test]
    fn change_base_pt_to_fixed_value() {
        // Fixed "N/M" value under the transitive verb frame — the same building
        // block as "become N/M", reached via "change … to".
        let (mods, _) = base_pt_set_mods("change ~'s base power and toughness to 0/2");
        assert!(mods.contains(&ContinuousModification::SetPower { value: 0 }));
        assert!(mods.contains(&ContinuousModification::SetToughness { value: 2 }));
    }

    #[test]
    fn change_verb_frame_requires_base_pt_subject() {
        // Guard: the "change" verb must not swallow unrelated "change … to"
        // clauses that are not base-P/T sets (no "'s base power" subject).
        let mut ctx = ParseContext::default();
        assert!(
            try_parse_subject_base_pt_set_clause_ast(
                "change the target of target spell to another creature",
                &mut ctx,
            )
            .is_none(),
            "non-base-P/T change clause must not match",
        );
    }

    #[test]
    fn change_base_toughness_only_to_dynamic() {
        // CR 208.1: toughness-only axis (Wall of Tombstones) — "change ~'s base
        // toughness to <dynamic>" sets ONLY base toughness, leaving base power
        // untouched (symmetric with the power-only axis).
        let (mods, _) = base_pt_set_mods(
            "change ~'s base toughness to 1 plus the number of creature cards in your graveyard",
        );
        assert!(
            mods.iter()
                .any(|m| matches!(m, ContinuousModification::SetToughnessDynamic { .. })),
            "expected SetToughnessDynamic, got {mods:?}",
        );
        assert!(
            !mods.iter().any(|m| matches!(
                m,
                ContinuousModification::SetPower { .. }
                    | ContinuousModification::SetPowerDynamic { .. }
            )),
            "toughness-only clause must not touch base power, got {mods:?}",
        );
    }

    // -----------------------------------------------------------------------
    // CR 701.15a: copula-goaded clause tests
    // -----------------------------------------------------------------------

    #[test]
    fn copula_goaded_permanent_duration() {
        // Jon Irenicus: "it's goaded for the rest of the game"
        let mut ctx = ParseContext::default();
        let clause =
            try_parse_copula_goaded_clause("it's goaded for the rest of the game", &mut ctx)
                .expect("should parse");
        assert!(matches!(clause.effect, Effect::Goad { .. }));
        assert_eq!(clause.duration, Some(Duration::Permanent));
    }

    #[test]
    fn copula_goaded_no_duration() {
        // Bare "it's goaded" with no trailing duration.
        let mut ctx = ParseContext::default();
        let clause = try_parse_copula_goaded_clause("it's goaded", &mut ctx).expect("should parse");
        assert!(matches!(clause.effect, Effect::Goad { .. }));
        assert_eq!(clause.duration, None);
    }

    #[test]
    fn copula_goaded_non_contracted() {
        // Non-contracted form: "it is goaded for the rest of the game"
        let mut ctx = ParseContext::default();
        let clause =
            try_parse_copula_goaded_clause("it is goaded for the rest of the game", &mut ctx)
                .expect("should parse");
        assert!(matches!(clause.effect, Effect::Goad { .. }));
        assert_eq!(clause.duration, Some(Duration::Permanent));
    }

    #[test]
    fn copula_goaded_for_as_long_as() {
        // Vislor Turlough: "it's goaded for as long as they control it"
        let mut ctx = ParseContext::default();
        let clause =
            try_parse_copula_goaded_clause("it's goaded for as long as they control it", &mut ctx);
        assert!(clause.is_some(), "should parse for-as-long-as duration");
        let clause = clause.unwrap();
        assert!(matches!(clause.effect, Effect::Goad { .. }));
        // Duration should be a ForAsLongAs variant, not None.
        assert!(clause.duration.is_some());
    }

    #[test]
    fn copula_goaded_rejects_non_goaded() {
        // "it's attacking" should NOT match this parser.
        let mut ctx = ParseContext::default();
        assert!(try_parse_copula_goaded_clause("it's attacking", &mut ctx).is_none());
    }

    #[test]
    fn copula_goaded_declines_trailing_clause_after_duration() {
        // A further conjunct after the duration must not be silently dropped.
        // The duration parser stops at "for the rest of the game", leaving
        // "and draws a card" — this helper does not lower that conjunct, so it
        // declines rather than emitting a Goad that loses the trailing effect.
        let mut ctx = ParseContext::default();
        assert!(try_parse_copula_goaded_clause(
            "it's goaded for the rest of the game and draws a card",
            &mut ctx,
        )
        .is_none());
    }

    #[test]
    fn copula_goaded_declines_trailing_clause_without_duration() {
        // No duration, but trailing non-duration text — likewise declined so the
        // remainder is not discarded.
        let mut ctx = ParseContext::default();
        assert!(try_parse_copula_goaded_clause("it's goaded and draws a card", &mut ctx).is_none());
    }

    // CR 509.1h: "Target unblocked attacking creature becomes blocked." parses to
    // `Effect::BecomeBlocked` whose target is a Typed(creature) filter carrying
    // both FilterProp::Unblocked and FilterProp::Attacking. SHAPE test — runtime
    // semantics are covered by the cast-pipeline tests in
    // tests/dazzling_beauty_become_blocked.rs.
    #[test]
    fn become_blocked_parses_with_unblocked_attacking_target() {
        use crate::types::ability::{FilterProp, TargetFilter, TypeFilter, TypedFilter};

        let effect =
            super::super::parse_effect("Target unblocked attacking creature becomes blocked.");
        let Effect::BecomeBlocked { target } = &effect else {
            panic!("expected Effect::BecomeBlocked, got {effect:?}");
        };
        let TargetFilter::Typed(TypedFilter {
            type_filters,
            properties,
            ..
        }) = target
        else {
            panic!("expected a Typed creature target, got {target:?}");
        };
        assert!(
            type_filters
                .iter()
                .any(|t| matches!(t, TypeFilter::Creature)),
            "target must be a creature filter, got {type_filters:?}"
        );
        assert!(
            properties
                .iter()
                .any(|p| matches!(p, FilterProp::Unblocked)),
            "target must carry FilterProp::Unblocked (CR 509.1h), got {properties:?}"
        );
        assert!(
            properties
                .iter()
                .any(|p| matches!(p, FilterProp::Attacking { .. })),
            "target must carry FilterProp::Attacking, got {properties:?}"
        );
        // Reach-guard against a vacuous parse: the effect is the concrete
        // BecomeBlocked variant, not Unimplemented.
        assert!(
            !matches!(effect, Effect::Unimplemented { .. }),
            "must not fall through to Unimplemented"
        );
    }
}
