use nom::branch::alt;
use nom::bytes::complete::tag;
use nom::combinator::{all_consuming, value};
use nom::Parser;
use nom_language::error::VerboseError;

use super::animation::{animation_modifications, parse_animation_spec};
use super::types::*;
use super::{resolve_it_pronoun, ParseContext};
use crate::types::ability::{
    AbilityDefinition, AbilityKind, ContinuousModification, ControllerRef, Duration, Effect,
    FilterProp, GainLifePlayer, MultiTargetSpec, PtValue, QuantityExpr, QuantityRef, RoundingMode,
    StaticDefinition, TargetFilter, TypedFilter,
};
use crate::types::game_state::DayNight;
use crate::types::statics::StaticMode;

use super::super::oracle_nom::primitives as nom_primitives;
use super::super::oracle_nom::target::parse_event_context_ref;
use super::super::oracle_static::parse_continuous_modifications;
use super::super::oracle_target::{parse_target, parse_type_phrase};
use super::super::oracle_util::{
    parse_number, TextPair, SELF_REF_PARSE_ONLY_PHRASES, SELF_REF_TYPE_PHRASES,
};

pub(super) fn try_parse_subject_predicate_ast(text: &str, ctx: &ParseContext) -> Option<ClauseAst> {
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
    ctx: &ParseContext,
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

fn try_parse_subject_continuous_clause(
    text: &str,
    ctx: &ParseContext,
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
    let application = parse_subject_application(subject, ctx)?;
    build_continuous_clause(application, predicate)
}

fn try_parse_subject_become_clause(text: &str, ctx: &ParseContext) -> Option<ParsedEffectClause> {
    let verb_start = find_predicate_start(text)?;
    let subject = text[..verb_start].trim();
    let predicate = deconjugate_verb(text[verb_start..].trim());
    let predicate_lower = predicate.to_lowercase();
    tag::<_, _, VerboseError<&str>>("become ")
        .parse(predicate_lower.as_str())
        .ok()?;
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
            condition: None,
            optional: false,
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
        return Some(ParsedEffectClause {
            effect: Effect::GenericEffect {
                static_abilities: vec![StaticDefinition::new(StaticMode::AssignNoCombatDamage)
                    .affected(application.affected)
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

/// CR 702.3b: "[subject] can attack [this turn] as though it didn't have defender"
/// Produces a GenericEffect with CanAttackWithDefender static mode.
fn try_parse_can_attack_with_defender(
    text: &str,
    ctx: &ParseContext,
) -> Option<ParsedEffectClause> {
    let lower = text.to_lowercase();
    let tp = TextPair::new(text, &lower);
    let pos = tp.find(" can attack")?;
    if !lower.contains("as though it didn't have defender") {
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
    Some(ParsedEffectClause {
        effect: Effect::GenericEffect {
            static_abilities: vec![StaticDefinition::new(StaticMode::CanAttackWithDefender)
                .affected(application.affected)
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
    })
}

pub(super) fn parse_subject_application(
    subject: &str,
    ctx: &ParseContext,
) -> Option<SubjectApplication> {
    if subject.trim().is_empty() {
        return None;
    }

    let lower = subject.to_lowercase();

    // CR 115.10a: "another target X" — target with Another filter property,
    // excluding the source object from legal targets.
    if tag::<_, _, VerboseError<&str>>("another target ")
        .parse(lower.as_str())
        .is_ok()
    {
        let (filter, _) = parse_target(&subject["another ".len()..]);
        let filter = add_another_property(filter);
        return subject_filter_application(filter, true);
    }
    if tag::<_, _, VerboseError<&str>>("target ")
        .parse(lower.as_str())
        .is_ok()
    {
        let (filter, _) = parse_target(subject);
        return subject_filter_application(filter, true);
    }
    if tag::<_, _, VerboseError<&str>>("up to ")
        .parse(lower.as_str())
        .is_ok()
    {
        let (target_text, multi_target) = super::strip_optional_target_prefix(subject);
        if multi_target.is_some() {
            let (filter, _) = parse_target(target_text);
            let mut application = subject_filter_application(filter, true)?;
            application.multi_target = multi_target;
            return Some(application);
        }
    }
    // CR 115.1d: "any number of target creatures" — variable-count targeting.
    // Strip "any number of " prefix, delegate to parse_target for the filter,
    // and attach MultiTargetSpec { min: 0, max: None } (unlimited).
    if let Ok((after_prefix, _)) =
        tag::<_, _, VerboseError<&str>>("any number of ").parse(lower.as_str())
    {
        let consumed = lower.len() - after_prefix.len();
        let target_text = &subject[consumed..];
        if tag::<_, _, VerboseError<&str>>("target ")
            .parse(after_prefix)
            .is_ok()
        {
            let (filter, _) = parse_target(target_text);
            let mut application = subject_filter_application(filter, true)?;
            application.multi_target = Some(MultiTargetSpec { min: 0, max: None });
            return Some(application);
        }
    }
    // "each of your opponents" / "each of those creatures" / "each of them" — variant of
    // "each" with an interposed "of" that parse_target doesn't handle directly.
    // Must check before "each " to avoid the generic "each" path swallowing "each of".
    if let Ok((remainder, _)) = tag::<_, _, VerboseError<&str>>("each of ").parse(lower.as_str()) {
        if alt((
            tag::<_, _, VerboseError<&str>>("your opponents"),
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
        if alt((tag::<_, _, VerboseError<&str>>("those "), tag("them")))
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
        alt((tag::<_, _, VerboseError<&str>>("all "), tag("each "))).parse(lower.as_str())
    {
        let consumed = lower.len() - rest_lower.len();
        let phrase = &subject[consumed..];
        let (filter, rest) = parse_type_phrase(phrase);
        let filter = merge_partial_type_phrase_filter(filter, rest.trim());
        return subject_filter_application(filter, false);
    }
    if alt((
        tag::<_, _, VerboseError<&str>>("enchanted creature"),
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
    if let Ok((_, _)) = tag::<_, _, VerboseError<&str>>("those ").parse(lower.as_str()) {
        return subject_filter_application(TargetFilter::ParentTarget, false);
    }

    // Bare plural noun phrase subjects ("creatures you control", "other creatures you control")
    // are implicit "all X" forms — strip any "other " prefix and route through parse_target.
    let (had_other, noun_subject) =
        if let Ok((rest, _)) = tag::<_, _, VerboseError<&str>>("other ").parse(lower.as_str()) {
            (true, rest)
        } else {
            (false, lower.as_str())
        };
    if alt((
        tag::<_, _, VerboseError<&str>>("target "),
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
    // CR 608.2k: "that player" / "the player" as subject.
    // In trigger context (`ctx.subject` is Some — set exclusively by
    // `oracle_trigger.rs::parse_trigger_line` via
    // `extract_trigger_subject_for_context`; non-trigger parse entry points
    // leave it as None), the phrase refers anaphorically to the player from the
    // triggering event (damaged player, casting player, etc.) regardless of
    // whether the trigger subject itself is SelfRef ("~ deals damage to a
    // player") or a typed object. Delegate to the single-authority
    // event-context combinator for the mapping.
    // Outside trigger context, fall back to TargetFilter::Player (preserving
    // pre-existing behavior for non-trigger phrasings).
    //
    // Dispatch via the single-authority event-context combinator —
    // `parse_event_context_ref` already recognizes both "that player" and
    // "the player" as TriggeringPlayer. `all_consuming` restricts the match
    // to standalone subject phrases (no trailing text) and restricts the
    // TriggeringPlayer branch here to the two player-referencing forms.
    if let Ok((_, ctx_filter)) = all_consuming(parse_event_context_ref).parse(lower.as_str()) {
        if matches!(ctx_filter, TargetFilter::TriggeringPlayer) {
            let affected = if ctx.subject.is_some() {
                ctx_filter
            } else {
                TargetFilter::Player
            };
            return Some(SubjectApplication {
                affected,
                target: None,
                multi_target: None,
                inherits_parent: false,
                is_optional: false,
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
    if tag::<_, _, VerboseError<&str>>("an opponent")
        .parse(lower.as_str())
        .is_ok()
    {
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
        tag::<_, _, VerboseError<&str>>("its controller may"),
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
        alt((tag::<_, _, VerboseError<&str>>("that "), tag("the "))).parse(lower.as_str())
    {
        // structural: not dispatch — the nom `alt(tag(...))` above is the dispatch
        // step that consumes the determiner; this `ends_with` is a post-tokenization
        // structural check that the remaining tail is `<noun>'s controller` /
        // `<noun>'s owner`, mirroring the existing `parse_target` path that uses
        // `find("'s controller")` for the same purpose.
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
    // Explicit self-reference — always SelfRef
    if matches!(lower.as_str(), "~" | "this")
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
    // CR 608.2k: Bare pronoun "it" — context-dependent
    if lower == "it" {
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
    if let Ok((rest_subject, _)) = tag::<_, _, VerboseError<&str>>("that ").parse(lower.as_str()) {
        // CR 608.2c: "that creature/permanent/land" — anaphoric back-reference to a
        // previously mentioned object in the same effect sequence. Strip "that " and parse
        // the remainder as a type phrase. Covers all "that [type]" patterns generically.
        let consumed = lower.len() - rest_subject.len();
        let original_rest = &subject[consumed..];
        let (filter, rem) = parse_type_phrase(original_rest);
        if rem.trim().is_empty() && !matches!(filter, TargetFilter::Any) {
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

    None
}

/// CR 608.2k: Resolve bare pronoun "they" based on parser context.
/// In trigger effects where the subject is a player (e.g., "an opponent"),
/// "they" refers to the triggering player (`TriggeringPlayer`). A player-type
/// trigger subject is identified by having no `type_filters` but a `controller`
/// ref (e.g., `controller: Opponent`). For object-type subjects, "they" refers
/// to the triggering source. Without trigger context, "they" is an anaphoric
/// reference to previously mentioned objects (`ParentTarget`).
fn resolve_they_pronoun(ctx: &ParseContext) -> TargetFilter {
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
    if alt((tag::<_, _, VerboseError<&str>>("become "), tag("become\n")))
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
        });
    }

    // Compound: "get +1/+1 until end of turn and you gain 1 life"
    // Split on " and " that follows a duration marker, producing a pump
    // with a chained sub_ability for the remainder.
    if let Some(clause) = try_split_pump_compound(&normalized, &application) {
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
        });
    }

    Some(ParsedEffectClause {
        effect: Effect::GenericEffect {
            static_abilities: vec![StaticDefinition::continuous()
                .affected(application.affected)
                .modifications(modifications)
                .description(predicate_text.to_string())],
            duration: duration.clone(),
            target: application.target,
        },
        duration,
        sub_ability: None,
        distribute: None,
        multi_target: None,
        condition: None,
        optional: false,
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

fn build_become_clause(
    application: SubjectApplication,
    predicate: &str,
) -> Option<ParsedEffectClause> {
    let normalized = deconjugate_verb(predicate);
    let (predicate, duration) = super::strip_trailing_duration(&normalized);
    // CR 722: "become the monarch" — special keyword action, not an animation.
    let predicate_lower = predicate.to_lowercase();
    let (become_rest, _) = tag::<_, _, VerboseError<&str>>("become ")
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
                mana_value_limit: None,
                additional_modifications: Vec::new(),
            },
            duration,
            sub_ability: None,
            distribute: None,
            multi_target: None,
            condition: None,
            optional: false,
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
        condition: None,
        optional: false,
    })
}

/// CR 119.5: Parse "life total becomes N" into SetLifeTotal effect.
/// Handles: "half that player's starting life total", numeric amounts,
/// "their starting life total", and other quantity expressions.
fn try_parse_set_life_total(
    become_text: &str,
    application: &SubjectApplication,
) -> Option<ParsedEffectClause> {
    let lower = become_text.to_lowercase();

    // Parse the amount expression
    let amount =
        if let Ok((rest, _)) = tag::<_, _, VerboseError<&str>>("half ").parse(lower.as_str()) {
            // "half their starting life total" / "half that player's starting life total"
            if rest.contains("starting life total") {
                QuantityExpr::HalfRounded {
                    inner: Box::new(QuantityExpr::Ref {
                        qty: QuantityRef::StartingLifeTotal,
                    }),
                    rounding: RoundingMode::Down,
                }
            } else {
                return None;
            }
        } else if lower.contains("starting life total") {
            QuantityExpr::Ref {
                qty: QuantityRef::StartingLifeTotal,
            }
        } else if let Some((n, rest)) = parse_number(&lower) {
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
            return None;
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
    })
}

/// CR 730.1: Parse "night" / "day" after "becomes" into SetDayNight effect.
fn try_parse_set_day_night(become_text: &str) -> Option<ParsedEffectClause> {
    let lower = become_text.to_lowercase();
    let to = if lower == "night" {
        DayNight::Night
    } else if lower == "day" {
        DayNight::Day
    } else {
        return None;
    };

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
        condition: None,
        optional: false,
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
    }
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

    // CR 502.3: "doesn't untap during its controller's next untap step" —
    // override duration to UntilControllerNextUntapStep when the predicate
    // contains "next untap step". Also inject AddStaticMode modification so
    // the transient continuous effect system can enforce it.
    let has_next_untap = normalized.to_lowercase().contains("next untap step")
        || predicate.to_lowercase().contains("next untap step");
    let duration = if has_next_untap && modes.iter().any(|m| matches!(m, StaticMode::CantUntap)) {
        Some(Duration::UntilControllerNextUntapStep)
    } else {
        duration
    };

    let static_abilities = modes
        .into_iter()
        .map(|mode| {
            let mut def = StaticDefinition::new(mode.clone())
                .affected(application.affected.clone())
                .description(predicate.to_string());
            // For CantUntap with a duration, inject the modification so the
            // transient effect system can propagate it through layers.
            if matches!(mode, StaticMode::CantUntap) && duration.is_some() {
                def = def.modifications(vec![ContinuousModification::AddStaticMode {
                    mode: StaticMode::CantUntap,
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
    })
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
    // Simple restrictions
    if lower == "can't block" || lower == "cannot block" {
        return Some(vec![StaticMode::CantBlock]);
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
    // CR 509.1a + "can't be blocked": Compound "can't block or be blocked"
    if lower == "can't block or be blocked" || lower == "cannot block or be blocked" {
        return Some(vec![StaticMode::CantBlock, StaticMode::CantBeBlocked]);
    }
    // CR 509.1c: "can't be blocked except by ..." — evasion restriction
    if let Ok((except_text, _)) = alt((
        tag::<_, _, VerboseError<&str>>("can't be blocked except by "),
        tag("cannot be blocked except by "),
    ))
    .parse(lower)
    {
        return Some(vec![StaticMode::CantBeBlockedExceptBy {
            filter: except_text.to_string(),
        }]);
    }
    // CR 509.1b: "can't be blocked by <filter>" — blocker restriction
    if let Ok((by_rest, _)) = alt((
        tag::<_, _, VerboseError<&str>>("can't be blocked by "),
        tag("cannot be blocked by "),
    ))
    .parse(lower)
    {
        let filter_text = by_rest.trim_end_matches('.').trim_end_matches(" this turn");
        let (filter, _) = parse_type_phrase(filter_text);
        if !matches!(filter, TargetFilter::Any) {
            return Some(vec![StaticMode::CantBeBlockedBy { filter }]);
        }
    }
    // CR 115.4: "can't be the target of ..." — hexproof variant
    if alt((
        tag::<_, _, VerboseError<&str>>("can't be the target of "),
        tag("cannot be the target of "),
    ))
    .parse(lower)
    .is_ok()
    {
        return Some(vec![StaticMode::CantBeTargeted]);
    }
    // CR 119.7: "can't gain life" — lifegain prevention
    if lower == "can't gain life" || lower == "cannot gain life" {
        return Some(vec![StaticMode::CantGainLife]);
    }
    // CR 302.6: "doesn't untap during [controller's] untap step"
    if alt((
        tag::<_, _, VerboseError<&str>>("doesn't untap"),
        tag("don't untap"),
    ))
    .parse(lower)
    .is_ok()
    {
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
    tag::<_, _, VerboseError<&str>>("its controller ")
        .parse(lower.as_str())
        .ok()?;
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
        let after = alt((tag::<_, _, VerboseError<&str>>("gains "), tag("gain ")))
            .parse(after)
            .map(|(rest, _)| rest)
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
    alt((
        alt((
            value((), tag::<_, _, VerboseError<&str>>("all ")),
            value((), tag("an opponent ")),
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
            value((), tag::<_, _, VerboseError<&str>>("its owner ")),
            value((), tag("~'s owner ")),
            value((), tag("target ")),
            value((), tag("that ")),
            value((), tag("the chosen ")),
            value((), tag("the player ")),
            value((), tag("they ")),
            value((), tag("this ")),
            value((), tag("those ")),
            value((), tag("up to ")),
            value((), tag("you ")),
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

    // CR 608.2c + CR 117.3a: "its/their controller [may]" anaphoric player subject.
    #[test]
    fn parse_subject_its_controller_bare() {
        let ctx = ParseContext::default();
        let result = parse_subject_application("its controller", &ctx);
        let app = result.expect("should recognize 'its controller'");
        assert_eq!(app.affected, TargetFilter::ParentTargetController);
        assert!(!app.is_optional, "no 'may' modal → not optional");
    }

    #[test]
    fn parse_subject_their_controller_bare() {
        let ctx = ParseContext::default();
        let result = parse_subject_application("their controller", &ctx);
        let app = result.expect("should recognize 'their controller'");
        assert_eq!(app.affected, TargetFilter::ParentTargetController);
        assert!(!app.is_optional);
    }

    #[test]
    fn parse_subject_its_controller_may() {
        let ctx = ParseContext::default();
        let result = parse_subject_application("its controller may", &ctx);
        let app = result.expect("should recognize 'its controller may'");
        assert_eq!(app.affected, TargetFilter::ParentTargetController);
        assert!(
            app.is_optional,
            "'may' modal must mark the subject as optional"
        );
    }

    #[test]
    fn parse_subject_their_controller_may() {
        let ctx = ParseContext::default();
        let result = parse_subject_application("their controller may", &ctx);
        let app = result.expect("should recognize 'their controller may'");
        assert_eq!(app.affected, TargetFilter::ParentTargetController);
        assert!(app.is_optional);
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

    // CR 115.1d: "any number of target" subject prefix tests
    #[test]
    fn parse_subject_any_number_of_target_creatures() {
        let ctx = ParseContext::default();
        let result = parse_subject_application("any number of target creatures", &ctx);
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
            Some(MultiTargetSpec { min: 0, max: None }),
            "should have unlimited multi_target"
        );
    }

    #[test]
    fn parse_subject_any_number_of_target_creatures_you_control() {
        let ctx = ParseContext::default();
        let result = parse_subject_application("any number of target creatures you control", &ctx);
        assert!(result.is_some());
        let app = result.unwrap();
        assert!(
            matches!(app.affected, TargetFilter::Typed(ref t)
                if t.type_filters.contains(&TypeFilter::Creature)
                && t.controller == Some(ControllerRef::You)),
            "should parse creature + controller, got {:?}",
            app.affected
        );
        assert_eq!(
            app.multi_target,
            Some(MultiTargetSpec { min: 0, max: None }),
        );
    }

    #[test]
    fn parse_subject_any_number_of_target_players() {
        let ctx = ParseContext::default();
        let result = parse_subject_application("any number of target players", &ctx);
        assert!(result.is_some());
        let app = result.unwrap();
        assert_eq!(
            app.multi_target,
            Some(MultiTargetSpec { min: 0, max: None }),
        );
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
}
