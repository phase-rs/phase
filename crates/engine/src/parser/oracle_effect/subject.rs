use crate::parser::oracle_nom::error::OracleError;
use nom::branch::alt;
use nom::bytes::complete::{tag, take_till, take_until};
use nom::combinator::{all_consuming, map, opt, rest, value, verify};
use nom::multi::separated_list1;
use nom::sequence::{delimited, preceded, terminated};
use nom::Parser;

use super::animation::{
    animation_modifications_with_replacement, has_in_addition_to_other_types, parse_animation_spec,
};
use super::imperative;
use super::lower::BOUNDED_TARGET_PHRASES;
use super::{resolve_it_pronoun, ParseContext};
use crate::parser::oracle_ir::ast::*;
use crate::types::ability::{
    AbilityDefinition, AbilityKind, ChosenSubtypeKind, ContinuousModification, ControllerRef,
    Duration, Effect, FilterProp, MultiTargetSpec, PlayerFilter, PlayerScope, PtValue,
    QuantityExpr, QuantityRef, StaticCondition, StaticDefinition, TargetFilter, TypedFilter,
};
use crate::types::game_state::DayNight;
use crate::types::keywords::Keyword;
use crate::types::phase::Phase;
use crate::types::statics::{ProhibitionScope, StaticMode};

use super::super::oracle_keyword::parse_keyword_from_oracle;
use super::super::oracle_nom::duration::parse_duration;
use super::super::oracle_nom::error::OracleResult;
use super::super::oracle_nom::primitives as nom_primitives;
use super::super::oracle_nom::quantity as nom_quantity;
use super::super::oracle_nom::target::parse_event_context_ref;
use super::super::oracle_quantity;
use super::super::oracle_static::{
    classify_block_exception, parse_additive_type_clause_modifications,
    parse_cant_be_activated_exemption_in_text, parse_chosen_qualifier_subject,
    parse_continuous_modifications, parse_static_line, parse_static_line_multi,
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
    build_continuous_clause(application, predicate, ctx)
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
        for &(phrase, min, max) in BOUNDED_TARGET_PHRASES {
            if tag::<_, _, OracleError<'_>>(phrase)
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
        value(
            ("that player", true),
            tag::<_, _, OracleError<'_>>("that player may"),
        ),
        value(("the player", true), tag("the player may")),
        value(
            ("that attacking player", false),
            tag("that attacking player"),
        ),
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
            TargetFilter::TriggeringPlayer | TargetFilter::DefendingPlayer
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
    let (power, toughness, duration) =
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
            // CR 702.122c: a granted crew/saddle/station power modifier (e.g. Stoic
            // Star-Captain's "Each creature you control crews … as though its power
            // were 2 greater") must propagate onto the affected creatures so the
            // crew/saddle power summation observes it via active_static_definitions.
            | StaticMode::CrewContribution { .. }
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
        // CR 702.122c: "crew [Vehicles]".
        value(
            vec![StaticMode::CantCrew],
            (tag("crew"), opt(tag(" vehicles"))),
        ),
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
        AbilityKind, ContinuousModification, ControllerRef, Effect, TypeFilter,
    };
    use crate::types::card_type::Supertype;
    use crate::types::statics::BlockExceptionKind;

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
}
