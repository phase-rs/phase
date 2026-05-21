use crate::parser::oracle_nom::error::OracleError;
use nom::branch::alt;
use nom::bytes::complete::{tag, take_until};
use nom::character::complete::one_of;
use nom::combinator::{all_consuming, eof, opt, peek, recognize, rest, value};
use nom::multi::many1;
use nom::sequence::{delimited, pair, preceded, terminated};
use nom::Parser;

use super::oracle_effect::{
    condition_text_is_rehomeable, lower_effect_chain_ir, parse_effect_chain_ir,
    try_parse_exile_top_each_library_with_collection_counter,
};
use super::oracle_ir::context::ParseContext;
use super::oracle_ir::trigger::{TriggerBody, TriggerIr, TriggerModifiers};
use super::oracle_nom::condition::parse_inner_condition;
use super::oracle_nom::error::OracleResult;
use super::oracle_nom::filter::parse_enters_origin_zone;
use super::oracle_nom::primitives::{
    self as nom_primitives, scan_contains, scan_preceded, scan_split_at_phrase,
};
use super::oracle_nom::quantity as nom_quantity;
use super::oracle_target::{
    attachment_kinds_filter_prop, parse_attachment_kind_disjunction, parse_type_phrase,
    starts_with_type_word,
};
use super::oracle_util::{
    canonicalize_subtype_name, is_core_type_name, is_non_subtype_subject_name, merge_or_filters,
    normalize_card_name_refs, parse_number, parse_ordinal, parse_subtype, strip_after,
    strip_reminder_text, TextPair, SELF_REF_PARSE_ONLY_PHRASES,
};
use crate::parser::oracle_ir::diagnostic::OracleDiagnostic;
use crate::types::ability::{
    AbilityCost, AbilityDefinition, AbilityKind, AttachmentKind, CastVariantPaid, Comparator,
    ControllerRef, CounterTriggerFilter, DamageKindFilter, Effect, FilterProp, OriginConstraint,
    PlayerFilter, QuantityExpr, QuantityRef, StaticCondition, TargetFilter, TriggerCondition,
    TriggerConstraint, TriggerDefinition, TypeFilter, TypedFilter, UnlessPayModifier,
    ZoneChangeClause,
};
use crate::types::card_type::CoreType;
use crate::types::counter::parse_counter_type;
use crate::types::events::PlayerActionKind;
use crate::types::mana::ManaColor;
use crate::types::phase::Phase;
use crate::types::triggers::{AttackTargetFilter, TriggerMode};
use crate::types::zones::Zone;

/// Returns true if `filter` references the trigger source itself — directly
/// (`TargetFilter::SelfRef`) or transitively inside an `Or`/`And`/`Not`
/// composition (e.g. "this creature or another creature", "a creature other
/// than ~"). Used to decide whether a trigger needs its `trigger_zones`
/// extended to non-battlefield zones so that LTB / similar triggers can fire
/// after the source object has moved.
fn filter_references_self(filter: &TargetFilter) -> bool {
    match filter {
        TargetFilter::SelfRef => true,
        TargetFilter::Or { filters } | TargetFilter::And { filters } => {
            filters.iter().any(filter_references_self)
        }
        TargetFilter::Not { filter } => filter_references_self(filter),
        _ => false,
    }
}

fn self_recursion_trigger_zone(ability: &crate::types::ability::AbilityDefinition) -> Option<Zone> {
    match ability.effect.as_ref() {
        crate::types::ability::Effect::ChangeZone {
            origin: Some(origin),
            target: TargetFilter::SelfRef,
            ..
        } if *origin != Zone::Battlefield => Some(*origin),
        crate::types::ability::Effect::Bounce {
            target: TargetFilter::SelfRef,
            destination,
        } if destination.is_none_or(|zone| zone == Zone::Hand) => Some(Zone::Graveyard),
        _ => ability
            .sub_ability
            .as_deref()
            .and_then(self_recursion_trigger_zone)
            .or_else(|| {
                ability
                    .else_ability
                    .as_deref()
                    .and_then(self_recursion_trigger_zone)
            }),
    }
}

fn effect_adds_mana_to_triggering_player(effect_lower: &str) -> bool {
    // CR 109.5 + CR 117.3a: "its controller" binds to the antecedent of the
    // trigger condition (e.g. "enchanted land" → the land's controller).
    // For TapsForMana triggers on Auras (Fertile Ground, Wild Growth, Utopia
    // Sprawl, Trace of Abundance, Verdant Haven, Market Festival, Weirding
    // Wood, Overgrowth), the antecedent is the enchanted permanent and its
    // controller equals the player who tapped it for mana — i.e. the
    // ManaAdded event's `player_id`, which `PlayerFilter::TriggeringPlayer`
    // extracts. Routing these effects through `TriggeringPlayer` keeps the
    // mana with the land's controller even when an opponent's Aura is
    // attached.
    value(
        (),
        pair(
            alt((
                tag::<_, _, OracleError<'_>>("that player "),
                tag("that opponent "),
                tag("its controller "),
            )),
            alt((tag("adds "), tag("add "))),
        ),
    )
    .parse(effect_lower.trim_start())
    .is_ok()
}

fn trigger_condition_source_zone(condition: &TriggerCondition) -> Option<Zone> {
    match condition {
        TriggerCondition::SourceInZone { zone } => Some(*zone),
        TriggerCondition::And { conditions } => {
            conditions.iter().find_map(trigger_condition_source_zone)
        }
        _ => None,
    }
}

/// CR 107.3a + CR 107.3i + CR 601.2f + CR 603.2: In an ETB trigger on a spell
/// cast for `{X}`, bare "X" in the trigger body refers to the value paid for
/// `{X}` during the cast. At runtime the `QuantityRef::Variable{name:"X"}`
/// branch of `resolve_ref` can read the trigger-event source's
/// `cost_x_paid`, but `PtValue::Variable("X"/"-X")` in `Effect::Pump`/
/// `Effect::PumpAll` has no such resolution — the runtime treats it as a
/// no-op. Rewriting to `CostXPaid` (wrapped in `Multiply{factor:-1,..}` for
/// the negative form) routes both paths through the same typed expression
/// machinery that already reads `cost_x_paid` from the entering permanent.
///
/// Mirrors `rewrite_variable_x_to_cost_x_paid` in `oracle_replacement.rs`
/// (enters-with-counters replacement effects) so ETB triggers and ETB
/// replacements share one convention for X propagation.
fn rewrite_trigger_pt_variable_x(value: &mut crate::types::ability::PtValue) {
    use crate::types::ability::{PtValue, QuantityExpr, QuantityRef};
    match value {
        PtValue::Variable(alias) if alias.eq_ignore_ascii_case("X") => {
            *value = PtValue::Quantity(QuantityExpr::Ref {
                qty: QuantityRef::CostXPaid,
            });
        }
        PtValue::Variable(alias) if alias.eq_ignore_ascii_case("-X") => {
            *value = PtValue::Quantity(QuantityExpr::Multiply {
                factor: -1,
                inner: Box::new(QuantityExpr::Ref {
                    qty: QuantityRef::CostXPaid,
                }),
            });
        }
        PtValue::Quantity(expr) => {
            super::oracle_replacement::rewrite_variable_x_to_cost_x_paid(expr);
        }
        _ => {}
    }
}

/// Walk an `Effect` and rewrite any `Variable("X")` / `PtValue::Variable("X"|"-X")`
/// occurrences to read from `cost_x_paid` on the entering permanent. See
/// `rewrite_trigger_pt_variable_x` for the rationale.
fn rewrite_cost_x_in_effect(effect: &mut crate::types::ability::Effect) {
    use super::oracle_replacement::rewrite_variable_x_to_cost_x_paid;
    use crate::types::ability::Effect;
    match effect {
        Effect::DealDamage { amount, .. }
        | Effect::GainLife { amount, .. }
        | Effect::LoseLife { amount, .. }
        | Effect::ChangeSpeed { amount, .. }
        | Effect::Draw { count: amount, .. }
        | Effect::Mill { count: amount, .. }
        | Effect::PutCounter { count: amount, .. }
        | Effect::PutCounterAll { count: amount, .. }
        | Effect::Token { count: amount, .. }
        | Effect::Dig { count: amount, .. }
        | Effect::DamageAll { amount, .. }
        | Effect::DamageEachPlayer { amount, .. } => {
            rewrite_variable_x_to_cost_x_paid(amount);
        }
        Effect::Pump {
            power, toughness, ..
        }
        | Effect::PumpAll {
            power, toughness, ..
        } => {
            rewrite_trigger_pt_variable_x(power);
            rewrite_trigger_pt_variable_x(toughness);
        }
        _ => {}
    }
}

/// Walk an `AbilityDefinition` tree and rewrite Variable("X") → CostXPaid.
/// Mirrors `apply_where_x_ability_expression` (`oracle_effect/mod.rs`) so
/// sub-abilities, else branches, and modal alternatives all inherit the rewrite.
fn rewrite_cost_x_in_ability(def: &mut crate::types::ability::AbilityDefinition) {
    rewrite_cost_x_in_effect(def.effect.as_mut());
    if let Some(sub) = def.sub_ability.as_mut() {
        rewrite_cost_x_in_ability(sub);
    }
    if let Some(else_ability) = def.else_ability.as_mut() {
        rewrite_cost_x_in_ability(else_ability);
    }
    for mode_ability in &mut def.mode_abilities {
        rewrite_cost_x_in_ability(mode_ability);
    }
}

/// Decide whether a trigger's execute body should have its bare `X`
/// references rewritten to read the entering permanent's `cost_x_paid`.
///
/// CR 603.6a + CR 107.3e: An ETB trigger on the source itself fires as the
/// source enters the battlefield; the cast that paid `{X}` is this permanent's
/// most recent cast, and `cost_x_paid` is stamped on the object by
/// `finalize_cast`. SelfRef/self-inclusive compound ETBs ("when ~ enters",
/// "when ~ or another creature enters") route through this rewrite.
fn trigger_should_rewrite_cost_x(def: &TriggerDefinition) -> bool {
    if def.mode != TriggerMode::ChangesZone {
        return false;
    }
    if def.destination != Some(Zone::Battlefield) {
        return false;
    }
    match def.valid_card.as_ref() {
        Some(filter) => filter_references_self(filter),
        None => false,
    }
}

/// Parse a trigger line that may contain compound trigger events into multiple
/// `TriggerDefinition`s. Compound patterns like "When X and when Y, effect" or
/// "Whenever X or deals combat damage to a player, effect" produce one trigger
/// per event, each sharing the same execute effect.
///
/// CR 603.2: A triggered ability may have multiple triggering events. Each event
/// is independently evaluated, producing separate trigger instances that share
/// the same effect.
///
/// Accepts raw card Oracle text; internally normalizes self-references via
/// `normalize_card_name_refs`. When invoked via [`parse_oracle_text`] the
/// text is already normalized and the internal call is an idempotent no-op.
///
/// External callers (and tests) pass `None` for the base trigger index, which
/// disables the "and it has this ability" arm of the BecomeCopy except-clause.
/// Production callers in [`parse_oracle_text`] pass the running
/// `result.triggers.len()` so each compound-split trigger receives a unique
/// index in the source object's `base_trigger_definitions` list (CR 707.9a).
pub fn parse_trigger_lines(text: &str, card_name: &str) -> Vec<TriggerDefinition> {
    parse_trigger_lines_at_index(text, card_name, None, &mut ParseContext::default())
}

pub(crate) fn parse_trigger_lines_at_index(
    text: &str,
    card_name: &str,
    base_trigger_index: Option<usize>,
    ctx: &mut ParseContext,
) -> Vec<TriggerDefinition> {
    parse_trigger_lines_at_index_ir(text, card_name, base_trigger_index, ctx)
        .iter()
        .map(lower_trigger_ir)
        .collect()
}

/// IR production for compound trigger splitting. Each compound half produces
/// its own `TriggerIr`.
pub(crate) fn parse_trigger_lines_at_index_ir(
    text: &str,
    card_name: &str,
    base_trigger_index: Option<usize>,
    ctx: &mut ParseContext,
) -> Vec<TriggerIr> {
    let stripped = strip_reminder_text(text);
    let normalized = normalize_self_refs(&stripped, card_name);
    let lower = normalized.to_lowercase();

    // Detect compound trigger patterns in the condition portion.
    // Split at the effect boundary first, then look for conjunctions in the condition.
    let tp = TextPair::new(&normalized, &lower);
    let (condition, effect) = split_trigger(tp);
    let cond_lower = condition.to_lowercase();

    // Pattern 1: "when/whenever X and when Y" or "when X and whenever Y"
    if let Some(halves) = split_and_when_compound(&cond_lower, &condition) {
        let mut results = Vec::with_capacity(halves.len());
        for (i, cond) in halves.into_iter().enumerate() {
            let trigger_text = if effect.is_empty() {
                cond
            } else {
                format!("{cond}, {effect}")
            };
            results.push(parse_trigger_line_with_index_ir(
                &trigger_text,
                card_name,
                base_trigger_index.map(|b| b + i),
                ctx,
            ));
        }
        return results;
    }

    // Pattern 2: "whenever ~ [event1] or [event2]" — compound events sharing a subject.
    if let Some(halves) = split_or_event_compound(&cond_lower, &condition) {
        let mut results = Vec::with_capacity(halves.len());
        for (i, cond) in halves.into_iter().enumerate() {
            let trigger_text = if effect.is_empty() {
                cond
            } else {
                format!("{cond}, {effect}")
            };
            results.push(parse_trigger_line_with_index_ir(
                &trigger_text,
                card_name,
                base_trigger_index.map(|b| b + i),
                ctx,
            ));
        }
        return results;
    }

    // No compound — single trigger.
    vec![parse_trigger_line_with_index_ir(
        text,
        card_name,
        base_trigger_index,
        ctx,
    )]
}

/// Part D: If `"for the first time each turn"` appears as a word-boundary
/// phrase in `condition`, strip it and return `(stripped, true)`; otherwise
/// return `(condition, false)` unchanged.
///
/// Stripping is load-bearing. The generic cycle-trigger handlers in
/// `try_parse_player_trigger` (and several other condition-level handlers)
/// use `matches!(lower, "exact" | "exact")` exact-string dispatch — so
/// Valiant Rescuer's condition `"whenever you cycle another card for the
/// first time each turn"` must have the qualifier removed before dispatch
/// or it falls through to `TriggerMode::Unknown`. Stripping once at the
/// condition-parse boundary is strictly smaller than adding a
/// `"... for the first time each turn"` variant to every exact-match arm.
///
/// Implementation: `scan_preceded` locates the phrase at a word boundary
/// (consistent with `scan_contains`), returning both the prefix and
/// post-phrase remainder in a single pass — no `str::find` fallback.
fn strip_first_time_each_turn_qualifier(condition: &str) -> (String, bool) {
    const PHRASE: &str = "for the first time each turn";
    let lower = condition.to_lowercase();
    let Some((before_lower, _, rest_lower)) =
        scan_preceded(&lower, |i| tag::<_, _, OracleError<'_>>(PHRASE).parse(i))
    else {
        return (condition.to_string(), false);
    };
    // ASCII-only phrase → byte offsets in `condition` align with `lower`.
    let start = before_lower.len();
    let end = condition.len() - rest_lower.len();
    let mut joined = String::with_capacity(condition.len() - (end - start));
    joined.push_str(&condition[..start]);
    joined.push_str(&condition[end..]);
    // Collapse any leading / trailing / double whitespace introduced by
    // removing the phrase.
    let stripped = joined.split_whitespace().collect::<Vec<_>>().join(" ");
    (stripped, true)
}

/// CR 109.4 + CR 115.1 + CR 506.2: Detect a trigger condition that introduces
/// a player target — currently the "you/an opponent/a player attack(s) a player"
/// family. When this returns true, follow-on possessive references inside the
/// effect ("that player controls/owns") refer to that introduced player and the
/// parser pushes a relative-player scope so they emit `ControllerRef::TargetPlayer`.
///
/// Built from composable nom alternatives so adding new condition shapes
/// (combat-damage-to-a-player, "deals damage to a player", etc.) is a one-line
/// change to the inner `alt()`.
fn condition_introduces_target_player(cond_lower: &str) -> bool {
    use nom::bytes::complete::tag;
    use nom::combinator::value;

    fn parse_actor(input: &str) -> Result<(&str, ()), nom::Err<OracleError<'_>>> {
        alt((
            value((), tag::<_, _, OracleError<'_>>("you ")),
            value((), tag("an opponent ")),
            value((), tag("a player ")),
            value((), tag("another player ")),
        ))
        .parse(input)
    }

    fn parse_attack_verb(input: &str) -> Result<(&str, ()), nom::Err<OracleError<'_>>> {
        alt((
            value((), tag::<_, _, OracleError<'_>>("attack ")),
            value((), tag("attacks ")),
        ))
        .parse(input)
    }

    /// CR 120.3: "deals [combat] damage to a player" — damage dealt to a player
    /// causes that player to lose life (CR 120.3a) and introduces the damaged
    /// player as the target-referring player, so "that player controls" in the
    /// effect refers to it
    /// (Dokuchi Silencer's "destroy target creature or planeswalker that player
    /// controls"). Mirrors `parse_attack_verb` — both verbs produce the same
    /// downstream scope.
    fn parse_damage_phrase(input: &str) -> Result<(&str, ()), nom::Err<OracleError<'_>>> {
        alt((
            value((), tag::<_, _, OracleError<'_>>("deals combat damage to ")),
            value((), tag("deals damage to ")),
            value((), tag("deal combat damage to ")),
            value((), tag("deal damage to ")),
        ))
        .parse(input)
    }

    // Walk word boundaries — the actor/verb pair may be preceded by "whenever",
    // "when", or quantifiers like "one or more creatures you control".
    let mut remaining = cond_lower;
    while !remaining.is_empty() {
        if let Ok((after_actor, ())) = parse_actor(remaining) {
            if let Ok((after_verb, ())) = parse_attack_verb(after_actor) {
                if tag::<_, _, OracleError<'_>>("a player")
                    .parse(after_verb)
                    .is_ok()
                {
                    return true;
                }
            }
        }
        // CR 120.3: "[anything] deals [combat] damage to a player" — introduces
        // the damaged player as the target-referring player. The subject can be
        // SelfRef ("~"), equipped creature ("equipped creature"), or any typed
        // subject, so match on the verb phrase alone.
        if let Ok((after_damage, ())) = parse_damage_phrase(remaining) {
            if tag::<_, _, OracleError<'_>>("a player")
                .parse(after_damage)
                .is_ok()
            {
                return true;
            }
        }
        // structural: not dispatch — advance to the next word boundary so the
        // nom alternatives above are retried at every word position (mirrors
        // `scan_timing_restrictions` in oracle_casting.rs).
        remaining = match remaining.find(' ') {
            Some(i) => remaining[i + 1..].trim_start(),
            None => "",
        };
    }
    false
}

/// Parse a full trigger line into a TriggerDefinition.
/// Input: a line starting with "When", "Whenever", or "At".
/// The card_name is used for self-reference substitution.
///
/// Accepts raw card Oracle text; internally normalizes self-references via
/// `normalize_card_name_refs`. When invoked via [`parse_oracle_text`] the
/// text is already normalized and the internal call is an idempotent no-op.
///
/// **Trigger index** (`current_trigger_index`): when known, the caller passes
/// the index this trigger will occupy in the source object's
/// `base_trigger_definitions` list. This is consumed by the BecomeCopy
/// except-clause parser (CR 707.9a) for "and it has this ability" — the
/// resulting `RetainPrintedTriggerFromSource { source_trigger_index }` points
/// back into the source's printed triggers so the copy retains the trigger
/// without needing a forward reference to the partial definition.
///
/// External callers (and the test API at `parse_trigger_line(text, card_name)`)
/// pass `None`, in which case any "has this ability" clause inside the trigger
/// body declines gracefully.
pub fn parse_trigger_line(text: &str, card_name: &str) -> TriggerDefinition {
    parse_trigger_line_with_index(text, card_name, None, &mut ParseContext::default())
}

/// IR production: extract all trigger fields into a `TriggerIr` without
/// performing final assembly into `TriggerDefinition`.
///
/// The scope guard for `ControllerRef::TargetPlayer` (D-04) is alive during
/// `parse_effect_chain_ir` — the guard must not be dropped before body parsing.
#[tracing::instrument(level = "debug", skip(card_name))]
pub(crate) fn parse_trigger_line_with_index_ir(
    text: &str,
    card_name: &str,
    current_trigger_index: Option<usize>,
    ctx: &mut ParseContext,
) -> TriggerIr {
    let text = strip_reminder_text(text);
    // Replace self-references: "this creature", "this enchantment", card name → ~
    let normalized = normalize_self_refs(&text, card_name);
    let lower = normalized.to_lowercase();
    let tp = TextPair::new(&normalized, &lower);

    // Split condition from effect at first ", " after the trigger phrase
    let (condition_text_raw, effect_text) = split_trigger(tp);

    // CR-uniform: `"for the first time each turn"` in trigger CONDITION text is
    // a trigger-frequency qualifier that maps to `OncePerTurn`. Detect and strip
    // before the condition is dispatched. Scoped to condition text (NOT full
    // text) so triggers whose EFFECT text coincidentally contains the phrase
    // aren't retroactively constrained.
    let (condition_text_stripped, first_time_each_turn_in_condition) =
        strip_first_time_each_turn_qualifier(&condition_text_raw);
    let condition_text: &str = &condition_text_stripped;

    let effect_lower = effect_text.to_lowercase();
    // Extract intervening-if condition from effect text first — a leading
    // "if X, " can hide the "you may " optional marker behind the if-clause.
    let (effect_without_if, if_condition) = extract_if_condition(&effect_text);

    // CR 609.3: "You may" at the start of the effect text makes the triggered
    // effect optional at resolution.
    let starts_with_you_may = |s: &str| tag::<_, _, OracleError<'_>>("you may ").parse(s).is_ok();
    let after_structural_if = effect_lower
        .strip_prefix("if ") // allow-noncombinator: structural if-clause skip when condition is unrecognized
        .and_then(|rest| rest.split_once(", "))
        .map(|(_cond, body)| body);
    let optional = starts_with_you_may(effect_lower.as_str())
        || starts_with_you_may(effect_without_if.trim_start())
        || after_structural_if.is_some_and(starts_with_you_may);

    // Strip constraint sentences so they don't leak into effect parsing as sub-abilities
    let effect_final = strip_constraint_sentences(&effect_without_if);

    let cond_lower = condition_text.to_lowercase();

    // CR 118.12: Detect "unless [player] pays {cost}" in effect text.
    let (effect_for_parse, unless_pay) = extract_unless_pay_modifier(&effect_final, &cond_lower);

    // CR 608.2k: Extract trigger subject for pronoun resolution in effect text.
    let trigger_subject = extract_trigger_subject_for_context(condition_text, ctx);
    let mut effect_ctx = ParseContext {
        subject: Some(trigger_subject.clone()),
        card_name: Some(card_name.to_string()),
        current_trigger_index,
        // CR 303.4 + CR 702.103: Propagate the enclosing card's typed host
        // self-reference (set by `parse_oracle_ir` for Aura/bestow cards) into
        // the per-trigger effect context. The trigger body's effect parser
        // needs it to remap a `"that creature"` copy-token anaphor to the
        // enchanted host (Springheart Nantuko's landfall trigger).
        host_self_reference: ctx.host_self_reference.clone(),
        ..Default::default()
    };

    // CR 109.4 + CR 115.1 + CR 506.2: Set relative-player scope for
    // TargetPlayer resolution inside the trigger effect body.
    if condition_introduces_target_player(&cond_lower) {
        effect_ctx.relative_player_scope = Some(ControllerRef::TargetPlayer);
    } else if condition_introduces_scoped_phase_player(&cond_lower) {
        effect_ctx.relative_player_scope = Some(ControllerRef::ScopedPlayer);
    }

    // Parse the effect body
    let effect_for_parse_lower = effect_for_parse.to_lowercase();
    let has_up_to = scan_contains(&effect_for_parse_lower, "up to one");
    let body = if !effect_for_parse.is_empty() {
        // CR 701.38 + CR 207.2c: Vote blocks produce AbilityDefinition directly.
        if let Some(vote_def) =
            crate::parser::oracle_vote::parse_vote_block(&effect_for_parse, AbilityKind::Spell)
        {
            let mut ability = vote_def;
            if has_up_to {
                ability.optional_targeting = true;
            }
            if effect_adds_mana_to_triggering_player(&effect_lower)
                && matches!(
                    ability.effect.as_ref(),
                    crate::types::ability::Effect::Mana { .. }
                )
            {
                ability.player_scope = Some(PlayerFilter::TriggeringPlayer);
            }
            if optional {
                ability.optional = true;
            }
            Some(TriggerBody::PreLowered(Box::new(ability)))
        } else if unless_pay.is_none()
            && scan_contains(&effect_for_parse_lower, "unless")
            && !has_later_sentence_if(&effect_for_parse_lower)
        {
            Some(TriggerBody::PreLowered(Box::new(AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::Unimplemented {
                    name: "Unsupported unless clause".to_string(),
                    description: Some(effect_for_parse.clone()),
                },
            ))))
        } else {
            try_parse_exile_top_each_library_with_collection_counter(
                &effect_for_parse,
                AbilityKind::Spell,
            )
            .map(|ability| TriggerBody::PreLowered(Box::new(ability)))
            .or_else(|| {
                let ir =
                    parse_effect_chain_ir(&effect_for_parse, AbilityKind::Spell, &mut effect_ctx);
                Some(TriggerBody::EffectChain(ir))
            })
        }
    } else {
        None
    };
    // Transfer diagnostics from the per-trigger effect context to the outer ctx.
    ctx.diagnostics.append(&mut effect_ctx.diagnostics);

    // Parse the condition to get TriggerMode + partial TriggerDefinition
    let (condition, partial_def) = parse_trigger_condition(condition_text, ctx);

    // Constraint from full text (parsed during IR production so lowering has it)
    let constraint = parse_trigger_constraint(&lower);

    TriggerIr {
        condition,
        partial_def,
        body,
        modifiers: TriggerModifiers {
            optional,
            unless_pay,
            intervening_if: if_condition,
            trigger_subject,
            first_time_each_turn: first_time_each_turn_in_condition,
            constraint,
            has_up_to,
            effect_lower: effect_lower.to_string(),
        },
        source_text: text.to_string(),
    }
}

fn has_later_sentence_if(lower: &str) -> bool {
    lower.split('.').skip(1).any(|sentence| {
        tag::<_, _, OracleError<'_>>("if ")
            .parse(sentence.trim_start())
            .is_ok()
    })
}

/// Lowering: assemble a `TriggerDefinition` from a `TriggerIr`.
///
/// Applies all post-extraction transforms: condition composition, target-player
/// surfacing, constraint merging, trigger zone derivation, cost-X rewriting.
pub(crate) fn lower_trigger_ir(ir: &TriggerIr) -> TriggerDefinition {
    let mut def = ir.partial_def.clone();
    let modifiers = &ir.modifiers;

    // Lower the body
    let execute = match &ir.body {
        Some(TriggerBody::EffectChain(chain_ir)) => {
            let mut ability = lower_effect_chain_ir(chain_ir);
            // CR 702.179c-d: fold trailing speed-floor sentences into the
            // preceding `ChangeSpeed` effect and drop the orphan node.
            crate::parser::oracle_effect::fold_speed_floor_sentences(&mut ability);
            if modifiers.has_up_to {
                ability.optional_targeting = true;
            }
            if effect_adds_mana_to_triggering_player(&modifiers.effect_lower)
                && matches!(
                    ability.effect.as_ref(),
                    crate::types::ability::Effect::Mana { .. }
                )
            {
                ability.player_scope = Some(PlayerFilter::TriggeringPlayer);
            }
            // CR 609.3: Propagate optional to execute ability.
            if modifiers.optional {
                ability.optional = true;
            }
            Some(Box::new(ability))
        }
        Some(TriggerBody::PreLowered(ability)) => Some(ability.clone()),
        None => None,
    };

    def.execute = execute;
    def.optional = modifiers.optional;
    def.unless_pay = modifiers.unless_pay.clone();

    // CR 603.4: Compose intervening-if with existing condition via And.
    def.condition = match (&modifiers.intervening_if, def.condition.take()) {
        (Some(if_cond), Some(existing)) => Some(TriggerCondition::And {
            conditions: vec![existing, if_cond.clone()],
        }),
        (Some(c), None) => Some(c.clone()),
        (None, Some(c)) => Some(c),
        (None, None) => None,
    };

    // CR 109.4 + CR 603.7c: Surface TargetFilter::Player when execute
    // references ControllerRef::TargetPlayer.
    if def.valid_target.is_none() {
        if let Some(execute) = def.execute.as_deref() {
            if execute_references_target_player(&execute.effect) {
                def.valid_target = Some(TargetFilter::Player);
            }
        }
    }

    // Text-based constraints take precedence; fall back to condition-parser constraint.
    def.constraint = modifiers.constraint.clone().or(def.constraint.take());

    // CR-uniform: apply OncePerTurn as fallback.
    if modifiers.first_time_each_turn && def.constraint.is_none() {
        def.constraint = Some(TriggerConstraint::OncePerTurn);
    }

    // Preserve original oracle text for coverage/UI annotation.
    def.description = Some(ir.source_text.clone());

    // CR 113.6k: Derive trigger source zone from typed trigger/effect data.
    if let Some(zone) = def
        .condition
        .as_ref()
        .and_then(trigger_condition_source_zone)
    {
        def.trigger_zones = vec![zone];
    } else if matches!(def.valid_card, Some(TargetFilter::SelfRef))
        && def.destination == Some(Zone::Graveyard)
    {
        def.trigger_zones = vec![Zone::Graveyard];
    } else if let Some(zone) = def.execute.as_deref().and_then(self_recursion_trigger_zone) {
        def.trigger_zones = vec![zone];
    }

    // CR 107.3a + CR 107.3i + CR 601.2f: Rewrite X in ETB-self triggers.
    if trigger_should_rewrite_cost_x(&def) {
        if let Some(execute) = def.execute.as_deref_mut() {
            rewrite_cost_x_in_ability(execute);
        }
    }

    // CR 608.2k + CR 603.7c: For event-source-bearing trigger modes, the "that
    // card / that creature / that permanent" anaphor in the effect body
    // refers to the *triggering object* carried by the event (the just-
    // discarded card, sacrificed permanent, drawn card, etc.) — not a chosen
    // target. The shared `parse_target` family returns `ParentTarget` for
    // these phrases because trigger context is not threaded through the
    // effect-parser entry points; this post-lowering pass rewrites the
    // top-level effect target from `ParentTarget` to `TriggeringSource` so
    // `extract_source_from_event` (game/targeting.rs:539) resolves it to the
    // correct event object id at runtime.
    //
    // Drives Tergrid, God of Fright's reanimation class:
    //   "Whenever an opponent sacrifices a nontoken permanent or discards a
    //    permanent card, you may put that card from a graveyard onto the
    //    battlefield under your control."
    //
    // Gated on:
    //   1. `def.mode` is an event-source-bearing mode (see
    //      `mode_carries_event_source_object`), AND
    //   2. the ability has no explicit targeting (`valid_target.is_none()`
    //      AND `optional_targeting == false`) — otherwise `ParentTarget`
    //      legitimately inherits the player's chosen target.
    if let Some(execute) = def.execute.as_deref_mut() {
        if mode_carries_event_source_object(&def.mode)
            && def.valid_target.is_none()
            && !execute.optional_targeting
        {
            lift_parent_target_to_triggering_source_in_ability(execute);
        }
    }

    def
}

/// CR 603.7c: Trigger modes whose firing event carries a specific source
/// object id retrievable via `extract_source_from_event`. The "that card /
/// that creature / that permanent" anaphor in these triggers' effect bodies
/// refers to *that* object.
///
/// Kept narrow on purpose — covers the high-confidence cases where lifting
/// `ParentTarget` → `TriggeringSource` is unambiguous. Additional modes can
/// be added as their patterns appear in real cards.
fn mode_carries_event_source_object(mode: &TriggerMode) -> bool {
    matches!(
        mode,
        TriggerMode::Discarded
            | TriggerMode::DiscardedAll
            | TriggerMode::Sacrificed
            | TriggerMode::SacrificedOnce
            | TriggerMode::Destroyed
            | TriggerMode::Cycled
            | TriggerMode::CycledOrDiscarded
            | TriggerMode::Milled
            | TriggerMode::MilledOnce
            // CR 608.2k: A single-object zone-change trigger ("Whenever a
            // nontoken Zombie you control enters") carries the entering/leaving
            // object as the event source; "that creature" in the body refers to
            // it (Necroduality, Mimic Vat class). The batched ChangesZoneAll
            // mode is excluded — its event carries a set, not one object.
            | TriggerMode::ChangesZone
    )
}

/// Top-level target rewrite: `ParentTarget` → `TriggeringSource` on the
/// effects whose primary `target` field is the just-acted-on object.
///
/// Scoped to the `Effect` variants that carry a top-level `target:
/// TargetFilter` and whose runtime semantics make sense against the event
/// object (e.g. `ChangeZone` operating on the just-discarded card). Other
/// effect variants are left untouched.
fn lift_parent_target_to_triggering_source(effect: &mut Effect) {
    // CR 608.2k: each variant carries a top-level `target` that, when the
    // surface anaphor was "that <object>", refers to the event object.
    let target = match effect {
        Effect::ChangeZone { target, .. } => target,
        // "create a token that's a copy of that creature" (Necroduality) — the
        // copy source is the entering object, not the trigger's own source.
        Effect::CopyTokenOf { target, .. } => target,
        _ => return,
    };
    if matches!(target, TargetFilter::ParentTarget) {
        *target = TargetFilter::TriggeringSource;
    }
}

/// CR 608.2k + CR 603.7c: Recurse `lift_parent_target_to_triggering_source`
/// through an ability's effect AND every chained `sub_ability`. Required
/// for the punisher-trigger class: a chained Tergrid-shape ability like
/// "...exile that card, then create a token" carries the "that card"
/// anaphor on the *first* sub-ability's effect, not the top-level effect.
/// Without the descent, the second link would silently bind to the trigger
/// source object instead of the just-acted-on event object.
fn lift_parent_target_to_triggering_source_in_ability(ability: &mut AbilityDefinition) {
    // CR 608.2c + CR 608.2k: Stop the descent as soon as a link introduces a
    // player-*chosen* object target. A later `ParentTarget` then refers to
    // *that* choice, not the trigger event — the enters-flicker class
    // ("exile target permanent, then return that card": Felidar Guardian,
    // Restoration Angel) keeps its sub-ability bound to the chosen permanent.
    // Necroduality (top-level `CopyTokenOf` with no prior choice) and Tergrid
    // ("put that card …, then create a token") still lift correctly.
    let mut node = Some(ability);
    while let Some(link) = node {
        if introduces_chosen_object_target(link.effect.as_ref()) {
            break;
        }
        lift_parent_target_to_triggering_source(link.effect.as_mut());
        node = link.sub_ability.as_deref_mut();
    }
}

/// CR 608.2c: Does this effect introduce a freshly *chosen* object target — one
/// that a subsequent "that <object>" (`ParentTarget`) anaphor binds to, rather
/// than the trigger event? True for player-selected object filters (`Typed`,
/// and `Or`/`And` combinations of them); false for anaphoric/contextual refs
/// (`ParentTarget`, `TriggeringSource`, `SelfRef`, …) and untargeted effects.
fn introduces_chosen_object_target(effect: &Effect) -> bool {
    fn is_chosen(filter: &TargetFilter) -> bool {
        match filter {
            TargetFilter::Typed(_) => true,
            TargetFilter::Or { filters } | TargetFilter::And { filters } => {
                filters.iter().any(is_chosen)
            }
            _ => false,
        }
    }
    effect.target_filter().is_some_and(is_chosen)
}

/// Thin wrapper: parse trigger line through IR production + lowering.
#[tracing::instrument(level = "debug", skip(card_name))]
pub(crate) fn parse_trigger_line_with_index(
    text: &str,
    card_name: &str,
    current_trigger_index: Option<usize>,
    ctx: &mut ParseContext,
) -> TriggerDefinition {
    let ir = parse_trigger_line_with_index_ir(text, card_name, current_trigger_index, ctx);
    lower_trigger_ir(&ir)
}

/// Parse trigger constraint from the full trigger text.
fn parse_trigger_constraint(lower: &str) -> Option<TriggerConstraint> {
    // Order is load-bearing: longer/more-specific matches must precede shorter ones
    // ("only once each turn" before "only once", etc.).
    if scan_contains(lower, "this ability triggers only once each turn")
        || scan_contains(lower, "triggers only once each turn")
        // CR 603.12: "Do this only once each turn" is functionally equivalent.
        || scan_contains(lower, "do this only once each turn")
    {
        return Some(TriggerConstraint::OncePerTurn);
    }
    if scan_contains(lower, "this ability triggers only once") {
        return Some(TriggerConstraint::OncePerGame);
    }
    if scan_contains(lower, "only during your turn") {
        return Some(TriggerConstraint::OnlyDuringYourTurn);
    }
    // CR 505.1: "during your main phase" restricts the trigger to precombat or postcombat
    // main phase of the controller's turn. Used by actor-side Saddle/Crew triggers
    // (Canyon Vaulter, Reckless Velocitaur).
    if scan_contains(lower, "during your main phase") {
        return Some(TriggerConstraint::OnlyDuringYourMainPhase);
    }
    // CR 603.4: "this ability triggers only the first N times each turn"
    // Delegates to nom_primitives::parse_number for the count (input already lowercase).
    if let Some(rest) = strip_after(lower, "triggers only the first ") {
        if let Ok((_, (n_text, _))) = nom_primitives::split_once_on(rest, " time") {
            if let Ok((_rem, n)) = nom_primitives::parse_number.parse(n_text) {
                return Some(TriggerConstraint::MaxTimesPerTurn { max: n });
            }
        }
    }
    None
}

/// Strip constraint sentences from effect text so they don't produce spurious sub-abilities.
/// The constraint itself is already extracted by `parse_trigger_constraint` from the full text.
fn strip_constraint_sentences(text: &str) -> String {
    let patterns = [
        "this ability triggers only once each turn.",
        "this ability triggers only once each turn",
        "triggers only once each turn.",
        "triggers only once each turn",
        "this ability triggers only once.",
        "this ability triggers only once",
        "this ability triggers only during your turn.",
        "this ability triggers only during your turn",
        "do this only once each turn.",
        "do this only once each turn",
    ];
    let mut result = text.to_string();
    // Case-insensitive match: Oracle text is mixed-case ("This ability triggers...")
    // but patterns are lowercase, so find on lowered text and remove from original.
    let lower_for_static = result.to_lowercase();
    for pattern in &patterns {
        if let Some(pos) = lower_for_static.find(pattern) {
            result.replace_range(pos..pos + pattern.len(), "");
            break; // At most one constraint sentence per trigger
        }
    }
    // Dynamic pattern: "this ability triggers only the first N time(s) each turn."
    // Uses scan_split_at_phrase + split_once_on instead of raw .find() for dispatch.
    let lower = result.to_lowercase();
    if let Some((prefix_text, matched_start)) = scan_split_at_phrase(&lower, |i| {
        tag::<_, _, OracleError<'_>>("this ability triggers only the first ").parse(i)
    }) {
        let start = prefix_text.len();
        if let Ok((_, (_, after_each_turn))) =
            nom_primitives::split_once_on(matched_start, "each turn")
        {
            let end_pos = lower.len() - after_each_turn.len();
            let end_pos = if tag::<_, _, OracleError<'_>>(".")
                .parse(after_each_turn)
                .is_ok()
            {
                end_pos + 1
            } else {
                end_pos
            };
            result = format!("{}{}", &result[..start], &result[end_pos..]);
        }
    }
    let result = result.trim().to_string();
    if result.ends_with('.') {
        result[..result.len() - 1].trim().to_string()
    } else {
        result
    }
}

/// CR 118.12: Detect "unless [player] pays {cost}" in trigger effect text.
/// Returns (cleaned effect text without the unless clause, optional UnlessPayModifier).
///
/// Patterns:
/// - "draw a card unless that player pays {X}, where X is ~ power"
/// - "create a token unless that player pays {2}"
/// - "sacrifice it unless you discard a card at random"  (CR 608.2c — UnlessCost::DiscardCard)
/// - "destroy it unless you sacrifice a creature"        (UnlessCost::Sacrifice)
/// - "draw a card unless you pay 2 life"                 (CR 119.4 — UnlessCost::PayLife)
/// - "sacrifice it unless you pay {E}{E}"                (CR 107.14 — UnlessCost::PayEnergy)
fn extract_unless_pay_modifier(
    text: &str,
    condition_lower: &str,
) -> (String, Option<UnlessPayModifier>) {
    let lower = text.to_lowercase();
    let tp = TextPair::new(text, &lower);
    let Some(unless_pos) = tp.find(" unless ") else {
        return (text.to_string(), None);
    };

    let after_unless = &lower[unless_pos + 8..];

    // CR 608.2c: When the primary effect is itself a discard imperative
    // ("discard a card unless you discard a creature card"), the discard-
    // effect parser (`parse_discard_unless_filter` in oracle_effect/imperative.rs)
    // encodes the unless-clause as a *type qualifier* on the mandatory discard,
    // not as an alternative cost on a different effect. Defer to that path.
    let primary_is_discard = tag::<_, _, OracleError<'_>>("discard ")
        .parse(lower[..unless_pos].trim_start())
        .is_ok();
    if primary_is_discard
        && tag::<_, _, OracleError<'_>>("you discard ")
            .parse(after_unless)
            .is_ok()
    {
        return (text.to_string(), None);
    }

    if let Some((cost, payer)) =
        parse_inferred_pronoun_unless_alt_cost(after_unless, &lower[..unless_pos], condition_lower)
    {
        let cleaned = text[..unless_pos].trim().to_string();
        return (cleaned, Some(UnlessPayModifier { cost, payer }));
    }

    // CR 118.12 + CR 608.2c + CR 119.4: Non-mana alternative costs ("you discard
    // a card", "you sacrifice a [filter]", "you pay N life") map to existing
    // `UnlessCost` variants — the runtime resolver in `engine_payment_choices.rs`
    // owns the payment choice.
    if let Some(cost) = parse_unless_alt_cost(after_unless) {
        let cleaned = text[..unless_pos].trim().to_string();
        return (
            cleaned,
            Some(UnlessPayModifier {
                cost,
                payer: TargetFilter::Controller,
            }),
        );
    }

    let they_pay_result = tag::<_, _, OracleError<'_>>("they pay ")
        .parse(after_unless)
        .ok()
        .and_then(|(rest, _)| {
            infer_pronoun_unless_payer(&lower[..unless_pos], condition_lower)
                .map(|payer| (rest, payer))
        });

    // Parse payer + payment verb as a single combinator: "(payer) pay(s) " → (TargetFilter, &str).
    let payer_result: Result<(&str, TargetFilter), _> = alt((
        value(
            TargetFilter::Controller,
            tag::<_, _, OracleError<'_>>("you pay "),
        ),
        value(
            TargetFilter::TriggeringPlayer,
            nom::sequence::pair(
                alt((tag("that player "), tag("that opponent "))),
                tag("pays "),
            ),
        ),
        value(
            TargetFilter::TriggeringSpellController,
            nom::sequence::pair(tag("its controller "), tag("pays ")),
        ),
    ))
    .parse(after_unless);

    let (cost_str, payer) = match they_pay_result {
        Some((rest, payer)) => (rest, payer),
        None => match payer_result {
            Ok((rest, p)) => (rest, p),
            Err(_) => return (text.to_string(), None),
        },
    };

    // CR 107.14 + CR 202.3: dynamic energy unless-cost ("an amount of {e}
    // equal to its mana value"). Must precede the brace-run cost extraction
    // below, which cannot handle the multi-word "an amount of" prefix.
    if let Some(amount) = super::oracle_effect::parse_dynamic_energy_unless_cost(cost_str) {
        let cleaned = text[..unless_pos].trim().to_string();
        return (
            cleaned,
            Some(UnlessPayModifier {
                cost: AbilityCost::PayEnergy { amount },
                payer,
            }),
        );
    }

    // Extract cost symbols
    let cost_end = cost_str
        .find(|c: char| c != '{' && c != '}' && !c.is_alphanumeric())
        .unwrap_or(cost_str.len());
    let cost_text = cost_str[..cost_end].trim();

    if cost_text.is_empty() || !cost_text.contains('{') {
        return (text.to_string(), None);
    }

    // Determine the cost type
    let cost = if let Some((amount, rest)) =
        super::oracle_effect::parse_fixed_energy_unless_cost(cost_text)
    {
        if !rest.trim().is_empty() {
            return (text.to_string(), None);
        }
        AbilityCost::PayEnergy {
            amount: QuantityExpr::Fixed {
                value: amount as i32,
            },
        }
    } else if cost_text == "{x}" || cost_text == "{X}" {
        // Check for "where X is" clause
        let remainder = &cost_str[cost_end..];
        if let Some(quantity) = parse_where_x_is_trigger(remainder) {
            AbilityCost::ManaDynamic { quantity }
        } else {
            return (text.to_string(), None);
        }
    } else {
        let mana_cost = crate::database::mtgjson::parse_mtgjson_mana_cost(cost_text);
        if mana_cost == crate::types::mana::ManaCost::NoCost
            || mana_cost == crate::types::mana::ManaCost::zero()
        {
            return (text.to_string(), None);
        }
        if let Some(cost) =
            super::oracle_effect::parse_unless_for_each_payment(&cost_str[cost_end..], &mana_cost)
        {
            cost
        } else {
            AbilityCost::Mana { cost: mana_cost }
        }
    };

    // Payer was already determined by the combinator above.

    // Strip the unless clause from the effect text
    let cleaned = text[..unless_pos].trim().to_string();

    (cleaned, Some(UnlessPayModifier { cost, payer }))
}

fn condition_introduces_scoped_phase_player(cond_lower: &str) -> bool {
    let phase_scope = preceded(
        tag::<_, _, OracleError<'_>>("at the beginning of "),
        alt((
            tag("each player's "),
            tag("each players "),
            tag("each opponent's "),
            tag("each opponents "),
        )),
    )
    .parse(cond_lower);

    let Ok((phase_text, _)) = phase_scope else {
        return false;
    };

    scan_for_phase(phase_text).is_some()
}

fn effect_references_that_player(effect_before_unless: &str) -> bool {
    scan_contains(effect_before_unless, "that player ")
        || scan_contains(effect_before_unless, "that opponent ")
        || scan_contains(effect_before_unless, "to that player")
        || scan_contains(effect_before_unless, "to that opponent")
}

fn infer_pronoun_unless_payer(
    effect_before_unless: &str,
    condition_lower: &str,
) -> Option<TargetFilter> {
    // CR 503.1a + CR 603.2: "At the beginning of each player's upkeep, that
    // player ... unless they pay" refers to the active player for that phase.
    if condition_introduces_scoped_phase_player(condition_lower)
        && effect_references_that_player(effect_before_unless)
    {
        return Some(TargetFilter::Controller);
    }
    // CR 603.2 + CR 118.12: "that player/that opponent ... unless they pay"
    // refers to the player from the triggering event.
    if effect_references_that_player(effect_before_unless) {
        return Some(TargetFilter::TriggeringPlayer);
    }
    // CR 608.2c: in "each opponent [does X] unless they pay", the lowered
    // ability has `player_scope = Opponent`; runtime binds `Controller` to
    // each scoped opponent before presenting the unless-payment choice.
    if scan_contains(effect_before_unless, "each opponent ") {
        return Some(TargetFilter::Controller);
    }
    if scan_contains(effect_before_unless, "creature's controller ") {
        return Some(TargetFilter::ParentTargetController);
    }
    None
}

fn parse_inferred_pronoun_unless_alt_cost(
    after_unless: &str,
    effect_before_unless: &str,
    condition_lower: &str,
) -> Option<(AbilityCost, TargetFilter)> {
    let cost = if let Ok((rest, _)) =
        tag::<_, _, OracleError<'_>>("they discard ").parse(after_unless)
    {
        parse_unless_discard_cost(rest)?
    } else if let Ok((rest, _)) = tag::<_, _, OracleError<'_>>("they pay ").parse(after_unless) {
        parse_unless_life_cost(rest)?
    } else {
        return None;
    };
    let payer = infer_pronoun_unless_payer(effect_before_unless, condition_lower)?;
    Some((cost, payer))
}

fn parse_unless_life_cost(rest: &str) -> Option<AbilityCost> {
    let (amount, after_num) = parse_number(rest)?;
    if tag::<_, _, OracleError<'_>>("life")
        .parse(after_num.trim_start())
        .is_ok()
    {
        return Some(AbilityCost::PayLife {
            amount: QuantityExpr::Fixed {
                value: amount as i32,
            },
        });
    }
    None
}

fn parse_unless_discard_cost(discard_tail: &str) -> Option<AbilityCost> {
    let trailing = discard_tail.trim().trim_end_matches('.').trim();
    let trailing = alt((
        tag::<_, _, OracleError<'_>>("a "),
        tag::<_, _, OracleError<'_>>("an "),
    ))
    .parse(trailing)
    .map(|(rest, _)| rest.trim())
    .unwrap_or(trailing);
    if !trailing.is_empty() {
        if let Ok((rest, _)) = tag::<_, _, OracleError<'_>>("card").parse(trailing) {
            let rest = rest.trim().trim_end_matches('.').trim();
            if rest.is_empty()
                || tag::<_, _, OracleError<'_>>("at random")
                    .parse(rest)
                    .is_ok()
            {
                return Some(AbilityCost::Discard {
                    count: QuantityExpr::Fixed { value: 1 },
                    filter: None,
                    random: false,
                    self_ref: false,
                });
            }
        }
        if let Some(filter) = super::oracle_effect::imperative::parse_discard_card_filter(trailing)
        {
            return Some(AbilityCost::Discard {
                count: QuantityExpr::Fixed { value: 1 },
                filter: Some(filter),
                random: false,
                self_ref: false,
            });
        }
    }
    None
}

/// CR 118.12 + CR 608.2c + CR 119.4: Recognize non-mana "unless" alternative
/// costs that map to existing `UnlessCost` variants. Operates on the lowercased
/// text immediately after `" unless "`.
///
/// Patterns recognized:
/// - `you discard a card[ at random][.]`     → `UnlessCost::DiscardCard`
/// - `you sacrifice [N] [filter][.]`         → `UnlessCost::Sacrifice { count, filter }`
/// - `you pay N life[.]`                     → `UnlessCost::PayLife { amount }`
///
/// Returns `None` for any other shape (mana costs and unknown forms fall
/// through to the existing mana-cost path in `extract_unless_pay_modifier`).
///
/// FIDELITY NOTE: `UnlessCost::DiscardCard` does not currently model "at random"
/// — the engine resolves via `WaitingFor::WardDiscardChoice` (player-chosen).
/// This is a known sub-fidelity gap (Balduvian Horde class). Post the
/// 2026-05-09 fold, the `random: bool` field on `AbilityCost::Discard` is
/// the natural home for this; wiring it into the runtime is future work.
fn parse_unless_alt_cost(after_unless: &str) -> Option<AbilityCost> {
    // "you discard a card" — match prefix, accept any trailing modifiers
    // ("at random", trailing punctuation) since the caller strips the entire
    // unless-clause wholesale.
    if let Ok((rest, _)) = tag::<_, _, OracleError<'_>>("you discard ").parse(after_unless) {
        return parse_unless_discard_cost(rest);
    }

    // "you pay N life" / "you pay N life." — life amount is bare integer.
    if let Ok((rest, _)) = tag::<_, _, OracleError<'_>>("you pay ").parse(after_unless) {
        if let Some(cost) = parse_unless_life_cost(rest) {
            return Some(cost);
        }
    }

    // "you sacrifice [count] [filter]" — delegates filter parsing to the shared
    // `parse_target` building block (oracle_target). Count is optional and
    // defaults to 1; articles ("a"/"an") are absorbed by `parse_target` via
    // its "target {phrase}" entry point.
    if let Ok((rest, _)) = tag::<_, _, OracleError<'_>>("you sacrifice ").parse(after_unless) {
        return parse_unless_sacrifice_filter(rest);
    }

    // CR 118.12: "you return [count] [filter] [you control] to its/their owner's hand"
    if let Ok((rest, _)) = tag::<_, _, OracleError<'_>>("you return ").parse(after_unless) {
        return parse_unless_return_to_hand(rest);
    }

    None
}

/// CR 118.12 + CR 118.12a: Parse a chain of "they"-pronoun alternative
/// payment verbs joined by " or " into either a single `AbilityCost` (one
/// branch) or `AbilityCost::OneOf { costs }` (two or more branches). Used by
/// the resolution-time "[Effect] unless they X or Y" pattern (Tergrid's
/// Lantern: "Target player loses 3 life unless they sacrifice a nonland
/// permanent of their choice or discard a card.").
///
/// The "of their choice" qualifier on `parse_unless_they_sacrifice_filter`
/// is structurally redundant — the runtime sacrifice-cost prompt
/// (`WaitingFor::WardSacrificeChoice`) already lets the paying player pick
/// the permanent — but the Oracle text often includes it for clarity and
/// must be absorbed to avoid leaving unparsed tail text on the cost.
///
/// `after_unless` is the lowercase tail immediately after the literal
/// `"unless "` prefix has been consumed; the sole caller
/// (`extract_resolution_unless_pay_modifier`) strips the entire unless
/// clause from the surrounding effect text using its own pre-computed
/// `before_unless` offset, so this combinator only needs to return the
/// parsed cost. Returns `None` if no "they X" branch is recognized.
pub(crate) fn parse_unless_they_alt_cost_chain(after_unless: &str) -> Option<AbilityCost> {
    let (first_cost, after_first) = parse_unless_they_single_alt_cost(after_unless)?;

    // CR 118.12a: Greedily consume " or {alt_cost}" continuations to build
    // a disjunctive `OneOf`. English elides the second-clause subject
    // pronoun ("unless they sacrifice X or discard Y" — the "they" before
    // "discard" is implicit). `parse_unless_they_continuation` accepts
    // either form, dispatching by verb.
    let mut costs = vec![first_cost];
    let mut remainder = after_first;
    while let Ok((after_or, _)) = tag::<_, _, OracleError<'_>>(" or ").parse(remainder) {
        let Some((next_cost, after_next)) = parse_unless_they_continuation(after_or) else {
            break;
        };
        costs.push(next_cost);
        remainder = after_next;
    }

    if costs.len() == 1 {
        costs.pop()
    } else {
        Some(AbilityCost::OneOf { costs })
    }
}

/// CR 118.12: Parse a single "they {verb} ..." alternative payment branch.
/// Mirrors the verb set of `parse_unless_alt_cost` but with the "they"
/// pronoun (the target player) instead of "you" (the resolving ability's
/// controller). Returns the cost and the unconsumed tail.
fn parse_unless_they_single_alt_cost(input: &str) -> Option<(AbilityCost, &str)> {
    // CR 118.12a: The first branch requires an explicit payer pronoun — it
    // anchors the unless-clause to the paying player. The pronoun axis is
    // parameterized: "they " (Tergrid's Lantern — a player target) and "that
    // player " / "that opponent " (Nicol Bolas, Torment of Hailfire — the
    // per-opponent scoped player). All forms resolve to the same payer, so
    // only the verb dispatch downstream needs the remainder. Continuation
    // branches omit the pronoun (English elision).
    let (rest, _) = alt((
        tag::<_, _, OracleError<'_>>("they "),
        tag("that player "),
        tag("that opponent "),
    ))
    .parse(input)
    .ok()?;
    parse_unless_they_branch_by_verb(rest)
}

/// CR 118.12a: Parse the second-or-later branch of a "unless they X or Y"
/// chain. English elides the repeated subject pronoun, so this combinator
/// dispatches on the bare verb (without the "they " prefix). The first
/// branch still requires "they " — see `parse_unless_they_single_alt_cost`.
fn parse_unless_they_continuation(input: &str) -> Option<(AbilityCost, &str)> {
    // Be tolerant of an explicitly re-stated "they " prefix; some Oracle
    // texts do repeat it.
    let rest = tag::<_, _, OracleError<'_>>("they ")
        .parse(input)
        .map(|(r, _)| r)
        .unwrap_or(input);
    parse_unless_they_branch_by_verb(rest)
}

/// CR 118.12: Verb dispatch shared by the first branch and continuation
/// branches of a "unless they X [or Y]" chain. Operates on the slice
/// immediately after the (consumed) "they " pronoun.
fn parse_unless_they_branch_by_verb(input: &str) -> Option<(AbilityCost, &str)> {
    // CR 118.12a: Accept both the base verb form ("they sacrifice") and the
    // third-person singular "-s" form ("that player sacrifices") — the verb
    // tense varies with the payer subject. Composed as a per-verb axis.
    // CR 701.21: "sacrifice(s) a [filter] [of their choice]"
    if let Ok((rest, _)) = alt((
        tag::<_, _, OracleError<'_>>("sacrifices "),
        tag("sacrifice "),
    ))
    .parse(input)
    {
        let (cost, after) = parse_unless_they_sacrifice_filter(rest)?;
        return Some((cost, after));
    }
    // CR 701.9: "discard(s) a card[ at random]"
    if let Ok((rest, _)) =
        alt((tag::<_, _, OracleError<'_>>("discards "), tag("discard "))).parse(input)
    {
        let (cost, after) = parse_unless_they_discard_cost(rest)?;
        return Some((cost, after));
    }
    // CR 119.4: "pay(s) N life"
    if let Ok((rest, _)) = alt((tag::<_, _, OracleError<'_>>("pays "), tag("pay "))).parse(input) {
        let (cost, after) = parse_unless_they_pay_life(rest)?;
        return Some((cost, after));
    }
    None
}

/// CR 701.21 + CR 118.12: Parse the tail of "they sacrifice ..." preserving
/// the unconsumed remainder. Mirrors `parse_unless_sacrifice_filter` but
/// stops at " or " / sentence boundary instead of consuming to EOL, so a
/// chained second branch ("or discard a card") can be parsed by the
/// outer combinator.
fn parse_unless_they_sacrifice_filter(input: &str) -> Option<(AbilityCost, &str)> {
    // Locate the branch boundary: " or " (chained branch) or "." / EOL
    // (clause terminator). Without this bounded slice, the inner
    // `parse_target` would consume the entire remainder and the chain
    // combinator would never see " or ".
    let boundary = unless_branch_boundary(input);
    let branch_text = input[..boundary].trim();
    let after = &input[boundary..];

    // Strip the redundant "of their choice" trailing qualifier — the
    // runtime sacrifice-cost prompt already lets the paying player choose.
    let branch_text = branch_text
        .strip_suffix(" of their choice") // allow-noncombinator: structural cleanup on a pre-tokenized chunk bounded by unless_branch_boundary; not parsing dispatch.
        .unwrap_or(branch_text)
        .trim();
    if branch_text.is_empty() {
        return None;
    }

    // Strip leading article so `parse_target("target <phrase>")` reaches
    // the type-phrase arm.
    let stripped = alt((tag::<_, _, OracleError<'_>>("a "), tag("an ")))
        .parse(branch_text)
        .map(|(rest, _)| rest)
        .unwrap_or(branch_text);
    let target_phrase = format!("target {stripped}");
    let (filter, remainder) = super::oracle_target::parse_target(&target_phrase);
    if matches!(filter, TargetFilter::Any) || !remainder.trim().is_empty() {
        return None;
    }
    // CR 109.4 + CR 115.1 + CR 118.12a: "they sacrifice" pins the sacrificed
    // permanent to the *payer* — the targeted player on the enclosing
    // activated/triggered ability. `ControllerRef::TargetPlayer` reads the
    // first `TargetRef::Player` from `ability.targets` at resolution time,
    // which is exactly the payer the unless-cost surfaces to. Mirrors the
    // "you sacrifice" branch's controller treatment in
    // `parse_unless_sacrifice_filter` (which carries `ControllerRef::You`
    // via the actor dispatch upstream); for the "they" form the actor *is*
    // the target player and we stamp it explicitly here.
    let filter = add_controller(filter, ControllerRef::TargetPlayer);
    Some((
        AbilityCost::Sacrifice {
            target: filter,
            count: 1,
        },
        after,
    ))
}

/// CR 701.9 + CR 118.12: Parse the tail of "they discard ..." preserving
/// the unconsumed remainder. Mirrors `parse_unless_discard_cost` but stops
/// at the branch boundary.
fn parse_unless_they_discard_cost(input: &str) -> Option<(AbilityCost, &str)> {
    let boundary = unless_branch_boundary(input);
    let branch_text = input[..boundary].trim();
    let after = &input[boundary..];
    if branch_text.is_empty() {
        return None;
    }
    // Strip article
    let stripped = alt((tag::<_, _, OracleError<'_>>("a "), tag("an ")))
        .parse(branch_text)
        .map(|(rest, _)| rest)
        .unwrap_or(branch_text);
    // Plain "card" / "card at random"
    if let Ok((rest, _)) = tag::<_, _, OracleError<'_>>("card").parse(stripped) {
        let rest = rest.trim();
        if rest.is_empty()
            || tag::<_, _, OracleError<'_>>("at random")
                .parse(rest)
                .is_ok()
        {
            return Some((
                AbilityCost::Discard {
                    count: QuantityExpr::Fixed { value: 1 },
                    filter: None,
                    random: false,
                    self_ref: false,
                },
                after,
            ));
        }
    }
    // Typed filter ("nonland card", "creature card", etc.)
    if let Some(filter) = super::oracle_effect::imperative::parse_discard_card_filter(stripped) {
        return Some((
            AbilityCost::Discard {
                count: QuantityExpr::Fixed { value: 1 },
                filter: Some(filter),
                random: false,
                self_ref: false,
            },
            after,
        ));
    }
    None
}

/// CR 119.4 + CR 118.12: Parse the tail of "they pay N life" preserving
/// the unconsumed remainder.
fn parse_unless_they_pay_life(input: &str) -> Option<(AbilityCost, &str)> {
    let (amount, after_num) = parse_number(input)?;
    let trimmed = after_num.trim_start();
    if let Ok((rest, _)) = tag::<_, _, OracleError<'_>>("life").parse(trimmed) {
        return Some((
            AbilityCost::PayLife {
                amount: QuantityExpr::Fixed {
                    value: amount as i32,
                },
            },
            rest,
        ));
    }
    None
}

/// Locate the byte offset where the current unless-clause branch ends.
/// The branch terminates at the first " or " (chained second branch) or
/// at sentence-ending punctuation. The returned offset is the start of
/// either " or " (for chained branches) or the period/end-of-input.
///
/// The chain combinator accepts both "they {verb}" and bare "{verb}"
/// continuations (English elision), so the boundary check accepts either
/// shape on the post-" or " slice. A trailing " or X" where X is not a
/// recognized continuation verb (e.g. a noun disjunction inside a filter —
/// "a creature or artifact") falls through to the sentence terminator.
fn unless_branch_boundary(input: &str) -> usize {
    // Walk every " or " occurrence — the *first* one that is followed by a
    // recognized continuation is the branch boundary. Earlier " or "
    // matches inside a filter phrase ("a creature or artifact") are
    // skipped.
    // Combinator: recognize a continuation verb prefix, optionally with a
    // preceding "they ". The chain combinator accepts both shapes; the
    // boundary scan must mirror that contract so its split point lines up.
    fn parse_continuation_verb_head(input: &str) -> OracleResult<'_, ()> {
        let (rest, _) = opt(tag::<_, _, OracleError<'_>>("they ")).parse(input)?;
        // Mirror `parse_unless_they_branch_by_verb`'s base + "-s" verb axis so
        // the boundary scan splits at the same point the chain combinator does.
        let (rest, _) = alt((
            value((), tag::<_, _, OracleError<'_>>("sacrifices ")),
            value((), tag("sacrifice ")),
            value((), tag("discards ")),
            value((), tag("discard ")),
            value((), tag("pays ")),
            value((), tag("pay ")),
        ))
        .parse(rest)?;
        Ok((rest, ()))
    }

    let mut search_start = 0;
    while let Ok((_, (before, after))) =
        nom_primitives::split_once_on(&input[search_start..], " or ")
    {
        if parse_continuation_verb_head(after).is_ok() {
            return search_start + before.len();
        }
        search_start += before.len() + 4; // advance past this " or "
    }
    // Sentence terminator. Use nom `take_until` to honor the same word-
    // boundary discipline as the rest of the combinator chain — the
    // returned prefix length is the boundary.
    if let Ok((_, before_dot)) =
        nom::bytes::complete::take_until::<_, _, OracleError<'_>>(".").parse(input)
    {
        return before_dot.len();
    }
    input.len()
}

/// Parse the tail of "you sacrifice ..." into an `AbilityCost::Sacrifice`.
/// Expects lowercased text. Accepts:
/// - `a creature` / `an artifact` / `a [type] you control`
/// - `two creatures` / `three lands`
/// - terminal sentence punctuation
fn parse_unless_sacrifice_filter(rest: &str) -> Option<AbilityCost> {
    // Trim trailing sentence punctuation so it doesn't leak into parse_target.
    let trimmed = rest.trim().trim_end_matches('.').trim();
    if trimmed.is_empty() {
        return None;
    }

    // Extract count: leading numeric word > 1 keeps as count, otherwise count=1.
    let (count, filter_text) = if let Some((n, after_num)) = parse_number(trimmed) {
        if n > 1 {
            (n, after_num.trim().to_string())
        } else {
            // n == 1 from a literal "1" — uncommon; treat as count=1 with
            // remainder as the filter phrase.
            (1u32, after_num.trim().to_string())
        }
    } else {
        // No count — strip leading article via nom combinator so parse_target
        // receives a bare type phrase (parse_target only strips "a"/"an" when
        // they precede "target", not when they precede a type word).
        let stripped = alt((tag::<_, _, OracleError<'_>>("a "), tag("an ")))
            .parse(trimmed)
            .map(|(rest, _)| rest)
            .unwrap_or(trimmed);
        (1u32, stripped.to_string())
    };

    if filter_text.is_empty() {
        return None;
    }

    // Delegate filter parsing to the shared building block. The `target {...}`
    // wrapper triggers the article-stripping + type-phrase path.
    let target_phrase = format!("target {}", filter_text);
    let (filter, remainder) = super::oracle_target::parse_target(&target_phrase);
    if matches!(filter, TargetFilter::Any) {
        return None;
    }
    // Reject if parse_target left meaningful unconsumed text (signals the
    // filter phrase wasn't fully understood — e.g. "two creatures with flying"
    // where "with flying" isn't absorbed; better to fall through than to
    // emit a partial filter).
    if !remainder.trim().is_empty() {
        return None;
    }

    Some(AbilityCost::Sacrifice {
        target: filter,
        count,
    })
}

/// CR 118.12: Parse "you return [count] [filter] [you control] to its/their
/// owner's hand" into `AbilityCost::ReturnToHand`. Expects lowercased text after
/// "you return ". Handles patterns like:
/// - "an artifact you control to its owner's hand"
/// - "two forests you control to their owner's hand"
/// - "another creature you control to its owner's hand"
/// - "an untapped island you control to its owner's hand"
/// - "a non-lair land you control to its owner's hand"
fn parse_unless_return_to_hand(rest: &str) -> Option<AbilityCost> {
    let to_pos = rest.find(" to ")?; // allow-noncombinator: delimiter split on pre-tokenized unless clause text
    let filter_part = rest[..to_pos].trim().trim_end_matches('.').trim();
    if filter_part.is_empty() {
        return None;
    }

    // Extract count: leading numeric word > 1 keeps as count, otherwise count=1.
    let (count, filter_text) = if let Some((n, after_num)) = parse_number(filter_part) {
        if n > 1 {
            (n, after_num.trim().to_string())
        } else {
            (1u32, after_num.trim().to_string())
        }
    } else {
        (1u32, filter_part.to_string())
    };

    if filter_text.is_empty() {
        return None;
    }

    // Delegate to parse_target which handles "another", articles, type phrases,
    // "you control" (controller suffix), and "from your graveyard" (zone suffix).
    let target_phrase = format!("target {}", filter_text);
    let (filter, remainder) = super::oracle_target::parse_target(&target_phrase);
    if matches!(filter, TargetFilter::Any) {
        return None;
    }
    if !remainder.trim().is_empty() {
        return None;
    }

    // Derive from_zone from FilterProp::InZone that parse_target absorbed from zone suffixes.
    let from_zone = filter.extract_in_zone();

    // Ensure controller scoping — parse_target sets it from "you control" but
    // some forms omit it (e.g., "a basic land card from your graveyard").
    let filter = match &filter {
        TargetFilter::Typed(tf) if tf.controller.is_some() => filter,
        _ => TargetFilter::And {
            filters: vec![TargetFilter::Controller, filter],
        },
    };

    Some(AbilityCost::ReturnToHand {
        count,
        filter: Some(filter),
        from_zone,
    })
}

/// Parse "where X is ~'s power" / "where X is this creature's power" etc.
/// Delegates to `nom_quantity::parse_quantity_ref` for the value reference after
/// stripping the "where X is" prefix.
fn parse_where_x_is_trigger(text: &str) -> Option<QuantityExpr> {
    let trimmed = text.trim().trim_start_matches(',').trim();
    let (rest, ()) = alt((
        value((), tag::<_, _, OracleError<'_>>("where x is ")),
        value((), tag("where X is ")),
    ))
    .parse(trimmed)
    .ok()?;
    let rest_lower = rest.to_lowercase();
    // Try nom quantity ref combinator first for common patterns
    if let Ok((_rem, qty)) = nom_quantity::parse_quantity_ref.parse(&rest_lower) {
        return Some(QuantityExpr::Ref { qty });
    }
    // Fall through to keyword-based matching for less common patterns
    if scan_contains(&rest_lower, "power") {
        Some(QuantityExpr::Ref {
            qty: QuantityRef::Power {
                scope: crate::types::ability::ObjectScope::Source,
            },
        })
    } else if scan_contains(&rest_lower, "toughness") {
        Some(QuantityExpr::Ref {
            qty: QuantityRef::Toughness {
                scope: crate::types::ability::ObjectScope::Source,
            },
        })
    } else {
        None
    }
}

/// CR 603.4: Rewrite any `FilterProp::Another` inside a `TargetFilter` to
/// `FilterProp::OtherThanTriggerObject` for trigger-scope quantity
/// comparisons. Recurses through `And`/`Or`/`Not` combinators and `Typed`
/// property lists so nested filters are covered.
fn substitute_another_in_filter(filter: &TargetFilter) -> TargetFilter {
    match filter {
        TargetFilter::Typed(tf) => {
            let mut rewritten = tf.clone();
            for prop in &mut rewritten.properties {
                if matches!(prop, FilterProp::Another) {
                    *prop = FilterProp::OtherThanTriggerObject;
                }
            }
            TargetFilter::Typed(rewritten)
        }
        TargetFilter::Not { filter: inner } => TargetFilter::Not {
            filter: Box::new(substitute_another_in_filter(inner)),
        },
        TargetFilter::And { filters } => TargetFilter::And {
            filters: filters.iter().map(substitute_another_in_filter).collect(),
        },
        TargetFilter::Or { filters } => TargetFilter::Or {
            filters: filters.iter().map(substitute_another_in_filter).collect(),
        },
        other => other.clone(),
    }
}

/// CR 603.4: Rewrite `Another` inside any `ObjectCount` / `ObjectCountDistinct`
/// or `Aggregate` filter carried by a `QuantityExpr`. Leaves non-population
/// refs untouched.
fn substitute_another_in_expr(expr: &QuantityExpr) -> QuantityExpr {
    match expr {
        QuantityExpr::Ref {
            qty: QuantityRef::ObjectCount { filter },
        } => QuantityExpr::Ref {
            qty: QuantityRef::ObjectCount {
                filter: substitute_another_in_filter(filter),
            },
        },
        QuantityExpr::Ref {
            qty: QuantityRef::ObjectCountDistinct { filter, qualities },
        } => QuantityExpr::Ref {
            qty: QuantityRef::ObjectCountDistinct {
                filter: substitute_another_in_filter(filter),
                qualities: qualities.clone(),
            },
        },
        // CR 603.4 + CR 109.3: Aggregate populations are also "object
        // populations" in the same sense as ObjectCount — when an
        // intervening-if references an aggregate over "each other <type>",
        // the exclusion must be trigger-relative, not source-relative.
        QuantityExpr::Ref {
            qty:
                QuantityRef::Aggregate {
                    function,
                    property,
                    filter,
                },
        } => QuantityExpr::Ref {
            qty: QuantityRef::Aggregate {
                function: *function,
                property: *property,
                filter: substitute_another_in_filter(filter),
            },
        },
        QuantityExpr::Offset { inner, offset } => QuantityExpr::Offset {
            inner: Box::new(substitute_another_in_expr(inner)),
            offset: *offset,
        },
        QuantityExpr::DivideRounded {
            inner,
            divisor,
            rounding,
        } => QuantityExpr::DivideRounded {
            inner: Box::new(substitute_another_in_expr(inner)),
            divisor: *divisor,
            rounding: *rounding,
        },
        QuantityExpr::Multiply { factor, inner } => QuantityExpr::Multiply {
            factor: *factor,
            inner: Box::new(substitute_another_in_expr(inner)),
        },
        QuantityExpr::Difference { left, right } => QuantityExpr::Difference {
            left: Box::new(substitute_another_in_expr(left)),
            right: Box::new(substitute_another_in_expr(right)),
        },
        other => other.clone(),
    }
}

/// Bridge a `StaticCondition` (from the nom condition parser) to a `TriggerCondition`.
///
/// Parallel to `static_condition_to_ability_condition` in `oracle_effect/mod.rs`.
/// Returns `None` for variants that have no `TriggerCondition` equivalent —
/// the caller falls through to the next strategy.
fn static_condition_to_trigger_condition(sc: &StaticCondition) -> Option<TriggerCondition> {
    match sc {
        StaticCondition::DuringYourTurn => Some(TriggerCondition::DuringPlayersTurn {
            player: PlayerFilter::Controller,
        }),
        StaticCondition::DayNightIs { .. } => None,

        // CR 608.2c: Quantity comparisons map 1:1 (same fields). The only
        // asymmetry is the `Another` → `OtherThanTriggerObject` substitution
        // inside filters: "other <type>" in a trigger's intervening-if means
        // "other than the triggering object" (CR 603.4, Valakut's ruling), not
        // "other than the ability source". The substitution is scoped to this
        // bridge so static-context `Another` (e.g. a land's ETB "if you control
        // two or more other lands" where source == the land that just entered)
        // keeps its source-exclusion semantics.
        StaticCondition::QuantityComparison {
            lhs,
            comparator,
            rhs,
        } => Some(TriggerCondition::QuantityComparison {
            lhs: substitute_another_in_expr(lhs),
            comparator: *comparator,
            rhs: substitute_another_in_expr(rhs),
        }),

        // CR 702.178a: Speed condition.
        StaticCondition::HasMaxSpeed => Some(TriggerCondition::HasMaxSpeed),

        // CR 103.1: Starting-player status — 1:1 bridge. Radiant Smite's
        // Cycling trigger reads "if you weren't the starting player".
        StaticCondition::WasStartingPlayer { controller } => {
            Some(TriggerCondition::WasStartingPlayer {
                controller: controller.clone(),
            })
        }

        // CR 702.185c: "a spell was warped this turn" — 1:1 bridge (same `variant`).
        StaticCondition::SpellCastWithVariantThisTurn { variant } => {
            Some(TriggerCondition::SpellCastWithVariantThisTurn {
                variant: *variant,
            })
        }

        // CR 716.2a: Class level condition.
        StaticCondition::ClassLevelGE { level } => {
            Some(TriggerCondition::ClassLevelGE { level: *level })
        }

        // IsPresent with filter → ControlsType (presence check).
        StaticCondition::IsPresent { filter } => {
            let f = filter.clone()?;
            Some(TriggerCondition::ControlsType { filter: f })
        }

        // Not combinator — handle common negation patterns.
        StaticCondition::Not { condition } => match condition.as_ref() {
            StaticCondition::DuringYourTurn => Some(TriggerCondition::Not {
                condition: Box::new(TriggerCondition::DuringPlayersTurn {
                    player: PlayerFilter::Controller,
                }),
            }),
            // Negate a quantity comparison by flipping the comparator.
            // Apply the same `Another` → `OtherThanTriggerObject` substitution
            // as the affirmative branch (CR 603.4).
            StaticCondition::QuantityComparison {
                lhs,
                comparator,
                rhs,
            } => Some(TriggerCondition::QuantityComparison {
                lhs: substitute_another_in_expr(lhs),
                comparator: comparator.negate(),
                rhs: substitute_another_in_expr(rhs),
            }),
            // Negate an IsPresent → ObjectCount == 0
            StaticCondition::IsPresent { filter } => {
                let f = filter.clone().unwrap_or(TargetFilter::Any);
                Some(TriggerCondition::QuantityComparison {
                    lhs: QuantityExpr::Ref {
                        qty: QuantityRef::ObjectCount { filter: f },
                    },
                    comparator: Comparator::EQ,
                    rhs: QuantityExpr::Fixed { value: 0 },
                })
            }
            // CR 110.5b: Not(SourceIsTapped) → source is untapped.
            StaticCondition::SourceIsTapped => Some(TriggerCondition::Not {
                condition: Box::new(TriggerCondition::SourceIsTapped),
            }),
            // CR 103.1: "you weren't the starting player" → Not(WasStartingPlayer).
            StaticCondition::WasStartingPlayer { controller } => Some(TriggerCondition::Not {
                condition: Box::new(TriggerCondition::WasStartingPlayer {
                    controller: controller.clone(),
                }),
            }),
            // CR 702.185c: "no spell was warped this turn" → Not(SpellCastWithVariantThisTurn).
            StaticCondition::SpellCastWithVariantThisTurn { variant } => {
                Some(TriggerCondition::Not {
                    condition: Box::new(TriggerCondition::SpellCastWithVariantThisTurn {
                        variant: *variant,
                    }),
                })
            }
            _ => None,
        },

        // And/Or — recursive. If ANY child is unmappable, the entire compound
        // returns None to avoid producing a less-restrictive condition.
        StaticCondition::And { conditions } => {
            let mapped: Option<Vec<_>> = conditions
                .iter()
                .map(static_condition_to_trigger_condition)
                .collect();
            Some(TriggerCondition::And {
                conditions: mapped?,
            })
        }
        StaticCondition::Or { conditions } => {
            let mapped: Option<Vec<_>> = conditions
                .iter()
                .map(static_condition_to_trigger_condition)
                .collect();
            Some(TriggerCondition::Or {
                conditions: mapped?,
            })
        }

        // CR 725.1: Monarch status bridges directly.
        StaticCondition::IsMonarch => Some(TriggerCondition::IsMonarch),
        // CR 725.1: "there is no monarch" bridges directly.
        StaticCondition::NoMonarch => Some(TriggerCondition::NoMonarch),
        // CR 702.131a: City's Blessing bridges directly.
        StaticCondition::HasCityBlessing => Some(TriggerCondition::HasCityBlessing),
        // CR 110.5b: Source tapped state bridges for trigger conditions like
        // "At the beginning of your upkeep, if this land is tapped, ..."
        StaticCondition::SourceIsTapped => Some(TriggerCondition::SourceIsTapped),
        // CR 113.6b: Source zone bridges for trigger conditions like
        // "At the beginning of your upkeep, if this card is in your graveyard, ..."
        StaticCondition::SourceInZone { zone } => {
            Some(TriggerCondition::SourceInZone { zone: *zone })
        }
        // CR 122.1: Source counter conditions bridge directly for trigger
        // intervening-if predicates such as Suspend's "if this card is suspended".
        StaticCondition::HasCounters {
            counters,
            minimum,
            maximum,
        } => Some(TriggerCondition::HasCounters {
            counters: counters.clone(),
            minimum: *minimum,
            maximum: *maximum,
        }),

        // Variants with no TriggerCondition equivalent (combat-only / source-state / cost).
        StaticCondition::SourceEnteredThisTurn
        | StaticCondition::IsRingBearer
        | StaticCondition::RingLevelAtLeast { .. }
        | StaticCondition::DevotionGE { .. }
        | StaticCondition::ChosenColorIs { .. }
        | StaticCondition::SpeedGE { .. }
        | StaticCondition::RecipientHasCounters { .. }
        | StaticCondition::SourceMatchesFilter { .. }
        | StaticCondition::DefendingPlayerControls { .. }
        | StaticCondition::SourceAttackingAlone
        | StaticCondition::SourceIsAttacking
        | StaticCondition::SourceIsBlocking
        | StaticCondition::SourceIsBlocked
        | StaticCondition::SourceIsEquipped
        | StaticCondition::SourceIsPaired
        | StaticCondition::SourceIsMonstrous
        | StaticCondition::SourceAttachedToCreature
        | StaticCondition::OpponentPoisonAtLeast { .. }
        | StaticCondition::UnlessPay { .. }
        | StaticCondition::Unrecognized { .. }
        | StaticCondition::EnchantedIsFaceDown
        | StaticCondition::SourceControllerEquals { .. }
        // CR 702.166a: Bargain payment is a cost-determination predicate with no
        // intervening-if (`TriggerCondition`) equivalent.
        | StaticCondition::AdditionalCostPaid
        | StaticCondition::None => None,

        // CR 309.7: Dungeon completion bridges directly.
        StaticCondition::CompletedADungeon => {
            Some(TriggerCondition::CompletedDungeon { specific: None })
        }

        // CR 903.3: Commander control bridges directly, carrying the ownership scope.
        StaticCondition::ControlsCommander { ownership } => {
            Some(TriggerCondition::ControlsCommander {
                ownership: *ownership,
            })
        }
    }
}

/// Extract an intervening-if condition from effect text.
/// Returns (cleaned effect text, optional condition).
///
/// Architecture: delegates to `parse_inner_condition` (the shared nom combinator)
/// via the `static_condition_to_trigger_condition` bridge for ALL game-state
/// conditions. Only source-referential patterns that require the trigger source
/// as context ("if you cast it", "if it's attacking", ninjutsu costs, "if it was a
/// [type]", defending player) are handled directly here.
fn extract_if_condition(text: &str) -> (String, Option<TriggerCondition>) {
    let lower = text.to_lowercase();
    let tp = TextPair::new(text, &lower);

    // CR 603.4: Only a true intervening-if is hoisted to the trigger-level condition.
    // A trigger-level `if` is one that IMMEDIATELY follows the trigger condition
    // clause ("When X, if Y, Z"). When the `if` is introduced by "then"
    // ("effect. Then if Y, effect2") the condition scopes only to the then-clause's
    // sub_ability and is attached by `strip_leading_general_conditional` during
    // per-clause effect parsing (parser/oracle_effect/conditions.rs).
    //
    // Guard: if the FIRST `if ` in the effect text belongs to a "then if" clause,
    // skip hoisting entirely. A legitimate intervening-if will appear before any
    // "then if" in effect order, so checking the first occurrence is sufficient.
    if let Some(first_if) = tp.find("if ") {
        if if_belongs_to_then_clause(&lower, first_if) {
            return (text.to_string(), None);
        }
        // CR 603.4: A true intervening-if immediately follows the trigger
        // condition clause. If the first `if ` appears AFTER a sentence
        // boundary (". "), it belongs to that later sentence and scopes only
        // to its own clause — let per-clause parsing attach it as an
        // `AbilityCondition` via `strip_leading_general_conditional`.
        // Example: "this creature gets +1/+1 until end of turn. If five or
        // more mana was spent to cast that spell, this creature also gains
        // double strike ..." — the second sentence's "if" must NOT hoist.
        if lower[..first_if].contains(". ") {
            return (text.to_string(), None);
        }
    }

    // --- Source-referential patterns (cannot be StaticConditions) ---
    // These require trigger-source context that StaticCondition can't express.

    // CR 701.57a: "if you cast it" — zoneless cast check (Discover ETBs).
    // Guard: must not be followed by " from" (zone-specific variant).
    if let Some(pos) = tp.find("if you cast it") {
        let after = &lower[pos + "if you cast it".len()..];
        if !after.starts_with(" from") {
            return (
                strip_condition_clause(text, pos, "if you cast it".len()),
                Some(TriggerCondition::WasCast),
            );
        }
    }

    // CR 603.4 + CR 601.2: "if none of them were cast or no mana was spent to cast them" —
    // compound intervening-if for batch enter triggers. The entering creature(s) must either
    // not have been cast at all, or have been cast for free (no mana spent).
    if let Some(pos) = tp.find("if none of them were cast or no mana was spent to cast them") {
        let pattern = "if none of them were cast or no mana was spent to cast them";
        return (
            strip_condition_clause(text, pos, pattern.len()),
            Some(TriggerCondition::Or {
                conditions: vec![
                    TriggerCondition::Not {
                        condition: Box::new(TriggerCondition::WasCast),
                    },
                    TriggerCondition::ManaSpentCondition {
                        text: "no mana was spent to cast them".to_string(),
                    },
                ],
            }),
        );
    }

    // CR 603.4 + CR 601.2h: "if no mana was spent to cast it/that spell" —
    // intervening-if for free-spell counter triggers (Lavinia / Vexing Bauble).
    if let Some(result) = try_extract_no_mana_spent_condition(&lower, text) {
        return result;
    }

    // CR 603.4 + CR 601.2: "if it was cast" — the entering permanent must have
    // been cast (not put onto the battlefield by another effect). Wedding
    // Ring's ETB token-copy is gated this way so the created copy *token* —
    // which is not cast — does not re-trigger the ability infinitely. The
    // literal "if it was cast" is not a substring of "if it wasn't cast"
    // (the latter has "wasn't", not "was cast"), so the two arms are disjoint.
    // NOTE: if a zone-specific "if it was cast from <zone>" trigger arm is ever
    // added, it must be ordered BEFORE this one — "if it was cast" is a prefix
    // of "if it was cast from your hand" and would shadow it here.
    let was_cast_pos = tp.find("if it was cast"); // allow-noncombinator: anchor for strip_condition_clause — structural if-clause excision, not parse dispatch
    if let Some(pos) = was_cast_pos {
        return (
            strip_condition_clause(text, pos, "if it was cast".len()),
            Some(TriggerCondition::WasCast),
        );
    }

    // CR 603.4 + CR 601.2: "if it wasn't cast" — negation of WasCast.
    if let Some(pos) = tp.find("if it wasn't cast") {
        return (
            strip_condition_clause(text, pos, "if it wasn't cast".len()),
            Some(TriggerCondition::Not {
                condition: Box::new(TriggerCondition::WasCast),
            }),
        );
    }

    // CR 603.4 + CR 702.138b: "unless it escaped" — the trigger fires unless
    // the source permanent was cast from a graveyard with its escape ability.
    // Phlage, Titan of Fire's Fury: "sacrifice it unless it escaped." The
    // condition inverts `CastVariantPaid { variant: Escape }` so reanimation
    // and hard-casts both satisfy the gate (per the WotC ruling: "causes you
    // to sacrifice it if you didn't cast it, or if it was cast using any
    // permission other than an escape ability").
    if let Some((prefix, _)) = scan_split_at_phrase(&lower, |i| {
        tag::<_, _, OracleError<'_>>("unless it escaped").parse(i)
    }) {
        return (
            strip_condition_clause(text, prefix.len(), "unless it escaped".len()),
            Some(TriggerCondition::Not {
                condition: Box::new(TriggerCondition::CastVariantPaid {
                    variant: CastVariantPaid::Escape,
                }),
            }),
        );
    }

    // Simple pattern→condition extractions (no dynamic parsing or guards needed).
    if let Some(result) = try_extract_simple_condition(
        &tp,
        text,
        &[
            // CR 508.1 / CR 603.4: attacking state.
            ("if it's attacking", TriggerCondition::SourceIsAttacking),
            ("if it is attacking", TriggerCondition::SourceIsAttacking),
            // CR 603.4: past-turn life loss.
            (
                "if an opponent lost life during their last turn",
                TriggerCondition::LostLifeLastTurn,
            ),
            // CR 702.104b: Tribute mechanic — "if tribute wasn't paid"
            ("if tribute wasn't paid", TriggerCondition::TributeNotPaid),
            // CR 207.2c: Addendum — "if you cast this spell during your main phase"
            (
                "if you cast this spell during your main phase",
                TriggerCondition::CastDuringPhase {
                    phases: vec![Phase::PreCombatMain, Phase::PostCombatMain],
                },
            ),
            // CR 400.7: "if it had counters on it" — past-state counter check
            (
                "if it had counters on it",
                TriggerCondition::HadCounters { counter_type: None },
            ),
            // CR 702.112a: "if it's renowned" / "if ~ is renowned" — renown state check
            ("if it's renowned", TriggerCondition::SourceIsRenowned),
            ("if ~ is renowned", TriggerCondition::SourceIsRenowned),
            // CR 506.2 + CR 508.1b + CR 603.4: "if none of those creatures attacked you" —
            // intervening-if for "whenever another player attacks with N or more creatures"
            // triggers that reward defensive (non-aggressor) opponents.
            (
                "if none of those creatures attacked you",
                TriggerCondition::NoneOfAttackersTargetedYou,
            ),
            (
                "if it's the first combat phase of the turn",
                TriggerCondition::FirstCombatPhaseOfTurn,
            ),
        ],
    ) {
        return result;
    }

    // CR 603.4 + CR 102.1: "if it's / it is / it isn't / it's not / it is not
    // that player's turn" — composed from two orthogonal axes (linking-verb
    // contraction and optional negation postfix) rather than enumerated as
    // five verbatim phrases.
    if let Some(result) = try_extract_that_players_turn(&tp, &lower, text) {
        return result;
    }

    // CR 309.7: "if you haven't completed [dungeon name]" — dynamic dungeon name parsing.
    if let Some(result) = try_extract_not_completed_dungeon(&tp, &lower, text) {
        return result;
    }

    // CR 400.7: "if it had a +1/+1 counter on it" — typed counter past-state check.
    // Dynamic: parses counter type from "if it had a [type] counter on it".
    if let Some(result) = try_extract_had_counter_condition(&tp, &lower, text) {
        return result;
    }

    // CR 207.2c: Adamant — "if at least N [color] mana was spent to cast this/it"
    if let Some(result) = try_extract_adamant_condition(&tp, &lower, text) {
        return result;
    }

    // CR 400.7d: Symbolic-form spent-mana — "if {C}{C}... was spent to cast it"
    // (Incarnation / hybrid-ETB cycle: Wistfulness, Vibrance, Deceit, Catharsis, Emptiness, ...).
    if let Some(result) = try_extract_symbolic_mana_spent_condition(&tp, &lower, text) {
        return result;
    }
    if let Some(result) = try_extract_symbolic_unless_mana_spent_condition(text) {
        return result;
    }

    // CR 702.49 + CR 603.4: "if [possessive] sneak/ninjutsu cost was paid [this turn]"
    // Guard: "instead" means conditional override, not intervening-if.
    if let Some(result) = try_extract_ninjutsu_condition(&tp, &lower, text) {
        return result;
    }

    // CR 400.7 + CR 603.10: "if it was a [type]" / "if it was an [type]"
    // Nom combinator: prefix dispatch + typed core type extraction.
    {
        fn was_type_combinator(i: &str) -> nom::IResult<&str, CoreType, OracleError<'_>> {
            let (i, _) = alt((tag("if it was an "), tag("if it was a "))).parse(i)?;
            alt((
                value(CoreType::Creature, tag("creature")),
                value(CoreType::Land, tag("land")),
                value(CoreType::Instant, tag("instant")),
                value(CoreType::Sorcery, tag("sorcery")),
                value(CoreType::Artifact, tag("artifact")),
                value(CoreType::Enchantment, tag("enchantment")),
                value(CoreType::Planeswalker, tag("planeswalker")),
                value(CoreType::Battle, tag("battle")),
            ))
            .parse(i)
        }
        if let Some((before, card_type, rest)) = scan_preceded(&lower, was_type_combinator) {
            let pos = before.len();
            let clause_len = lower.len() - before.len() - rest.len();
            return (
                strip_condition_clause(text, pos, clause_len),
                Some(TriggerCondition::WasType { card_type }),
            );
        }
    }

    if let Some(result) = try_extract_zone_change_object_filter_condition(&lower, text) {
        return result;
    }

    // CR 509.1a + CR 603.4: "if defending player controls no [type]"
    // Nom combinator prefix dispatch + parse_type_phrase for the remainder.
    {
        fn def_prefix(i: &str) -> nom::IResult<&str, (), OracleError<'_>> {
            let (i, _) = tag("if defending player controls no ").parse(i)?;
            Ok((i, ()))
        }
        if let Some((before, _, _type_start)) = scan_preceded(&lower, def_prefix) {
            let pos = before.len();
            let prefix_len = "if defending player controls no ".len();
            let after = &text[pos + prefix_len..];
            let (filter, rest) = parse_type_phrase(after);
            if !matches!(filter, TargetFilter::Any) {
                let consumed = after.len() - rest.len();
                return (
                    strip_condition_clause(text, pos, prefix_len + consumed),
                    Some(TriggerCondition::DefendingPlayerControlsNone { filter }),
                );
            }
        }
    }

    // CR 603.4: A leading `if` immediately follows the trigger condition and
    // is a true intervening predicate. A post-effect `if` is gated by
    // `PostEffectPolicy::DeferIfRehomeable` — left in the effect text when a
    // downstream re-homer (`strip_suffix_conditional`) can attach it as a
    // clause-level `AbilityCondition`, hoisted only when it cannot be re-homed.
    //
    // CR 603.5: A trailing `unless` is NOT a CR 603.4 intervening predicate —
    // it is resolution-checked, not trigger-gated. We hoist it via
    // `PostEffectPolicy::AlwaysHoist` as a pragmatic engine simplification:
    // there is no reachable downstream re-homer for `unless`, and the guard at
    // the head of trigger-effect lowering turns any leftover "unless" into
    // `Effect::Unimplemented`. `unless` is the negation of `if`, so wrap the
    // parsed predicate in `Not`. Cost-form `unless` ("unless you pay {2}",
    // "unless you sacrifice a creature") is already stripped upstream by
    // `extract_unless_pay_modifier`.
    if let Some(result) = try_extract_intervening(
        &tp,
        &lower,
        text,
        "if ",
        PostEffectPolicy::DeferIfRehomeable,
        |c| c,
    ) {
        return result;
    }
    if let Some(result) = try_extract_intervening(
        &tp,
        &lower,
        text,
        " unless ",
        PostEffectPolicy::AlwaysHoist,
        |c| TriggerCondition::Not {
            condition: Box::new(c),
        },
    ) {
        return result;
    }

    (text.to_string(), None)
}

fn try_extract_zone_change_object_filter_condition(
    lower: &str,
    text: &str,
) -> Option<(String, Option<TriggerCondition>)> {
    let (before, condition, rest) =
        scan_preceded(lower, parse_zone_change_object_filter_condition)?;
    let next_char_is_boundary = rest
        .chars()
        .next()
        .is_none_or(|c| !c.is_alphanumeric() && c != '_');
    if !next_char_is_boundary {
        return None;
    }

    let consumed = lower.len() - before.len() - rest.len();
    Some((
        strip_condition_clause(text, before.len(), consumed),
        Some(condition),
    ))
}

fn parse_zone_change_object_filter_condition(input: &str) -> OracleResult<'_, TriggerCondition> {
    preceded(tag("if it "), parse_zone_change_object_filter_predicate).parse(input)
}

fn parse_zone_change_object_filter_predicate(input: &str) -> OracleResult<'_, TriggerCondition> {
    let (rest, negated) = alt((
        value(false, tag("was ")),
        value(true, tag("wasn't ")),
        value(true, tag("was not ")),
    ))
    .parse(input)?;

    let (rest, prop) = alt((
        value(FilterProp::Blocking, tag("blocking")),
        map_attachment_kind_filter_prop,
    ))
    .parse(rest)?;
    let condition = TriggerCondition::ZoneChangeObjectMatchesFilter {
        origin: Some(Zone::Battlefield),
        destination: Zone::Graveyard,
        filter: TargetFilter::Typed(TypedFilter::creature().properties(vec![prop])),
    };

    if negated {
        Ok((
            rest,
            TriggerCondition::Not {
                condition: Box::new(condition),
            },
        ))
    } else {
        Ok((rest, condition))
    }
}

fn map_attachment_kind_filter_prop(input: &str) -> OracleResult<'_, FilterProp> {
    let (rest, kinds) = parse_attachment_kind_disjunction(input)?;
    Ok((rest, attachment_kinds_filter_prop(kinds, None)))
}

fn try_extract_no_mana_spent_condition(
    lower: &str,
    text: &str,
) -> Option<(String, Option<TriggerCondition>)> {
    let (before, clause_text, rest) = scan_preceded(lower, |i| {
        preceded(tag("if "), parse_no_mana_spent_clause).parse(i)
    })?;
    let rest_trimmed = rest.trim_start();
    if !(rest_trimmed.is_empty() || rest_trimmed.starts_with(',') || rest_trimmed.starts_with('.'))
    {
        return None;
    }
    let clause_start = before.len();
    let clause_len = lower.len() - before.len() - rest.len();
    Some((
        strip_condition_clause(text, clause_start, clause_len),
        Some(TriggerCondition::ManaSpentCondition {
            text: clause_text.to_string(),
        }),
    ))
}

fn parse_no_mana_spent_clause(i: &str) -> OracleResult<'_, &str> {
    recognize(pair(
        tag("no mana was spent to cast "),
        alt((
            tag("it"),
            tag("that spell"),
            tag("this spell"),
            tag("them"),
            tag("~"),
        )),
    ))
    .parse(i)
}

/// CR 603.4 + CR 102.1: Extract "if it's / it is / it isn't /
/// it's not / it is not that player's turn" — intervening-if where "that
/// player" anaphors to the triggering event's player. Source-referential
/// (no static-condition equivalent) because the player binding lives in
/// the trigger event, not in static game state.
///
/// Two orthogonal axes compose: the linking-verb form (`'s` / ` is` /
/// ` isn't`) and an optional ` not` postfix. The five surface phrases
/// reduce to a single `negated: bool` derived from `verb == " isn't"` OR
/// `postfix == Some(" not")`.
fn try_extract_that_players_turn(
    tp: &TextPair<'_>,
    lower: &str,
    text: &str,
) -> Option<(String, Option<TriggerCondition>)> {
    // TextPair anchor lookup; nom combinator below validates the full clause.
    let pos = tp.find("if it")?; // allow-noncombinator: anchor only, not dispatch
    let tail = &lower[pos..];
    let (rest, (_, verb_negated, postfix, _)) = (
        tag::<_, _, OracleError<'_>>("if it"),
        alt((
            value(true, tag(" isn't")),
            value(false, tag("'s")),
            value(false, tag(" is")),
        )),
        opt(tag(" not")),
        tag(" that player's turn"),
    )
        .parse(tail)
        .ok()?;
    let negated = verb_negated || postfix.is_some();
    let consumed = tail.len() - rest.len();
    let base = TriggerCondition::DuringPlayersTurn {
        player: PlayerFilter::TriggeringPlayer,
    };
    let condition = if negated {
        TriggerCondition::Not {
            condition: Box::new(base),
        }
    } else {
        base
    };
    Some((strip_condition_clause(text, pos, consumed), Some(condition)))
}

/// Policy for a post-effect occurrence of the intervening keyword (one that
/// appears AFTER the effect verb, not immediately after the trigger condition).
enum PostEffectPolicy {
    /// `if`: leave a re-homeable post-effect condition in the effect text for
    /// `strip_suffix_conditional` to re-home onto a clause-level
    /// `AbilityCondition`; hoist only non-re-homeable (source-referential) ones.
    DeferIfRehomeable,
    /// `unless`: always hoist. `unless` has no reachable downstream re-homer —
    /// the trigger-effect lowering guard turns any leftover "unless" into
    /// `Effect::Unimplemented` before `parse_effect_chain_ir` runs.
    AlwaysHoist,
}

/// Try to extract an intervening predicate introduced by `keyword`.
///
/// Runs `parse_inner_condition` on the fragment after `keyword`, accepts only
/// if it stops at a clause boundary with no dangling `otherwise` branch (which
/// would change the semantics from intervening-if to a conditional override
/// pair), then bridges to `TriggerCondition` via `static_condition_to_trigger_condition`
/// and applies `wrap`. Used for both `if X` (wrap = identity) and
/// `unless X` (wrap = `Not`).
///
/// CR 603.4 + CR 608.2h: A true intervening-`if` "immediately follows the
/// trigger condition" ("When X, if Y, Z") — a *leading* keyword. Such a
/// keyword is always hoisted to `TriggerDefinition.condition`. A *post-effect*
/// keyword ("When X, Z if Y") appears AFTER the effect verb and is governed by
/// `policy`:
///
/// - `PostEffectPolicy::DeferIfRehomeable` (the `if` path): a re-homeable
///   post-effect condition is left in the effect text so
///   `strip_suffix_conditional` can attach it as a clause-level
///   `AbilityCondition` (`execute.condition`, single-checked at resolution per
///   CR 608.2h) — e.g. Odric, Lunarch Marshal's per-keyword "if". A
///   non-re-homeable, source-referential condition (e.g. Selvala, Heart of the
///   Wilds' "if its power is greater than each other creature's power") has no
///   `AbilityCondition` form, so it is hoisted to the trigger-level condition
///   instead. Re-homeability is decided by `condition_text_is_rehomeable`.
/// - `PostEffectPolicy::AlwaysHoist` (the `unless` path): always hoist. There
///   is no reachable downstream re-homer for `unless`.
fn try_extract_intervening(
    tp: &TextPair<'_>,
    lower: &str,
    text: &str,
    keyword: &str,
    policy: PostEffectPolicy,
    wrap: impl FnOnce(TriggerCondition) -> TriggerCondition,
) -> Option<(String, Option<TriggerCondition>)> {
    let pos = tp.find(keyword)?;
    let is_leading = lower[..pos].trim().is_empty();
    // CR 603.4: a leading keyword immediately follows the trigger condition — a
    // true intervening predicate; always hoist. A post-effect occurrence is
    // gated by policy.
    if !is_leading {
        if let PostEffectPolicy::DeferIfRehomeable = policy {
            let condition_text = lower[pos + keyword.len()..].trim_end_matches('.').trim();
            if condition_text_is_rehomeable(condition_text) {
                return None; // leave it for strip_suffix_conditional to re-home
            }
        }
        // AlwaysHoist, or non-re-homeable: fall through and hoist as before.
    }
    let cond_fragment = &lower[pos + keyword.len()..];
    let (rest, sc) = parse_inner_condition(cond_fragment).ok()?;
    let rest_trimmed = rest.trim();
    let after_dots = rest_trimmed.trim_start_matches('.').trim_start();
    let has_otherwise = tag::<_, _, OracleError<'_>>("otherwise")
        .parse(after_dots)
        .is_ok();
    let at_boundary =
        rest_trimmed.is_empty() || rest_trimmed.starts_with(',') || rest_trimmed.starts_with('.');
    if has_otherwise || !at_boundary {
        return None;
    }
    let inner = static_condition_to_trigger_condition(&sc)?;
    let consumed = cond_fragment.len() - rest.len();
    Some((
        strip_condition_clause(text, pos, keyword.len() + consumed),
        Some(wrap(inner)),
    ))
}

/// CR 702.49a + CR 702.142b: Parse "whenever you activate a [keyword] ability" triggers.
/// Matches ninjutsu-family and boast activation patterns.
fn try_parse_keyword_activation_trigger(lower: &str) -> Option<(TriggerMode, TriggerDefinition)> {
    for prefix in ["whenever you activate ", "when you activate "] {
        let Ok((rest, ())) = value((), tag::<_, _, OracleError<'_>>(prefix)).parse(lower) else {
            continue;
        };
        // CR 702.49a: Match "a ninjutsu ability" — covers the ninjutsu-family keyword
        if tag::<_, _, OracleError<'_>>("a ninjutsu ability")
            .parse(rest)
            .is_ok()
        {
            let mut def = make_base();
            def.mode = TriggerMode::NinjutsuActivated;
            return Some((TriggerMode::NinjutsuActivated, def));
        }
        // CR 702.142b: Match "a boast ability" — covers boast keyword activation
        if tag::<_, _, OracleError<'_>>("a boast ability")
            .parse(rest)
            .is_ok()
        {
            let mut def = make_base();
            def.mode = TriggerMode::BoastAbilityActivated;
            return Some((TriggerMode::BoastAbilityActivated, def));
        }
        if all_consuming(tag::<_, _, OracleError<'_>>("an exhaust ability"))
            .parse(rest)
            .is_ok()
        {
            let mut def = make_base();
            def.mode = TriggerMode::ExhaustAbilityActivated;
            return Some((TriggerMode::ExhaustAbilityActivated, def));
        }
        if all_consuming(tag::<_, _, OracleError<'_>>(
            "an exhaust ability that isn't a mana ability",
        ))
        .parse(rest)
        .is_ok()
        {
            let mut def = make_base();
            def.mode = TriggerMode::ExhaustAbilityActivated;
            def.condition = Some(TriggerCondition::ActivatedAbilityIsNonMana);
            return Some((TriggerMode::ExhaustAbilityActivated, def));
        }
    }
    None
}

/// CR 702.49: Extract ninjutsu/sneak cost-paid conditions.
/// Guard: "instead" after the condition means conditional override, not intervening-if.
fn try_extract_ninjutsu_condition(
    tp: &TextPair<'_>,
    lower: &str,
    text: &str,
) -> Option<(String, Option<TriggerCondition>)> {
    for (keyword, variant) in &[
        ("sneak cost was paid", CastVariantPaid::Sneak),
        ("ninjutsu cost was paid", CastVariantPaid::Ninjutsu),
    ] {
        if scan_contains(lower, keyword) && !scan_contains(lower, "instead") {
            let pos = tp.find("if ").unwrap_or(0);
            let kw_pos = tp.find(keyword)?;
            let after = &lower[kw_pos + keyword.len()..];
            let extra = if after.starts_with(" this turn") {
                " this turn".len()
            } else {
                0
            };
            let end = kw_pos + keyword.len() + extra;
            return Some((
                strip_condition_clause(text, pos, end - pos),
                Some(TriggerCondition::CastVariantPaid { variant: *variant }),
            ));
        }
    }
    None
}

/// Try extracting a simple pattern→condition from text via search-and-strip.
///
/// For source-referential conditions that cannot be `StaticCondition`s and don't need
/// dynamic parsing — just a fixed pattern mapping to a fixed `TriggerCondition`.
fn try_extract_simple_condition(
    tp: &TextPair<'_>,
    text: &str,
    patterns: &[(&str, TriggerCondition)],
) -> Option<(String, Option<TriggerCondition>)> {
    for (pattern, condition) in patterns {
        if let Some(pos) = tp.find(pattern) {
            return Some((
                strip_condition_clause(text, pos, pattern.len()),
                Some(condition.clone()),
            ));
        }
    }
    None
}

/// CR 309.7: Extract "if you haven't completed [dungeon name]" conditions.
///
/// Parses the dungeon name dynamically from the text rather than matching a
/// verbatim Oracle string — handles any dungeon, not just Tomb of Annihilation.
fn try_extract_not_completed_dungeon(
    tp: &TextPair<'_>,
    lower: &str,
    text: &str,
) -> Option<(String, Option<TriggerCondition>)> {
    use crate::game::dungeon::DungeonId;

    let prefix = "if you haven't completed ";
    let pos = tp.find(prefix)?;
    let after = &lower[pos + prefix.len()..];

    // Try each known dungeon display name (lowercase) against the remainder.
    let dungeons = [
        ("lost mine of phandelver", DungeonId::LostMineOfPhandelver),
        ("dungeon of the mad mage", DungeonId::DungeonOfTheMadMage),
        ("tomb of annihilation", DungeonId::TombOfAnnihilation),
        ("undercity", DungeonId::Undercity),
        ("baldur's gate wilderness", DungeonId::BaldursGateWilderness),
    ];

    for (name, id) in &dungeons {
        if after.starts_with(name) {
            let clause_len = prefix.len() + name.len();
            return Some((
                strip_condition_clause(text, pos, clause_len),
                Some(TriggerCondition::Not {
                    condition: Box::new(TriggerCondition::CompletedDungeon {
                        specific: Some(*id),
                    }),
                }),
            ));
        }
    }
    None
}

/// CR 400.7: Extract "if it had a [type] counter on it" conditions.
///
/// Uses nom `tag()` + `take_until()` to extract the counter type dynamically.
fn try_extract_had_counter_condition(
    tp: &TextPair<'_>,
    lower: &str,
    text: &str,
) -> Option<(String, Option<TriggerCondition>)> {
    use nom::bytes::complete::take_until;
    let prefix = "if it had a ";
    let pos = tp.find(prefix)?;
    let after = &lower[pos + prefix.len()..];
    // Parse: "[counter_type] counter on it"
    let (rest, counter_type_text) = take_until::<_, _, OracleError<'_>>(" counter on it")
        .parse(after)
        .ok()?;
    let (rest, _) = tag::<_, _, OracleError<'_>>(" counter on it")
        .parse(rest)
        .ok()?;
    let clause_len = prefix.len() + (after.len() - rest.len());
    Some((
        strip_condition_clause(text, pos, clause_len),
        Some(TriggerCondition::HadCounters {
            counter_type: Some(parse_counter_type(counter_type_text)),
        }),
    ))
}

/// CR 207.2c: Extract Adamant conditions — "if at least N [color] mana was spent to cast"
///
/// Uses nom combinators to parse the mana color and minimum count.
fn try_extract_adamant_condition(
    tp: &TextPair<'_>,
    lower: &str,
    text: &str,
) -> Option<(String, Option<TriggerCondition>)> {
    let prefix = "if at least ";
    let pos = tp.find(prefix)?;
    let after = &lower[pos + prefix.len()..];
    // Parse: "N [color] mana was spent to cast [this spell/it/them/~]".
    // Delegates the object-reference alt to `parse_spent_to_cast_tail`, which is
    // shared with the symbolic-form extractor.
    let (rest, _) = nom_primitives::parse_number(after).ok()?;
    let rest = rest.trim_start();
    let (rest, color) = alt((
        value(ManaColor::White, tag::<_, _, OracleError<'_>>("white")),
        value(ManaColor::Blue, tag("blue")),
        value(ManaColor::Black, tag("black")),
        value(ManaColor::Red, tag("red")),
        value(ManaColor::Green, tag("green")),
    ))
    .parse(rest)
    .ok()?;
    let (rest, _) = preceded(tag(" mana"), parse_spent_to_cast_tail)
        .parse(rest)
        .ok()?;
    // Re-parse N from the original to get the number
    let (_, n) = nom_primitives::parse_number(&lower[pos + prefix.len()..]).ok()?;
    let clause_len = prefix.len() + (after.len() - rest.len());
    Some((
        strip_condition_clause(text, pos, clause_len),
        Some(TriggerCondition::ManaColorSpent { color, minimum: n }),
    ))
}

/// CR 400.7d: Extract symbolic-form mana-spent conditions — the Incarnation /
/// hybrid-ETB phrasing `"if {C}{C}... was spent to cast it"` where the required
/// mana is expressed as a run of identical colored mana symbols rather than as
/// words. Semantically identical to Adamant (`ManaColorSpent`), only the surface
/// syntax differs. Per CR 400.7d, a permanent's ability can reference "what mana
/// was spent to pay [its casting] costs."
///
/// Accepts runs of one or more pure-color symbols (`{W}`, `{U}`, `{B}`,
/// `{R}`, `{G}`), including mixed-color runs that require each listed color to
/// have been spent. Hybrid, phyrexian, colorless, snow, generic (`{2}`), and
/// `{X}` symbols are rejected — they correspond to different rules-level
/// conditions and must not be conflated here.
fn try_extract_symbolic_mana_spent_condition(
    _tp: &TextPair<'_>,
    _lower: &str,
    text: &str,
) -> Option<(String, Option<TriggerCondition>)> {
    try_extract_symbolic_mana_spent_clause(text, SymbolicManaSpentIntro::If)
}

/// Extract trailing symbolic-form unless clauses like
/// `"sacrifice it unless {U} was spent to cast it"` as the negation of the
/// existing mana-color-spent trigger condition.
fn try_extract_symbolic_unless_mana_spent_condition(
    text: &str,
) -> Option<(String, Option<TriggerCondition>)> {
    try_extract_symbolic_mana_spent_clause(text, SymbolicManaSpentIntro::Unless)
}

#[derive(Clone, Copy)]
enum SymbolicManaSpentIntro {
    If,
    Unless,
}

impl SymbolicManaSpentIntro {
    fn tag(self) -> &'static str {
        match self {
            SymbolicManaSpentIntro::If => "if ",
            SymbolicManaSpentIntro::Unless => "unless ",
        }
    }

    fn condition(self, color_counts: Vec<(ManaColor, u32)>) -> TriggerCondition {
        let condition = match color_counts.as_slice() {
            [(color, minimum)] => TriggerCondition::ManaColorSpent {
                color: *color,
                minimum: *minimum,
            },
            _ => TriggerCondition::And {
                conditions: color_counts
                    .into_iter()
                    .map(|(color, minimum)| TriggerCondition::ManaColorSpent { color, minimum })
                    .collect(),
            },
        };
        match self {
            SymbolicManaSpentIntro::If => condition,
            SymbolicManaSpentIntro::Unless => TriggerCondition::Not {
                condition: Box::new(condition),
            },
        }
    }
}

fn try_extract_symbolic_mana_spent_clause(
    text: &str,
    intro: SymbolicManaSpentIntro,
) -> Option<(String, Option<TriggerCondition>)> {
    // Scan for the clause at any word boundary using a composed combinator:
    //   ("if " | "unless ") → many1(pure_color_symbol) → " was spent to cast <ref>".
    // `scan_preceded` threads (before, value, rest) in one pass — no re-parse.
    let (before, (colors, _), tail_rest) = nom_primitives::scan_preceded(text, |i| {
        preceded(
            tag(intro.tag()),
            pair(many1(parse_pure_color_symbol_ci), parse_spent_to_cast_tail),
        )
        .parse(i)
    })?;

    let tail_trimmed = tail_rest.trim_start();
    if !(tail_trimmed.is_empty() || tail_trimmed.starts_with('.') || tail_trimmed.starts_with(','))
    {
        return None;
    }
    let mut color_counts: Vec<(ManaColor, u32)> = Vec::new();
    for color in colors {
        if let Some((_, count)) = color_counts.iter_mut().find(|(seen, _)| *seen == color) {
            *count += 1;
        } else {
            color_counts.push((color, 1));
        }
    }

    let clause_start = before.len();
    let clause_len = text.len() - before.len() - tail_rest.len();

    Some((
        strip_condition_clause(text, clause_start, clause_len),
        Some(intro.condition(color_counts)),
    ))
}

/// Case-insensitive parser for a single pure-color mana symbol (`{W}`/`{w}`,
/// `{U}`/`{u}`, etc.). Rejects hybrid, phyrexian, colorless, snow, `{X}`, and
/// generic `{N}` symbols — those don't correspond to a `ManaColorSpent`
/// condition and must fall through to alternative handlers.
fn parse_pure_color_symbol_ci(i: &str) -> OracleResult<'_, ManaColor> {
    delimited(
        tag("{"),
        alt((
            value(ManaColor::White, alt((tag("W"), tag("w")))),
            value(ManaColor::Blue, alt((tag("U"), tag("u")))),
            value(ManaColor::Black, alt((tag("B"), tag("b")))),
            value(ManaColor::Red, alt((tag("R"), tag("r")))),
            value(ManaColor::Green, alt((tag("G"), tag("g")))),
        )),
        tag("}"),
    )
    .parse(i)
}

/// Match the fixed tail that follows a mana-symbol run in spent-mana conditions:
/// `" was spent to cast "` + one of `this spell` / `it` / `them` / `~`.
/// Shared by both the word-form (Adamant) and symbol-form extractors.
fn parse_spent_to_cast_tail(i: &str) -> OracleResult<'_, ()> {
    value(
        (),
        preceded(
            tag(" was spent to cast "),
            alt((tag("this spell"), tag("it"), tag("them"), tag("~"))),
        ),
    )
    .parse(i)
}

/// Strip a condition clause from text, joining the before and after portions.
/// Handles the clause appearing at the start, end, or middle of the text.
fn strip_condition_clause(text: &str, clause_start: usize, clause_len: usize) -> String {
    let before = text[..clause_start].trim_end().trim_end_matches(',');
    let after = text[clause_start + clause_len..]
        .trim_start_matches(',')
        .trim_start()
        .trim_end_matches('.')
        .trim();
    if before.is_empty() {
        after.to_string()
    } else if after.is_empty() {
        before.to_string()
    } else {
        format!("{before} {after}")
    }
}

/// CR 603.4: True when the `if` at `if_pos` belongs to a "then if ..." clause
/// introduced by a preceding sentence boundary ("effect. Then if ..." or
/// "effect, then if ...").
///
/// A genuine intervening-if (per CR 603.4) has its `if` **immediately following**
/// the trigger condition clause, with no intervening "then". When `if` appears
/// inside a "then if" sub-clause, the condition scopes only to that clause's
/// sub_ability — not to the whole trigger — and is handled by the per-clause
/// condition extractor `strip_leading_general_conditional` in
/// `parser/oracle_effect/conditions.rs`.
///
/// Implementation: two detection paths —
///
/// 1. Sentence-boundary form: the last ". " before `if_pos` is followed only
///    by "then" / "then," (e.g. "effect. Then if Y, effect2").
/// 2. Inline form: the token immediately preceding `if ` is "then" or "then,"
///    with no sentence boundary required (covers punctuation-free variants).
///
/// Structural scan only — not parser dispatch.
fn if_belongs_to_then_clause(lower: &str, if_pos: usize) -> bool {
    let before = &lower[..if_pos];

    // Path 1: sentence-boundary form. The segment between the last ". " and
    // the `if` is exactly "then" / "then," (Felidar Sovereign: "...double your
    // life total. Then, if you have 1,000 or more life, you lose the game").
    let sentence_start = before.rfind(". ").map_or(0, |i| i + 2);
    let between = lower[sentence_start..if_pos].trim_start();
    if alt((tag::<_, _, OracleError<'_>>("then, "), tag("then ")))
        .parse(between)
        .map(|(rest, _)| rest.trim().is_empty())
        .unwrap_or(false)
    {
        return true;
    }

    // Path 2: inline form. Find the last word boundary in `before` and run
    // the same tag-based dispatch over the trailing word. Word-boundary
    // lookup (rfind on space/comma) is structural; dispatch goes through
    // the `tag` combinator per parser policy.
    let trimmed = before.trim_end();
    let word_start = trimmed.rfind([' ', ',']).map_or(0, |i| i + 1);
    let candidate = &trimmed[word_start..];
    alt((
        tag::<_, _, OracleError<'_>>("then,"),
        tag::<_, _, OracleError<'_>>("then"),
    ))
    .parse(candidate)
    .map(|(rest, _)| rest.is_empty())
    .unwrap_or(false)
}

/// Parse "if you control N or more [type]" → (condition, end_byte_offset).
///
fn normalize_self_refs(text: &str, card_name: &str) -> String {
    normalize_card_name_refs(text, card_name)
}

/// Split compound conditions joined by " and when " or " and whenever ".
/// Returns `Some(vec![first_condition, second_condition])` with proper trigger keywords,
/// or `None` if no compound conjunction is found.
///
/// Examples:
/// - "When you cycle ~ and when ~ dies" → ["When you cycle ~", "When ~ dies"]
/// - "When ~ enters and whenever you cast an Elemental spell" → ["When ~ enters", "Whenever you cast an Elemental spell"]
fn split_and_when_compound(cond_lower: &str, condition: &str) -> Option<Vec<String>> {
    // Use nom split_once_on to detect " and when " or " and whenever " conjunctions.
    // Try " and whenever " first (longer match) to avoid " and when " matching the "when" prefix.
    use super::oracle_nom::primitives::split_once_on;
    if let Ok((_, (before, _))) = split_once_on(cond_lower, " and whenever ") {
        let pos = before.len();
        let first = condition[..pos].trim().to_string();
        let second_start = pos + " and ".len();
        // Capitalize: the second half already starts with "whenever"
        let second =
            normalize_compound_pronouns(&capitalize_first(condition[second_start..].trim()));
        return Some(vec![first, second]);
    }
    if let Ok((_, (before, _))) = split_once_on(cond_lower, " and when ") {
        let pos = before.len();
        let first = condition[..pos].trim().to_string();
        let second_start = pos + " and ".len();
        let second =
            normalize_compound_pronouns(&capitalize_first(condition[second_start..].trim()));
        return Some(vec![first, second]);
    }
    None
}

/// In compound trigger splits, the second half may use pronouns ("it", "its")
/// that refer to the source permanent. Replace these with the self-reference
/// marker "~" so the trigger condition parser recognizes them.
fn normalize_compound_pronouns(text: &str) -> String {
    // Replace " it" at word boundaries (end of string or followed by space/comma/period).
    // Be careful not to replace "it" inside words like "wait" or "remit".
    let mut result = text.to_string();
    // "sacrifice it" → "sacrifice ~", "exile it" → "exile ~", etc.
    // Use word-boundary-safe replacement: " it" at end, " it," or " it "
    for (from, to) in [(" it,", " ~,"), (" it.", " ~."), (" it ", " ~ ")] {
        result = result.replace(from, to);
    }
    // Handle " it" at end of string
    if result.ends_with(" it") {
        let len = result.len();
        result.replace_range(len - 2.., "~");
    }
    result
}

/// Split compound conditions where "or" joins two event verbs sharing the same subject.
/// Returns `Some(vec![first_trigger, second_trigger])` with reconstructed trigger lines,
/// or `None` if no compound event "or" is found.
///
/// Detects "or" followed by a known event verb (dies, deals, enters, attacks, blocks,
/// is sacrificed, is exiled, leaves). Does NOT match "or" between subjects (e.g.,
/// "a creature or artifact enters").
///
/// Examples:
/// - "Whenever ~ enters or deals combat damage to a player" → ["Whenever ~ enters", "Whenever ~ deals combat damage to a player"]
/// - "Whenever ~ deals combat damage to a player or dies" → ["Whenever ~ deals combat damage to a player", "Whenever ~ dies"]
fn split_or_event_compound(cond_lower: &str, condition: &str) -> Option<Vec<String>> {
    // Known event verb prefixes that signal a compound event "or".
    //
    // CR 603.1 + CR 701.21 + CR 701.9: Player-subject active-voice verbs
    // ("sacrifices", "discards") cover punisher triggers like Tergrid, God of
    // Fright ("Whenever an opponent sacrifices a nontoken permanent or
    // discards a permanent card, ..."). Each branch then routes to the
    // existing `try_parse_sacrifice_trigger` / `try_parse_discard_trigger`
    // handlers via the per-half re-parse loop.
    fn is_event_verb_start(text: &str) -> bool {
        let combat_or_zone = alt((
            value((), tag::<_, _, OracleError<'_>>("dies")),
            value((), tag("die ")),
            value((), tag("deals ")),
            value((), tag("deal ")),
            value((), tag("enters")),
            value((), tag("enter ")),
            value((), tag("attacks")),
            value((), tag("attack ")),
            value((), tag("blocks")),
            value((), tag("block ")),
            value((), tag("leaves")),
            value((), tag("is put into")),
        ));
        let player_actions = alt((
            value((), tag::<_, _, OracleError<'_>>("is sacrificed")),
            value((), tag("are sacrificed")),
            value((), tag("sacrifices ")),
            value((), tag("sacrifice ")),
            value((), tag("discards ")),
            value((), tag("discard ")),
            value((), tag("is exiled")),
            value((), tag("are exiled")),
            // CR 305.1 + CR 601.2: Player-action verbs for Rocco-class
            // "a player plays a land from exile or casts a spell from exile".
            value((), tag("plays ")),
            value((), tag("play ")),
            value((), tag("casts ")),
            value((), tag("cast ")),
        ));
        alt((combat_or_zone, player_actions)).parse(text).is_ok()
    }

    // Patterns already handled as dedicated compound TriggerMode variants
    // (EntersOrAttacks, AttacksOrBlocks) — do not split these.
    fn is_existing_compound_mode(cond_lower: &str) -> bool {
        scan_contains(cond_lower, "enters or attacks")
            || scan_contains(cond_lower, "enters the battlefield or attacks")
            || scan_contains(cond_lower, "attacks or blocks")
    }
    if is_existing_compound_mode(cond_lower) {
        return None;
    }

    // Scan for " or " occurrences using split_once_on, checking if what follows is an event verb.
    use super::oracle_nom::primitives::split_once_on;
    let mut search_start = 0;
    while let Ok((_, (before, after))) = split_once_on(&cond_lower[search_start..], " or ") {
        let pos = search_start + before.len();
        if is_event_verb_start(after) {
            // Found a compound event "or". Extract the trigger keyword and subject
            // from the first half to reconstruct the second trigger line.
            let first = condition[..pos].trim().to_string();

            // Extract the trigger keyword ("When"/"Whenever") and subject from the first condition.
            // The subject is everything between the keyword and the first event verb.
            let keyword_and_subject = extract_keyword_and_subject(&cond_lower[..pos]);
            let second_event = condition[pos + 4..].trim();
            let second = format!("{keyword_and_subject} {second_event}");

            return Some(vec![first, second]);
        }
        search_start = pos + 4;
    }
    None
}

/// Extract the trigger keyword + subject from a condition prefix.
/// E.g., "whenever ~ enters" → "Whenever ~" (strips the event verb).
/// E.g., "whenever ~ deals combat damage to a player" → "Whenever ~".
fn extract_keyword_and_subject(cond_lower: &str) -> String {
    // Strip trigger keyword
    let (keyword, after_keyword) = if let Ok((rest, ())) =
        value((), tag::<_, _, OracleError<'_>>("whenever ")).parse(cond_lower)
    {
        ("Whenever", rest)
    } else if let Ok((rest, ())) =
        value((), tag::<_, _, OracleError<'_>>("when ")).parse(cond_lower)
    {
        ("When", rest)
    } else {
        // Fallback: return as-is with capitalized first letter
        return capitalize_first(cond_lower);
    };

    // Parse the subject using the existing subject parser — it returns (subject, rest_after_subject).
    // We need the text span of the subject, not the parsed filter.
    // Reconstruct by taking everything from after_keyword up to where the event verb starts.
    let subject_text = extract_subject_text(after_keyword);
    format!("{keyword} {subject_text}")
}

/// Extract the subject text span from the beginning of condition text (after keyword).
/// Returns the text up to the first recognized event verb.
fn extract_subject_text(text: &str) -> &str {
    // Known event verb starts that end the subject span.
    // scan_split_at_phrase tries the combinator at each word boundary,
    // returning (prefix, matched_start) on the first hit.
    if let Some((prefix, _)) = scan_split_at_phrase(text, |i| {
        let combat_or_zone = alt((
            tag("enters"),
            tag("enter "),
            tag("dies"),
            tag("die "),
            tag("deals "),
            tag("deal "),
            tag("attacks"),
            tag("attack "),
            tag("blocks"),
            tag("block "),
            tag("leaves"),
            tag("is put into"),
        ));
        let player_actions = alt((
            tag("is sacrificed"),
            tag("are sacrificed"),
            // CR 701.21 + CR 701.9: Active-voice player-subject verbs paired
            // with `is_event_verb_start` (above) so player-actor compound
            // triggers ("an opponent sacrifices ... or discards ...") split
            // into independently parsed halves.
            tag("sacrifices "),
            tag("sacrifice "),
            tag("discards "),
            tag("discard "),
            tag("is exiled"),
            tag("are exiled"),
            // CR 305.1 + CR 601.2: Keep in sync with `is_event_verb_start`
            // for Rocco-class play-land / cast-spell compound triggers.
            tag("plays "),
            tag("play "),
            tag("casts "),
            tag("cast "),
        ));
        alt((combat_or_zone, player_actions)).parse(i)
    }) {
        if !prefix.is_empty() {
            return prefix.trim_end();
        }
    }
    // Fallback: return the entire text as subject
    text.trim()
}

/// Capitalize the first character of a string.
fn capitalize_first(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        None => String::new(),
        Some(c) => c.to_uppercase().to_string() + chars.as_str(),
    }
}

fn split_trigger(tp: TextPair<'_>) -> (String, String) {
    if let Some(comma_pos) = find_effect_boundary(tp.lower) {
        let condition = tp.original[..comma_pos].trim().to_string();
        let effect = tp.original[comma_pos + 2..].trim().to_string();
        (condition, effect)
    } else {
        (tp.original.to_string(), String::new())
    }
}

fn find_effect_boundary(lower: &str) -> Option<usize> {
    use super::oracle_nom::primitives::split_once_on;
    let mut search_start = 0;
    while let Ok((_, (before, after))) = split_once_on(&lower[search_start..], ", ") {
        let comma_pos = search_start + before.len();
        if !continues_player_action_list(after)
            && !continues_disjunctive_zone_change_condition(after)
        {
            return Some(comma_pos);
        }
        search_start = comma_pos + 2;
    }
    None
}

/// CR 603.1 + CR 603.2: Parser-as-detector — returns `true` when the text after
/// a `, ` boundary is another clause of a disjunctive zone-change trigger
/// condition ("..., or a creature card leaves your graveyard, ..."), meaning the
/// comma is a condition continuation rather than the effect boundary.
fn continues_disjunctive_zone_change_condition(after_comma: &str) -> bool {
    let trimmed = after_comma.trim_start();
    // A disjunctive continuation always begins with the "or " connector.
    let Ok((after_or, ())) = value((), tag::<_, _, OracleError<'_>>("or ")).parse(trimmed) else {
        return false;
    };
    // Examine only the single clause segment up to the next ", " — beyond that
    // lies either the next clause (handled on its own boundary) or the effect.
    let clause = match nom_primitives::split_once_on(after_or, ", ") {
        Ok((_, (before, _))) => before,
        Err(_) => after_or,
    };
    let mut ctx = ParseContext::default();
    let (subject, verb) = parse_trigger_subject(clause.trim(), &mut ctx);
    parse_zone_change_clause(&subject, verb).is_some()
}

fn continues_player_action_list(after_comma: &str) -> bool {
    let trimmed = after_comma.trim_start();
    let candidate = value((), tag::<_, _, OracleError<'_>>("or "))
        .parse(trimmed)
        .map(|(rest, _)| rest)
        .unwrap_or(trimmed)
        .split(", ")
        .next()
        .unwrap_or(trimmed)
        .trim();
    if parse_player_action_phrase(candidate).is_some() {
        return true;
    }

    // Recognize type-phrase continuations in comma-separated type lists.
    // E.g. "a creature, planeswalker, or battle enters" — after ", " we see
    // "planeswalker" (a bare type word) or "or battle enters" ("or" + type word).
    // Strip optional "or "/"and " conjunction, then check if the next word is a type.
    //
    // Guard: a type word followed by a predicate verb indicates a new subject-predicate
    // sentence (the effect body), not a type list continuation.
    // E.g. "creatures you control get +1/+1" starts with "creatures" (type word) but
    // has "get" (predicate verb) — this is the effect, not a continuation.
    let after_conjunction = alt((
        value((), tag::<_, _, OracleError<'_>>("or ")),
        value((), tag("and ")),
    ))
    .parse(trimmed)
    .map(|(rest, _)| rest)
    .unwrap_or(trimmed);
    if !starts_with_type_word(after_conjunction) {
        return false;
    }
    // Type word found — distinguish continuation from new sentence.
    // A continuation has no predicate verb before the trigger event verb;
    // a new sentence has a subject + predicate verb ("creatures you control get").
    !is_new_sentence_not_type_continuation(after_conjunction)
}

/// Check if the text starting at a type word is a new subject-predicate sentence
/// rather than a type-list continuation.
///
/// A type-list continuation: "planeswalker, or battle enters" — just a type word
/// optionally followed by more type words and a trigger event verb.
/// A new effect sentence: "creatures you control get +1/+1" — a type word followed
/// by a controller clause and a predicate verb before the next comma.
///
/// The heuristic: check only the words before the next ", " boundary. If a
/// predicate verb appears there, it's a new sentence.
fn is_new_sentence_not_type_continuation(text: &str) -> bool {
    use crate::parser::oracle_effect::normalize_verb_token;
    use crate::parser::oracle_effect::subject::PREDICATE_VERBS;
    // Only examine up to the next ", " (or end of text) to avoid looking through
    // subsequent clauses that legitimately contain predicate verbs.
    let clause = text.split(", ").next().unwrap_or(text);
    let lower = clause.to_lowercase();
    // Skip the first word (the type word itself) and check remaining words.
    lower.split_whitespace().skip(1).any(|w| {
        let normalized = normalize_verb_token(w);
        PREDICATE_VERBS.contains(&normalized.as_str())
    })
}

fn make_base() -> TriggerDefinition {
    TriggerDefinition::new(TriggerMode::Unknown("unknown".to_string()))
        .trigger_zones(vec![Zone::Battlefield])
}

pub(crate) fn parse_trigger_condition(
    condition: &str,
    ctx: &mut ParseContext,
) -> (TriggerMode, TriggerDefinition) {
    // CR 603.4: Reset any stale relative-clause state before parsing this
    // trigger line. Every early-return path below (phase/player/counter
    // triggers, disjunctive zone-change, the `Unknown` fallback) and the
    // compound-`or` subject recursion must start from a clean slate so a
    // "who controls F" clause from a previous trigger line cannot leak into
    // this one.
    ctx.pending_trigger_subject_clause = None;

    let lower = condition.to_lowercase();

    if let Some(result) = try_parse_named_trigger_mode(&lower) {
        return result;
    }

    if let Some(result) = try_parse_special_trigger_pattern(&lower) {
        return result;
    }

    // --- Phase triggers: "At the beginning of..." ---
    if let Some(result) = try_parse_phase_trigger(&lower) {
        return result;
    }

    // --- Player triggers: "you gain life", "you cast a spell", "you draw a card" ---
    if let Some(result) = try_parse_player_trigger(&lower) {
        return result;
    }

    // Counter-related events: "a +1/+1 counter is put on ~" /
    // "one or more counters are put on ~" / "the twelfth hour counter is put
    // on ~". These are passive event subjects where the object after "on" is
    // the trigger subject; parse them before generic subject decomposition so
    // ordinal counter phrases don't emit a degraded-subject diagnostic first.
    if let Some(result) = try_parse_counter_trigger(&lower) {
        return result;
    }

    // CR 603.1 + CR 603.2: Disjunctive zone-change trigger — a single triggered
    // ability whose trigger event is a disjunction of distinct zone-change
    // shapes ("whenever [clause], or [clause], or [clause]"). Must precede
    // subject decomposition / `try_parse_event`, which only handle one clause.
    if let Some(clauses) = parse_disjunctive_zone_change_condition(condition) {
        let mut def = make_base();
        def.mode = TriggerMode::ChangesZone;
        def.zone_change_clauses = clauses;
        return (TriggerMode::ChangesZone, def);
    }

    // --- Subject + event decomposition ---
    // Strip leading "when"/"whenever" using nom alt()
    let after_keyword = alt((
        value((), tag::<_, _, OracleError<'_>>("whenever ")),
        value((), tag("when ")),
    ))
    .parse(lower.as_str())
    .map(|(rest, _)| rest)
    .unwrap_or(&lower);

    // Parse the subject ("~", "another creature you control", "a creature", etc.)
    // CR 603.2c: Detect "one or more" quantifier for batched trigger semantics.
    // Scan the full subject text (not just the start) because compound subjects like
    // "~ and/or one or more other creatures" place "one or more" after the first branch.
    let is_batched = scan_contains(after_keyword, "one or more ");

    // Snapshot diagnostics before subject parsing — if the trigger ends up as Unknown,
    // the subject diagnostic is redundant (the coverage system already tracks Unknown triggers).
    // Only keep subject diagnostics when the event verb parses successfully (meaning the trigger
    // works but has a degraded subject filter).
    let pre_snapshot = ctx.diagnostics.len();
    let (subject, rest) = parse_trigger_subject(after_keyword, ctx);
    let subject_diagnostics: Vec<OracleDiagnostic> =
        ctx.diagnostics.drain(pre_snapshot..).collect();
    // ctx.diagnostics now contains only pre-existing diagnostics (restored to snapshot)

    // Parse event verb from the remaining text.
    if let Some((mode, mut def)) = try_parse_event(&subject, rest, &lower) {
        // Re-emit subject diagnostics — the trigger parsed but the subject degraded to Any.
        ctx.diagnostics.extend(subject_diagnostics);
        if is_batched {
            def.batched = true;
        }
        // CR 603.4: "an opponent who controls F draws a card" — the
        // relative clause parsed by `parse_single_subject` is an intervening-if:
        // the trigger fires only when the triggering player controls >= 1
        // permanent matching F. Rewrite F's controller to `TriggeringPlayer`
        // (the drawer/life-gainer) and AND an `ObjectCount >= 1` check into the
        // trigger's condition.
        if let Some(clause_filter) = ctx.pending_trigger_subject_clause.take() {
            lift_subject_clause(&mut def, clause_filter);
        }
        return (mode, def);
    }

    // CR 603.4: ViaPlayerTrigger verbs (cast/sacrifice/discard) are not parsed
    // by `try_parse_event`. When a who-controls clause was stashed and the
    // verb at the head of `rest` routes to `try_parse_player_trigger`,
    // reconstruct a canonical player-trigger line and re-dispatch through it
    // (ctx-free — the stashed who-controls clause is preserved). The bare
    // "an opponent <verb>" prefix matches; the who-controls clause is applied
    // via the stashed-filter lift. Mirrors `split_or_event_compound`'s
    // reconstruct-and-reparse precedent.
    if ctx.pending_trigger_subject_clause.is_some() {
        if let (Ok((_, PlayerEventVerbRoute::ViaPlayerTrigger)), Some(actor)) = (
            parse_player_event_verb_head(rest.trim_start()),
            canonical_actor_phrase(&subject),
        ) {
            let reconstructed = format!("whenever {actor} {}", rest.trim_start());
            if let Some((mode, mut def)) = try_parse_player_trigger(&reconstructed) {
                ctx.diagnostics.extend(subject_diagnostics);
                if is_batched {
                    def.batched = true;
                }
                if let Some(clause_filter) = ctx.pending_trigger_subject_clause.take() {
                    lift_subject_clause(&mut def, clause_filter);
                }
                return (mode, def);
            }
        }
    }

    // --- Fallback: discard subject_warnings (trigger is Unknown, redundant) ---
    let mut def = make_base();
    let mode = TriggerMode::Unknown(condition.to_string());
    def.mode = mode.clone();
    def.description = Some(condition.to_string());
    (mode, def)
}

/// CR 109.4 + CR 603.7c: Returns `true` when any filter inside the execute
/// ability's effect chain references `ControllerRef::TargetPlayer`. Walks
/// sub-abilities so triggers like Dokuchi Silencer (outer Discard, inner
/// Destroy targeting "that player controls") trigger the companion
/// `valid_target = Player` surface in `parse_trigger_line`.
fn execute_references_target_player(effect: &crate::types::ability::Effect) -> bool {
    fn filter_references(filter: &TargetFilter) -> bool {
        match filter {
            TargetFilter::Typed(TypedFilter { controller, .. }) => {
                matches!(controller, Some(ControllerRef::TargetPlayer))
            }
            TargetFilter::And { filters } | TargetFilter::Or { filters } => {
                filters.iter().any(filter_references)
            }
            TargetFilter::Not { filter } => filter_references(filter),
            _ => false,
        }
    }
    if let Some(filter) = effect.target_filter() {
        if filter_references(filter) {
            return true;
        }
    }
    false
}

/// CR 608.2k: Extract the trigger subject from condition text for pronoun context.
/// Reuses `parse_trigger_subject` but only needs the `TargetFilter`, not the remainder.
/// For subjectless triggers (phase, player-action, game mechanics), the result is `Any`
/// and `resolve_it_pronoun` falls back to `SelfRef`.
///
/// Warnings from `parse_trigger_subject` are discarded — this function is a best-effort
/// subject extraction for pronoun resolution, not a diagnostic site. Warnings for
/// degraded subjects are emitted by the main trigger condition path instead.
fn extract_trigger_subject_for_context(
    condition_text: &str,
    ctx: &mut ParseContext,
) -> TargetFilter {
    let lower = condition_text.to_lowercase();
    let after_keyword = alt((
        value((), tag::<_, _, OracleError<'_>>("whenever ")),
        value((), tag("when ")),
    ))
    .parse(lower.as_str())
    .map(|(rest, _)| rest)
    .unwrap_or(&lower);

    // CR 608.2k: Player-actor subjects ("another player attacks …", "an opponent
    // attacks …") — return a player-typed filter carrying `ControllerRef::Opponent`
    // so `resolve_they_pronoun` in effect parsing maps "they" to `TriggeringPlayer`.
    // Must precede `parse_trigger_subject`, which is object-oriented and would miss
    // these.
    if alt((
        tag::<_, _, OracleError<'_>>("another player "),
        tag("an opponent "),
    ))
    .parse(after_keyword)
    .is_ok()
    {
        return TargetFilter::Typed(TypedFilter::default().controller(ControllerRef::Opponent));
    }

    // Snapshot diagnostics, call parse_trigger_subject, discard any diagnostics
    // it emits (truncate back to snapshot). This avoids maintaining a parallel
    // list of "subjectless" trigger patterns.
    let pre_snapshot = ctx.diagnostics.len();
    let (subject, _) = parse_trigger_subject(after_keyword, ctx);
    ctx.diagnostics.truncate(pre_snapshot);
    subject
}

// ---------------------------------------------------------------------------
// Subject parsing: extracts the trigger subject filter and remaining text
// ---------------------------------------------------------------------------

/// Parse a trigger subject from the beginning of the condition text (after when/whenever).
/// Returns (TargetFilter for valid_card, remaining text after subject).
///
/// Handles compound subjects joined by "or":
///   "~ or another creature or artifact you control enters"
///   → Or { SelfRef, Typed{Creature, You, [Another]}, Typed{Artifact, You, [Another]} }
///   with remaining text "enters"
fn parse_trigger_subject<'a>(text: &'a str, ctx: &mut ParseContext) -> (TargetFilter, &'a str) {
    let (first, rest) = parse_single_subject(text, ctx);

    // Check for "and/or " or "or " combinator to build compound subjects.
    // CR 603.2c: "~ and/or one or more other creatures" means the trigger fires
    // when any matching object enters — semantically equivalent to "or" for triggers.
    let rest_trimmed = rest.trim_start();
    if let Ok((after_sep, ())) = alt((
        value((), tag::<_, _, OracleError<'_>>("and/or ")),
        value((), tag::<_, _, OracleError<'_>>("or ")),
    ))
    .parse(rest_trimmed)
    {
        let (second, final_rest) = parse_trigger_subject(after_sep, ctx);
        return (merge_or_filters(first, second), final_rest);
    }

    (first, rest)
}

/// Which downstream parser handles a recognized player-subject event verb.
#[derive(Clone, Copy, PartialEq, Eq)]
enum PlayerEventVerbRoute {
    /// `try_parse_event` parses this verb on `verb_rest` (draw / life / mill).
    ViaEvent,
    /// `try_parse_player_trigger` parses this verb on a reconstructed line
    /// (cast / sacrifice / discard).
    ViaPlayerTrigger,
}

/// CR 603.4: Recognize a player-subject event-verb head and report which
/// downstream parser handles it. ViaEvent verbs (CR 121.1 draw, CR 119.3 life,
/// CR 701.17a mill) are parsed by `try_parse_event` on `verb_rest`;
/// ViaPlayerTrigger verbs (CR 601.2 cast, CR 701.21 sacrifice, CR 701.9
/// discard) are parsed only by `try_parse_player_trigger`.
fn parse_player_event_verb_head(input: &str) -> OracleResult<'_, PlayerEventVerbRoute> {
    alt((
        value(
            PlayerEventVerbRoute::ViaEvent,
            alt((
                tag("draws a card"),
                tag("gains life"),
                tag("loses life"),
                tag("mills "),
            )),
        ),
        value(
            PlayerEventVerbRoute::ViaPlayerTrigger,
            alt((tag("casts "), tag("sacrifices "), tag("discards "))),
        ),
    ))
    .parse(input)
}

/// CR 603.4: Recognize an event verb at the head of `input`. Returns the
/// remainder after the verb. Used by `find_clause_verb_boundary` to detect
/// where a trigger-subject relative clause ends and the event verb begins.
/// Delegates to `parse_player_event_verb_head` so the full player-event-verb
/// set (draw / life / mill / cast / sacrifice / discard) is recognized.
fn event_verb_lookahead(input: &str) -> OracleResult<'_, ()> {
    parse_player_event_verb_head(input).map(|(rest, _)| (rest, ()))
}

/// CR 603.4: Map a parsed player-subject filter back to its canonical actor
/// phrase, for reconstructing a player-trigger line to re-dispatch through
/// `try_parse_player_trigger`. Inverse of the player-subject `value(...)` arms
/// in `parse_single_subject`.
fn canonical_actor_phrase(subject: &TargetFilter) -> Option<&'static str> {
    match subject {
        TargetFilter::Typed(tf) if tf.controller == Some(ControllerRef::Opponent) => {
            Some("an opponent")
        }
        TargetFilter::Player => Some("a player"),
        _ => None,
    }
}

/// CR 603.4: AND an `ObjectCount >= 1` intervening-if onto `def.condition` from
/// a stashed "who controls F" relative clause. F's controller is rewritten to
/// `TriggeringPlayer` so the count is over permanents the triggering player
/// controls. Shared by the `try_parse_event` path and the `ViaPlayerTrigger`
/// reconstruction fallback in `parse_trigger_condition`.
fn lift_subject_clause(def: &mut TriggerDefinition, clause_filter: TargetFilter) {
    let f_with_triggering_player = with_triggering_player_controller(clause_filter);
    let clause_cond = TriggerCondition::QuantityComparison {
        lhs: QuantityExpr::Ref {
            qty: QuantityRef::ObjectCount {
                filter: f_with_triggering_player,
            },
        },
        comparator: Comparator::GE,
        rhs: QuantityExpr::Fixed { value: 1 },
    };
    def.condition = Some(match def.condition.take() {
        Some(existing) => TriggerCondition::And {
            conditions: vec![existing, clause_cond],
        },
        None => clause_cond,
    });
}

/// CR 603.4: Split `text` at the first word boundary where an event verb
/// begins. Scans word boundaries (the `scan_timing_restrictions`/`scan_for_phase`
/// idiom) rather than substring-searching. Returns `(clause_text, verb_rest)`
/// where `clause_text` is the relative-clause filter source and `verb_rest`
/// starts at the event verb. Returns `None` when no event verb is found.
fn find_clause_verb_boundary(text: &str) -> Option<(&str, &str)> {
    let mut offset = 0;
    loop {
        let candidate = &text[offset..];
        if event_verb_lookahead(candidate).is_ok() {
            return Some((text[..offset].trim_end(), candidate));
        }
        // Advance to the next word boundary.
        let space = candidate.find(' ')?;
        offset += space + 1;
    }
}

/// Parse a single (non-compound) trigger subject.
fn parse_single_subject<'a>(text: &'a str, ctx: &mut ParseContext) -> (TargetFilter, &'a str) {
    // Self-reference: "~"
    if let Ok((rest, ())) = value((), tag::<_, _, OracleError<'_>>("~ ")).parse(text) {
        return (TargetFilter::SelfRef, rest);
    }
    if text == "~" {
        return (TargetFilter::SelfRef, "");
    }

    if let Ok((rest, ())) = value((), tag::<_, _, OracleError<'_>>("this ")).parse(text) {
        let noun_end = rest.find(' ').unwrap_or(rest.len());
        if noun_end > 0 {
            return (TargetFilter::SelfRef, rest[noun_end..].trim_start());
        }
    }

    // CR 608.2c + CR 603.7c: In delayed triggers created by targeted spells,
    // "that <noun>" refers back to the parent ability's chosen target.
    if let Ok((rest, ())) = value((), tag::<_, _, OracleError<'_>>("that ")).parse(text) {
        let noun_end = rest.find(' ').unwrap_or(rest.len());
        if noun_end > 0 {
            return (TargetFilter::ParentTarget, rest[noun_end..].trim_start());
        }
    }

    // "equipped creature" / "enchanted creature/land/permanent" / "enchanted <basic-type>"
    // → AttachedTo. The Enchant keyword already constrains the attach target's type,
    // so `AttachedTo` alone is sufficient here (CR 702.5a). Utopia Sprawl's
    // "enchanted Forest" trigger lowercases to "enchanted forest" before this runs.
    // Use nom alt() for the set of fixed attached-to prefixes (input already lowercase).
    fn parse_attached_to_prefix(input: &str) -> OracleResult<'_, ()> {
        alt((
            value((), tag("equipped creature ")),
            value((), tag("enchanted creature ")),
            value((), tag("enchanted land ")),
            value((), tag("enchanted permanent ")),
            // CR 205.3i: basic land types — used by Auras that enchant a specific basic
            // (Utopia Sprawl's "enchanted Forest", Thriving Isle-style "enchanted Island", etc.).
            value((), tag("enchanted plains ")),
            value((), tag("enchanted island ")),
            value((), tag("enchanted swamp ")),
            value((), tag("enchanted mountain ")),
            value((), tag("enchanted forest ")),
        ))
        .parse(input)
    }
    if let Ok((rest, ())) = parse_attached_to_prefix.parse(text) {
        return (TargetFilter::AttachedTo, rest);
    }
    // Exact-match variants (no trailing space — end of input)
    fn parse_attached_to_exact(input: &str) -> OracleResult<'_, ()> {
        alt((
            value((), tag("equipped creature")),
            value((), tag("enchanted creature")),
            value((), tag("enchanted land")),
            value((), tag("enchanted permanent")),
            // CR 205.3i: basic land types (exact-match end-of-input variants).
            value((), tag("enchanted plains")),
            value((), tag("enchanted island")),
            value((), tag("enchanted swamp")),
            value((), tag("enchanted mountain")),
            value((), tag("enchanted forest")),
        ))
        .parse(input)
    }
    if let Ok((_rest, ())) = parse_attached_to_exact.parse(text) {
        return (TargetFilter::AttachedTo, "");
    }

    // "another <type phrase>" — compose with FilterProp::Another
    if let Ok((after_another, ())) = value((), tag::<_, _, OracleError<'_>>("another ")).parse(text)
    {
        let (filter, rest) = parse_type_phrase(after_another);
        let with_another = add_another_prop(filter);
        return (with_another, rest);
    }

    if let Ok((after_quantifier, ())) =
        value((), tag::<_, _, OracleError<'_>>("one or more ")).parse(text)
    {
        // CR 122.6: Passive voice counter placement: "one or more [type] counters are put on [subject]"
        // The subject is the object receiving counters, not the counters themselves.
        // Use split_once_on to find the " are put on " / " are placed on " boundary.
        if let Ok((_, (_, subject_text))) =
            nom_primitives::split_once_on(after_quantifier, " are put on ")
        {
            let (filter, rest) = parse_single_subject(subject_text, ctx);
            return (filter, rest);
        }
        if let Ok((_, (_, subject_text))) =
            nom_primitives::split_once_on(after_quantifier, " are placed on ")
        {
            let (filter, rest) = parse_single_subject(subject_text, ctx);
            return (filter, rest);
        }

        let (filter, rest) = parse_type_phrase(after_quantifier);
        if rest.len() < after_quantifier.len() {
            return (filter, rest);
        }
    }

    // "you put one or more [type] counters on [subject]" — active voice counter placement.
    // Use split_once_on to locate the " on " boundary after counter type text.
    if let Ok((after_put, ())) =
        value((), tag::<_, _, OracleError<'_>>("you put one or more ")).parse(text)
    {
        if let Ok((_, (_, subject_text))) = nom_primitives::split_once_on(after_put, " on ") {
            let (filter, rest) = parse_single_subject(subject_text, ctx);
            return (filter, rest);
        }
    }

    // CR 608.2k: Player subjects for pronoun resolution in trigger effects.
    // "an opponent", "a player", "each opponent" — these are player-type subjects,
    // not object types. Must fire before the generic "a "/"an " + parse_type_phrase
    // path, which would send "opponent" to parse_type_phrase and return Any.
    // "each opponent" maps to the same filter as "an opponent" for subject extraction;
    // the trigger mode (not the subject filter) determines per-opponent firing.
    if let Ok((rest, filter)) = alt((
        value(
            TargetFilter::Typed(TypedFilter::default().controller(ControllerRef::Opponent)),
            alt((tag::<_, _, OracleError<'_>>("an opponent"), tag("opponent"))),
        ),
        value(
            TargetFilter::Typed(TypedFilter::default().controller(ControllerRef::Opponent)),
            tag("each opponent"),
        ),
        value(TargetFilter::Player, alt((tag("a player"), tag("player")))),
    ))
    .parse(text)
    {
        let rest = rest.trim_start();
        // CR 603.4: Relative clause "who control[s] <filter>" —
        // a control-presence restriction on the player subject. When present,
        // parse the clause filter, stash it on the context for
        // `parse_trigger_condition` to lift into an intervening-if, and
        // continue parsing the event verb from after the clause.
        if let Ok((after_who, ())) = value(
            (),
            (
                tag::<_, _, OracleError<'_>>("who control"),
                opt(tag("s")),
                tag(" "),
            ),
        )
        .parse(rest)
        {
            if let Some((clause_text, verb_rest)) = find_clause_verb_boundary(after_who) {
                let (clause_filter, clause_remainder) = parse_type_phrase(clause_text);
                // Only treat this as a control-relative clause when the clause
                // text parsed cleanly into a typed filter.
                if clause_remainder.trim().is_empty() && !matches!(clause_filter, TargetFilter::Any)
                {
                    ctx.pending_trigger_subject_clause = Some(clause_filter);
                    return (filter, verb_rest);
                }
            }
        }
        return (filter, rest);
    }

    // "a "/"an " + type phrase (general subject)
    if let Ok((after, ())) = alt((
        value((), tag::<_, _, OracleError<'_>>("a ")),
        value((), tag("an ")),
    ))
    .parse(text)
    {
        let (filter, rest) = parse_type_phrase(after);
        return (filter, rest);
    }

    let (filter, rest) = parse_type_phrase(text);
    if rest.len() < text.len() {
        return (filter, rest);
    }

    ctx.push_diagnostic(OracleDiagnostic::TargetFallback {
        context: "trigger subject parse fell back to Any".into(),
        text: text.trim().into(),
        line_index: 0,
    });
    (TargetFilter::Any, text)
}

/// Add FilterProp::Another to a TargetFilter. Distributes into Or branches recursively.
fn add_another_prop(filter: TargetFilter) -> TargetFilter {
    match filter {
        TargetFilter::Typed(TypedFilter {
            type_filters,
            controller,
            mut properties,
        }) => {
            properties.push(FilterProp::Another);
            TargetFilter::Typed(TypedFilter {
                type_filters,
                controller,
                properties,
            })
        }
        TargetFilter::Or { filters } => TargetFilter::Or {
            filters: filters.into_iter().map(add_another_prop).collect(),
        },
        _ => TargetFilter::Typed(TypedFilter::default().properties(vec![FilterProp::Another])),
    }
}

fn add_controller(filter: TargetFilter, controller: ControllerRef) -> TargetFilter {
    match filter {
        TargetFilter::Typed(TypedFilter {
            type_filters,
            controller: existing,
            properties,
        }) => TargetFilter::Typed(TypedFilter {
            type_filters,
            controller: Some(existing.unwrap_or(controller)),
            properties,
        }),
        TargetFilter::Or { filters } => TargetFilter::Or {
            filters: filters
                .into_iter()
                .map(|filter| add_controller(filter, controller.clone()))
                .collect(),
        },
        other => other,
    }
}

/// CR 603.2 + CR 109.4: Rewrite a relative-clause filter's top-level
/// `TypedFilter.controller` to `ControllerRef::TriggeringPlayer` so the
/// `ObjectCount` intervening-if counts permanents controlled by the
/// triggering player ("an opponent **who controls F**"). Distributes into
/// `Or` branches; leaves non-`Typed` filters unchanged.
fn with_triggering_player_controller(filter: TargetFilter) -> TargetFilter {
    match filter {
        TargetFilter::Typed(TypedFilter {
            type_filters,
            controller: _,
            properties,
        }) => TargetFilter::Typed(TypedFilter {
            type_filters,
            controller: Some(ControllerRef::TriggeringPlayer),
            properties,
        }),
        TargetFilter::Or { filters } => TargetFilter::Or {
            filters: filters
                .into_iter()
                .map(with_triggering_player_controller)
                .collect(),
        },
        other => other,
    }
}

// ---------------------------------------------------------------------------
// Event verb parsing: matches the event after the subject
// ---------------------------------------------------------------------------

/// Parse the "to <target>" qualifier that follows a damage verb.
/// Returns a `TargetFilter` for the three common cases:
/// - "to a player"         → `Player`
/// - "to an opponent"      → opponent-controlled TypedFilter
/// - "to you"              → `Controller`
///
fn parse_damage_to_qualifier(after_verb: &str) -> Option<TargetFilter> {
    let (rest, ()) = value((), tag::<_, _, OracleError<'_>>("to "))
        .parse(after_verb.trim_start())
        .ok()?;
    // Use nom alt() to match damage target qualifiers (input already lowercase)
    fn parse_damage_target(input: &str) -> OracleResult<'_, TargetFilter> {
        alt((
            value(
                TargetFilter::Or {
                    filters: vec![
                        TargetFilter::Player,
                        TargetFilter::Typed(TypedFilter::new(TypeFilter::Planeswalker)),
                    ],
                },
                alt((
                    tag("a player or planeswalker"),
                    tag("a player or a planeswalker"),
                )),
            ),
            value(TargetFilter::Player, tag("a player")),
            value(
                TargetFilter::Typed(TypedFilter::default().controller(ControllerRef::Opponent)),
                tag("an opponent"),
            ),
            value(
                TargetFilter::Typed(TypedFilter::default().controller(ControllerRef::Opponent)),
                tag("one of your opponents"),
            ),
            value(TargetFilter::Controller, tag("you")),
        ))
        .parse(input)
    }
    parse_damage_target
        .parse(rest)
        .ok()
        .map(|(_, filter)| filter)
}

/// CR 603.6a + CR 110.5b: After consuming the `"enter"` prefix in a ChangesZone
/// trigger clause, recognize an optional tapped-state rider — `"enters tapped"`
/// or `"enters untapped"` — and produce the corresponding intervening-if
/// condition so the trigger only fires when the *entering* permanent's post-ETB
/// tapped state matches.
///
/// The condition emitted is `ZoneChangeObjectIsTapped` (optionally `Not`-wrapped
/// for the untapped sense): its runtime evaluator inspects the permanent named
/// by the triggering zone-change event — i.e. the *entering* permanent — not the
/// ability-owning source. This is required for observer triggers whose subject
/// is another permanent (`valid_card` ≠ `SelfRef`), e.g. Amulet of Vigor seeing
/// a different permanent enter tapped. For `SelfRef` triggers the entering
/// permanent IS the source, so the evaluator's `source_id` fallback resolves to
/// the same object.
///
/// The `input` here is the remainder after `tag("enter")`, so the rider begins
/// with `"s "` (the rest of "enters" plus a space) followed by the state word.
/// A trailing word-boundary check ensures we don't swallow `"untapped creatures"`
/// or similar accidental prefix matches — only an exact phrase terminator
/// (end-of-string, space, or punctuation) is accepted.
///
/// Covers the Throne of Eldraine dual-land cycle triggers
/// (Gingerbread Cabin, Idyllic Grange, Dwarven Mine, Mystic Sanctuary,
/// Witch's Cottage), Charismatic Conqueror's untapped-ETB trigger, and the
/// parallel `"enters tapped"` class (Amulet of Vigor, Tiller Engine).
fn parse_enters_tapped_state_rider(input: &str) -> Option<TriggerCondition> {
    // Must start with "s " (completing the "enters" event verb) followed by
    // the state word. Using nom tags keeps dispatch structural, not string-
    // matched.
    let (after_state, negated) = preceded(
        tag::<_, _, OracleError<'_>>("s "),
        alt((
            value(true, tag::<_, _, OracleError<'_>>("untapped")),
            value(false, tag::<_, _, OracleError<'_>>("tapped")),
        )),
    )
    .parse(input)
    .ok()?;

    // Word-boundary: reject false prefix matches like "untapped creatures".
    // Accept end-of-string or any non-alphanumeric terminator (space, comma,
    // period, etc.).
    if !after_state.is_empty()
        && after_state
            .chars()
            .next()
            .is_some_and(|c| c.is_alphanumeric() || c == '_')
    {
        return None;
    }

    Some(if negated {
        TriggerCondition::Not {
            condition: Box::new(TriggerCondition::ZoneChangeObjectIsTapped),
        }
    } else {
        TriggerCondition::ZoneChangeObjectIsTapped
    })
}

/// CR 701.17a: Parse the milled-card filter from the predicate tail of an
/// active-voice mill trigger ("mills **a nonland card**", "mills **one or more
/// creature cards**"). Optionally consumes a leading "one or more " quantifier
/// (semantically redundant — `valid_card` matching is per-object), then
/// delegates the typed-filter recognition to `parse_type_phrase` (which strips
/// the "a "/"an " article itself). Returns `None` when the tail is not a
/// recognizable type phrase, so the caller falls through to the next arm.
fn parse_milled_object_filter(input: &str) -> Option<TargetFilter> {
    let after_quantifier = value((), tag::<_, _, OracleError<'_>>("one or more "))
        .parse(input)
        .map(|(rest, _)| rest)
        .unwrap_or(input);
    let (filter, rest) = parse_type_phrase(after_quantifier);
    // Require the type phrase to have consumed something — a bare fallthrough
    // (rest == after_quantifier) means no typed filter was recognized.
    if rest.len() < after_quantifier.len() {
        Some(filter)
    } else {
        None
    }
}

/// Returns `true` when the trigger subject is an opponent-scoped player filter
/// (produced by `parse_single_subject` for "an opponent" / "each opponent").
/// Used by the active-voice mill arm to scope the milled-card filter to the
/// opponent's library.
fn subject_is_opponent(subject: &TargetFilter) -> bool {
    matches!(
        subject,
        TargetFilter::Typed(TypedFilter {
            controller: Some(ControllerRef::Opponent),
            ..
        })
    )
}

/// Try to parse an event verb and build a TriggerDefinition from subject + event.
fn try_parse_event(
    subject: &TargetFilter,
    rest: &str,
    full_lower: &str,
) -> Option<(TriggerMode, TriggerDefinition)> {
    let rest = rest.trim_start();

    // --- Compound triggers (nom alt for prefix matching) ---
    // "enters or attacks" / "enters the battlefield or attacks"
    if tag::<_, _, OracleError<'_>>("enters or attacks")
        .parse(rest)
        .is_ok()
        || tag::<_, _, OracleError<'_>>("enters the battlefield or attacks")
            .parse(rest)
            .is_ok()
    {
        let mut def = make_base();
        def.mode = TriggerMode::EntersOrAttacks;
        def.destination = Some(Zone::Battlefield);
        def.valid_card = Some(subject.clone());
        return Some((TriggerMode::EntersOrAttacks, def));
    }

    // "attacks or blocks"
    if tag::<_, _, OracleError<'_>>("attacks or blocks")
        .parse(rest)
        .is_ok()
    {
        let mut def = make_base();
        def.mode = TriggerMode::AttacksOrBlocks;
        def.valid_card = Some(subject.clone());
        return Some((TriggerMode::AttacksOrBlocks, def));
    }

    // "enters [the battlefield]" / "enter [the battlefield]" (plural for "one or more" subjects)
    if let Ok((after_enter, ())) = value((), tag::<_, _, OracleError<'_>>("enter")).parse(rest) {
        let mut def = make_base();
        def.mode = TriggerMode::ChangesZone;
        def.destination = Some(Zone::Battlefield);
        def.valid_card = Some(subject.clone());

        // CR 603.6 + CR 603.6a: "enters from <zone>" — origin-zone qualifier on
        // an ETB trigger restricts which zone-change events match. Without
        // this, a battlefield permanent with "whenever a creature enters from
        // your graveyard" would fire on every creature entering from anywhere
        // (cast from hand, returned from exile, commander from command zone),
        // because the runtime trigger matcher treats `origin: None` as
        // "any origin zone." Issue #396 (Flayer of the Hatebound firing on
        // commanders cast from the command zone) is exactly this drop.
        //
        // The origin qualifier may appear immediately after the verb
        // ("enters from your graveyard, …") or after the battlefield phrase
        // ("enters the battlefield from a graveyard"). Scan the condition
        // segment at word boundaries with the typed combinator so the order
        // of the qualifier does not matter. (The effect segment is already
        // separated upstream by `split_trigger`, so a tail clause like
        // "return that card from your graveyard" cannot poison this scan.)
        //
        // TODO: extend `parse_enters_origin_zone` to handle "from anywhere",
        // "from anywhere other than <zone>", and opponent-scoped origins as
        // those patterns surface in real cards.
        let mut scan = after_enter.trim_start();
        while !scan.is_empty() {
            if let Ok((_, origin)) = parse_enters_origin_zone(scan) {
                def.origin = Some(origin);
                break;
            }
            scan = scan.find(' ').map_or("", |i| scan[i + 1..].trim_start());
        }

        // CR 603.6a + CR 110.5b: "enters untapped" / "enters tapped" — conditional
        // ETB trigger gated on the *entering* permanent's tapped state at
        // trigger-check time. The tapped-state check examines the object after
        // ETB replacement effects (e.g. "enters tapped unless you control three
        // or more Forests") have resolved, per CR 603.6a's "check at the moment
        // the event fires". The `ZoneChangeObjectIsTapped` runtime evaluator
        // (game/triggers.rs) inspects `obj.tapped` on the object named by the
        // triggering zone-change event, which by then reflects the
        // post-replacement state.
        if let Some(cond) = parse_enters_tapped_state_rider(after_enter) {
            def.condition = Some(cond);
        }

        return Some((TriggerMode::ChangesZone, def));
    }

    // CR 700.4: "Dies"/"die" means "is put into a graveyard from the battlefield."
    fn parse_dies_verb(input: &str) -> OracleResult<'_, ()> {
        alt((
            value((), tag("die")),
            value((), tag("is put into a graveyard from the battlefield")),
            value((), tag("are put into a graveyard from the battlefield")),
        ))
        .parse(input)
    }
    if parse_dies_verb.parse(rest).is_ok() {
        let mut def = make_base();
        def.mode = TriggerMode::ChangesZone;
        def.origin = Some(Zone::Battlefield);
        def.destination = Some(Zone::Graveyard);
        def.valid_card = Some(subject.clone());
        return Some((TriggerMode::ChangesZone, def));
    }

    // CR 120.1: "deals combat damage" / "deal combat damage" (plural for &-names)
    if let Ok((after, ())) = alt((
        value((), tag::<_, _, OracleError<'_>>("deals combat damage")),
        value((), tag("deal combat damage")),
    ))
    .parse(rest)
    {
        let mut def = make_base();
        def.mode = TriggerMode::DamageDone;
        def.damage_kind = DamageKindFilter::CombatOnly;
        def.valid_source = Some(subject.clone());
        def.valid_target = parse_damage_to_qualifier(after);
        return Some((TriggerMode::DamageDone, def));
    }

    // CR 120.1: "deals damage" / "deal damage" (plural for &-names)
    if let Ok((after, ())) = alt((
        value((), tag::<_, _, OracleError<'_>>("deals damage")),
        value((), tag("deal damage")),
    ))
    .parse(rest)
    {
        let mut def = make_base();
        def.mode = TriggerMode::DamageDone;
        def.valid_source = Some(subject.clone());
        def.valid_target = parse_damage_to_qualifier(after);
        return Some((TriggerMode::DamageDone, def));
    }

    // CR 508.1a: "~ and at least N other creatures attack" (Battalion/Pack Tactics)
    if let Ok((after_and, ())) = alt((
        value((), tag::<_, _, OracleError<'_>>("and at least ")),
        value((), tag("and ")),
    ))
    .parse(rest)
    {
        if scan_contains(after_and, "attack") {
            if let Some((n, _rest_after_n)) = parse_number(after_and) {
                let mut def = make_base();
                def.mode = TriggerMode::Attacks;
                def.valid_card = Some(subject.clone());
                def.condition = Some(TriggerCondition::MinCoAttackers { minimum: n });
                return Some((TriggerMode::Attacks, def));
            }
        }
    }

    // "attacks" (singular) or "attack" (plural — multi-name cards like "Raph & Leo")
    // Guard against false-matching "attacker"/"attacking".
    let attacks_result = tag::<_, _, OracleError<'_>>("attacks")
        .parse(rest)
        .map(|(r, _)| r)
        .ok()
        .or_else(|| {
            tag::<_, _, OracleError<'_>>("attack")
                .parse(rest)
                .ok()
                .map(|(r, _)| r)
                .filter(|r| !r.starts_with("er") && !r.starts_with("ing"))
        });
    if let Some(after) = attacks_result {
        let attacks_and_isnt_blocked = value(
            (),
            alt((
                tag::<_, _, OracleError<'_>>(" and isn't blocked"),
                tag(" and isn\u{2019}t blocked"),
            )),
        )
        .parse(after)
        .is_ok();
        // CR 508.3a: Detect attack target qualifier ("attacks a planeswalker" etc.)
        fn parse_attack_target(input: &str) -> OracleResult<'_, AttackTargetFilter> {
            alt((
                value(
                    AttackTargetFilter::PlayerOrPlaneswalker,
                    alt((
                        tag(" you or a planeswalker you control"),
                        tag(" you and/or one or more planeswalkers you control"),
                    )),
                ),
                value(AttackTargetFilter::Planeswalker, tag(" a planeswalker")),
                value(AttackTargetFilter::Player, tag(" a player")),
                value(AttackTargetFilter::Player, tag(" you")),
                value(AttackTargetFilter::Battle, tag(" a battle")),
            ))
            .parse(input)
        }
        let attack_target_filter = parse_attack_target.parse(after).ok().map(|(_, f)| f);
        let mut def = make_base();
        def.mode = if attacks_and_isnt_blocked && matches!(subject, TargetFilter::SelfRef) {
            TriggerMode::AttackerUnblocked
        } else {
            TriggerMode::Attacks
        };
        def.valid_card = Some(subject.clone());
        def.attack_target_filter = attack_target_filter;
        if matches!(
            def.attack_target_filter,
            Some(AttackTargetFilter::PlayerOrPlaneswalker) | Some(AttackTargetFilter::Player)
        ) && tag::<_, _, OracleError<'_>>(" you").parse(after).is_ok()
        {
            def.valid_target = Some(TargetFilter::Controller);
        }
        return Some((TriggerMode::Attacks, def));
    }

    // "blocks" — fires for the blocking creature.
    if tag::<_, _, OracleError<'_>>("blocks").parse(rest).is_ok() {
        let mut def = make_base();
        def.mode = TriggerMode::Blocks;
        def.valid_card = Some(subject.clone());
        return Some((TriggerMode::Blocks, def));
    }

    // "leaves the battlefield" / "leaves"
    if alt((
        value((), tag::<_, _, OracleError<'_>>("leaves the battlefield")),
        value((), tag("leaves")),
    ))
    .parse(rest)
    .is_ok()
    {
        let mut def = make_base();
        def.mode = TriggerMode::LeavesBattlefield;
        def.valid_card = Some(subject.clone());
        // CR 113.6k + CR 603.10: Self-referential LTB triggers (e.g. Oblivion Ring,
        // "when ~ leaves the battlefield") must continue to function after the
        // source has moved to graveyard/exile, because the trigger ability is tied
        // to the object that left. Non-self-referential LTB triggers (e.g. "whenever
        // a creature you control leaves the battlefield") live on a permanent that
        // is still on the battlefield, so `trigger_zones` stays empty (battlefield
        // default).
        if filter_references_self(subject) {
            def.trigger_zones = vec![Zone::Battlefield, Zone::Graveyard, Zone::Exile];
        }
        return Some((TriggerMode::LeavesBattlefield, def));
    }

    // CR 700.4: "is put into a graveyard from [zone]" / "is put into [possessive] graveyard [from zone]"
    if let Some(result) = try_parse_put_into_graveyard(subject, rest) {
        return Some(result);
    }

    // CR 400.3 + CR 603.10: "is put into your hand from your graveyard" — dredge-style
    // reanimate triggers (Golgari Brownscale). Fires from the graveyard zone, so
    // trigger_zones must extend beyond the battlefield default.
    if let Some(result) = try_parse_put_into_hand_from(subject, rest) {
        return Some(result);
    }

    // CR 701.13a: "[subject] is put into exile from [zone]" — explicit zone-change
    // form of the exile trigger (God-Eternal Oketra). Self-referential triggers
    // need trigger_zones beyond battlefield because the source is in exile when
    // the ability resolves.
    if let Some(result) = try_parse_put_into_exile_from(subject, rest) {
        return Some(result);
    }

    // CR 701.13a: "is exiled" / "are exiled" — exile trigger
    if alt((
        value((), tag::<_, _, OracleError<'_>>("is exiled")),
        value((), tag("are exiled")),
    ))
    .parse(rest)
    .is_ok()
    {
        let mut def = make_base();
        def.mode = TriggerMode::Exiled;
        def.valid_card = Some(subject.clone());
        return Some((TriggerMode::Exiled, def));
    }

    // CR 701.21: "is sacrificed" / "are sacrificed" — sacrifice trigger
    if alt((
        value((), tag::<_, _, OracleError<'_>>("is sacrificed")),
        value((), tag("are sacrificed")),
    ))
    .parse(rest)
    .is_ok()
    {
        let mut def = make_base();
        def.mode = TriggerMode::Sacrificed;
        def.valid_card = Some(subject.clone());
        return Some((TriggerMode::Sacrificed, def));
    }

    // CR 701.8: "is destroyed" / "are destroyed" — destroy trigger
    if alt((
        value((), tag::<_, _, OracleError<'_>>("is destroyed")),
        value((), tag("are destroyed")),
    ))
    .parse(rest)
    .is_ok()
    {
        let mut def = make_base();
        def.mode = TriggerMode::Destroyed;
        def.valid_card = Some(subject.clone());
        return Some((TriggerMode::Destroyed, def));
    }

    // CR 701.17a: "is milled" / "are milled" — passive-voice mill trigger.
    // "For a player to mill a number of cards, that player puts that many cards
    // from the top of their library into their graveyard." Mirrors the `is exiled`
    // / `is sacrificed` arms above. The passive subject (parsed by
    // `parse_trigger_subject`) IS the milled card, so it maps straight to
    // `valid_card`. The per-batch vs per-card distinction (CR 603.2c — an
    // ability triggers only once each time its trigger event occurs) is carried
    // by `def.batched`, which the caller stamps from the "one or more …"
    // subject quantifier — this arm emits `TriggerMode::Milled` unconditionally.
    if alt((
        value((), tag::<_, _, OracleError<'_>>("is milled")),
        value((), tag("are milled")),
    ))
    .parse(rest)
    .is_ok()
    {
        let mut def = make_base();
        def.mode = TriggerMode::Milled;
        def.valid_card = Some(subject.clone());
        return Some((TriggerMode::Milled, def));
    }

    // CR 701.17a + CR 603.2c: "mills <object filter>" — active-voice mill trigger
    // ("a player mills a nonland card", "an opponent mills a nonland card",
    // "a player mills one or more creature cards"). Here the parsed subject is
    // the *milling player*, not the milled card; the milled card is the typed
    // filter in the predicate tail. This mirrors how `DamageDone` separates
    // `valid_source` from `valid_target`. When the milling player is an
    // opponent, the milled card lives in that opponent's library, so the
    // milled-card filter carries `ControllerRef::Opponent`. As above, the mode
    // is emitted unconditionally; `def.batched` is the caller's responsibility.
    if let Ok((after_mills, ())) = value((), tag::<_, _, OracleError<'_>>("mills ")).parse(rest) {
        if let Some(card_filter) = parse_milled_object_filter(after_mills) {
            let card_filter = if subject_is_opponent(subject) {
                add_controller(card_filter, ControllerRef::Opponent)
            } else {
                card_filter
            };
            let mut def = make_base();
            def.mode = TriggerMode::Milled;
            def.valid_card = Some(card_filter);
            return Some((TriggerMode::Milled, def));
        }
    }

    // CR 701.14: "fights" / "fight" — fight trigger
    // Guard against false-matching "fighting".
    {
        let fights_result = tag::<_, _, OracleError<'_>>("fights")
            .parse(rest)
            .map(|(r, _)| r)
            .ok()
            .or_else(|| {
                tag::<_, _, OracleError<'_>>("fight")
                    .parse(rest)
                    .ok()
                    .map(|(r, _)| r)
                    .filter(|r| !r.starts_with("ing") && !r.starts_with("s"))
            });
        if let Some(_after) = fights_result {
            let mut def = make_base();
            def.mode = TriggerMode::Fight;
            def.valid_card = Some(subject.clone());
            return Some((TriggerMode::Fight, def));
        }
    }

    // Simple event verbs using nom alt() — each maps to a single TriggerMode
    // These are all "is_some()" pattern strip_prefix calls
    #[derive(Clone)]
    enum SimpleEvent {
        BecomesBlocked,
        BecomesSaddled,
        BecomesCrewed,
        BecomesTargetSpellOrAbility,
        BecomesTargetSpellOnly,
        DealtCombatDamage,
        DealtDamage,
        BecomesTapped,
        TappedForMana,
        BecomesUntapped,
        TurnFaceUp,
        Mutates,
        ExploitsCreature,
        Exploits,
        Transforms,
        Stations,
        SaddlesOrCrews,
        Crews,
        Saddles,
        // CR 701.3a: Equipment/Aura becomes attached to a permanent.
        BecomesAttached,
    }
    fn parse_simple_event(input: &str) -> OracleResult<'_, SimpleEvent> {
        alt((
            value(SimpleEvent::BecomesBlocked, tag("becomes blocked")),
            // CR 702.171b: Mount becomes saddled (saddled designation acquired).
            value(SimpleEvent::BecomesSaddled, tag("becomes saddled")),
            // CR 702.122d: "Whenever [this Vehicle] becomes crewed" — trigger fires
            // when a crew ability of this Vehicle resolves. Needed for Mighty Servant
            // of Leuk-O, Mindlink Mech, etc.
            value(SimpleEvent::BecomesCrewed, tag("becomes crewed")),
            value(
                SimpleEvent::BecomesTargetSpellOrAbility,
                tag("becomes the target of a spell or ability"),
            ),
            value(
                SimpleEvent::BecomesTargetSpellOnly,
                tag("becomes the target of a spell"),
            ),
            value(
                SimpleEvent::DealtCombatDamage,
                tag("is dealt combat damage"),
            ),
            value(SimpleEvent::DealtDamage, tag("is dealt damage")),
            value(SimpleEvent::BecomesTapped, tag("becomes tapped")),
            value(SimpleEvent::TappedForMana, tag("is tapped for mana")),
        ))
        .or(alt((
            value(SimpleEvent::BecomesUntapped, tag("becomes untapped")),
            value(SimpleEvent::BecomesUntapped, tag("untaps")),
            value(SimpleEvent::TurnFaceUp, tag("is turned face up")),
            value(SimpleEvent::Mutates, tag("mutates")),
            // CR 702.110b: "exploits a creature" — exploit trigger
            value(SimpleEvent::ExploitsCreature, tag("exploits a creature")),
            value(SimpleEvent::Exploits, tag("exploits")),
            // CR 712.14: "transforms" / "transforms into"
            value(SimpleEvent::Transforms, tag("transforms")),
            // CR 702.184a: "stations ~" — actor-side Station trigger.
            value(SimpleEvent::Stations, tag("stations ")),
            // CR 702.122 + CR 702.171c: compound actor-side — MUST precede singular
            // arms so "saddles a mount or crews a vehicle" is matched whole.
            value(
                SimpleEvent::SaddlesOrCrews,
                tag("saddles a mount or crews a vehicle"),
            ),
            // CR 702.122: Actor-side crew trigger.
            value(SimpleEvent::Crews, tag("crews a vehicle")),
            // CR 702.171c: Actor-side saddle trigger (reserved — no cards today without
            // the compound, but the arm is ready for future printings).
            value(SimpleEvent::Saddles, tag("saddles a mount")),
            // CR 701.3a: "becomes attached to [a creature / a permanent / …]" —
            // Equipment/Aura attach trigger. The trailing target phrase ("to a
            // creature", "to a permanent") is parsed to populate `valid_target`.
            value(SimpleEvent::BecomesAttached, tag("becomes attached to ")),
            // Short form: "becomes attached" without a trailing target phrase
            // (future-proofing; no current Oracle cards use this form).
            value(SimpleEvent::BecomesAttached, tag("becomes attached")),
        )))
        .parse(input)
    }
    if let Ok((remaining, event)) = parse_simple_event.parse(rest) {
        let mut def = make_base();
        match event {
            SimpleEvent::BecomesBlocked => {
                def.mode = TriggerMode::BecomesBlocked;
                def.valid_card = Some(subject.clone());
            }
            SimpleEvent::BecomesTargetSpellOrAbility => {
                def.mode = TriggerMode::BecomesTarget;
                def.valid_card = Some(subject.clone());
            }
            SimpleEvent::BecomesTargetSpellOnly => {
                def.mode = TriggerMode::BecomesTarget;
                def.valid_card = Some(subject.clone());
                def.valid_source = Some(TargetFilter::StackSpell);
            }
            SimpleEvent::DealtCombatDamage => {
                def.mode = TriggerMode::DamageReceived;
                def.damage_kind = DamageKindFilter::CombatOnly;
                def.valid_card = Some(subject.clone());
            }
            SimpleEvent::DealtDamage => {
                def.mode = TriggerMode::DamageReceived;
                def.valid_card = Some(subject.clone());
            }
            SimpleEvent::BecomesTapped => {
                def.mode = TriggerMode::Taps;
                def.valid_card = Some(subject.clone());
            }
            SimpleEvent::TappedForMana => {
                def.mode = TriggerMode::TapsForMana;
                def.valid_card = Some(subject.clone());
            }
            SimpleEvent::BecomesUntapped => {
                def.mode = TriggerMode::Untaps;
                def.valid_card = Some(subject.clone());
            }
            SimpleEvent::TurnFaceUp => {
                def.mode = TriggerMode::TurnFaceUp;
                def.valid_card = Some(subject.clone());
            }
            SimpleEvent::Mutates => {
                def.mode = TriggerMode::Mutates;
                def.valid_card = Some(subject.clone());
            }
            SimpleEvent::ExploitsCreature | SimpleEvent::Exploits => {
                def.mode = TriggerMode::Exploited;
                def.valid_card = Some(subject.clone());
            }
            SimpleEvent::Transforms => {
                def.mode = TriggerMode::Transformed;
                def.valid_source = Some(subject.clone());
            }
            SimpleEvent::Stations => {
                // CR 702.184a: Station ability resolution; "a creature stations ~"
                // is the Oracle idiom. valid_source records the actor (pronoun context);
                // match_stationed filters on spacecraft_id == source_id regardless.
                def.mode = TriggerMode::Stationed;
                def.valid_source = Some(subject.clone());
            }
            SimpleEvent::BecomesSaddled => {
                // CR 702.171b: Mount becomes saddled (saddled designation acquired).
                def.mode = TriggerMode::BecomesSaddled;
                def.valid_card = Some(subject.clone());
            }
            SimpleEvent::BecomesCrewed => {
                // CR 702.122d: "Whenever [this Vehicle] becomes crewed" — fires when
                // a crew ability of this Vehicle resolves. Runtime matcher
                // (match_vehicle_crewed) already handles TriggerMode::BecomesCrewed.
                def.mode = TriggerMode::BecomesCrewed;
                def.valid_card = Some(subject.clone());
            }
            SimpleEvent::Crews => {
                // CR 702.122: Actor-side crew trigger. valid_card records the actor
                // filter; match_crews evaluates it against each creature in
                // event.creatures via matches_target_filter.
                def.mode = TriggerMode::Crews;
                def.valid_card = Some(subject.clone());
            }
            SimpleEvent::Saddles => {
                // CR 702.171c: Actor-side saddle trigger.
                def.mode = TriggerMode::Saddles;
                def.valid_card = Some(subject.clone());
            }
            SimpleEvent::SaddlesOrCrews => {
                // CR 702.122 + CR 702.171c: Compound actor-side trigger. Fires on
                // either saddling a Mount or crewing a Vehicle.
                def.mode = TriggerMode::SaddlesOrCrews;
                def.valid_card = Some(subject.clone());
            }
            SimpleEvent::BecomesAttached => {
                // CR 701.3a: "Whenever [this Equipment/Aura] becomes attached to
                // [a creature / a permanent]" — fires when an Attach or AttachAll
                // effect resolves and the source is now attached to something.
                // Non-self subjects need an event payload naming the object that
                // became attached; EffectResolved only carries the ability source.
                if !matches!(subject, TargetFilter::SelfRef) {
                    return None;
                }
                def.mode = TriggerMode::Attached;
                def.valid_card = Some(subject.clone());
                let remaining = remaining.trim();
                if !remaining.is_empty() {
                    let (filter, rest) = parse_type_phrase(remaining);
                    if !rest.trim().is_empty() {
                        return None;
                    }
                    def.valid_target = Some(filter);
                }
            }
        }
        return Some((def.mode.clone(), def));
    }

    // CR 119.3 + CR 603.2: "Whenever [subject] loses life" — player-scoped life-loss
    // trigger. Subject filter (`a player`, `an opponent`, etc.) becomes `valid_target`
    // so the matcher's `valid_player_matches` honors the scoping.
    // Covers Exquisite Blood ("Whenever an opponent loses life, ..."),
    // Vito, Thorn of the Dusk Rose ("Whenever you gain life, each opponent loses..."),
    // Bloodchief Ascension-adjacent cards.
    fn parse_life_verb(input: &str) -> OracleResult<'_, TriggerMode> {
        alt((
            value(TriggerMode::LifeLost, tag("loses life")),
            value(TriggerMode::LifeLost, tag("lose life")),
            value(TriggerMode::LifeGained, tag("gains life")),
            value(TriggerMode::LifeGained, tag("gain life")),
        ))
        .parse(input)
    }
    if let Ok((tail, mode)) = parse_life_verb.parse(rest) {
        let mut def = make_base();
        def.mode = mode.clone();
        def.valid_target = Some(subject.clone());
        // CR 603.4 + CR 102.1: "gains life during their turn" / "loses life
        // during their turn" — the trailing timing tail restricts the trigger
        // to the acting player's own turn. Composed with the shared typed
        // `parse_timing_tail` combinator rather than a bespoke strip.
        attach_event_timing_tail(&mut def, tail);
        return Some((mode, def));
    }

    // CR 121.1 + CR 603.2: "Whenever [subject] draws a card" — generic draw trigger
    // (e.g. Rhystic Study, Sylvan Library patterns where subject is `a player`; Sheoldred's
    // first trigger where subject is `~`/you). Subject filter flows into `valid_target`
    // so `match_drawn` correctly scopes to the right player.
    fn parse_draws_card(input: &str) -> OracleResult<'_, ()> {
        alt((
            value((), tag("draws a card")),
            value((), tag("draw a card")),
        ))
        .parse(input)
    }
    if let Ok((tail, ())) = parse_draws_card.parse(rest) {
        let mut def = make_base();
        def.mode = TriggerMode::Drawn;
        def.valid_target = Some(subject.clone());
        // CR 603.4 + CR 102.1: "draws a card during their turn" — the trailing
        // timing tail restricts the trigger to the acting player's own turn.
        // Composed with the shared typed `parse_timing_tail` combinator.
        attach_event_timing_tail(&mut def, tail);
        // CR 121.1 + CR 504.1 + CR 603.4: Detect Orcish Bowmasters' "except the
        // first one [you|they] draw in each of [your|their] draw steps" clause
        // (shape-compatible with Alhammarret's Archive's replacement variant —
        // shared combinator in `oracle_replacement`). When present, gate the
        // trigger so it does NOT fire on the active player's first draw of
        // the draw step.
        if super::oracle_replacement::has_except_first_draw_in_draw_step_clause(rest)
            || super::oracle_replacement::has_except_first_draw_in_draw_step_clause(full_lower)
        {
            // CR 121.1: AND the draw-step exemption into any timing condition
            // `attach_event_timing_tail` already set (e.g. "during their turn")
            // rather than clobbering it. When the tail set no condition this is
            // identical to assigning `ExceptFirstDrawInDrawStep` directly.
            def.condition = Some(match def.condition.take() {
                Some(existing) => TriggerCondition::And {
                    conditions: vec![existing, TriggerCondition::ExceptFirstDrawInDrawStep],
                },
                None => TriggerCondition::ExceptFirstDrawInDrawStep,
            });
        }
        return Some((TriggerMode::Drawn, def));
    }

    None
}

fn try_parse_named_trigger_mode(lower: &str) -> Option<(TriggerMode, TriggerDefinition)> {
    let mut def = make_base();

    if matches!(lower, "whenever chaos ensues" | "when chaos ensues") {
        def.mode = TriggerMode::ChaosEnsues;
        return Some((TriggerMode::ChaosEnsues, def));
    }

    if matches!(
        lower,
        "when you set this scheme in motion" | "whenever you set this scheme in motion"
    ) {
        def.mode = TriggerMode::SetInMotion;
        return Some((TriggerMode::SetInMotion, def));
    }

    if matches!(
        lower,
        "whenever you crank this contraption"
            | "when you crank this contraption"
            | "whenever you crank this ~"
            | "when you crank this ~"
    ) {
        def.mode = TriggerMode::CrankContraption;
        return Some((TriggerMode::CrankContraption, def));
    }

    None
}

fn try_parse_special_trigger_pattern(lower: &str) -> Option<(TriggerMode, TriggerDefinition)> {
    if let Some(result) = try_parse_self_or_another_controlled_subtype_enters(lower) {
        return Some(result);
    }

    if let Some(result) = try_parse_another_controlled_subtype_enters(lower) {
        return Some(result);
    }

    // Non-"another" variant: "whenever a/an [subtype] you control enters".
    // Must follow the "another" variant so its stricter match wins first.
    if let Some(result) = try_parse_controlled_subtype_enters(lower) {
        return Some(result);
    }

    if let Some(result) = try_parse_controlled_subtype_attacks(lower) {
        return Some(result);
    }

    if let Some(result) = try_parse_one_or_more_combat_damage_to_player(lower) {
        return Some(result);
    }

    if let Some(result) = try_parse_n_or_more_attacks(lower) {
        return Some(result);
    }

    // CR 508.1 + CR 603.2c: "whenever [actor] attack[s] with N or more creatures" —
    // controller-scoped inverse phrasing of the subject-led "N or more creatures attack"
    // handled above. Covers Firemane Commando's dual triggers (you / another player).
    if let Some(result) = try_parse_attack_with_n_creatures(lower) {
        return Some(result);
    }

    if let Some(result) = try_parse_one_or_more_die(lower) {
        return Some(result);
    }

    if let Some(result) = try_parse_one_or_more_tokens_created(lower) {
        return Some(result);
    }

    if let Some(result) = try_parse_one_or_more_leave_graveyard(lower) {
        return Some(result);
    }

    if let Some(result) = try_parse_one_or_more_put_into_exile_from(lower) {
        return Some(result);
    }

    if let Some(result) = try_parse_one_or_more_put_into_graveyard(lower) {
        return Some(result);
    }

    if let Some(result) = try_parse_one_or_more_put_into_library(lower) {
        return Some(result);
    }

    // CR 120.2b: "a source you control deals noncombat damage to an opponent"
    for prefix in [
        "whenever a source you control deals noncombat damage to an opponent",
        "when a source you control deals noncombat damage to an opponent",
    ] {
        if lower == prefix {
            let mut def = make_base();
            def.mode = TriggerMode::DamageDone;
            def.damage_kind = DamageKindFilter::NoncombatOnly;
            def.valid_source = Some(TargetFilter::Typed(
                TypedFilter::default().controller(ControllerRef::You),
            ));
            def.valid_target = Some(TargetFilter::Typed(
                TypedFilter::default().controller(ControllerRef::Opponent),
            ));
            return Some((TriggerMode::DamageDone, def));
        }
    }

    if matches!(
        lower,
        "whenever you commit a crime" | "when you commit a crime"
    ) {
        let mut def = make_base();
        def.mode = TriggerMode::CommitCrime;
        return Some((TriggerMode::CommitCrime, def));
    }

    if matches!(
        lower,
        "whenever day becomes night or night becomes day"
            | "when day becomes night or night becomes day"
    ) {
        let mut def = make_base();
        def.mode = TriggerMode::DayTimeChanges;
        return Some((TriggerMode::DayTimeChanges, def));
    }

    fn parse_you_unlock_this_door(input: &str) -> OracleResult<'_, ()> {
        all_consuming(preceded(
            alt((tag("when "), tag("whenever "))),
            value((), tag("you unlock this door")),
        ))
        .parse(input)
    }
    if parse_you_unlock_this_door(lower).is_ok() {
        let mut def = make_base();
        def.mode = TriggerMode::UnlockDoor;
        return Some((TriggerMode::UnlockDoor, def));
    }

    fn parse_you_fully_unlock_a_room(input: &str) -> OracleResult<'_, ()> {
        all_consuming(preceded(
            alt((tag("when "), tag("whenever "))),
            value((), tag("you fully unlock a room")),
        ))
        .parse(input)
    }
    if parse_you_fully_unlock_a_room(lower).is_ok() {
        let mut def = make_base();
        def.mode = TriggerMode::FullyUnlock;
        def.valid_target = Some(TargetFilter::Controller);
        def.valid_card = Some(TargetFilter::Typed(
            TypedFilter::default().subtype("Room".to_string()),
        ));
        return Some((TriggerMode::FullyUnlock, def));
    }

    fn parse_this_card_becomes_plotted(input: &str) -> OracleResult<'_, ()> {
        all_consuming(preceded(
            alt((tag("when "), tag("whenever "))),
            value((), tag("this card becomes plotted")),
        ))
        .parse(input)
    }
    if parse_this_card_becomes_plotted(lower).is_ok() {
        let mut def = make_base();
        def.mode = TriggerMode::BecomesPlotted;
        def.trigger_zones = vec![Zone::Exile];
        return Some((TriggerMode::BecomesPlotted, def));
    }

    // CR 701.62 + CR 701.62b: "Whenever you manifest dread" — actor-side
    // Manifest Dread trigger. "You" constrains the acting player to the
    // trigger's controller via `TargetFilter::Controller`.
    fn parse_manifest_dread_prefix(input: &str) -> OracleResult<'_, ()> {
        let (rest, _) = alt((tag("whenever "), tag("when "))).parse(input)?;
        value((), tag("you manifest dread")).parse(rest)
    }
    if parse_manifest_dread_prefix(lower).is_ok() {
        let mut def = make_base();
        def.mode = TriggerMode::ManifestDread;
        def.valid_target = Some(TargetFilter::Controller);
        return Some((TriggerMode::ManifestDread, def));
    }

    // CR 708 + CR 701.40b + CR 701.58b: "Whenever you turn a permanent/creature
    // face up" — actor-side TurnFaceUp trigger. Subject after "turn " must be a
    // face-down-capable noun phrase; `valid_card` records the type filter,
    // `valid_target = Controller` gates on the turning player being the trigger
    // controller.
    fn parse_turn_face_up_prefix(input: &str) -> OracleResult<'_, TypeFilter> {
        let (rest, _) = alt((tag("whenever "), tag("when "))).parse(input)?;
        let (rest, _) = tag("you turn ").parse(rest)?;
        let (rest, _) = alt((tag("a "), tag("an "))).parse(rest)?;
        let (rest, ty) = alt((
            value(TypeFilter::Permanent, tag("permanent")),
            value(TypeFilter::Creature, tag("creature")),
        ))
        .parse(rest)?;
        let (rest, _) = tag(" face up").parse(rest)?;
        Ok((rest, ty))
    }
    if let Ok((_, ty)) = parse_turn_face_up_prefix(lower) {
        let mut def = make_base();
        def.mode = TriggerMode::TurnFaceUp;
        def.valid_card = Some(TargetFilter::Typed(TypedFilter::new(ty)));
        def.valid_target = Some(TargetFilter::Controller);
        return Some((TriggerMode::TurnFaceUp, def));
    }

    // CR 508.1a: "enchanted player is attacked" — the aura enchants a player,
    // and the trigger fires when any creature attacks that player.
    for prefix in [
        "whenever enchanted player is attacked",
        "when enchanted player is attacked",
    ] {
        if tag::<_, _, OracleError<'_>>(prefix).parse(lower).is_ok() {
            let mut def = make_base();
            def.mode = TriggerMode::Attacks;
            // AttachedTo here references the player the aura is attached to
            def.valid_target = Some(TargetFilter::AttachedTo);
            return Some((TriggerMode::Attacks, def));
        }
    }

    for prefix in ["whenever you cast or copy ", "when you cast or copy "] {
        if let Ok((rest, ())) = value((), tag::<_, _, OracleError<'_>>(prefix)).parse(lower) {
            if matches!(
                rest,
                "an instant or sorcery spell" | "a instant or sorcery spell"
            ) {
                let mut def = make_base();
                def.mode = TriggerMode::SpellCastOrCopy;
                def.valid_card = Some(TargetFilter::Or {
                    filters: vec![
                        TargetFilter::Typed(TypedFilter::new(TypeFilter::Instant)),
                        TargetFilter::Typed(TypedFilter::new(TypeFilter::Sorcery)),
                    ],
                });
                def.valid_target = Some(TargetFilter::Controller);
                return Some((TriggerMode::SpellCastOrCopy, def));
            }
        }
    }

    // CR 700.4 + CR 120.1: "a creature dealt damage by ~ this turn dies"
    // This is a death trigger gated on the dying creature having received damage from
    // the trigger source during the current turn. Maps to ChangesZone (dies) with
    // a DealtDamageBySourceThisTurn condition.
    for prefix in [
        "whenever a creature dealt damage by ~ this turn dies",
        "when a creature dealt damage by ~ this turn dies",
    ] {
        if tag::<_, _, OracleError<'_>>(prefix).parse(lower).is_ok() {
            let mut def = make_base();
            def.mode = TriggerMode::ChangesZone;
            def.origin = Some(Zone::Battlefield);
            def.destination = Some(Zone::Graveyard);
            def.valid_card = Some(TargetFilter::Typed(TypedFilter::creature()));
            def.condition = Some(TriggerCondition::DealtDamageBySourceThisTurn);
            return Some((TriggerMode::ChangesZone, def));
        }
    }

    None
}

/// CR 303.4 + CR 301.5: Detect a trailing "that are enchanted/equipped by an
/// <attachment-type> you control" relative clause in a subject phrase and
/// return the subject minus the clause plus the corresponding `FilterProp`.
/// Returns `(subject_without_clause, Some(prop))` when the clause is present,
/// else `(original_subject, None)`.
///
/// Covers:
/// - "creatures that are enchanted by an Aura you control" (Killian).
/// - Future "creatures that are equipped by an Equipment you control" patterns.
fn strip_attachment_relative_clause(subject: &str) -> (&str, Option<FilterProp>) {
    // Enumerated suffix alternatives — equivalent to `alt(tag(...))` over a lowercase
    // tail. Kept as `strip_suffix` for dual-string safety; patterns are static.
    // structural: not dispatch
    let alts: &[(&str, FilterProp)] = &[
        (
            " that are enchanted by an aura you control",
            FilterProp::HasAttachment {
                kind: AttachmentKind::Aura,
                controller: Some(ControllerRef::You),
            },
        ),
        (
            " that is enchanted by an aura you control",
            FilterProp::HasAttachment {
                kind: AttachmentKind::Aura,
                controller: Some(ControllerRef::You),
            },
        ),
        (
            " that are equipped by an equipment you control",
            FilterProp::HasAttachment {
                kind: AttachmentKind::Equipment,
                controller: Some(ControllerRef::You),
            },
        ),
        (
            " that is equipped by an equipment you control",
            FilterProp::HasAttachment {
                kind: AttachmentKind::Equipment,
                controller: Some(ControllerRef::You),
            },
        ),
    ];
    for (suffix, prop) in alts {
        if let Some(stripped) = subject.strip_suffix(suffix) {
            return (stripped, Some(prop.clone()));
        }
    }
    (subject, None)
}

/// Append `attachment_prop` to a `TargetFilter::Typed`'s properties if present,
/// else return the filter unchanged. Non-Typed filters are returned as-is.
fn apply_attachment_prop(filter: TargetFilter, prop: Option<FilterProp>) -> TargetFilter {
    match (filter, prop) {
        (TargetFilter::Typed(mut tf), Some(p)) => {
            tf.properties.push(p);
            TargetFilter::Typed(tf)
        }
        (other, _) => other,
    }
}

/// Parse "whenever N or more creatures [you control] attack [a player]" patterns.
/// CR 508.1a: Handles both "one or more" and "two or more" quantifiers.
fn try_parse_n_or_more_attacks(lower: &str) -> Option<(TriggerMode, TriggerDefinition)> {
    for (prefix, min_count) in [
        ("whenever one or more ", 1u32),
        ("when one or more ", 1),
        ("whenever two or more ", 2),
        ("when two or more ", 2),
    ] {
        let Ok((rest, ())) = value((), tag::<_, _, OracleError<'_>>(prefix)).parse(lower) else {
            continue;
        };
        // Strip optional " a player" target suffix before checking for "attack"
        let (subject_text, attacks_player) =
            if let Some(before) = rest.strip_suffix(" attack a player") {
                (before, true)
            } else if let Some(before) = rest.strip_suffix(" attack") {
                (before, false)
            } else if let Some(before) = rest.strip_suffix(" attacks") {
                (before, false)
            } else {
                continue;
            };

        // CR 303.4: Strip optional "that are enchanted/equipped by an <X> you control"
        // relative clause and capture it as a non-source-relative attachment filter.
        let (subject_core, attachment_prop) = strip_attachment_relative_clause(subject_text);

        let (filter, remainder) = parse_type_phrase(subject_core);
        if !remainder.trim().is_empty() {
            continue;
        }

        let filter = apply_attachment_prop(filter, attachment_prop);

        let mut def = make_base();
        def.mode = TriggerMode::YouAttack;
        def.valid_card = Some(filter);
        if attacks_player {
            def.valid_target = Some(TargetFilter::Player);
        }
        if min_count > 1 {
            def.condition = Some(TriggerCondition::MinCoAttackers {
                minimum: min_count - 1,
            });
        }
        // CR 603.2c: "One or more creatures ... attack" fires once per batch of
        // simultaneous attackers (not once per attacker). Killian's trigger relies
        // on this to yield exactly one draw when multiple enchanted creatures
        // attack together.
        def.batched = true;
        return Some((TriggerMode::YouAttack, def));
    }

    None
}

/// CR 508.1 + CR 603.2c: Parse "whenever [actor] attack[s] with N or more creatures".
///
/// Covers three actor scopes via nom prefix dispatch, mirroring the Tier 1.3
/// sacrifice-trigger idiom (`Option<ControllerRef>`):
///   - `you attack with ...`          → `ControllerRef::You`
///   - `another player attacks with`  → `ControllerRef::Opponent`
///   - `an opponent attacks with ...` → `ControllerRef::Opponent`
///   - `a player attacks with ...`    → `None` (any player)
///
/// Produces a `TriggerMode::YouAttack` (batched) with:
///   - `valid_target = TypedFilter::default().controller(scope)` when scope is
///     known — this drives `match_you_attack`'s attacking-player filter AND
///     feeds `resolve_they_pronoun` so a trailing "they draw a card" resolves
///     to `TargetFilter::TriggeringPlayer`.
///   - `condition = AttackersDeclaredMin { scope, minimum }` so only batches
///     with at least N attackers from the scoped player fire the trigger.
fn try_parse_attack_with_n_creatures(lower: &str) -> Option<(TriggerMode, TriggerDefinition)> {
    use nom::combinator::opt;

    let (after_prefix, ()) = alt((
        value((), tag::<_, _, OracleError<'_>>("whenever ")),
        value((), tag("when ")),
    ))
    .parse(lower)
    .ok()?;

    // Actor dispatch. Only scoped actors are handled here. "A player attacks
    // with N or more creatures" (any-player scope, e.g. Aurelia the Law Above)
    // would need a distinct any-player variant to be correct — until that
    // exists, leave those triggers Unknown rather than misclassify.
    let (after_actor, actor): (&str, ControllerRef) = alt((
        value(
            ControllerRef::You,
            tag::<_, _, OracleError<'_>>("you attack"),
        ),
        value(ControllerRef::Opponent, tag("another player attacks")),
        value(ControllerRef::Opponent, tag("an opponent attacks")),
    ))
    .parse(after_prefix)
    .ok()?;

    // Required " with " separator.
    let (after_with, ()) = value((), tag::<_, _, OracleError<'_>>(" with "))
        .parse(after_actor)
        .ok()?;

    // Parse the count word/digit. `parse_number` already maps "one"→1 as well as
    // digits and other number-words; do NOT add a duplicate `value(1, tag("one"))`.
    let (after_n, n) = nom_primitives::parse_number.parse(after_with).ok()?;
    let (after_or_more, ()) = value((), tag::<_, _, OracleError<'_>>(" or more "))
        .parse(after_n)
        .ok()?;

    if n < 1 {
        return None;
    }

    if n == 1 {
        // CR 508.1 + CR 603.2c: actor-led "you attack with one or more <TYPE>".
        // The count==1 form needs no count condition — the matcher's
        // "≥ 1 attacker matching valid_card" semantics IS "one or more". Capture
        // the head-noun type phrase via the shared `parse_type_phrase` building
        // block (handles negated subtypes like "non-Gnome" and controller scope
        // like "Gods you control") and store it on `valid_card`.
        let (filter, remainder) = parse_type_phrase(after_or_more);
        // Accept optional trailing " each turn" / " this turn" qualifier; the
        // remainder after the type phrase must otherwise be empty.
        let (rest, _) = opt(alt((
            tag::<_, _, OracleError<'_>>(" each turn"),
            tag(" this turn"),
        )))
        .parse(remainder)
        .ok()?;
        if !rest.trim().is_empty() {
            return None;
        }

        let mut def = make_base();
        def.mode = TriggerMode::YouAttack;
        def.batched = true;
        // `valid_target` drives both the matcher's attacking-player check and the
        // "they" pronoun resolver in the effect body.
        def.valid_target = Some(TargetFilter::Typed(
            TypedFilter::default().controller(actor),
        ));
        // CR 508.1: the attacker-type gate the matcher now reads.
        def.valid_card = Some(filter);
        return Some((TriggerMode::YouAttack, def));
    }

    // count > 1 ("two or more creatures"): byte-identical to the pre-existing
    // behavior — hardcoded "creatures" head noun, count condition via
    // `AttackersDeclaredMin`, no `valid_card`. Typed count>1 ("two or more Gods")
    // is deferred — populating `valid_card` for count>1 without a condition-level
    // type axis would over-fire (≥1 God + ≥2 any-attackers).
    let (after_creatures, ()) = value((), tag::<_, _, OracleError<'_>>("creatures"))
        .parse(after_or_more)
        .ok()?;
    // Accept optional trailing " each turn" / " this turn" qualifier (unused here,
    // but keeps the matcher permissive for CR 603.4 timing qualifiers). Must end
    // at the condition boundary — the caller already split the effect text off,
    // so `after_creatures` should be empty or punctuation-only.
    let (rest, _) = opt(alt((
        tag::<_, _, OracleError<'_>>(" each turn"),
        tag(" this turn"),
    )))
    .parse(after_creatures)
    .ok()?;
    if !rest.trim().is_empty() {
        return None;
    }

    let mut def = make_base();
    def.mode = TriggerMode::YouAttack;
    def.batched = true;

    // `valid_target` drives both the matcher's attacking-player check and the
    // "they" pronoun resolver in the effect body.
    def.valid_target = Some(TargetFilter::Typed(
        TypedFilter::default().controller(actor.clone()),
    ));
    def.condition = Some(TriggerCondition::AttackersDeclaredMin {
        scope: actor,
        minimum: n,
    });

    Some((TriggerMode::YouAttack, def))
}

/// Parse "whenever one or more [subject] die" patterns.
/// CR 603.2c: "One or more" triggers fire once per batch of simultaneous events.
fn try_parse_one_or_more_die(lower: &str) -> Option<(TriggerMode, TriggerDefinition)> {
    for prefix in ["whenever one or more ", "when one or more "] {
        let Ok((rest, ())) = value((), tag::<_, _, OracleError<'_>>(prefix)).parse(lower) else {
            continue;
        };
        let Some(subject_text) = rest
            .strip_suffix(" die")
            .or_else(|| rest.strip_suffix(" dies"))
        else {
            continue;
        };

        let (filter, remainder) = parse_type_phrase(subject_text);
        if !remainder.trim().is_empty() {
            continue;
        }

        let mut def = make_base();
        def.mode = TriggerMode::ChangesZoneAll;
        def.origin = Some(Zone::Battlefield);
        def.destination = Some(Zone::Graveyard);
        def.valid_card = Some(filter);
        def.batched = true;
        return Some((TriggerMode::ChangesZoneAll, def));
    }

    None
}

/// Parse "whenever you create one or more [type-phrase] tokens" patterns.
/// CR 111.1 + CR 603.2c: Token creation is its own event (tokens come into
/// existence directly on the battlefield); "one or more" triggers fire once
/// per batch of simultaneous token-creation events.
///
/// Supported shapes:
/// - "whenever you create one or more creature tokens"
/// - "whenever you create one or more tokens"
/// - "whenever you create one or more artifact tokens"
///
/// The type-phrase (e.g., "creature") is parsed into a `TargetFilter` stored
/// on `valid_card`; controller ("you") is stored on `valid_target` via the
/// shared Controller scope pattern. The matcher evaluates both against the
/// `TokenCreated` event's `object_id`.
fn try_parse_one_or_more_tokens_created(lower: &str) -> Option<(TriggerMode, TriggerDefinition)> {
    let (_, rest) = alt((
        value(
            (),
            tag::<_, _, OracleError<'_>>("whenever you create one or more "),
        ),
        value(
            (),
            tag::<_, _, OracleError<'_>>("when you create one or more "),
        ),
    ))
    .parse(lower)
    .map(|(r, _)| ((), r))
    .ok()?;

    // Accept bare "tokens"/"token" (no type phrase) as well as "[type] tokens".
    let subject_text = if rest == "tokens" || rest == "token" {
        ""
    } else {
        rest.strip_suffix(" tokens")
            .or_else(|| rest.strip_suffix(" token"))?
    };

    // Bare "tokens" (no type phrase) → match any token.
    let valid_card = if subject_text.trim().is_empty() {
        None
    } else {
        let (filter, remainder) = parse_type_phrase(subject_text);
        if !remainder.trim().is_empty() {
            return None;
        }
        Some(filter)
    };

    let mut def = make_base();
    def.mode = TriggerMode::TokenCreated;
    def.valid_card = valid_card;
    def.valid_target = Some(TargetFilter::Controller);
    def.batched = true;
    Some((TriggerMode::TokenCreated, def))
}

/// Parse "whenever one or more [subject] cards leave your graveyard" patterns.
/// CR 603.2c: "One or more" triggers fire once per batch of simultaneous events.
fn try_parse_one_or_more_leave_graveyard(lower: &str) -> Option<(TriggerMode, TriggerDefinition)> {
    for prefix in ["whenever one or more ", "when one or more "] {
        let Ok((rest, ())) = value((), tag::<_, _, OracleError<'_>>(prefix)).parse(lower) else {
            continue;
        };

        // Strip trailing constraint clauses ("during your turn") before matching
        let (base, during_your_turn) =
            if let Some(stripped) = rest.strip_suffix(" during your turn") {
                (stripped, true)
            } else {
                (rest, false)
            };

        let Some(subject_text) = base
            .strip_suffix(" leave your graveyard")
            .or_else(|| base.strip_suffix(" leaves your graveyard"))
        else {
            continue;
        };

        // Parse subject type filter: "creature cards", "artifact and/or creature cards", "cards"
        let filter = if subject_text == "cards" {
            None
        } else if let Some(type_text) = subject_text.strip_suffix(" cards") {
            // Handle "artifact and/or creature" → OR filter
            if scan_contains(type_text, "and/or") {
                let parts: Vec<&str> = type_text.split(" and/or ").collect();
                let filters: Vec<TargetFilter> = parts
                    .iter()
                    .filter_map(|part| {
                        let (f, rem) = parse_type_phrase(part.trim());
                        if rem.trim().is_empty() {
                            Some(f)
                        } else {
                            None
                        }
                    })
                    .collect();
                if filters.len() == parts.len() && filters.len() > 1 {
                    Some(TargetFilter::Or { filters })
                } else {
                    continue;
                }
            } else {
                let (filter, remainder) = parse_type_phrase(type_text);
                if !remainder.trim().is_empty() {
                    continue;
                }
                Some(filter)
            }
        } else {
            continue;
        };

        let mut def = make_base();
        def.mode = TriggerMode::ChangesZoneAll;
        def.origin = Some(Zone::Graveyard);
        def.valid_card = filter;
        def.batched = true;
        // LTB-from-graveyard triggers need to fire from graveyard zone context
        def.trigger_zones = vec![Zone::Battlefield, Zone::Graveyard, Zone::Exile];
        if during_your_turn {
            def.constraint = Some(TriggerConstraint::OnlyDuringYourTurn);
        }
        return Some((TriggerMode::ChangesZoneAll, def));
    }

    None
}

/// Parse a single zone token: "your library" → Zone::Library, "your graveyard" → Zone::Graveyard.
/// Returns the typed zone and the remaining input. Used by the disjunctive
/// source-zone combinator below.
fn parse_your_zone_token(input: &str) -> nom::IResult<&str, Zone, OracleError<'_>> {
    alt((
        value(Zone::Library, tag("your library")),
        value(Zone::Graveyard, tag("your graveyard")),
    ))
    .parse(input)
}

/// Parse a zone-set phrase such as "your library", "your graveyard",
/// or "your library and/or your graveyard" / "your graveyard and/or your library".
/// Returns the list of source zones in reading order.
///
/// Composable: one `parse_your_zone_token` invocation per alternative, joined
/// by an optional "and/or" / "or" / "and" disjunction combinator.
fn parse_disjunctive_zone_set(input: &str) -> nom::IResult<&str, Vec<Zone>, OracleError<'_>> {
    let (input, first) = parse_your_zone_token(input)?;
    // Optional second zone joined by "and/or" (canonical), "or", or "and".
    let rest_parser = |i| -> nom::IResult<&str, Zone, OracleError<'_>> {
        let (i, _) = alt((tag(" and/or "), tag(" or "), tag(" and "))).parse(i)?;
        parse_your_zone_token(i)
    };
    match rest_parser(input) {
        Ok((rest, second)) => Ok((rest, vec![first, second])),
        Err(_) => Ok((input, vec![first])),
    }
}

/// Parse "whenever one or more cards are put into exile from <zone-set>" — a batched
/// zone-change trigger with disjunctive source zones and fixed destination = Exile.
/// CR 603.2c + CR 603.10a: "One or more" triggers fire once per batch of
/// simultaneous zone-change events.
fn try_parse_one_or_more_put_into_exile_from(
    lower: &str,
) -> Option<(TriggerMode, TriggerDefinition)> {
    for prefix in [
        "whenever one or more cards are put into exile from ",
        "when one or more cards are put into exile from ",
    ] {
        let Ok((rest, ())) = value((), tag::<_, _, OracleError<'_>>(prefix)).parse(lower) else {
            continue;
        };
        let Ok((after_zones, zones)) = parse_disjunctive_zone_set(rest) else {
            continue;
        };
        // Trailing text (after the optional zone-set) may be empty or a
        // constraint clause we don't handle here. Any non-empty trailing text
        // means this isn't a clean match — bail so another parser can try.
        if !after_zones.is_empty() {
            continue;
        }

        let mut def = make_base();
        def.mode = TriggerMode::ChangesZoneAll;
        def.origin_zones = zones;
        def.destination = Some(Zone::Exile);
        def.batched = true;
        // Source can fire from any public zone context since cards move from
        // library/graveyard — trigger source (e.g. Laelia) is on the battlefield,
        // but keeping these zones mirrors the leave-graveyard precedent.
        def.trigger_zones = vec![Zone::Battlefield, Zone::Graveyard, Zone::Exile];
        return Some((TriggerMode::ChangesZoneAll, def));
    }

    None
}

fn try_parse_one_or_more_combat_damage_to_player(
    lower: &str,
) -> Option<(TriggerMode, TriggerDefinition)> {
    for prefix in ["whenever one or more ", "when one or more "] {
        let Ok((rest, ())) = value((), tag::<_, _, OracleError<'_>>(prefix)).parse(lower) else {
            continue;
        };
        let Some(subject_text) = rest
            .strip_suffix(" deal combat damage to a player")
            .or_else(|| rest.strip_suffix(" deals combat damage to a player"))
        else {
            continue;
        };

        let (filter, remainder) = parse_type_phrase(subject_text);
        let filter = if remainder.trim().is_empty() {
            filter
        } else if let Some(or_filter) = try_split_or_compound_type_phrase(subject_text) {
            // CR 205.3m: Handle "ninja or rogue creatures you control" compound subtypes
            or_filter
        } else {
            continue;
        };

        let mut def = make_base();
        def.mode = TriggerMode::DamageDoneOnceByController;
        def.damage_kind = DamageKindFilter::CombatOnly;
        def.valid_source = Some(filter);
        def.valid_target = Some(TargetFilter::Player);
        return Some((TriggerMode::DamageDoneOnceByController, def));
    }

    None
}

/// CR 205.3m: Try to split "subtype or subtype [card_type] [you control]" into an Or filter.
/// Handles patterns like "ninja or rogue creatures you control" where parse_type_phrase
/// can't natively handle the "or" compound with a shared card_type suffix.
/// Parses the full right-side phrase ("rogue creatures you control") as a complete type phrase,
/// then applies the shared card_type and controller to the left-side bare subtype.
fn try_split_or_compound_type_phrase(text: &str) -> Option<TargetFilter> {
    let (_, (left, right)) = nom_primitives::split_once_on(text, " or ").ok()?;
    let left_trimmed = left.trim();
    // Parse the full right side as a type phrase — "rogue creatures you control" is a complete phrase
    // that parse_type_phrase handles as subtype-only + trailing text. Instead, parse the whole
    // "subtype card_type controller" suffix manually by feeding "right" to parse_type_phrase
    // but appending it to make a single-subtype phrase.
    // The simplest correct approach: parse the entire text AFTER stripping the "subtype or " prefix
    // from the left, treating the rest as a single type phrase that gives us card_type + controller.
    let right_trimmed = right.trim();
    // Try parsing the entire right side as a type phrase
    let (right_filter, right_remainder) = parse_type_phrase(right_trimmed);
    // If parse_type_phrase didn't fully consume, the right side has "subtype card_type you control"
    // pattern. Reconstruct: the right_filter has subtype, and remainder has "card_type you control".
    let (primary_type, controller) = if right_remainder.trim().is_empty() {
        // Fully consumed
        if let TargetFilter::Typed(ref tf) = right_filter {
            (tf.get_primary_type().cloned(), tf.controller.clone())
        } else {
            return None;
        }
    } else if let TargetFilter::Typed(ref tf) = right_filter {
        // Partially consumed: right_filter has subtype, remainder has "creatures you control"
        let (suffix_filter, suffix_rem) = parse_type_phrase(right_remainder.trim());
        if !suffix_rem.trim().is_empty() {
            return None;
        }
        if let TargetFilter::Typed(ref stf) = suffix_filter {
            (
                stf.get_primary_type()
                    .cloned()
                    .or(tf.get_primary_type().cloned()),
                stf.controller.clone().or(tf.controller.clone()),
            )
        } else {
            return None;
        }
    } else {
        return None;
    };
    // Extract right-side subtype
    let right_subtype = if let TargetFilter::Typed(ref tf) = right_filter {
        tf.get_subtype().map(|s| s.to_string())
    } else {
        return None;
    };
    // CR 205.3m: Canonicalize the left subtype (e.g. "ninjas" → "Ninja", "elves" → "Elf")
    let left_subtype = parse_subtype(left_trimmed)
        .map(|(canonical, _)| canonical)
        .unwrap_or_else(|| canonicalize_subtype_name(left_trimmed));
    let mut left_tf = TypedFilter::default().subtype(left_subtype);
    let mut right_tf = TypedFilter::default();
    if let Some(ref pt) = primary_type {
        left_tf = left_tf.with_type(pt.clone());
        right_tf = right_tf.with_type(pt.clone());
    }
    if let Some(rs) = right_subtype {
        right_tf = right_tf.subtype(rs);
    }
    left_tf.controller = controller.clone();
    right_tf.controller = controller;
    let filters = vec![TargetFilter::Typed(left_tf), TargetFilter::Typed(right_tf)];
    Some(TargetFilter::Or { filters })
}

fn try_parse_self_or_another_controlled_subtype_enters(
    lower: &str,
) -> Option<(TriggerMode, TriggerDefinition)> {
    for prefix in ["whenever ~ or another ", "when ~ or another "] {
        let Ok((rest, ())) = value((), tag::<_, _, OracleError<'_>>(prefix)).parse(lower) else {
            continue;
        };
        let Some(subject_text) = rest
            .strip_suffix(" enters")
            .or_else(|| rest.strip_suffix(" enters the battlefield"))
        else {
            continue;
        };
        let Some(subtype_text) = subject_text.trim().strip_suffix(" you control") else {
            continue;
        };
        let (_, remainder) = parse_type_phrase(subtype_text);
        if remainder.len() < subtype_text.len() {
            continue;
        }
        if !is_subtype_phrase(subtype_text) {
            continue;
        }

        let Some(subtype_filters) =
            build_controlled_subtype_filters(subtype_text, true, ControllerRef::You)
        else {
            continue;
        };
        if subtype_filters.is_empty() {
            continue;
        }

        let mut filters = vec![TargetFilter::SelfRef];
        filters.extend(subtype_filters);

        let mut def = make_base();
        def.mode = TriggerMode::ChangesZone;
        def.destination = Some(Zone::Battlefield);
        def.valid_card = Some(TargetFilter::Or { filters });
        return Some((TriggerMode::ChangesZone, def));
    }

    None
}

/// Parse "whenever a/an [subtype] you control enters [the battlefield]" (no
/// "another" prefix). Covers Bat Colony's "Whenever a Cave you control enters"
/// pattern and similar — the source itself is permitted to match if its subtype
/// is the same, unlike the "another" variant which excludes self.
///
/// Composed from nom combinators: prefix `alt`, subtype extraction via
/// `take_until`, `you control enters` sentinel, and optional ` the battlefield`
/// trailing token. Fails fast on unknown trailing input rather than silently
/// truncating.
fn try_parse_controlled_subtype_enters(lower: &str) -> Option<(TriggerMode, TriggerDefinition)> {
    use nom::bytes::complete::take_until;

    let (after_prefix, ()) = alt((
        value((), tag::<_, _, OracleError<'_>>("whenever a ")),
        value((), tag("whenever an ")),
        value((), tag("when a ")),
        value((), tag("when an ")),
    ))
    .parse(lower)
    .ok()?;

    let (after_subtype, subtype_text) = take_until::<_, _, OracleError<'_>>(" you control enters")
        .parse(after_prefix)
        .ok()?;

    let (after_sentinel, ()) = value((), tag::<_, _, OracleError<'_>>(" you control enters"))
        .parse(after_subtype)
        .ok()?;

    // Accept either bare "enters" or "enters the battlefield".
    let (tail, ()) = alt((
        value((), tag::<_, _, OracleError<'_>>(" the battlefield")),
        value((), tag("")),
    ))
    .parse(after_sentinel)
    .ok()?;

    if !tail.is_empty() {
        return None;
    }

    let (_, remainder) = parse_type_phrase(subtype_text);
    if remainder.len() < subtype_text.len() {
        return None;
    }
    if !is_subtype_phrase(subtype_text) {
        return None;
    }

    let valid_card = build_controlled_subtype_filter(subtype_text, false, ControllerRef::You)?;

    let mut def = make_base();
    def.mode = TriggerMode::ChangesZone;
    def.destination = Some(Zone::Battlefield);
    def.valid_card = Some(valid_card);
    Some((TriggerMode::ChangesZone, def))
}

fn try_parse_another_controlled_subtype_enters(
    lower: &str,
) -> Option<(TriggerMode, TriggerDefinition)> {
    for prefix in ["whenever another ", "when another "] {
        let Ok((rest, ())) = value((), tag::<_, _, OracleError<'_>>(prefix)).parse(lower) else {
            continue;
        };
        let Some(subject_text) = rest
            .strip_suffix(" enters")
            .or_else(|| rest.strip_suffix(" enters the battlefield"))
        else {
            continue;
        };
        let Some(subtype_text) = subject_text.trim().strip_suffix(" you control") else {
            continue;
        };
        let (_, remainder) = parse_type_phrase(subtype_text);
        if remainder.len() < subtype_text.len() {
            continue;
        }
        if !is_subtype_phrase(subtype_text) {
            continue;
        }

        let valid_card = build_controlled_subtype_filter(subtype_text, true, ControllerRef::You)?;

        let mut def = make_base();
        def.mode = TriggerMode::ChangesZone;
        def.destination = Some(Zone::Battlefield);
        def.valid_card = Some(valid_card);
        return Some((TriggerMode::ChangesZone, def));
    }

    None
}

fn try_parse_controlled_subtype_attacks(lower: &str) -> Option<(TriggerMode, TriggerDefinition)> {
    for prefix in ["whenever a ", "whenever an ", "when a ", "when an "] {
        let Ok((rest, ())) = value((), tag::<_, _, OracleError<'_>>(prefix)).parse(lower) else {
            continue;
        };
        let Some(subject_text) = rest.strip_suffix(" attacks") else {
            continue;
        };
        let Some(subtype_text) = subject_text.trim().strip_suffix(" you control") else {
            continue;
        };
        let (_, remainder) = parse_type_phrase(subtype_text);
        if remainder.len() < subtype_text.len() {
            continue;
        }
        if !is_subtype_phrase(subtype_text) {
            continue;
        }

        let valid_card = build_controlled_subtype_filter(subtype_text, false, ControllerRef::You)?;

        let mut def = make_base();
        def.mode = TriggerMode::Attacks;
        def.valid_card = Some(valid_card);
        return Some((TriggerMode::Attacks, def));
    }

    None
}

fn is_subtype_phrase(text: &str) -> bool {
    text.split(" or ").all(|part| {
        let trimmed = part.trim();
        !trimmed.is_empty() && !is_core_type_name(trimmed) && !is_non_subtype_subject_name(trimmed)
    })
}

fn build_controlled_subtype_filter(
    subtype_text: &str,
    another: bool,
    controller: ControllerRef,
) -> Option<TargetFilter> {
    let filters = build_controlled_subtype_filters(subtype_text, another, controller)?;
    Some(match filters.as_slice() {
        [single] => single.clone(),
        _ => TargetFilter::Or { filters },
    })
}

fn build_controlled_subtype_filters(
    subtype_text: &str,
    another: bool,
    controller: ControllerRef,
) -> Option<Vec<TargetFilter>> {
    let mut filters = Vec::new();

    for subtype in subtype_text
        .split(" or ")
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        if is_core_type_name(subtype) || is_non_subtype_subject_name(subtype) {
            return None;
        }

        let mut typed = TypedFilter::default()
            .subtype(canonicalize_subtype_name(subtype))
            .controller(controller.clone());
        if another {
            typed = typed.properties(vec![FilterProp::Another]);
        }
        filters.push(TargetFilter::Typed(typed));
    }

    if filters.is_empty() {
        None
    } else {
        Some(filters)
    }
}

// ---------------------------------------------------------------------------
// Category parsers
// ---------------------------------------------------------------------------

/// Parse phase triggers: "At the beginning of your upkeep/end step/combat/draw step"
fn try_parse_phase_trigger(lower: &str) -> Option<(TriggerMode, TriggerDefinition)> {
    // CR 511.2: "at end of combat" triggers as the end of combat step begins.
    if let Ok((rest, ())) = alt((
        value((), tag::<_, _, OracleError<'_>>("at end of combat")),
        value((), tag("at the end of combat")),
    ))
    .parse(lower)
    {
        let mut def = make_base();
        def.mode = TriggerMode::Phase;
        def.phase = Some(Phase::EndCombat);
        // CR 511.2: "on your turn" restricts to active player's combat.
        let rest = rest.trim();
        if alt((
            value((), tag::<_, _, OracleError<'_>>("on your turn")),
            value((), tag("on each of your turns")),
        ))
        .parse(rest)
        .is_ok()
        {
            def.constraint = Some(TriggerConstraint::OnlyDuringYourTurn);
        }
        return Some((TriggerMode::Phase, def));
    }

    let (stripped, ()) = value((), tag::<_, _, OracleError<'_>>("at the beginning of"))
        .parse(lower)
        .ok()?;
    let phase_text = stripped.trim();
    let mut def = make_base();
    def.mode = TriggerMode::Phase;
    def.phase = scan_for_phase(phase_text);

    // CR 503.1a / CR 507.1: Parse possessive qualifier and trailing suffix for turn constraint.
    // Uses nom prefix dispatch: opponent possessives checked before bare "your" to avoid
    // "your opponent's" matching as "your".
    def.constraint = parse_turn_constraint(phase_text);
    if scan_contains(phase_text, "enchanted player's") {
        def.valid_target = Some(TargetFilter::AttachedTo);
    }
    if scan_contains(phase_text, "first upkeep") && scan_contains(phase_text, "each turn") {
        def.constraint = Some(TriggerConstraint::MaxTimesPerTurn { max: 1 });
    }
    // "each player's upkeep" / "each upkeep" / "the end step" → no constraint (fires every turn)

    Some((TriggerMode::Phase, def))
}

/// Parse player-centric triggers: "you gain life", "you cast a/an ...", "you draw a card"
fn try_parse_player_trigger(lower: &str) -> Option<(TriggerMode, TriggerDefinition)> {
    if let Some(result) = try_parse_player_action_trigger(lower) {
        return Some(result);
    }

    // CR 702.49a: "whenever you activate a ninjutsu ability" — ninjutsu-family activation trigger.
    // Covers all ninjutsu variants (ninjutsu, commander ninjutsu, sneak).
    if let Some(result) = try_parse_keyword_activation_trigger(lower) {
        return Some(result);
    }

    // CR 119.3 + CR 603.2: "Whenever you gain life" scopes the trigger event to the
    // source's controller. Without `valid_target = Controller`, `valid_player_matches`
    // accepts any player, so opponent life-gain incorrectly triggers (e.g. Vito,
    // Thorn of the Dusk Rose; Ajani's Pridemate; Heliod, Sun-Crowned).
    if scan_contains(lower, "you gain life") {
        let mut def = make_base();
        def.mode = TriggerMode::LifeGained;
        def.valid_target = Some(TargetFilter::Controller);
        return Some((TriggerMode::LifeGained, def));
    }

    // "whenever you cast your Nth spell each turn" — must precede generic "you cast a"
    if let Some(result) = try_parse_nth_spell_trigger(lower) {
        return Some(result);
    }

    // "whenever you draw your Nth card each turn" — must precede generic "you draw a card"
    if let Some(result) = try_parse_nth_draw_trigger(lower) {
        return Some(result);
    }

    // CR 700.14: "whenever you expend N" — cumulative mana spent on spells this turn
    // CR 700.14: Delegate number parsing to nom combinator (input already lowercase)
    for prefix in ["whenever you expend ", "when you expend "] {
        if let Ok((rest, ())) = value((), tag::<_, _, OracleError<'_>>(prefix)).parse(lower) {
            if let Ok((_rem, n)) = nom_primitives::parse_number.parse(rest) {
                let mut def = make_base();
                def.mode = TriggerMode::ManaExpend;
                def.expend_threshold = Some(n);
                return Some((TriggerMode::ManaExpend, def));
            }
        }
    }

    // CR 603.8: "when you control no [type]" — state trigger that fires when the
    // controller controls no permanents matching a type/subtype filter.
    // Handles: "when you control no islands", "when you control no other creatures",
    // "when you control no artifacts", "when you control no forests", etc.
    for prefix in ["whenever you control no ", "when you control no "] {
        if let Ok((rest, ())) = value((), tag::<_, _, OracleError<'_>>(prefix)).parse(lower) {
            if let Some(filter) = parse_control_none_filter(rest) {
                let mut def = make_base();
                def.mode = TriggerMode::StateCondition;
                def.condition = Some(TriggerCondition::ControlsNone { filter });
                def.valid_card = Some(TargetFilter::SelfRef);
                return Some((TriggerMode::StateCondition, def));
            }
        }
    }

    // Discard triggers: prefix-based matching for broader card coverage.
    // Handles "you discard", "an opponent discards", "a player discards",
    // "each player discards" with optional type filters.
    if let Some(discard_result) = try_parse_discard_trigger(lower, &make_base) {
        return Some(discard_result);
    }

    // CR 603 + CR 701.21: Player-actor sacrifice triggers. Handles "you sacrifice",
    // "an opponent sacrifices", "a player sacrifices", "each player sacrifices"
    // with any subject filter (permanent, creature, another permanent, ...).
    if let Some(sac_result) = try_parse_sacrifice_trigger(lower, &make_base) {
        return Some(sac_result);
    }

    if matches!(
        lower,
        "whenever a player cycles a card" | "when a player cycles a card"
    ) {
        let mut def = make_base();
        def.mode = TriggerMode::Cycled;
        return Some((TriggerMode::Cycled, def));
    }

    if matches!(lower, "whenever you cycle a card" | "when you cycle a card") {
        let mut def = make_base();
        def.mode = TriggerMode::Cycled;
        def.valid_target = Some(TargetFilter::Controller);
        return Some((TriggerMode::Cycled, def));
    }

    // CR 305.1 + CR 603.2 + CR 701.18a: "whenever a player plays a land
    // [from <zone>]" fires on the CR 305 special action. The optional
    // from-zone tail rides through `parse_type_phrase`, matching the existing
    // cast-spell trigger shape used by Rocco, Street Chef.
    if let Ok((_, (who, _))) = nom_primitives::split_once_on(lower, " plays a land") {
        let mut def = make_base();
        def.mode = TriggerMode::LandPlayed;

        if scan_contains(who, "opponent") {
            def.valid_target = Some(TargetFilter::Typed(
                TypedFilter::default().controller(ControllerRef::Opponent),
            ));
        }

        let after_plays = &lower[who.len() + " plays a land".len()..].trim_start();
        let clause = nom_primitives::split_once_on(after_plays, ", ")
            .map(|(_, (before, _))| before)
            .unwrap_or(after_plays);
        let (filter, _) = parse_type_phrase(clause);
        if !matches!(filter, TargetFilter::Any) {
            def.valid_card = Some(filter);
        }

        return Some((TriggerMode::LandPlayed, def));
    }

    // CR 702.29: "whenever you cycle another card" — cycle trigger excluding source
    if matches!(
        lower,
        "whenever you cycle another card" | "when you cycle another card"
    ) {
        let mut def = make_base();
        def.mode = TriggerMode::Cycled;
        def.valid_target = Some(TargetFilter::Controller);
        def.valid_card = Some(TargetFilter::Typed(
            TypedFilter::default().properties(vec![FilterProp::Another]),
        ));
        return Some((TriggerMode::Cycled, def));
    }

    // CR 702.29d: "whenever you cycle or discard a card" — fires on either event, once per cycling
    if matches!(
        lower,
        "whenever you cycle or discard a card" | "when you cycle or discard a card"
    ) {
        let mut def = make_base();
        def.mode = TriggerMode::CycledOrDiscarded;
        def.valid_target = Some(TargetFilter::Controller);
        return Some((TriggerMode::CycledOrDiscarded, def));
    }

    // CR 702.29d: "whenever you cycle or discard another card"
    if matches!(
        lower,
        "whenever you cycle or discard another card" | "when you cycle or discard another card"
    ) {
        let mut def = make_base();
        def.mode = TriggerMode::CycledOrDiscarded;
        def.valid_target = Some(TargetFilter::Controller);
        def.valid_card = Some(TargetFilter::Typed(
            TypedFilter::default().properties(vec![FilterProp::Another]),
        ));
        return Some((TriggerMode::CycledOrDiscarded, def));
    }
    // CR 701.43a: "Whenever you exert a creature" — actor-side exert trigger.
    // The player actor belongs in valid_target; the exerted permanent belongs
    // in valid_card.
    fn parse_exert_trigger_line(i: &str) -> OracleResult<'_, (Option<ControllerRef>, &str)> {
        preceded(
            alt((tag("whenever "), tag("when "))),
            pair(
                alt((
                    value(Some(ControllerRef::You), tag("you exert ")),
                    value(Some(ControllerRef::Opponent), tag("an opponent exerts ")),
                    value(None, tag("a player exerts ")),
                )),
                rest,
            ),
        )
        .parse(i)
    }
    if let Ok((rem, (actor, subject_text))) = parse_exert_trigger_line(lower) {
        if !rem.trim().is_empty() {
            return None;
        }

        let (filter, remainder) = super::oracle_target::parse_target(subject_text);
        if !remainder.trim().is_empty() || matches!(filter, TargetFilter::Any) {
            return None;
        }

        let mut def = make_base();
        def.mode = TriggerMode::Exerted;
        def.valid_target = Some(match actor {
            Some(ControllerRef::You) => TargetFilter::Controller,
            Some(ControllerRef::Opponent) => {
                TargetFilter::Typed(TypedFilter::default().controller(ControllerRef::Opponent))
            }
            None => TargetFilter::Player,
            Some(_) => TargetFilter::Player,
        });
        def.valid_card = Some(filter);
        return Some((TriggerMode::Exerted, def));
    }

    if matches!(
        lower,
        "whenever an opponent draws a card" | "when an opponent draws a card"
    ) {
        let mut def = make_base();
        def.mode = TriggerMode::Drawn;
        def.valid_target = Some(TargetFilter::Typed(
            TypedFilter::default().controller(ControllerRef::Opponent),
        ));
        return Some((TriggerMode::Drawn, def));
    }

    // CR 701.21: "you tap an untapped creature an opponent controls"
    for prefix in [
        "whenever you tap an untapped creature an opponent controls",
        "when you tap an untapped creature an opponent controls",
    ] {
        if lower == prefix {
            let mut def = make_base();
            def.mode = TriggerMode::Taps;
            def.valid_card = Some(TargetFilter::Typed(
                TypedFilter::creature().controller(ControllerRef::Opponent),
            ));
            return Some((TriggerMode::Taps, def));
        }
    }

    for prefix in ["whenever you tap ", "when you tap "] {
        let Ok((rest, ())) = value((), tag::<_, _, OracleError<'_>>(prefix)).parse(lower) else {
            continue;
        };
        let Some(subject_text) = rest.strip_suffix(" for mana") else {
            continue;
        };
        let (filter, remainder) = parse_trigger_subject(subject_text, &mut ParseContext::default());
        if !remainder.trim().is_empty() {
            continue;
        }

        let mut def = make_base();
        def.mode = TriggerMode::TapsForMana;
        // CR 603.2 + CR 106.12a: "Whenever you tap a <subject> for mana" — the
        // trigger event must match a land *you* tapped, so the subject filter is
        // scoped to the trigger source's controller via
        // add_controller(ControllerRef::You). Mirrors the opponent arm below,
        // which scopes to ControllerRef::Opponent.
        def.valid_card = Some(add_controller(filter, ControllerRef::You));
        def.valid_target = Some(TargetFilter::Controller);
        return Some((TriggerMode::TapsForMana, def));
    }

    // CR 603.2 + CR 605.1a: "Whenever <actor> taps <subject> for mana".
    // Shared frame for both:
    //   - "a player taps …"  → no controller constraint on the source
    //   - "an opponent taps …" → source must be opponent-controlled (Vorinclex class)
    // Nested nom dispatch: outer trigger verb → actor → subject-up-to-"for mana".
    fn parse_taps_for_mana_line(i: &str) -> OracleResult<'_, (Option<ControllerRef>, &str)> {
        preceded(
            alt((tag("whenever "), tag("when "))),
            pair(
                alt((
                    value(Some(ControllerRef::Opponent), tag("an opponent taps ")),
                    value(None, tag("a player taps ")),
                )),
                terminated(take_until(" for mana"), tag(" for mana")),
            ),
        )
        .parse(i)
    }
    if let Ok((rem, (actor_controller, subject_text))) = parse_taps_for_mana_line(lower) {
        if rem.trim().is_empty() {
            let (mut filter, sub_rem) =
                parse_trigger_subject(subject_text, &mut ParseContext::default());
            if sub_rem.trim().is_empty() {
                if let Some(c) = actor_controller {
                    // Constrain subject to opponent-controlled permanents via the
                    // single `add_controller` authority (shared with the "you tap"
                    // arm above).
                    filter = add_controller(filter, c);
                }
                let mut def = make_base();
                def.mode = TriggerMode::TapsForMana;
                def.valid_card = Some(filter);
                return Some((TriggerMode::TapsForMana, def));
            }
        }
    }

    if matches!(lower, "whenever you lose life" | "when you lose life") {
        let mut def = make_base();
        def.mode = TriggerMode::LifeLost;
        def.valid_target = Some(TargetFilter::Controller);
        return Some((TriggerMode::LifeLost, def));
    }

    if matches!(
        lower,
        "whenever you lose life during your turn" | "when you lose life during your turn"
    ) {
        let mut def = make_base();
        def.mode = TriggerMode::LifeLost;
        def.valid_target = Some(TargetFilter::Controller);
        def.constraint = Some(TriggerConstraint::OnlyDuringYourTurn);
        return Some((TriggerMode::LifeLost, def));
    }

    // CR 601.2: "Whenever you cast a/an [type] spell [post-spell modifier]" — extract
    // the spell filter. Handles pre-spell type qualifier, post-spell modifier
    // (e.g. "with {X} in its mana cost", CR 107.3 + CR 202.1), or both.
    for prefix in ["you cast an ", "you cast a "] {
        if let Some(after) = strip_after(lower, prefix) {
            let mut def = make_base();
            def.mode = TriggerMode::SpellCast;
            // "you" = trigger's controller
            def.valid_target = Some(TargetFilter::Controller);

            // Truncate at ", " so any effect clause doesn't leak into the type parser.
            let payload = nom_primitives::split_once_on(after, ", ")
                .map(|(_, (before, _))| before)
                .unwrap_or(after)
                .trim();

            // CR 601.2a: pre-extract the "from <zone>" cast-origin tail BEFORE
            // running the type-phrase parser. `parse_type_phrase`'s
            // `parse_zone_suffix` would otherwise attach the zone as a
            // `FilterProp::InZone` on `valid_card` — semantically wrong for
            // SpellCast triggers because the spell object's zone at
            // fire-time is `Stack`, not its cast origin. Pulling the tail
            // first keeps `valid_card` clean and routes the constraint
            // through the matcher's typed `spell_cast_origin` gate.
            let (payload, cast_origin) = match nom_primitives::split_once_on(payload, " from ") {
                Ok((_, (before, after))) => {
                    // Re-prepend the "from " literal so the tail
                    // combinator's leading-tag matcher sees its expected
                    // shape.
                    let tail = format!("from {after}");
                    let constraint =
                        parse_origin_constraint_tail(tail.as_str(), parse_cast_origin_zone)
                            .map(|(_, c)| c)
                            .unwrap_or(OriginConstraint::Any);
                    (before, constraint)
                }
                Err(_) => (payload, OriginConstraint::Any),
            };
            def.spell_cast_origin = cast_origin;

            // First, try the post-spell-modifier-aware decomposition for shapes
            // that include "with {X} in its mana cost" etc.
            if let Some(filter) = parse_spell_qualifier_payload(payload) {
                def.valid_card = Some(filter);
                return Some((TriggerMode::SpellCast, def));
            }

            // Fall back to the classic type-phrase parser for bare type filters.
            // TypeFilter::Card alone means "spell" with no type restriction — skip it.
            let (filter, _rest) = parse_type_phrase(payload);
            let is_meaningful = match &filter {
                TargetFilter::Typed(tf) => tf.has_meaningful_type_constraint(),
                // Or-filters are always meaningful (e.g. "instant or sorcery spell")
                TargetFilter::Or { .. } => true,
                _ => false,
            };
            if is_meaningful {
                def.valid_card = Some(filter);
            }
            return Some((TriggerMode::SpellCast, def));
        }
    }

    // "an opponent casts a [quality] spell" / "a player casts a spell from a graveyard"
    if let Ok((_, (who, _))) = nom_primitives::split_once_on(lower, " casts a") {
        // CR 603.4: `split_once_on` scans anywhere in the string. A "who
        // control(s)" relative clause in the pre-`" casts a"` slice means the
        // actor is a who-controls subject; decline here so subject
        // decomposition + the who-controls clause-lift path
        // (`parse_single_subject`) handles it instead of silently dropping the
        // clause.
        if !scan_contains(who, "who controls") && !scan_contains(who, "who control") {
            let mut def = make_base();
            def.mode = TriggerMode::SpellCast;

            // Determine the caster filter
            if scan_contains(who, "opponent") {
                def.valid_target = Some(TargetFilter::Typed(
                    TypedFilter::default().controller(ControllerRef::Opponent),
                ));
            }

            // Parse the spell quality generically (e.g., "creature spell", "multicolored spell")
            // using the same parse_type_phrase building block as the "you cast" branch above.
            // Truncate at ", " to avoid passing the effect clause (e.g., ", you gain 1 life")
            // into parse_type_phrase where it would cause infinite recursion.
            let after_casts = &lower[who.len() + " casts a".len()..].trim_start();
            let after_article = value((), tag::<_, _, OracleError<'_>>("n ")) // "an" → strip the trailing "n "
                .parse(after_casts)
                .map(|(rest, _)| rest)
                .unwrap_or(after_casts)
                .trim_start();
            let spell_clause = nom_primitives::split_once_on(after_article, ", ")
                .map(|(_, (before, _))| before)
                .unwrap_or(after_article);
            // CR 601.2a: pre-extract the "from <zone>" cast-origin tail (see
            // the rationale in the "you cast a/an" branch above). Without
            // this, the zone constraint is silently dropped (Ghostly Pilferer
            // class) or mis-routed into `valid_card` via `parse_zone_suffix`.
            // Only strip the tail when the zone combinator recognises it as
            // a well-formed cast origin; an unrecognized residual
            // ("from <some other phrase>") leaves the spell_clause intact
            // so downstream parsing observes the original shape.
            let (spell_clause, cast_origin) =
                match nom_primitives::split_once_on(spell_clause, " from ") {
                    Ok((_, (before, after))) => {
                        let tail = format!("from {after}");
                        match parse_origin_constraint_tail(tail.as_str(), parse_cast_origin_zone) {
                            Ok((_, constraint)) => (before, constraint),
                            Err(_) => (spell_clause, OriginConstraint::Any),
                        }
                    }
                    Err(_) => (spell_clause, OriginConstraint::Any),
                };
            def.spell_cast_origin = cast_origin;
            // Handle "with mana value equal to the chosen number" (Talion, the Kindly Lord)
            // CR 202.3: Mana value comparison against a dynamic reference quantity.
            let chosen = spell_clause.strip_suffix("with mana value equal to the chosen number"); // allow-noncombinator: structural suffix on tokenized spell-quality chunk
            let that = spell_clause.strip_suffix("with mana value equal to that number"); // allow-noncombinator: structural suffix on tokenized spell-quality chunk
            if let Some(rest) = chosen.or(that) {
                let rest = rest.trim();
                // Parse the base type if present (e.g., "creature spell with mana value...")
                let mut base_tf = if rest.is_empty() || rest == "spell" {
                    TypedFilter::default()
                } else {
                    let (filter, _) = parse_type_phrase(rest);
                    match filter {
                        TargetFilter::Typed(tf) => tf,
                        _ => TypedFilter::default(),
                    }
                };
                base_tf = base_tf.properties(vec![FilterProp::Cmc {
                    comparator: Comparator::EQ,
                    value: QuantityExpr::Ref {
                        qty: QuantityRef::ChosenNumber,
                    },
                }]);
                def.valid_card = Some(TargetFilter::Typed(base_tf));
                return Some((TriggerMode::SpellCast, def));
            }
            // Handle "multicolored" as a spell property (not a type phrase)
            if scan_contains(spell_clause, "multicolored") {
                def.valid_card = Some(TargetFilter::Typed(TypedFilter::default().properties(
                    vec![FilterProp::ColorCount {
                        comparator: Comparator::GE,
                        count: 2,
                    }],
                )));
            } else {
                let (filter, _rest) = parse_type_phrase(spell_clause);
                let is_meaningful = match &filter {
                    TargetFilter::Typed(tf) => tf.has_meaningful_type_constraint(),
                    TargetFilter::Or { .. } => true,
                    _ => false,
                };
                if is_meaningful {
                    def.valid_card = Some(filter);
                }
            }

            return Some((TriggerMode::SpellCast, def));
        }
    }

    if scan_contains(lower, "you draw a card") {
        // CR 121.1 + CR 603.2: "Whenever you draw a card" — scope to the trigger's
        // controller. Without this filter, `match_drawn` would fire for all players'
        // draws (Sheoldred's first trigger misfires on opponent draws).
        let mut def = make_base();
        def.mode = TriggerMode::Drawn;
        def.valid_target = Some(TargetFilter::Controller);
        return Some((TriggerMode::Drawn, def));
    }

    // "whenever you attack" — player-centric attack trigger.
    //
    // CR 508.1 + CR 603.2: matches when the active player declares attackers.
    // Anchored (NOT a substring scan): an optional "whenever "/"when " prefix
    // followed by "you attack". The bare "you attack" form is the prefix-stripped
    // delayed-trigger condition emitted by `try_parse_whenever_this_turn`
    // (CR 603.7c) for cards like Dalkovan Encampment.
    //
    // The trailing `peek` is a word-boundary guard: "you attack" must be followed
    // by end-of-input, a space, or a comma so that "you attacked this turn" (a
    // condition, not a trigger) does not match. The more-specific arms
    // ("whenever you attack with N or more creatures", "N or more creatures
    // attack a player") are dispatched earlier via `try_parse_special_trigger_pattern`
    // and `try_parse_n_or_more_attacks`, so this arm cannot shadow them.
    if preceded(
        opt(alt((
            tag::<_, _, OracleError<'_>>("whenever "),
            tag("when "),
        ))),
        terminated(tag("you attack"), peek(alt((eof, recognize(one_of(" ,")))))),
    )
    .parse(lower)
    .is_ok()
    {
        let mut def = make_base();
        def.mode = TriggerMode::YouAttack;
        return Some((TriggerMode::YouAttack, def));
    }

    // CR 707.10: "whenever you copy a spell" — fires when the player creates a copy of a spell.
    if matches!(lower, "whenever you copy a spell" | "when you copy a spell") {
        let mut def = make_base();
        def.mode = TriggerMode::SpellCopy;
        def.valid_target = Some(TargetFilter::Controller);
        return Some((TriggerMode::SpellCopy, def));
    }

    // CR 601.2i + CR 603.2: Self-cast trigger — all phrasings that describe
    // *this spell being cast* fire the same event. The triggered ability fires
    // as the spell is put onto the stack; per CR 117.2a and CR 113.6, the
    // trigger's source is the stack object, so `trigger_zones = [Stack]`.
    //
    // Composed as a 2×3 cross product: {when, whenever} × {you cast this
    // spell, you cast ~, ~ is cast}. After `normalize_card_name_refs`
    // (CR 201.5) the card name becomes `~`, so direct-name references like
    // "When you cast Taught by Surrak" reach this combinator as "when you
    // cast ~". `scan_at_word_boundaries` admits leading qualifiers without
    // anchoring the trigger to start-of-line. Per CLAUDE.md "Compose nom
    // combinators, don't enumerate permutations" this is a single composed
    // combinator rather than 6 sibling `tag` arms.
    if nom_primitives::scan_at_word_boundaries(lower, |i| {
        preceded(
            alt((tag::<_, _, OracleError<'_>>("when "), tag("whenever "))),
            alt((
                tag("you cast this spell"),
                tag("you cast ~"),
                tag("~ is cast"),
            )),
        )
        .parse(i)
    })
    .is_some()
    {
        let mut def = make_base();
        def.mode = TriggerMode::SpellCast;
        def.valid_card = Some(TargetFilter::SelfRef);
        // CR 117.2a + CR 113.6: cast triggers fire while the spell is on the stack
        def.trigger_zones = vec![Zone::Stack];
        return Some((TriggerMode::SpellCast, def));
    }

    // "when you cycle this card" / "when you cycle ~" — cycling self-trigger
    // The card is in the graveyard by the time this trigger is checked.
    if scan_contains(lower, "you cycle this card") || scan_contains(lower, "you cycle ~") {
        let mut def = make_base();
        def.mode = TriggerMode::Cycled;
        def.valid_card = Some(TargetFilter::SelfRef);
        def.trigger_zones = vec![Zone::Graveyard];
        return Some((TriggerMode::Cycled, def));
    }

    // CR 120.1: "whenever you're dealt combat damage" — must precede generic "dealt damage"
    if matches!(
        lower,
        "whenever you're dealt combat damage" | "when you're dealt combat damage"
    ) {
        let mut def = make_base();
        def.mode = TriggerMode::DamageReceived;
        def.damage_kind = DamageKindFilter::CombatOnly;
        def.valid_target = Some(TargetFilter::Controller);
        return Some((TriggerMode::DamageReceived, def));
    }

    // CR 120.1: "whenever you're dealt damage"
    if matches!(
        lower,
        "whenever you're dealt damage" | "when you're dealt damage"
    ) {
        let mut def = make_base();
        def.mode = TriggerMode::DamageReceived;
        def.valid_target = Some(TargetFilter::Controller);
        return Some((TriggerMode::DamageReceived, def));
    }

    // CR 120.2b: "whenever an opponent is dealt noncombat damage"
    if matches!(
        lower,
        "whenever an opponent is dealt noncombat damage"
            | "when an opponent is dealt noncombat damage"
    ) {
        let mut def = make_base();
        def.mode = TriggerMode::DamageReceived;
        def.damage_kind = DamageKindFilter::NoncombatOnly;
        def.valid_target = Some(TargetFilter::Typed(
            TypedFilter::default().controller(ControllerRef::Opponent),
        ));
        return Some((TriggerMode::DamageReceived, def));
    }

    None
}

fn try_parse_player_action_trigger(lower: &str) -> Option<(TriggerMode, TriggerDefinition)> {
    for (prefix, valid_target) in [
        ("whenever you ", Some(TargetFilter::Controller)),
        (
            "whenever an opponent ",
            Some(TargetFilter::Typed(
                TypedFilter::default().controller(ControllerRef::Opponent),
            )),
        ),
        ("whenever a player ", None),
        ("when you ", Some(TargetFilter::Controller)),
        (
            "when an opponent ",
            Some(TargetFilter::Typed(
                TypedFilter::default().controller(ControllerRef::Opponent),
            )),
        ),
        ("when a player ", None),
    ] {
        let Ok((rest, ())) = value((), tag::<_, _, OracleError<'_>>(prefix)).parse(lower) else {
            continue;
        };
        let actions = parse_player_action_list(rest)?;
        let mut def = make_base();
        def.valid_target = valid_target.clone();
        match actions.as_slice() {
            [PlayerActionKind::SearchedLibrary] => {
                def.mode = TriggerMode::SearchedLibrary;
                return Some((TriggerMode::SearchedLibrary, def));
            }
            [PlayerActionKind::Scry] => {
                def.mode = TriggerMode::Scry;
                return Some((TriggerMode::Scry, def));
            }
            [PlayerActionKind::Surveil] => {
                def.mode = TriggerMode::Surveil;
                return Some((TriggerMode::Surveil, def));
            }
            _ => {
                def.mode = TriggerMode::PlayerPerformedAction;
                def.player_actions = Some(actions.clone());
                return Some((TriggerMode::PlayerPerformedAction, def));
            }
        }
    }

    None
}

fn parse_player_action_list(text: &str) -> Option<Vec<PlayerActionKind>> {
    let normalized = text
        .replace(", or ", "|")
        .replace(" or ", "|")
        .replace(", ", "|");
    let parts: Vec<_> = normalized.split('|').collect();
    if parts.is_empty() {
        return None;
    }

    let mut actions = Vec::with_capacity(parts.len());
    for part in parts {
        actions.push(parse_player_action_phrase(part.trim())?);
    }
    Some(actions)
}

fn parse_player_action_phrase(text: &str) -> Option<PlayerActionKind> {
    match text {
        "search your library" | "searches their library" => Some(PlayerActionKind::SearchedLibrary),
        "scry" | "scries" => Some(PlayerActionKind::Scry),
        "surveil" | "surveils" => Some(PlayerActionKind::Surveil),
        _ => None,
    }
}

/// Parse "whenever you cast your Nth spell each turn" (or "in a turn") and
/// "whenever an opponent casts their Nth [noncreature] spell each turn" into a SpellCast
/// trigger with a NthSpellThisTurn constraint.
fn try_parse_nth_spell_trigger(lower: &str) -> Option<(TriggerMode, TriggerDefinition)> {
    // Branch 1: "you cast your <ordinal> [qualifier] spell each turn"
    if let Some(result) = try_parse_nth_spell_you(lower) {
        return Some(result);
    }
    // Branch 2: "an opponent casts their <ordinal> [qualifier] spell each turn"
    if let Some(result) = try_parse_nth_spell_opponent(lower) {
        return Some(result);
    }
    // Branch 3: "a player casts their <ordinal> [qualifier] spell each turn"
    if let Some(result) = try_parse_nth_spell_any_player(lower) {
        return Some(result);
    }
    None
}

/// Timing-clause kind for nth-spell/nth-draw triggers.
/// CR 601.2 + CR 603.4: The trailing "each turn" / "in a turn" (unrestricted
/// timing), "during each opponent's turn" (restricted to opponent's turn), or
/// "during their turn" (restricted to the acting player's own turn — the
/// triggering player must be the active player; e.g. The Council of Four).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NthEventTimingKind {
    /// "each turn" / "in a turn" — no turn-ownership restriction.
    Unrestricted,
    /// "during each opponent's turn" — restricted to an opponent's turn.
    OpponentsTurnOnly,
    /// "during their turn" — restricted to the acting player's own turn.
    ActorsTurnOnly,
}

/// Map a timing kind to the intervening-if condition it implies, if any.
/// `Unrestricted` implies no condition. The restricted kinds gate on
/// the active player via `TriggerCondition::DuringPlayersTurn`.
fn timing_condition(timing: NthEventTimingKind) -> Option<TriggerCondition> {
    match timing {
        NthEventTimingKind::Unrestricted => None,
        // CR 603.4 + CR 102.1: opponent's-turn restriction.
        NthEventTimingKind::OpponentsTurnOnly => Some(TriggerCondition::DuringPlayersTurn {
            player: PlayerFilter::Opponent,
        }),
        // CR 603.4 + CR 102.1: "during their turn" — the triggering player
        // (drawer/caster) must be the active player.
        NthEventTimingKind::ActorsTurnOnly => Some(TriggerCondition::DuringPlayersTurn {
            player: PlayerFilter::TriggeringPlayer,
        }),
    }
}

/// CR 603.4 + CR 102.1: Attach a trailing timing tail (e.g. "during their
/// turn") parsed off a draw / life-gain / life-loss trigger predicate to the
/// produced `TriggerDefinition`. The tail is dispatched through the shared
/// typed `parse_timing_tail` combinator; a recognized restricted tail becomes
/// a `DuringPlayersTurn` intervening-if condition. An empty / unrecognized
/// tail leaves the definition unrestricted. When the definition already
/// carries a condition, the timing restriction is ANDed in.
fn attach_event_timing_tail(def: &mut TriggerDefinition, tail: &str) {
    let trimmed = tail.trim();
    if trimmed.is_empty() {
        return;
    }
    let Ok((_, kind)) = parse_timing_tail(trimmed) else {
        return;
    };
    let Some(timing) = timing_condition(kind) else {
        return;
    };
    def.condition = Some(match def.condition.take() {
        Some(existing) => TriggerCondition::And {
            conditions: vec![existing, timing],
        },
        None => timing,
    });
}

/// Nom combinator for a complete timing-tail clause: matches "each turn",
/// "in a turn", "during each opponent's turn" (apostrophe-normalized), or
/// "during their turn". Wrapped in `all_consuming` so it succeeds only when
/// the clause consumes the entire (already-trimmed) input. Shared by the
/// nth-spell and nth-draw timing classifiers.
fn parse_timing_tail(i: &str) -> OracleResult<'_, NthEventTimingKind> {
    all_consuming(alt((
        value(NthEventTimingKind::Unrestricted, tag("each turn")),
        value(NthEventTimingKind::Unrestricted, tag("in a turn")),
        value(
            NthEventTimingKind::OpponentsTurnOnly,
            alt((
                tag("during each opponent's turn"),
                tag("during each opponent\u{2019}s turn"),
            )),
        ),
        value(NthEventTimingKind::ActorsTurnOnly, tag("during their turn")),
    )))
    .parse(i)
}

/// Inspect the text after the nth-spell ordinal+qualifier payload to determine
/// the timing clause kind. The timing clause terminates the string; the
/// preceding qualifier text ("spell …") is skipped by scanning word boundaries
/// for the first position where `parse_timing_tail` matches to end-of-input.
/// Returns `None` when no recognized timing tail terminates the text.
fn classify_nth_event_timing(rest: &str) -> Option<NthEventTimingKind> {
    let mut remaining = rest.trim();
    loop {
        if let Ok((_, kind)) = parse_timing_tail(remaining) {
            return Some(kind);
        }
        // allow-noncombinator: word-boundary scan to advance to the next
        // candidate position — the timing tail is matched by parse_timing_tail.
        let idx = remaining.find(' ')?;
        remaining = remaining[idx + 1..].trim_start();
    }
}

/// Parse the draw-trigger payload "card <timing>" into a timing kind.
/// The literal "card" is consumed by a nom `tag()`, then the timing tail is
/// dispatched via the shared `parse_timing_tail` combinator. Rejects the
/// "during each opponent's turn" form, which has no coherent meaning on a
/// per-draw subject. Returns `None` on any other (or absent) tail so the
/// caller falls through to other trigger patterns.
fn classify_nth_draw_timing(rest: &str) -> Option<NthEventTimingKind> {
    let (tail, ()) = value((), tag::<_, _, OracleError<'_>>("card"))
        .parse(rest.trim())
        .ok()?;
    parse_timing_tail(tail.trim())
        .ok()
        .map(|(_, kind)| kind)
        .filter(|kind| *kind != NthEventTimingKind::OpponentsTurnOnly)
}

/// "you cast your <ordinal> [qualifier] spell [post-spell modifier] each turn"
/// Also handles "during each opponent's turn" variant (CR 601.2).
fn try_parse_nth_spell_you(lower: &str) -> Option<(TriggerMode, TriggerDefinition)> {
    let prefix = "you cast your ";
    let after = strip_after(lower, prefix)?;
    let (n, rest) = parse_ordinal(after)?;
    let timing = classify_nth_event_timing(rest)?;
    let filter = extract_spell_type_filter(rest);
    let mut def = make_base();
    def.mode = TriggerMode::SpellCast;
    // CR 603.2: Trigger event must match — gate on caster=you so opponent's
    // Nth spell does not fire this trigger. Mirrors the `Opponent` branch below
    // that sets `valid_target` for symmetric per-caster scoping.
    def.valid_target = Some(TargetFilter::Typed(
        TypedFilter::default().controller(ControllerRef::You),
    ));
    def.constraint = Some(TriggerConstraint::NthSpellThisTurn { n, filter });
    def.condition = timing_condition(timing);
    Some((TriggerMode::SpellCast, def))
}

/// "an opponent casts their <ordinal> [qualifier] spell [post-spell modifier] each turn"
fn try_parse_nth_spell_opponent(lower: &str) -> Option<(TriggerMode, TriggerDefinition)> {
    let prefix = "an opponent casts their ";
    let after = strip_after(lower, prefix)?;
    let (n, rest) = parse_ordinal(after)?;
    // Opponents-path supports unrestricted timing and "during their turn" (the
    // opponent's own turn). "during each opponent's turn" is redundant wording
    // for an opponent-scoped trigger and is rejected.
    let timing = classify_nth_event_timing(rest)?;
    if timing == NthEventTimingKind::OpponentsTurnOnly {
        return None;
    }
    let filter = extract_spell_type_filter(rest);
    let mut def = make_base();
    def.mode = TriggerMode::SpellCast;
    def.valid_target = Some(TargetFilter::Typed(
        TypedFilter::default().controller(ControllerRef::Opponent),
    ));
    def.constraint = Some(TriggerConstraint::NthSpellThisTurn { n, filter });
    def.condition = timing_condition(timing);
    Some((TriggerMode::SpellCast, def))
}

/// "a player casts their <ordinal> [qualifier] spell [post-spell modifier] each turn"
/// CR 603.2: No valid_target filter — fires for any player's spell.
/// NthSpellThisTurn constraint extracts caster from the SpellCast event
/// and checks per-player counts via spells_cast_this_turn_by_player.
fn try_parse_nth_spell_any_player(lower: &str) -> Option<(TriggerMode, TriggerDefinition)> {
    let prefix = "a player casts their ";
    let after = strip_after(lower, prefix)?;
    let (n, rest) = parse_ordinal(after)?;
    // "a player" can act on any turn — accept unrestricted timing and the
    // "during their turn" restriction (The Council of Four). "during each
    // opponent's turn" has no coherent meaning for an any-player subject.
    let timing = classify_nth_event_timing(rest)?;
    if timing == NthEventTimingKind::OpponentsTurnOnly {
        return None;
    }
    let filter = extract_spell_type_filter(rest);
    let mut def = make_base();
    def.mode = TriggerMode::SpellCast;
    def.constraint = Some(TriggerConstraint::NthSpellThisTurn { n, filter });
    def.condition = timing_condition(timing);
    Some((TriggerMode::SpellCast, def))
}

/// Extract a spell filter from the qualifier between ordinal and the trailing
/// "each turn" / "in a turn" / "during each opponent's turn" clause.
///
/// Handles three qualifier shapes, which may combine:
/// 1. Pre-spell type qualifier: `"noncreature spell each turn"` → `TypeFilter::Non(Creature)`.
/// 2. Post-spell X-in-cost qualifier: `"spell with {x} in its mana cost each turn"`
///    → `FilterProp::HasXInManaCost` (CR 107.3 + CR 202.1).
/// 3. Both combined: `"creature spell with {x} in its mana cost each turn"`.
///
/// Returns `None` when no meaningful qualifier is present — the caller treats
/// that as an unrestricted spell filter.
fn extract_spell_type_filter(after_ordinal: &str) -> Option<TargetFilter> {
    let trimmed = after_ordinal.trim();
    // Isolate the qualifier payload by word-boundary scanning for the position
    // where `parse_timing_tail` matches to end-of-input; everything before that
    // position is the qualifier. Returns `None` if no timing tail is present.
    let qualifier = split_off_timing_tail(trimmed)?;
    parse_spell_qualifier_payload(qualifier.trim())
}

/// Word-boundary scan: return the text preceding the trailing timing clause,
/// or `None` if `text` does not end with a `parse_timing_tail`-recognized
/// clause. Used to isolate the qualifier payload from the timing tail.
fn split_off_timing_tail(text: &str) -> Option<&str> {
    let mut idx = 0;
    loop {
        if parse_timing_tail(text[idx..].trim_start()).is_ok() {
            return Some(text[..idx].trim_end());
        }
        // allow-noncombinator: word-boundary scan to advance to the next
        // candidate position — the timing tail is matched by parse_timing_tail.
        let next = text[idx..].find(' ')?;
        idx += next + 1;
    }
}

/// Parse the qualifier payload between the ordinal and the timing clause.
/// The payload must contain the word "spell" at some position; text before
/// "spell" is the type phrase, text after "spell" is a post-spell modifier.
fn parse_spell_qualifier_payload(qualifier: &str) -> Option<TargetFilter> {
    // Bare "spell" with no pre- or post-modifier means "no filter" (any spell).
    if qualifier == "spell" {
        return None;
    }
    // The payload is one of three shapes:
    //   (a) "<type-phrase> spell"                 — type only
    //   (b) "spell <post-modifier>"               — post-modifier only
    //   (c) "<type-phrase> spell <post-modifier>" — both
    // Detect shape (b) by a leading "spell " literal before attempting the
    // " spell" word-boundary split (which only separates shape (a)/(c)).
    let (pre_spell, post_spell) = if let Some(rest) = qualifier.strip_prefix("spell ") {
        ("", rest.trim())
    } else {
        // Split on " spell" (word-boundary) to separate type phrase from post-spell modifier.
        // Delegates to nom_primitives::split_once_on for word-boundary-safe splitting.
        match crate::parser::oracle_nom::primitives::split_once_on(qualifier, " spell") {
            Ok((_, (pre, post))) => (pre.trim(), post.trim()),
            Err(_) => {
                // No " spell" split — treat as a type-only qualifier.
                return type_only_filter(qualifier);
            }
        }
    };

    let type_filter = if pre_spell.is_empty() {
        None
    } else {
        type_only_filter(pre_spell)
    };
    let post_filter = if post_spell.is_empty() {
        None
    } else {
        // Non-empty post-spell text that does NOT match a recognized modifier
        // (e.g. "that targets only ~" — handled by the legacy `parse_type_phrase`
        // pathway). `?` propagates None so the caller can fall back.
        Some(parse_post_spell_modifier(post_spell)?)
    };

    match (type_filter, post_filter) {
        (None, None) => None,
        (Some(f), None) | (None, Some(f)) => Some(f),
        (Some(a), Some(b)) => Some(TargetFilter::And {
            filters: vec![a, b],
        }),
    }
}

/// Parse a bare type phrase (e.g. "noncreature", "creature") as a `TargetFilter`.
/// Returns `None` if `parse_type_phrase` reports `TargetFilter::Any` or leaves
/// residual text — both indicate the phrase was not a pure type qualifier.
fn type_only_filter(qualifier: &str) -> Option<TargetFilter> {
    let (filter, remainder) = parse_type_phrase(qualifier);
    if remainder.trim().is_empty() && !matches!(filter, TargetFilter::Any) {
        Some(filter)
    } else {
        None
    }
}

/// Parse a post-spell modifier phrase (text between "spell" and the timing tail).
///
/// Currently supports:
/// - "with {x} in its mana cost" — CR 107.3 + CR 202.1. Produces a `TargetFilter`
///   containing `FilterProp::HasXInManaCost`.
///
/// Extend by adding more combinator branches as additional post-spell modifiers
/// (e.g. "with converted mana cost N", "that targets you") become supported.
///
/// Shared with `oracle_effect::try_parse_when_next_event` (delayed-trigger variant
/// of the same filter shape) — exposed as `pub(crate)` to keep the combinator
/// definition in a single place.
pub(crate) fn parse_post_spell_modifier(modifier: &str) -> Option<TargetFilter> {
    use crate::types::ability::{FilterProp, TypedFilter};

    // "with {X} in its mana cost" (Brass Infiniscope): X literally appears in the mana cost.
    if let Ok((rest, ())) = alt((
        value(
            (),
            tag::<_, _, OracleError<'_>>("with {x} in its mana cost"),
        ),
        value((), tag("with an {x} in its mana cost")),
    ))
    .parse(modifier)
    {
        if rest.trim().is_empty() {
            return Some(TargetFilter::Typed(
                TypedFilter::default().properties(vec![FilterProp::HasXInManaCost]),
            ));
        }
    }

    // CR 202.3: "with mana value N or less" / "with mana value N or greater" /
    // "with mana value N" — numeric CMC comparator. Delegates to the shared
    // `parse_mana_value_suffix` combinator so the full set of comparator forms
    // (static N, X-variable, ObjectManaValue { CostPaidObject }) is supported here for
    // free alongside the search filter and target filter call sites.
    if let Some((prop, consumed)) =
        super::oracle_target::parse_mana_value_suffix(modifier, &mut ParseContext::default())
    {
        if modifier[consumed..].trim().is_empty() {
            return Some(TargetFilter::Typed(
                TypedFilter::default().properties(vec![prop]),
            ));
        }
    }

    None
}

/// Parse "whenever [subject] draw(s) [possessive] Nth card each turn" into a Drawn trigger
/// with a NthDrawThisTurn constraint.
/// Follows the same decomposition pattern as `try_parse_nth_spell_trigger`.
fn try_parse_nth_draw_trigger(lower: &str) -> Option<(TriggerMode, TriggerDefinition)> {
    if let Some(result) = try_parse_nth_draw_you(lower) {
        return Some(result);
    }
    if let Some(result) = try_parse_nth_draw_opponent(lower) {
        return Some(result);
    }
    if let Some(result) = try_parse_nth_draw_any_player(lower) {
        return Some(result);
    }
    None
}

/// "you draw your <ordinal> card each turn"
///
/// CR 121.2 + CR 603.2: The "you" subject restricts the trigger to the
/// controller's draws. `valid_target` carries a `ControllerRef::You` filter so
/// `match_drawn` / `valid_player_matches` reject events where the drawing
/// player is not the trigger controller — mirroring the opponent arm below.
fn try_parse_nth_draw_you(lower: &str) -> Option<(TriggerMode, TriggerDefinition)> {
    let prefix = "you draw your ";
    let after = strip_after(lower, prefix)?;
    let (n, rest) = parse_ordinal(after)?;
    let timing = classify_nth_draw_timing(rest)?;
    let mut def = make_base();
    def.mode = TriggerMode::Drawn;
    def.valid_target = Some(TargetFilter::Typed(
        TypedFilter::default().controller(ControllerRef::You),
    ));
    def.constraint = Some(TriggerConstraint::NthDrawThisTurn { n });
    def.condition = timing_condition(timing);
    Some((TriggerMode::Drawn, def))
}

/// "an opponent draws their <ordinal> card each turn"
fn try_parse_nth_draw_opponent(lower: &str) -> Option<(TriggerMode, TriggerDefinition)> {
    let prefix = "an opponent draws their ";
    let after = strip_after(lower, prefix)?;
    let (n, rest) = parse_ordinal(after)?;
    let timing = classify_nth_draw_timing(rest)?;
    let mut def = make_base();
    def.mode = TriggerMode::Drawn;
    def.valid_target = Some(TargetFilter::Typed(
        TypedFilter::default().controller(ControllerRef::Opponent),
    ));
    def.constraint = Some(TriggerConstraint::NthDrawThisTurn { n });
    def.condition = timing_condition(timing);
    Some((TriggerMode::Drawn, def))
}

/// "a player draws their <ordinal> card each turn"
/// CR 121.2: No valid_target filter — fires for any player's draw.
fn try_parse_nth_draw_any_player(lower: &str) -> Option<(TriggerMode, TriggerDefinition)> {
    let prefix = "a player draws their ";
    let after = strip_after(lower, prefix)?;
    let (n, rest) = parse_ordinal(after)?;
    let timing = classify_nth_draw_timing(rest)?;
    let mut def = make_base();
    def.mode = TriggerMode::Drawn;
    def.constraint = Some(TriggerConstraint::NthDrawThisTurn { n });
    def.condition = timing_condition(timing);
    Some((TriggerMode::Drawn, def))
}

/// Parse counter-placement triggers from Oracle text.
/// Handles all patterns: passive ("a counter is put on ~"), active ("you put counters on ~"),
/// and with arbitrary subjects ("counters are put on another creature you control").
fn try_parse_counter_trigger(lower: &str) -> Option<(TriggerMode, TriggerDefinition)> {
    if !scan_contains(lower, "counter") {
        return None;
    }

    // CR 122.1: "a [type] counter is removed from ~" — counter removal trigger.
    // Check removal before placement to avoid false-matching "removed" as "put".
    if let Some(result) = try_parse_counter_removed(lower) {
        return Some(result);
    }

    // Must mention both a counter and a placement verb
    if !scan_contains(lower, "put") && !scan_contains(lower, "placed") {
        return None;
    }

    // Find "counter(s) ... on SUBJECT" — locate "counter" then " on " after it.
    // Uses scan_split_at_phrase for word-boundary-aware "counter" match,
    // then split_once_on for the positional " on " split.
    let (counter_prefix, counter_start) =
        scan_split_at_phrase(lower, |i| tag::<_, _, OracleError<'_>>("counter").parse(i))?;
    let Ok((_, (_, subject_text))) = nom_primitives::split_once_on(counter_start, " on ") else {
        return None;
    };
    let subject_text = subject_text.trim();

    let mut def = make_base();
    def.mode = TriggerMode::CounterAdded;
    // CR 122.1: capture an explicit counter restriction so the trigger fires
    // only on the named counter kind — a threshold form ("the twelfth hour
    // counter") or a plain type form ("one or more -1/-1 counters", Hapatra).
    // No type word ("one or more counters") leaves the filter unset → any kind.
    if let Some(filter) = parse_counter_threshold_prefix(counter_prefix)
        .or_else(|| parse_counter_type_prefix(counter_prefix))
    {
        def = def.counter_filter(filter);
    }

    // Parse the subject after "on "
    if tag::<_, _, OracleError<'_>>("~")
        .parse(subject_text)
        .is_ok()
    {
        def.valid_card = Some(TargetFilter::SelfRef);
    } else {
        let (filter, _) = parse_single_subject(subject_text, &mut ParseContext::default());
        def.valid_card = Some(filter);
    }

    Some((TriggerMode::CounterAdded, def))
}

/// "When the twelfth hour counter is put on ~" — thresholded counter triggers.
/// Uses the same `CounterTriggerFilter` building block as Saga chapters, so the
/// runtime fires only when the object crosses the named counter threshold.
fn parse_counter_threshold_prefix(prefix: &str) -> Option<CounterTriggerFilter> {
    let (rest, _) = opt(alt((
        tag::<_, _, OracleError<'_>>("when "),
        tag("whenever "),
    )))
    .parse(prefix.trim_start())
    .ok()?;
    let (rest, _) = tag::<_, _, OracleError<'_>>("the ")
        .parse(rest.trim_start())
        .ok()?;
    let (threshold, rest) = parse_ordinal(rest)?;
    let counter_type_text = rest.trim();
    if counter_type_text.is_empty() {
        return None;
    }
    Some(CounterTriggerFilter {
        counter_type: crate::types::counter::parse_counter_type(counter_type_text),
        threshold: Some(threshold),
    })
}

/// CR 122.1: Extract an explicit counter *type* from the text preceding
/// "counter" in a placement trigger ("you put one or more -1/-1 counters
/// on …", "a +1/+1 counter is put on …"). Strips the leading timing word,
/// optional placement verb, and quantity phrase (article / "one or more" /
/// numeral); whatever typed remainder names the counter type. Returns `None`
/// when no type word remains ("one or more counters" → any kind, no filter),
/// so the trigger keeps firing on every counter as printed.
fn parse_counter_type_prefix(prefix: &str) -> Option<CounterTriggerFilter> {
    let (rest, _) = opt(alt((
        tag::<_, _, OracleError<'_>>("whenever "),
        tag("when "),
    )))
    .parse(prefix.trim_start())
    .ok()?;
    let (rest, _) = opt(alt((
        tag::<_, _, OracleError<'_>>("you've put "),
        tag("you have put "),
        tag("you put "),
    )))
    .parse(rest)
    .ok()?;
    let (rest, _) = opt(alt((
        tag::<_, _, OracleError<'_>>("one or more "),
        tag("another "),
        tag("an "),
        tag("a "),
    )))
    .parse(rest)
    .ok()?;
    // Optional numeral count ("you put two +1/+1 counters …"). Counter types
    // never begin with a bare digit, so a leading "number " is safely a
    // quantity; consume it (number + trailing space) as one combinator.
    let rest = (
        nom_primitives::parse_number,
        tag::<_, _, OracleError<'_>>(" "),
    )
        .parse(rest)
        .map_or(rest, |(after, _)| after);
    // Only build a filter when the remainder is a genuinely recognized counter
    // type. The prefix strip is deliberately loose, so non-"you" subjects ("an
    // opponent puts …", "you create a token or put a …") leave leftover subject
    // text here; `try_parse_counter_type` rejects multi-word junk (→ None), so
    // those any-counter triggers keep firing on every counter as printed.
    let counter_type = crate::types::counter::try_parse_counter_type(rest.trim())?;
    Some(CounterTriggerFilter {
        counter_type,
        threshold: None,
    })
}

/// CR 122.1: Parse "a [type] counter is removed from [subject]" patterns.
/// Also handles zone constraints like "while it's exiled" (e.g. suspend cards).
fn try_parse_counter_removed(lower: &str) -> Option<(TriggerMode, TriggerDefinition)> {
    // Pattern: "a [type] counter is removed from [subject] [while ...]"
    let (after_prefix, _) = opt(alt((
        tag::<_, _, OracleError<'_>>("whenever "),
        tag("when "),
    )))
    .parse(lower)
    .ok()?;
    let (after_a, ()) = value((), tag::<_, _, OracleError<'_>>("a "))
        .parse(after_prefix)
        .ok()?;

    let (_, (counter_type, subject_rest)) =
        nom_primitives::split_once_on(after_a, " counter is removed from ").ok()?;
    let counter_type = counter_type.trim();
    let subject_rest = subject_rest.trim();

    let mut def = make_base();
    def.mode = TriggerMode::CounterRemoved;

    // Parse optional "while it's exiled" / "while ~ is exiled" zone constraint
    let (subject_text, zone_constraint) =
        if let Some(before) = subject_rest.strip_suffix("while it's exiled") {
            (before.trim(), Some(Zone::Exile))
        } else if let Some(before) = subject_rest.strip_suffix("while ~ is exiled") {
            (before.trim(), Some(Zone::Exile))
        } else {
            (subject_rest, None)
        };

    // Parse subject
    if subject_text == "~" || SELF_REF_PARSE_ONLY_PHRASES.contains(&subject_text) {
        def.valid_card = Some(TargetFilter::SelfRef);
    } else {
        let (filter, _) = parse_single_subject(subject_text, &mut ParseContext::default());
        def.valid_card = Some(filter);
    }

    // Set counter type as description metadata (the counter_filter field could be extended
    // but for now the type info is captured in the description)
    if !counter_type.is_empty() {
        def.description = Some(format!("{counter_type} counter"));
    }

    // CR 122.1: Zone constraint for cards that trigger from exile (e.g. suspend)
    if let Some(zone) = zone_constraint {
        def.trigger_zones = vec![zone];
    }

    Some((TriggerMode::CounterRemoved, def))
}

/// CR 700.4 + CR 603.6c + CR 109.5: Parse "is/are put into [possessive] graveyard
/// [from <origin-possessive>]" patterns.
///
/// Handles all forms:
/// - "is put into a graveyard from anywhere" (no origin restriction)
/// - "is put into a graveyard from the battlefield" (equivalent to "dies")
/// - "is put into your graveyard [from your library]" (controller filter + optional origin)
/// - "is put into an opponent's graveyard from anywhere" (opponent controller filter)
/// - "is put into an opponent's graveyard from their library" (CR 109.5: "their"
///   anaphor binds back to the previously named opponent — Undead Alchemist class)
/// - "is put into a player's graveyard from any library" (Bloodchief Ascension class)
/// - "are put into your graveyard from your library" (plural form for batched triggers)
///
/// The graveyard's possessive narrows `valid_card.controller` (CR 109.5: a card
/// going into player P's graveyard is P's card). This is what `match_changes_zone`
/// actually reads when deciding to fire — historically the possessive only set
/// `valid_target`, which `match_changes_zone` ignores for ChangesZone, so the
/// trigger fired for any card going to any graveyard. `valid_target` is also
/// preserved so downstream effect resolution (e.g. `target: ParentTarget`) keeps
/// the narrowed scope.
/// CR 603.6c: Parse the source-zone tail of a "put into a graveyard" clause.
/// Handles "from the battlefield" → `Equals`, "from anywhere other than the
/// battlefield" → `NotEquals`, "from anywhere" / no clause → `Any`, and the
/// possessive-zone forms reused from `parse_graveyard_origin_zone`.
///
/// Thin caller around `parse_origin_constraint_tail` parameterized on the
/// graveyard-context zone combinator (`parse_graveyard_origin_zone`). Preserves
/// the historical "no `from` tail → `Any`" semantics that the disjunctive
/// zone-change matcher relies on.
fn parse_put_into_graveyard_origin(input: &str) -> OracleResult<'_, OriginConstraint> {
    parse_origin_constraint_tail(input, parse_graveyard_origin_zone)
}

/// CR 601.2a + CR 603.6: Parse a "from <zone>" source-zone tail into an
/// `OriginConstraint`. Handles `from anywhere other than <zone>` (NotEquals),
/// bare `from anywhere` (Any), single-zone tails (Equals), and the absent-
/// clause case (Any).
///
/// `zone_combinator` returns `Option<Zone>`: `Some(z)` for a constrained zone,
/// `None` for the bare "anywhere" leaf (so the caller's primitives can decide
/// which zones are recognized in their context — graveyard-context recognizes
/// "the battlefield" / "your library" etc.; cast-context recognizes "their
/// hand" / "your graveyard" / "exile" / "any library" / "the command zone").
fn parse_origin_constraint_tail<'a, F>(
    input: &'a str,
    mut zone_combinator: F,
) -> OracleResult<'a, OriginConstraint>
where
    F: FnMut(&'a str) -> OracleResult<'a, Option<Zone>>,
{
    let input = input.trim_start();
    // No "from" clause — any source zone.
    let Ok((after_from, ())) = value((), tag::<_, _, OracleError<'_>>("from ")).parse(input) else {
        return Ok((input, OriginConstraint::Any));
    };
    let after_from = after_from.trim_start();
    // CR 603.6c + CR 601.2a: "from anywhere other than <zone>" — the
    // negative-discriminator form (Ghostly Pilferer: "from anywhere other
    // than their hand"; Syr Konrad clause 2: "from anywhere other than the
    // battlefield"). The zone token comes from the caller's combinator.
    if let Ok((after_other, ())) =
        value((), tag::<_, _, OracleError<'_>>("anywhere other than ")).parse(after_from)
    {
        let (rest, zone_opt) = zone_combinator(after_other)?;
        // `None` from the zone combinator means the inner phrase was a bare
        // "anywhere" — "from anywhere other than anywhere" is malformed; fall
        // through to the generic path which will treat the residual as Any.
        if let Some(zone) = zone_opt {
            return Ok((rest, OriginConstraint::NotEquals(zone)));
        }
    }
    // Single-zone tail or bare "anywhere".
    let (rest, zone) = zone_combinator(after_from)?;
    Ok((
        rest,
        match zone {
            Some(z) => OriginConstraint::Equals(z),
            None => OriginConstraint::Any,
        },
    ))
}

/// CR 601.2a + CR 400.1: Parse a cast-origin zone phrase, including the
/// possessive forms ("your graveyard", "their hand", "an opponent's library",
/// "a player's graveyard"), bare possessive-less forms ("exile", "the command
/// zone", "a library", "any library"), and the bare "anywhere" leaf.
///
/// Returns `Some(Zone)` for a constrained zone, `None` for the bare
/// "anywhere" leaf. The valid_card filter on the trigger (already narrowed by
/// `valid_target.controller`) provides the player-scope binding when needed.
fn parse_cast_origin_zone(input: &str) -> OracleResult<'_, Option<Zone>> {
    alt((
        // CR 603.6c: bare "anywhere" → no origin restriction.
        value(None, tag("anywhere")),
        // Hand — every possessive form maps to Zone::Hand. CR 109.5: "their"
        // is an anaphor back to the caster named earlier in the trigger
        // condition; possessive does not narrow the zone selection itself.
        value(Some(Zone::Hand), tag("your hand")),
        value(Some(Zone::Hand), tag("their hand")),
        value(Some(Zone::Hand), tag("an opponent's hand")),
        value(Some(Zone::Hand), tag("a player's hand")),
        value(Some(Zone::Hand), tag("any hand")),
        // Graveyard — Snapcaster-class "from your graveyard"; flashback-payoff
        // forms; opponent-graveyard payoffs.
        value(Some(Zone::Graveyard), tag("your graveyard")),
        value(Some(Zone::Graveyard), tag("their graveyard")),
        value(Some(Zone::Graveyard), tag("an opponent's graveyard")),
        value(Some(Zone::Graveyard), tag("a player's graveyard")),
        value(Some(Zone::Graveyard), tag("a graveyard")),
        value(Some(Zone::Graveyard), tag("any graveyard")),
        // Library — cascade / impulse-draw "from exile" variants and library
        // casts (rare but well-defined per CR 400.1).
        value(Some(Zone::Library), tag("your library")),
        value(Some(Zone::Library), tag("their library")),
        value(Some(Zone::Library), tag("an opponent's library")),
        value(Some(Zone::Library), tag("a player's library")),
        value(Some(Zone::Library), tag("any library")),
        // Exile — Mizzix-class exile-cast payoffs; flashback / suspend etc.
        // Exile has no per-player partition (CR 400.1), so possessive forms
        // are rare in printed text; bare "exile" is the dominant phrasing.
        value(Some(Zone::Exile), tag("exile")),
        // Command zone — commander-payoff cards.
        value(Some(Zone::Command), tag("the command zone")),
        value(Some(Zone::Command), tag("a command zone")),
        // Battlefield (rare for cast triggers but defined per CR 400.1).
        value(Some(Zone::Battlefield), tag("the battlefield")),
    ))
    .parse(input)
}

/// CR 603.6 + CR 603.2: Parse one clause of a disjunctive zone-change trigger
/// from `[subject] [verb-phrase]`. Returns the typed `ZoneChangeClause`, or
/// `None` if the verb phrase is not a recognized zone-change shape.
fn parse_zone_change_clause(subject: &TargetFilter, rest: &str) -> Option<ZoneChangeClause> {
    let rest = rest.trim_start();

    // CR 700.4: "dies" / "is put into a graveyard from the battlefield" —
    // battlefield → graveyard.
    if let Ok((tail, ())) = alt((
        value((), tag::<_, _, OracleError<'_>>("dies")),
        value((), tag("die")),
        value((), tag("is put into a graveyard from the battlefield")),
        value((), tag("are put into a graveyard from the battlefield")),
    ))
    .parse(rest)
    {
        if !tail.trim().is_empty() {
            return None;
        }
        return Some(ZoneChangeClause {
            origin: OriginConstraint::Equals(Zone::Battlefield),
            destination: Some(Zone::Graveyard),
            valid_card: Some(subject.clone()),
        });
    }

    // CR 603.6c: "is/are put into [a] graveyard [from <zone>]" — the general
    // put-into-graveyard form. Source-zone constraint comes from the optional
    // "from" tail.
    if let Ok((after_verb, ())) = alt((
        value((), tag::<_, _, OracleError<'_>>("is put into ")),
        value((), tag("are put into ")),
    ))
    .parse(rest)
    {
        let (after_gy, possessive) = parse_graveyard_possessive.parse(after_verb).ok()?;
        let (tail, origin) = parse_put_into_graveyard_origin(after_gy).ok()?;
        if !tail.trim().is_empty() {
            return None;
        }
        let valid_card = match possessive {
            Some(ctrl) => Some(add_controller(subject.clone(), ctrl)),
            None => Some(subject.clone()),
        };
        return Some(ZoneChangeClause {
            origin,
            destination: Some(Zone::Graveyard),
            valid_card,
        });
    }

    // CR 603.10a: "leaves your graveyard" — a leaves-a-graveyard look-back
    // trigger. The destination is unconstrained (the card may go to any zone).
    // CR 603.10a + CR 400.1: "your graveyard" narrows the card to one in the
    // controller's graveyard via `ControllerRef::You` on the card filter.
    if let Ok((tail, ())) =
        value((), tag::<_, _, OracleError<'_>>("leaves your graveyard")).parse(rest)
    {
        if !tail.trim().is_empty() {
            return None;
        }
        return Some(ZoneChangeClause {
            origin: OriginConstraint::Equals(Zone::Graveyard),
            destination: None,
            valid_card: Some(add_controller(subject.clone(), ControllerRef::You)),
        });
    }

    None
}

/// CR 603.1 + CR 603.2: Decompose a disjunctive zone-change trigger condition
/// ("whenever [clause], or [clause], or [clause]") into a `Vec<ZoneChangeClause>`.
/// Returns `None` unless there are two or more clauses and EVERY clause parses —
/// a single-clause condition stays on the scalar path, and a partial parse falls
/// back to `Unknown` rather than dropping clauses.
fn parse_disjunctive_zone_change_condition(condition: &str) -> Option<Vec<ZoneChangeClause>> {
    let lower = condition.to_lowercase();
    let after_keyword = alt((
        value((), tag::<_, _, OracleError<'_>>("whenever ")),
        value((), tag("when ")),
    ))
    .parse(lower.as_str())
    .map(|(rest, _)| rest)
    .unwrap_or(&lower);

    // Split on the disjunction separator. Each clause is "[subject] [verb]".
    let mut clauses = Vec::new();
    let mut remaining = after_keyword;
    loop {
        // Find the next ", or " / " or " boundary, splitting off one clause.
        let (clause_text, rest) = match nom_primitives::split_once_on(remaining, ", or ")
            .or_else(|_| nom_primitives::split_once_on(remaining, " or "))
        {
            Ok((_, (before, after))) => (before, Some(after)),
            Err(_) => (remaining, None),
        };
        let mut ctx = ParseContext::default();
        let (subject, verb) = parse_trigger_subject(clause_text.trim(), &mut ctx);
        clauses.push(parse_zone_change_clause(&subject, verb)?);
        match rest {
            Some(after) => remaining = after,
            None => break,
        }
    }

    (clauses.len() >= 2).then_some(clauses)
}

fn try_parse_put_into_graveyard(
    subject: &TargetFilter,
    rest: &str,
) -> Option<(TriggerMode, TriggerDefinition)> {
    // Match the verb prefix: "is put into " or "are put into "
    let (after_verb, ()) = alt((
        value((), tag::<_, _, OracleError<'_>>("is put into ")),
        value((), tag("are put into ")),
    ))
    .parse(rest)
    .ok()?;

    let (after_gy, possessive) = parse_graveyard_possessive.parse(after_verb).ok()?;

    // Parse optional "from [zone]" clause
    let after_gy = after_gy.trim_start();
    let origin = if let Ok((after_from, ())) =
        value((), tag::<_, _, OracleError<'_>>("from ")).parse(after_gy)
    {
        let after_from = after_from.trim_start();
        parse_graveyard_origin_zone
            .parse(after_from)
            .ok()
            .map(|(_, z)| z)
            .unwrap_or(None)
    } else {
        // No "from" clause -- no origin restriction (any zone to graveyard)
        None
    };

    let valid_card = match possessive.clone() {
        Some(ctrl) => Some(add_controller(subject.clone(), ctrl)),
        None => Some(subject.clone()),
    };
    let valid_target =
        possessive.map(|ctrl| TargetFilter::Typed(TypedFilter::default().controller(ctrl)));

    let mut def = make_base();
    def.mode = TriggerMode::ChangesZone;
    def.destination = Some(Zone::Graveyard);
    def.origin = origin;
    def.valid_card = valid_card;
    def.valid_target = valid_target;
    Some((TriggerMode::ChangesZone, def))
}

/// CR 109.5: Parse the graveyard possessive in "put into [possessive] graveyard".
/// Returns the controller scope of the graveyard's owner, or `None` for unowned
/// ("a graveyard"). Shared by `try_parse_put_into_graveyard` and the batched
/// "one or more" variant so the two parse paths stay in lockstep.
fn parse_graveyard_possessive(input: &str) -> OracleResult<'_, Option<ControllerRef>> {
    alt((
        value(None, tag("a graveyard")),
        value(Some(ControllerRef::You), tag("your graveyard")),
        value(
            Some(ControllerRef::Opponent),
            tag("an opponent's graveyard"),
        ),
        // CR 109.5: "their graveyard" is an anaphor; in every "put into [...]
        // graveyard" pattern that uses it the antecedent is the opponent named
        // earlier in the same clause (Undead Alchemist class).
        value(Some(ControllerRef::Opponent), tag("their graveyard")),
        // CR 109.5: "a player's graveyard" — Bloodchief Ascension class. Bare
        // "a player" leaves controller unscoped (matches any).
        value(None, tag("a player's graveyard")),
    ))
    .parse(input)
}

/// CR 603.6c + CR 109.5: Parse the origin zone for a put-into-graveyard trigger.
/// Returns `Some(Zone)` for a constrained origin, or `None` for "anywhere".
/// Shared by `try_parse_put_into_graveyard` and the batched variant.
fn parse_graveyard_origin_zone(input: &str) -> OracleResult<'_, Option<Zone>> {
    alt((
        value(Some(Zone::Battlefield), tag("the battlefield")),
        // CR 603.6c: "from anywhere" — no origin restriction; the trigger
        // is explicitly NOT treated as a leaves-the-battlefield ability.
        value(None, tag("anywhere")),
        // Library origins: every possessive form maps to Zone::Library.
        // CR 109.5: "their library" is an anaphor back to the graveyard's owner;
        // "a player's library" / "any library" leave the owner unconstrained
        // (the valid_card filter from the graveyard possessive already handles
        // owner narrowing when present).
        value(Some(Zone::Library), tag("your library")),
        value(Some(Zone::Library), tag("their library")),
        value(Some(Zone::Library), tag("an opponent's library")),
        value(Some(Zone::Library), tag("a player's library")),
        value(Some(Zone::Library), tag("any library")),
        value(Some(Zone::Hand), tag("your hand")),
    ))
    .parse(input)
}

/// Parse "[subject] is/are put into [possessive] hand from [zone]" — dredge-style
/// zone-change triggers that fire when a card moves from graveyard (or library) to
/// its owner's hand. Mirrors `try_parse_put_into_graveyard` with hand as the
/// destination. Example: Golgari Brownscale — "When this card is put into your
/// hand from your graveyard, you gain 2 life."
///
/// CR 400.3 + CR 603.10: The trigger event is a zone change ending in hand; the
/// ability fires from the origin zone context (graveyard), so `trigger_zones`
/// includes Graveyard + Battlefield + Exile.
fn try_parse_put_into_hand_from(
    subject: &TargetFilter,
    rest: &str,
) -> Option<(TriggerMode, TriggerDefinition)> {
    let (after_verb, ()) = alt((
        value((), tag::<_, _, OracleError<'_>>("is put into ")),
        value((), tag("are put into ")),
    ))
    .parse(rest)
    .ok()?;

    fn parse_hand_possessive(input: &str) -> OracleResult<'_, Option<TargetFilter>> {
        alt((
            value(
                Some(TargetFilter::Typed(
                    TypedFilter::default().controller(ControllerRef::You),
                )),
                tag("your hand"),
            ),
            value(
                Some(TargetFilter::Typed(
                    TypedFilter::default().controller(ControllerRef::Opponent),
                )),
                tag("an opponent's hand"),
            ),
            value(None, tag("a hand")),
        ))
        .parse(input)
    }
    let (after_hand, valid_target) = parse_hand_possessive.parse(after_verb).ok()?;

    let after_hand = after_hand.trim_start();
    let origin = if let Ok((after_from, ())) =
        value((), tag::<_, _, OracleError<'_>>("from ")).parse(after_hand)
    {
        let after_from = after_from.trim_start();
        fn parse_origin_zone(input: &str) -> OracleResult<'_, Option<Zone>> {
            alt((
                value(Some(Zone::Graveyard), tag("your graveyard")),
                value(Some(Zone::Library), tag("your library")),
                value(Some(Zone::Battlefield), tag("the battlefield")),
                value(None, tag("anywhere")),
            ))
            .parse(input)
        }
        parse_origin_zone
            .parse(after_from)
            .ok()
            .map(|(_, z)| z)
            .unwrap_or(None)
    } else {
        None
    };

    let mut def = make_base();
    def.mode = TriggerMode::ChangesZone;
    def.destination = Some(Zone::Hand);
    def.origin = origin;
    def.valid_card = Some(subject.clone());
    def.valid_target = valid_target;
    // The trigger source is in graveyard (or library) at resolution time, so the
    // ability must be able to fire from beyond the battlefield. Matches the
    // self-referential LTB pattern above.
    if filter_references_self(subject) {
        def.trigger_zones = vec![Zone::Battlefield, Zone::Graveyard, Zone::Exile];
    }
    Some((TriggerMode::ChangesZone, def))
}

/// Parse "[subject] is/are put into exile [from <zone>]" — explicit zone-change
/// form of the exile trigger. Mirror of `try_parse_put_into_graveyard` with exile
/// as the destination. Example: God-Eternal Oketra — "When ~ is put into exile
/// from the battlefield, you may put it into its owner's library third from the
/// top." For self-referential triggers, `trigger_zones` extends to Exile so the
/// ability can fire while the source is in exile.
fn try_parse_put_into_exile_from(
    subject: &TargetFilter,
    rest: &str,
) -> Option<(TriggerMode, TriggerDefinition)> {
    let (after_verb, ()) = alt((
        value((), tag::<_, _, OracleError<'_>>("is put into exile")),
        value((), tag("are put into exile")),
    ))
    .parse(rest)
    .ok()?;

    let after_verb = after_verb.trim_start();
    let origin = if let Ok((after_from, ())) =
        value((), tag::<_, _, OracleError<'_>>("from ")).parse(after_verb)
    {
        let after_from = after_from.trim_start();
        fn parse_origin_zone(input: &str) -> OracleResult<'_, Option<Zone>> {
            alt((
                value(Some(Zone::Battlefield), tag("the battlefield")),
                value(None, tag("anywhere")),
                value(Some(Zone::Library), tag("your library")),
                value(Some(Zone::Hand), tag("your hand")),
                value(Some(Zone::Graveyard), tag("your graveyard")),
            ))
            .parse(input)
        }
        parse_origin_zone
            .parse(after_from)
            .ok()
            .map(|(_, z)| z)
            .unwrap_or(None)
    } else if after_verb.is_empty() {
        None
    } else {
        // Unknown trailing text — bail rather than silently truncate.
        return None;
    };

    let mut def = make_base();
    def.mode = TriggerMode::ChangesZone;
    def.destination = Some(Zone::Exile);
    def.origin = origin;
    def.valid_card = Some(subject.clone());
    if filter_references_self(subject) {
        def.trigger_zones = vec![Zone::Battlefield, Zone::Graveyard, Zone::Exile];
    }
    Some((TriggerMode::ChangesZone, def))
}

/// Parse "whenever one or more [type] cards are put into [your] graveyard from [your library]".
/// CR 603.2c: "One or more" triggers fire once per batch of simultaneous events.
fn try_parse_one_or_more_put_into_graveyard(
    lower: &str,
) -> Option<(TriggerMode, TriggerDefinition)> {
    for prefix in ["whenever one or more ", "when one or more "] {
        let Ok((rest, ())) = value((), tag::<_, _, OracleError<'_>>(prefix)).parse(lower) else {
            continue;
        };

        // Find "are put into" / "is put into" to split subject from destination.
        // Uses split_once_on with each separator variant.
        let (subject_text, after_put) = if let Ok((_, (subj, aft))) =
            nom_primitives::split_once_on(rest, " are put into ")
        {
            (subj, aft)
        } else if let Ok((_, (subj, aft))) = nom_primitives::split_once_on(rest, " is put into ") {
            (subj, aft)
        } else {
            return None;
        };

        // CR 109.5 + CR 603.6c: Shared graveyard possessive + origin combinators
        // (see try_parse_put_into_graveyard) keep singular/batched paths in
        // lockstep — every new possessive or origin form lives in one place.
        let Ok((after_gy, possessive)) = parse_graveyard_possessive.parse(after_put) else {
            continue;
        };

        // Parse optional "from [zone]" clause using nom
        let after_gy = after_gy.trim_start();
        let origin = if let Ok((after_from, ())) =
            value((), tag::<_, _, OracleError<'_>>("from ")).parse(after_gy)
        {
            let after_from = after_from.trim_start();
            parse_graveyard_origin_zone
                .parse(after_from)
                .ok()
                .map(|(_, z)| z)
                .unwrap_or(None)
        } else {
            None
        };

        // Parse the subject type filter: "creature cards", "land cards", "cards"
        let base_filter = if subject_text == "cards" {
            None
        } else if let Some(type_text) = subject_text.strip_suffix(" cards") {
            let (f, remainder) = parse_type_phrase(type_text);
            if !remainder.trim().is_empty() {
                continue;
            }
            Some(f)
        } else {
            continue;
        };

        // CR 109.5: Merge the graveyard owner's controller scope into valid_card
        // (the field `match_changes_zone` actually consults). valid_target is
        // preserved for downstream effect targeting / displays.
        let valid_card = match (base_filter, possessive.clone()) {
            (Some(f), Some(ctrl)) => Some(add_controller(f, ctrl)),
            (Some(f), None) => Some(f),
            (None, Some(ctrl)) => {
                Some(TargetFilter::Typed(TypedFilter::default().controller(ctrl)))
            }
            (None, None) => None,
        };
        let valid_target =
            possessive.map(|ctrl| TargetFilter::Typed(TypedFilter::default().controller(ctrl)));

        let mut def = make_base();
        def.mode = TriggerMode::ChangesZoneAll;
        def.destination = Some(Zone::Graveyard);
        def.origin = origin;
        def.valid_card = valid_card;
        def.valid_target = valid_target;
        def.batched = true;
        return Some((TriggerMode::ChangesZoneAll, def));
    }

    None
}

/// Parse "whenever one or more cards are put into [a|your|an opponent's] library
/// [from <zone>]" — batched zone-change triggers with library destination.
/// CR 603.2c + CR 603.10a: "One or more" triggers fire once per batch of
/// simultaneous zone-change events. Example: Wan Shi Tong, All-Knowing —
/// "Whenever one or more cards are put into a library from anywhere, create..."
fn try_parse_one_or_more_put_into_library(lower: &str) -> Option<(TriggerMode, TriggerDefinition)> {
    for prefix in [
        "whenever one or more cards are put into ",
        "when one or more cards are put into ",
    ] {
        let Ok((rest, ())) = value((), tag::<_, _, OracleError<'_>>(prefix)).parse(lower) else {
            continue;
        };

        fn parse_library_possessive(input: &str) -> OracleResult<'_, Option<TargetFilter>> {
            alt((
                value(None, tag("a library")),
                value(
                    Some(TargetFilter::Typed(
                        TypedFilter::default().controller(ControllerRef::You),
                    )),
                    tag("your library"),
                ),
                value(
                    Some(TargetFilter::Typed(
                        TypedFilter::default().controller(ControllerRef::Opponent),
                    )),
                    tag("an opponent's library"),
                ),
            ))
            .parse(input)
        }
        let Ok((after_lib, valid_target)) = parse_library_possessive.parse(rest) else {
            continue;
        };

        let after_lib = after_lib.trim_start();
        let origin = if let Ok((after_from, ())) =
            value((), tag::<_, _, OracleError<'_>>("from ")).parse(after_lib)
        {
            let after_from = after_from.trim_start();
            fn parse_origin_zone(input: &str) -> OracleResult<'_, Option<Zone>> {
                alt((
                    value(None, tag("anywhere")),
                    value(Some(Zone::Battlefield), tag("the battlefield")),
                    value(Some(Zone::Hand), tag("your hand")),
                    value(Some(Zone::Graveyard), tag("your graveyard")),
                ))
                .parse(input)
            }
            parse_origin_zone
                .parse(after_from)
                .ok()
                .map(|(_, z)| z)
                .unwrap_or(None)
        } else if after_lib.is_empty() {
            None
        } else {
            // Unknown trailing text — bail rather than silently truncate.
            continue;
        };

        let mut def = make_base();
        def.mode = TriggerMode::ChangesZoneAll;
        def.destination = Some(Zone::Library);
        def.origin = origin;
        def.valid_target = valid_target;
        def.batched = true;
        return Some((TriggerMode::ChangesZoneAll, def));
    }

    None
}

/// Parse discard trigger patterns with prefix-based matching.
/// Handles: "whenever you discard a card", "whenever an opponent discards a card",
/// "whenever a player discards a card", batched "one or more" variants,
/// and optional type filters ("a creature card", "a nonland card").
fn try_parse_discard_trigger(
    lower: &str,
    make_base: &dyn Fn() -> TriggerDefinition,
) -> Option<(TriggerMode, TriggerDefinition)> {
    // Strip "whenever " / "when " prefix to get the event clause
    let (event, ()) = alt((
        value((), tag::<_, _, OracleError<'_>>("whenever ")),
        value((), tag("when ")),
    ))
    .parse(lower)
    .ok()?;

    // CR 603.2c: Batched discard triggers — "one or more" fire once per batch.
    if tag::<_, _, OracleError<'_>>("you discard one or more")
        .parse(event)
        .is_ok()
    {
        let mut def = make_base();
        def.mode = TriggerMode::DiscardedAll;
        def.valid_target = Some(TargetFilter::Controller);
        def.batched = true;
        return Some((TriggerMode::DiscardedAll, def));
    }
    if tag::<_, _, OracleError<'_>>("one or more players discard one or more")
        .parse(event)
        .is_ok()
    {
        let mut def = make_base();
        def.mode = TriggerMode::DiscardedAll;
        def.batched = true;
        return Some((TriggerMode::DiscardedAll, def));
    }

    // Determine subject and find "discards"/"discard" verb using nom alt()
    fn parse_discard_subject(input: &str) -> OracleResult<'_, Option<ControllerRef>> {
        alt((
            value(Some(ControllerRef::You), tag("you discard ")),
            value(Some(ControllerRef::Opponent), tag("an opponent discards ")),
            value(None, tag("a player discards ")),
            value(None, tag("each player discards ")),
        ))
        .parse(input)
    }
    let (after_verb, controller_ref) = parse_discard_subject.parse(event).ok()?;

    let mut def = make_base();
    def.mode = TriggerMode::Discarded;

    // CR 701.9a + CR 603.2c: parse the type qualifier on the discarded card
    // ("a land card", "a creature card", "a nonland card") so the trigger only
    // fires when the matching card type is discarded. Reuses `parse_type_phrase`
    // (the same building block `parse_discard_card_filter` uses for cost-form
    // discards in `oracle_effect/imperative.rs`). The actor-derived
    // `controller_ref` is preserved on the resulting filter.
    let parsed_typed = {
        let (filter, _rest) = parse_type_phrase(after_verb);
        match filter {
            TargetFilter::Typed(tf) => Some(tf),
            _ => None,
        }
    };
    let type_filter = match parsed_typed {
        Some(tf)
            if tf
                .type_filters
                .iter()
                .any(|t| !matches!(t, TypeFilter::Card | TypeFilter::Any))
                || !tf.properties.is_empty() =>
        {
            match controller_ref {
                Some(cr) => tf.controller(cr),
                None => tf,
            }
        }
        _ => match controller_ref {
            Some(cr) => TypedFilter::new(TypeFilter::Card).controller(cr),
            None => TypedFilter::new(TypeFilter::Card),
        },
    };
    def.valid_card = Some(TargetFilter::Typed(type_filter));

    Some((TriggerMode::Discarded, def))
}

/// CR 603 + CR 701.21: Parse player-actor sacrifice trigger patterns.
/// Handles "whenever you sacrifice ...", "whenever an opponent sacrifices ...",
/// "whenever a player sacrifices ...", "whenever each player sacrifices ..."
/// with any subject filter produced by `parse_trigger_subject`
/// (covers "a permanent", "another permanent", "a creature", "a land you control", etc.).
///
/// The actor dispatch sets the `ControllerRef` on the resulting filter:
///   - `Some(You)` → only the trigger controller's sacrifices fire it.
///   - `Some(Opponent)` → only an opponent's sacrifices fire it.
///   - `None` → any player's sacrifice matching the filter fires it.
///
/// "Another" self-exclusion (e.g., Mazirek's "another permanent") is carried by
/// `FilterProp::Another` from `parse_trigger_subject`; the runtime matcher enforces
/// it via `FilterProp::Another` → `object_id != source.id` in `filter.rs`.
fn try_parse_sacrifice_trigger(
    lower: &str,
    make_base: &dyn Fn() -> TriggerDefinition,
) -> Option<(TriggerMode, TriggerDefinition)> {
    // Strip "whenever " / "when " prefix.
    let (event, ()) = alt((
        value((), tag::<_, _, OracleError<'_>>("whenever ")),
        value((), tag("when ")),
    ))
    .parse(lower)
    .ok()?;

    // Actor dispatch. `None` means "any player" (no controller constraint on filter).
    fn parse_sacrifice_actor(input: &str) -> OracleResult<'_, Option<ControllerRef>> {
        alt((
            value(Some(ControllerRef::You), tag("you sacrifice ")),
            value(
                Some(ControllerRef::Opponent),
                tag("an opponent sacrifices "),
            ),
            value(None, tag("a player sacrifices ")),
            value(None, tag("each player sacrifices ")),
        ))
        .parse(input)
    }
    let (after_verb, actor) = parse_sacrifice_actor.parse(event).ok()?;

    let (filter, remainder) = parse_trigger_subject(after_verb, &mut ParseContext::default());

    // CR 603.2 + CR 603.7: Optional trailing turn constraint — "during your
    // turn", "during an opponent's turn", etc. Szarel, Genesis Shepherd and
    // similar cards append this to a sacrifice trigger; the constraint
    // narrows when the trigger fires without changing its event structure.
    // Strip the "during " conjunction with nom, then delegate to
    // `parse_turn_constraint` which recognizes the turn-possessive phrases.
    let turn_constraint = tag::<_, _, OracleError<'_>>("during ")
        .parse(remainder.trim())
        .ok()
        .and_then(|(body, _)| parse_turn_constraint(body));

    if turn_constraint.is_none() && !remainder.trim().is_empty() {
        return None;
    }

    let mut def = make_base();
    def.mode = TriggerMode::Sacrificed;
    def.valid_card = Some(match actor {
        Some(cr) => add_controller(filter, cr),
        None => filter,
    });
    if let Some(constraint) = turn_constraint {
        def.constraint = Some(constraint);
    }
    Some((TriggerMode::Sacrificed, def))
}

// ---------------------------------------------------------------------------
// Phase trigger combinators
// ---------------------------------------------------------------------------

/// Nom combinator: parse a phase keyword from the current position.
/// More specific phases (postcombat main, draw step) are tried before generic ones
/// (combat, upkeep) to avoid prefix matches.
fn parse_phase_keyword(input: &str) -> nom::IResult<&str, Phase, OracleError<'_>> {
    alt((
        // CR 505.1: Main phases — specific variants before generic
        value(
            Phase::PostCombatMain,
            alt((tag("postcombat main phase"), tag("second main phase"))),
        ),
        value(
            Phase::PreCombatMain,
            alt((tag("precombat main phase"), tag("first main phase"))),
        ),
        // CR 513.1: End step triggers fire at the beginning of the end step.
        value(Phase::End, tag("end step")),
        value(Phase::Draw, tag("draw step")),
        value(Phase::Upkeep, tag("upkeep")),
        // Generic "combat" — must be last to avoid matching "postcombat"
        value(Phase::BeginCombat, tag("combat")),
    ))
    .parse(input)
}

/// Scan phase_text for a phase keyword at each word boundary using nom combinators.
fn scan_for_phase(text: &str) -> Option<Phase> {
    super::oracle_nom::primitives::scan_at_word_boundaries(text, parse_phase_keyword)
}

/// CR 503.1a / CR 507.1: Parse turn constraint from phase text using nom prefix dispatch.
///
/// Tries opponent possessives first (more specific) before bare "your" to avoid
/// the substring ambiguity where "your opponent's" would match "your".
/// Also checks for trailing "on your turn" suffix.
fn parse_turn_constraint(phase_text: &str) -> Option<TriggerConstraint> {
    // Prefix-based: try at the start of the text
    if alt((
        tag::<_, _, OracleError<'_>>("each opponent's "),
        tag("each opponents\u{2019} "),
        tag("each opponents' "),
        tag("your opponent's "),
        tag("your opponents\u{2019} "),
        tag("your opponents' "),
    ))
    .parse(phase_text)
    .is_ok()
    {
        return Some(TriggerConstraint::OnlyDuringOpponentsTurn);
    }
    if tag::<_, _, OracleError<'_>>("your ")
        .parse(phase_text)
        .is_ok()
    {
        return Some(TriggerConstraint::OnlyDuringYourTurn);
    }
    // Suffix-based: "combat on your turn", "each combat on your turn"
    let mut remaining = phase_text;
    while !remaining.is_empty() {
        if tag::<_, _, OracleError<'_>>("on your turn")
            .parse(remaining)
            .is_ok()
        {
            return Some(TriggerConstraint::OnlyDuringYourTurn);
        }
        remaining = remaining
            .find(' ')
            .map_or("", |i| remaining[i + 1..].trim_start());
    }
    None
}

/// CR 603.8: Parse the filter from "you control no [filter]" state trigger conditions.
/// Handles subtypes (Islands, Swamps, Forests), types (artifacts, creatures, lands),
/// "other" prefix (other creatures, other artifacts), and adjective-type combos (snow lands).
fn parse_control_none_filter(text: &str) -> Option<TargetFilter> {
    let text = text.trim().trim_end_matches('.');

    // Check for "other" prefix → FilterProp::Another
    let (has_other, remainder) =
        if let Ok((rest, ())) = value((), tag::<_, _, OracleError<'_>>("other ")).parse(text) {
            (true, rest)
        } else {
            (false, text)
        };

    // Try parsing as a type phrase first (handles "creatures", "artifacts", "lands", etc.)
    let (filter, rest) = parse_type_phrase(remainder);
    if !rest.trim().is_empty() {
        return None;
    }

    match filter {
        TargetFilter::Typed(mut tf) => {
            tf.controller = Some(ControllerRef::You);
            if has_other {
                tf.properties.push(FilterProp::Another);
            }
            Some(TargetFilter::Typed(tf))
        }
        TargetFilter::Or { filters } => {
            // Distribute controller to all branches
            let filters = filters
                .into_iter()
                .map(|f| {
                    if let TargetFilter::Typed(mut tf) = f {
                        tf.controller = Some(ControllerRef::You);
                        if has_other {
                            tf.properties.push(FilterProp::Another);
                        }
                        TargetFilter::Typed(tf)
                    } else {
                        f
                    }
                })
                .collect();
            Some(TargetFilter::Or { filters })
        }
        _ => None,
    }
}

/// CR 702.xxx: Prepare (Strixhaven) — ETB-rider combinator for the
/// `"<self> enters prepared"` shorthand. Structurally analogous to other
/// enters-rider shorthand (`enters tapped`, `enters transformed`), except
/// prepared is a triggered-ability shorthand rather than a replacement effect:
/// it synthesizes a self-ETB trigger whose effect is
/// `BecomePrepared { target: SelfRef }`.
///
/// Accepts three self-subject forms: `"~ enters prepared"`,
/// `"this creature enters prepared"`, and `"it enters prepared"` — composed
/// as a nom `alt` over the subject prefix, followed by the shared
/// `" enters prepared"` tail and an optional trailing period. Returns
/// `Some(def)` only when the line is exactly this shorthand, so normal
/// trigger parsing handles `"When ~ enters, ..."` forms unchanged. Assign
/// when WotC publishes SOS CR update.
pub fn try_parse_enters_prepared_rider(line: &str) -> Option<TriggerDefinition> {
    use crate::types::ability::{AbilityDefinition, Effect};
    use nom::combinator::{eof, opt};
    use nom::sequence::{preceded, terminated};

    let lower = line.to_lowercase();
    // Compose from nom primitives: subject-prefix alt + shared suffix + eof.
    let parser_fn = |input| -> OracleResult<'_, ()> {
        value(
            (),
            terminated(
                preceded(
                    alt((
                        tag::<_, _, OracleError<'_>>("~"),
                        tag("this creature"),
                        tag("it"),
                    )),
                    (tag(" enters prepared"), opt(tag("."))),
                ),
                eof,
            ),
        )
        .parse(input)
    };
    parser_fn(lower.trim()).ok()?;

    let effect_def = AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::BecomePrepared {
            target: TargetFilter::SelfRef,
        },
    );
    let trigger = TriggerDefinition::new(TriggerMode::ChangesZone)
        .destination(Zone::Battlefield)
        .valid_card(TargetFilter::SelfRef)
        .trigger_zones(vec![Zone::Battlefield])
        .execute(effect_def)
        .description(line.to_string());
    Some(trigger)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::oracle_ir::context::ParseContext;
    use crate::parser::oracle_ir::diagnostic::OracleDiagnostic;
    use crate::types::ability::{
        AbilityCondition, AbilityCost, AbilityKind, AggregateFunction, CastingPermission,
        Comparator, ContinuousModification, ControllerRef, CountScope, DamageModification,
        DelayedTriggerCondition, Duration, Effect, FilterProp, ManaSpendPermission, ObjectScope,
        PlayerFilter, PlayerScope, PtValue, QuantityExpr, QuantityRef, TargetFilter, TypeFilter,
        TypedFilter,
    };
    use crate::types::counter::{CounterMatch, CounterType};
    use crate::types::replacements::ReplacementEvent;
    use crate::types::statics::CastFrequency;

    fn blocking_source_beyond_first_expr() -> QuantityExpr {
        QuantityExpr::Offset {
            inner: Box::new(QuantityExpr::Ref {
                qty: QuantityRef::ObjectCount {
                    filter: TargetFilter::Typed(TypedFilter {
                        type_filters: vec![TypeFilter::Creature],
                        controller: None,
                        properties: vec![FilterProp::BlockingSource],
                    }),
                },
            }),
            offset: -1,
        }
    }

    #[test]
    fn static_condition_to_trigger_condition_source_in_battlefield() {
        // SUB-FIX B regression: the existing SourceInZone mapper must pass
        // Zone::Battlefield through unchanged so the new battlefield condition
        // arm hoists to a trigger-level intervening-if (CR 603.4).
        use crate::types::zones::Zone;
        let mapped = static_condition_to_trigger_condition(&StaticCondition::SourceInZone {
            zone: Zone::Battlefield,
        });
        assert_eq!(
            mapped,
            Some(TriggerCondition::SourceInZone {
                zone: Zone::Battlefield,
            }),
        );
    }

    #[test]
    fn trigger_etb_self() {
        let def = parse_trigger_line(
            "When this creature enters, it deals 1 damage to each opponent.",
            "Goblin Chainwhirler",
        );
        assert_eq!(def.mode, TriggerMode::ChangesZone);
        assert_eq!(def.destination, Some(Zone::Battlefield));
        assert_eq!(def.valid_card, Some(TargetFilter::SelfRef));
        assert!(def.execute.is_some());
    }

    // SHAPE TEST — issue #411: Syr Konrad's three-way disjunctive zone-change
    // trigger. Asserts the parsed `TriggerDefinition` SHAPE; it does not drive
    // the runtime apply pipeline (see `triggers.rs` for the runtime test).
    #[test]
    fn parse_syr_konrad_disjunctive_zone_change() {
        let def = parse_trigger_line(
            "Whenever another creature dies, or a creature card is put into a graveyard \
             from anywhere other than the battlefield, or a creature card leaves your \
             graveyard, Syr Konrad, the Grim deals 1 damage to each opponent.",
            "Syr Konrad, the Grim",
        );
        assert_eq!(def.mode, TriggerMode::ChangesZone);
        assert_eq!(
            def.zone_change_clauses.len(),
            3,
            "expected 3 disjunctive clauses, got {:?}",
            def.zone_change_clauses
        );

        // Clause 1: another creature dies (battlefield -> graveyard).
        let c1 = &def.zone_change_clauses[0];
        assert_eq!(c1.origin, OriginConstraint::Equals(Zone::Battlefield));
        assert_eq!(c1.destination, Some(Zone::Graveyard));
        let TargetFilter::Typed(tf1) = c1.valid_card.as_ref().expect("clause 1 valid_card") else {
            panic!("clause 1 valid_card not Typed: {:?}", c1.valid_card);
        };
        assert!(tf1.type_filters.contains(&TypeFilter::Creature));
        assert!(tf1.properties.contains(&FilterProp::Another));

        // Clause 2: creature card put into graveyard from anywhere but battlefield.
        let c2 = &def.zone_change_clauses[1];
        assert_eq!(c2.origin, OriginConstraint::NotEquals(Zone::Battlefield));
        assert_eq!(c2.destination, Some(Zone::Graveyard));
        assert!(c2.valid_card.is_some());

        // Clause 3: creature card leaves your graveyard (any destination).
        let c3 = &def.zone_change_clauses[2];
        assert_eq!(c3.origin, OriginConstraint::Equals(Zone::Graveyard));
        assert_eq!(c3.destination, None);
        let TargetFilter::Typed(tf3) = c3.valid_card.as_ref().expect("clause 3 valid_card") else {
            panic!("clause 3 valid_card not Typed: {:?}", c3.valid_card);
        };
        assert_eq!(tf3.controller, Some(ControllerRef::You));

        // The effect must be DamageEachPlayer — no Unimplemented leakage.
        let execute = def.execute.as_ref().expect("execute ability");
        fn has_damage_each_player(ability: &AbilityDefinition) -> bool {
            matches!(*ability.effect, Effect::DamageEachPlayer { .. })
                || ability
                    .sub_ability
                    .as_ref()
                    .is_some_and(|s| has_damage_each_player(s))
        }
        fn has_unimplemented(ability: &AbilityDefinition) -> bool {
            matches!(*ability.effect, Effect::Unimplemented { .. })
                || ability
                    .sub_ability
                    .as_ref()
                    .is_some_and(|s| has_unimplemented(s))
        }
        assert!(
            has_damage_each_player(execute),
            "expected DamageEachPlayer effect, got {:?}",
            execute.effect
        );
        assert!(
            !has_unimplemented(execute),
            "effect chain leaked Unimplemented: {:?}",
            execute
        );
    }

    // CR 603.6a + CR 110.5b: "When this land enters untapped, ..." — Gingerbread
    // Cabin class. The trigger must carry `Not { Box::new(ZoneChangeObjectIsTapped) }`
    // so it only fires when the ETB-tapped replacement did NOT apply. For a
    // SelfRef trigger the entering object IS the source, so the evaluator's
    // `source_id` fallback resolves to the same permanent.
    #[test]
    fn trigger_etb_self_enters_untapped_attaches_condition() {
        let def = parse_trigger_line(
            "When this land enters untapped, create a Food token.",
            "Gingerbread Cabin",
        );
        assert_eq!(def.mode, TriggerMode::ChangesZone);
        assert_eq!(def.destination, Some(Zone::Battlefield));
        assert_eq!(def.valid_card, Some(TargetFilter::SelfRef));
        assert_eq!(
            def.condition,
            Some(TriggerCondition::Not {
                condition: Box::new(TriggerCondition::ZoneChangeObjectIsTapped)
            })
        );
        assert!(def.execute.is_some());
    }

    #[test]
    fn trigger_intervening_if_spell_from_hand_this_turn_attaches_condition() {
        let def = parse_trigger_line(
            "At the beginning of your end step, if you haven't cast a spell from your hand this turn, draw a card.",
            "Jem Lightfoote, Sky Explorer",
        );
        match def.condition {
            Some(TriggerCondition::QuantityComparison {
                lhs:
                    QuantityExpr::Ref {
                        qty:
                            QuantityRef::SpellsCastThisTurn {
                                scope: CountScope::Controller,
                                filter: Some(TargetFilter::Typed(TypedFilter { properties, .. })),
                            },
                    },
                comparator: Comparator::EQ,
                rhs: QuantityExpr::Fixed { value: 0 },
            }) => assert!(properties.contains(&FilterProp::InZone { zone: Zone::Hand })),
            Some(TriggerCondition::And { conditions }) => {
                assert!(conditions.iter().any(|condition| matches!(
                    condition,
                    TriggerCondition::QuantityComparison {
                        lhs: QuantityExpr::Ref {
                            qty: QuantityRef::SpellsCastThisTurn {
                                scope: CountScope::Controller,
                                filter: Some(TargetFilter::Typed(TypedFilter { properties, .. })),
                            },
                        },
                        comparator: Comparator::EQ,
                        rhs: QuantityExpr::Fixed { value: 0 },
                    } if properties.contains(&FilterProp::InZone { zone: Zone::Hand })
                )))
            }
            other => panic!("expected cast-origin intervening condition, got {other:?}"),
        }
    }

    #[test]
    fn trigger_intervening_if_source_didnt_attack_this_turn_attaches_condition() {
        let def = parse_trigger_line(
            "At the beginning of your end step, if this creature didn't attack this turn, put a +1/+1 counter on it.",
            "Air Nomad Student",
        );
        assert!(matches!(
            def.condition,
            Some(TriggerCondition::QuantityComparison {
                lhs: QuantityExpr::Ref {
                    qty: QuantityRef::ObjectCount {
                        filter: TargetFilter::And { .. },
                    },
                },
                comparator: Comparator::EQ,
                rhs: QuantityExpr::Fixed { value: 0 },
            })
        ));
    }

    // CR 603.6a + CR 110.5b: "Whenever a permanent you control enters tapped, ..." —
    // Amulet of Vigor class. The `enters tapped` rider must set
    // `ZoneChangeObjectIsTapped` (fires only when the *entering* permanent is
    // tapped — NOT the ability source). For observer triggers the entering
    // permanent differs from the ability source, so the subject-correct variant
    // is required.
    #[test]
    fn trigger_etb_subject_enters_tapped_attaches_condition() {
        let def = parse_trigger_line(
            "Whenever a permanent you control enters tapped, untap it.",
            "Amulet of Vigor",
        );
        assert_eq!(def.mode, TriggerMode::ChangesZone);
        assert_eq!(def.destination, Some(Zone::Battlefield));
        assert_eq!(
            def.condition,
            Some(TriggerCondition::ZoneChangeObjectIsTapped)
        );
    }

    // CR 603.6a + CR 110.5b: "Whenever an artifact or creature an opponent
    // controls enters untapped, ..." — Charismatic Conqueror class. The
    // `enters untapped` rider must wrap `ZoneChangeObjectIsTapped` in `Not`,
    // and the subject is the entering (opponent's) permanent — an observer
    // trigger where source ≠ entering object.
    #[test]
    fn trigger_etb_subject_enters_untapped_attaches_negated_condition() {
        let def = parse_trigger_line(
            "Whenever an artifact or creature an opponent controls enters untapped, they may tap that permanent. If they don't, you create a 1/1 white Vampire creature token with lifelink.",
            "Charismatic Conqueror",
        );
        assert_eq!(def.mode, TriggerMode::ChangesZone);
        assert_eq!(def.destination, Some(Zone::Battlefield));
        assert_eq!(
            def.condition,
            Some(TriggerCondition::Not {
                condition: Box::new(TriggerCondition::ZoneChangeObjectIsTapped)
            })
        );
    }

    // Guard: a bare "enters" (no tapped-state rider) must NOT attach a
    // tapped-state condition (neither `SourceIsTapped` nor
    // `ZoneChangeObjectIsTapped`).
    #[test]
    fn trigger_etb_bare_enters_has_no_tapped_condition() {
        let def = parse_trigger_line(
            "When this creature enters, draw a card.",
            "Elvish Visionary",
        );
        assert_eq!(def.mode, TriggerMode::ChangesZone);
        let is_tapped_condition = |c: &TriggerCondition| {
            matches!(
                c,
                TriggerCondition::SourceIsTapped | TriggerCondition::ZoneChangeObjectIsTapped
            )
        };
        assert!(
            !def.condition.as_ref().is_some_and(is_tapped_condition)
                && !matches!(
                    &def.condition,
                    Some(TriggerCondition::Not { condition })
                        if is_tapped_condition(condition)
                ),
            "bare `enters` must not attach a tapped-state condition; got {:?}",
            def.condition
        );
    }

    // Guard: Genesis Chamber's "if ~ is untapped" intervening-if goes through
    // `static_condition_to_trigger_condition` — a DIFFERENT parser path from
    // `parse_enters_tapped_state_rider` — and must keep emitting the
    // source-bound `SourceIsTapped` (the subject is the ability's own source,
    // not a zone-change event object). This pins the two paths as distinct so
    // the ETB-rider fix does not leak into source-bound intervening-ifs.
    #[test]
    fn trigger_intervening_if_source_untapped_keeps_source_is_tapped() {
        let def = parse_trigger_line(
            "Whenever a nontoken creature enters, if this artifact is untapped, that creature's controller creates a 1/1 colorless Myr artifact creature token.",
            "Genesis Chamber",
        );
        assert_eq!(def.mode, TriggerMode::ChangesZone);
        assert_eq!(
            def.condition,
            Some(TriggerCondition::Not {
                condition: Box::new(TriggerCondition::SourceIsTapped)
            })
        );
    }

    #[test]
    fn trigger_first_combat_phase_condition() {
        let def = parse_trigger_line(
            "Whenever equipped creature attacks, if it's the first combat phase of the turn, untap it. After this phase, there is an additional combat phase.",
            "Genji Glove",
        );
        assert_eq!(
            def.condition,
            Some(TriggerCondition::FirstCombatPhaseOfTurn)
        );
    }

    #[test]
    fn trigger_first_combat_phase_followup_condition() {
        let def = parse_trigger_line(
            "Whenever a Samurai or Warrior you control attacks alone, untap it. If it's the first combat phase of the turn, there is an additional combat phase after this phase.",
            "A-Raiyuu, Storm's Edge",
        );
        assert!(def.condition.is_none());
        let followup = def
            .execute
            .as_ref()
            .and_then(|ability| ability.sub_ability.as_deref())
            .expect("additional combat follow-up");
        assert_eq!(
            followup.condition,
            Some(AbilityCondition::FirstCombatPhaseOfTurn)
        );
    }

    // Word-boundary guard: "enters untapped creatures" (hypothetical) must not
    // accidentally match — the combinator requires a terminator after the
    // state word.
    #[test]
    fn trigger_etb_untapped_rider_requires_word_boundary() {
        assert!(parse_enters_tapped_state_rider("s untappedness").is_none());
        assert!(parse_enters_tapped_state_rider("s tappedly").is_none());
        // Valid terminators:
        assert_eq!(
            parse_enters_tapped_state_rider("s untapped"),
            Some(TriggerCondition::Not {
                condition: Box::new(TriggerCondition::ZoneChangeObjectIsTapped)
            })
        );
        assert_eq!(
            parse_enters_tapped_state_rider("s untapped "),
            Some(TriggerCondition::Not {
                condition: Box::new(TriggerCondition::ZoneChangeObjectIsTapped)
            })
        );
        assert_eq!(
            parse_enters_tapped_state_rider("s tapped,"),
            Some(TriggerCondition::ZoneChangeObjectIsTapped)
        );
    }

    // B1: ETB-rider combinator for "~ enters prepared.". Must synthesize the
    // same TriggerDefinition the old verbatim-match block produced; must NOT
    // match when extra trailing content is present (so normal trigger parsing
    // still handles "When ~ enters, ...").
    #[test]
    fn enters_prepared_rider_builds_self_etb_trigger() {
        let def =
            try_parse_enters_prepared_rider("~ enters prepared.").expect("rider should match");
        assert_eq!(def.mode, TriggerMode::ChangesZone);
        assert_eq!(def.destination, Some(Zone::Battlefield));
        assert_eq!(def.valid_card, Some(TargetFilter::SelfRef));
        let exec = def.execute.as_deref().expect("execute set");
        assert!(matches!(
            exec.effect.as_ref(),
            Effect::BecomePrepared {
                target: TargetFilter::SelfRef
            }
        ));
    }

    #[test]
    fn enters_prepared_rider_tolerates_missing_period() {
        assert!(try_parse_enters_prepared_rider("~ enters prepared").is_some());
        // Whitespace is trimmed before dispatch.
        assert!(try_parse_enters_prepared_rider("  ~ enters prepared.  ").is_some());
    }

    #[test]
    fn enters_prepared_rider_accepts_all_subject_forms() {
        // Raw Oracle text uses "This creature enters prepared." (Adventurous
        // Eater); the ETB-rider combinator must accept this without relying
        // on `normalize_self_refs` having run first (the dispatch site in
        // `oracle.rs` operates on pre-normalized lines).
        assert!(try_parse_enters_prepared_rider("This creature enters prepared.").is_some());
        assert!(try_parse_enters_prepared_rider("It enters prepared.").is_some());
        assert!(try_parse_enters_prepared_rider("~ enters prepared.").is_some());
    }

    #[test]
    fn enters_prepared_rider_rejects_non_rider_shapes() {
        assert!(try_parse_enters_prepared_rider("when ~ enters, draw a card.").is_none());
        assert!(try_parse_enters_prepared_rider("~ enters tapped.").is_none());
        assert!(try_parse_enters_prepared_rider("~ enters prepared and tapped.").is_none());
    }

    // Dispatch-level regression: the rider combinator only accepts `~`,
    // `this creature`, or `it` as the subject — but Oracle text ships with
    // the card's short name (e.g. "Lluwen enters prepared."). The parser
    // entry must normalize self-refs before dispatching, so the short-name
    // form must synthesize the same ETB trigger as `~ enters prepared.`.
    #[test]
    fn enters_prepared_rider_dispatch_normalizes_short_name_subject() {
        use crate::parser::oracle::parse_oracle_text;

        let parsed = parse_oracle_text(
            "Lluwen enters prepared.",
            "Lluwen, Exchange Student",
            &[],
            &["Creature".to_string()],
            &[],
        );
        assert_eq!(
            parsed.triggers.len(),
            1,
            "one trigger should be synthesized"
        );
        let trigger = &parsed.triggers[0];
        assert_eq!(trigger.mode, TriggerMode::ChangesZone);
        assert_eq!(trigger.destination, Some(Zone::Battlefield));
        assert_eq!(trigger.valid_card, Some(TargetFilter::SelfRef));
        let exec = trigger.execute.as_ref().expect("execute should be set");
        assert!(matches!(
            *exec.effect,
            crate::types::ability::Effect::BecomePrepared {
                target: TargetFilter::SelfRef
            }
        ));
        // Description is set from the normalized line — `parse_oracle_text`
        // pre-normalizes self-refs at the single entry point, so descriptions
        // uniformly use `~` for the card's self-reference (matching the
        // codebase-wide trigger description convention).
        assert_eq!(trigger.description.as_deref(), Some("~ enters prepared."));
    }

    #[test]
    fn trigger_dies() {
        let def = parse_trigger_line(
            "When this creature dies, create a 1/1 white Spirit creature token.",
            "Some Card",
        );
        assert_eq!(def.mode, TriggerMode::ChangesZone);
        assert_eq!(def.origin, Some(Zone::Battlefield));
        assert_eq!(def.destination, Some(Zone::Graveyard));
    }

    #[test]
    fn trigger_combat_damage_to_player() {
        let def = parse_trigger_line(
            "Whenever Eye Collector deals combat damage to a player, each player mills a card.",
            "Eye Collector",
        );
        assert_eq!(def.mode, TriggerMode::DamageDone);
        assert_eq!(def.damage_kind, DamageKindFilter::CombatOnly);
        assert_eq!(def.valid_target, Some(TargetFilter::Player));
    }

    #[test]
    fn that_creature_subject_resolves_to_parent_target() {
        let mut ctx = ParseContext::default();
        let (_, def) = parse_trigger_condition(
            "that creature deals combat damage to a player or planeswalker",
            &mut ctx,
        );
        assert_eq!(def.mode, TriggerMode::DamageDone);
        assert_eq!(def.damage_kind, DamageKindFilter::CombatOnly);
        assert_eq!(def.valid_source, Some(TargetFilter::ParentTarget));
        assert_eq!(
            def.valid_target,
            Some(TargetFilter::Or {
                filters: vec![
                    TargetFilter::Player,
                    TargetFilter::Typed(TypedFilter::new(TypeFilter::Planeswalker)),
                ],
            })
        );
    }

    #[test]
    fn that_permanent_attacks_subject_resolves_to_parent_target() {
        let mut ctx = ParseContext::default();
        let (_, def) = parse_trigger_condition("that permanent attacks", &mut ctx);
        assert_eq!(def.mode, TriggerMode::Attacks);
        assert_eq!(def.valid_card, Some(TargetFilter::ParentTarget));
    }

    #[test]
    fn creature_subject_still_returns_typed_filter() {
        let mut ctx = ParseContext::default();
        let (_, def) =
            parse_trigger_condition("a creature deals combat damage to a player", &mut ctx);
        assert_eq!(def.mode, TriggerMode::DamageDone);
        assert_eq!(
            def.valid_source,
            Some(TargetFilter::Typed(TypedFilter::creature()))
        );
    }

    #[test]
    fn hunters_insight_class_builds_whenever_event_delayed_trigger() {
        use crate::parser::oracle::parse_oracle_text;

        let parsed = parse_oracle_text(
            "Choose target creature you control. Whenever that creature deals combat damage to a player or planeswalker this turn, draw that many cards.",
            "Hunter's Insight",
            &[],
            &["Instant".to_string()],
            &[],
        );
        assert_eq!(parsed.abilities.len(), 1);

        let delayed = parsed.abilities[0]
            .sub_ability
            .as_deref()
            .expect("target choice should chain into delayed trigger");
        let Effect::CreateDelayedTrigger {
            condition, effect, ..
        } = delayed.effect.as_ref()
        else {
            panic!("expected CreateDelayedTrigger, got {:?}", delayed.effect);
        };
        let DelayedTriggerCondition::WheneverEvent { trigger } = condition else {
            panic!("expected WheneverEvent, got {condition:?}");
        };
        assert_eq!(trigger.mode, TriggerMode::DamageDone);
        assert_eq!(trigger.damage_kind, DamageKindFilter::CombatOnly);
        assert_eq!(trigger.valid_source, Some(TargetFilter::ParentTarget));

        let Effect::Draw { count, target } = effect.effect.as_ref() else {
            panic!("expected Draw, got {:?}", effect.effect);
        };
        assert_eq!(
            *count,
            QuantityExpr::Ref {
                qty: QuantityRef::EventContextAmount
            }
        );
        assert_eq!(*target, TargetFilter::Controller);
    }

    // Thrummingbird: pins the trigger event shape AND the parsed execute body
    // (Effect::Proliferate) — siblings above only assert the event shape.
    // CR 603.2 + CR 701.34a.
    #[test]
    fn trigger_thrummingbird_combat_damage_proliferate() {
        let def = parse_trigger_line(
            "Whenever ~ deals combat damage to a player, proliferate.",
            "Thrummingbird",
        );
        assert_eq!(def.mode, TriggerMode::DamageDone);
        assert_eq!(def.damage_kind, DamageKindFilter::CombatOnly);
        assert_eq!(def.valid_target, Some(TargetFilter::Player));
        let exec = def.execute.as_deref().expect("execute should be set");
        assert!(
            matches!(exec.effect.as_ref(), Effect::Proliferate),
            "execute body should be Effect::Proliferate, got {:?}",
            exec.effect,
        );
    }

    #[test]
    fn trigger_combat_damage_installs_until_next_turn_damage_doubler() {
        let def = parse_trigger_line(
            "Whenever Lightning deals combat damage to a player, until your next turn, if a source would deal damage to that player or a permanent that player controls, it deals double that damage instead.",
            "Lightning, Army of One",
        );
        assert_eq!(def.mode, TriggerMode::DamageDone);
        assert_eq!(def.damage_kind, DamageKindFilter::CombatOnly);
        assert_eq!(def.valid_source, Some(TargetFilter::SelfRef));
        assert_eq!(def.valid_target, Some(TargetFilter::Player));

        let exec = def.execute.as_deref().expect("execute should be set");
        assert_eq!(
            exec.duration,
            Some(Duration::UntilNextTurnOf {
                player: PlayerScope::Controller
            })
        );
        let Effect::AddTargetReplacement {
            replacement,
            target,
        } = exec.effect.as_ref()
        else {
            panic!("expected AddTargetReplacement, got {:?}", exec.effect);
        };
        assert_eq!(*target, TargetFilter::TriggeringPlayer);
        assert_eq!(replacement.event, ReplacementEvent::DamageDone);
        assert_eq!(
            replacement.damage_modification,
            Some(DamageModification::Double)
        );
        assert!(replacement.damage_target_filter.is_none());
    }

    #[test]
    fn trigger_combat_damage_to_opponent() {
        let def = parse_trigger_line(
            "Whenever ~ deals combat damage to an opponent, draw a card.",
            "Test Card",
        );
        assert_eq!(def.mode, TriggerMode::DamageDone);
        assert_eq!(def.damage_kind, DamageKindFilter::CombatOnly);
        assert_eq!(
            def.valid_target,
            Some(TargetFilter::Typed(
                TypedFilter::default().controller(ControllerRef::Opponent)
            ))
        );
    }

    #[test]
    fn trigger_subject_warns_on_any_fallback() {
        let mut ctx = ParseContext::default();
        let (filter, rest) = parse_single_subject("xyzzy", &mut ctx);
        assert_eq!(filter, TargetFilter::Any);
        assert_eq!(rest, "xyzzy");
        assert!(ctx.diagnostics.iter().any(
            |d| matches!(d, OracleDiagnostic::TargetFallback { context, text, .. }
                if context == "trigger subject parse fell back to Any" && text == "xyzzy")
        ));
    }

    #[test]
    fn trigger_combat_damage_no_qualifier() {
        // "deals combat damage" with no "to X" — fires for any target
        let def = parse_trigger_line(
            "Whenever ~ deals combat damage, put a +1/+1 counter on ~.",
            "Test Card",
        );
        assert_eq!(def.mode, TriggerMode::DamageDone);
        assert_eq!(def.damage_kind, DamageKindFilter::CombatOnly);
        assert_eq!(def.valid_target, None);
    }

    #[test]
    fn trigger_one_or_more_creatures_you_control_deal_combat_damage_to_player() {
        let def = parse_trigger_line(
            "Whenever one or more creatures you control deal combat damage to a player, create a Treasure token.",
            "Professional Face-Breaker",
        );
        assert_eq!(def.mode, TriggerMode::DamageDoneOnceByController);
        assert_eq!(def.damage_kind, DamageKindFilter::CombatOnly);
        assert_eq!(
            def.valid_source,
            Some(TargetFilter::Typed(
                TypedFilter::creature().controller(ControllerRef::You)
            ))
        );
        assert_eq!(def.valid_target, Some(TargetFilter::Player));
    }

    #[test]
    fn trigger_combat_damage_create_treasure_and_manifest_that_players_library() {
        let def = parse_trigger_line(
            "Whenever one or more creatures you control deal combat damage to a player, create a Treasure token and manifest the top card of that player's library.",
            "Orochi Soul-Reaver",
        );
        assert_eq!(def.mode, TriggerMode::DamageDoneOnceByController);

        let execute = def.execute.as_ref().expect("trigger should have execute");
        assert!(matches!(*execute.effect, Effect::Token { .. }));

        let sub = execute
            .sub_ability
            .as_ref()
            .expect("manifest should be chained after Treasure creation");
        assert!(
            matches!(
                *sub.effect,
                Effect::Manifest {
                    target: TargetFilter::TriggeringPlayer,
                    count: QuantityExpr::Fixed { value: 1 }
                }
            ),
            "expected Manifest {{ TriggeringPlayer, count: 1 }}, got: {:?}",
            sub.effect
        );
    }

    #[test]
    fn trigger_upkeep() {
        let def = parse_trigger_line(
            "At the beginning of your upkeep, look at the top card of your library.",
            "Delver of Secrets",
        );
        assert_eq!(def.mode, TriggerMode::Phase);
        assert_eq!(def.phase, Some(Phase::Upkeep));
    }

    #[test]
    fn trigger_optional_you_may() {
        let def = parse_trigger_line(
            "When this creature enters, you may draw a card.",
            "Some Card",
        );
        assert!(def.optional);
    }

    #[test]
    fn trigger_attacks() {
        let def = parse_trigger_line(
            "Whenever Goblin Guide attacks, defending player reveals the top card of their library.",
            "Goblin Guide",
        );
        assert_eq!(def.mode, TriggerMode::Attacks);
    }

    #[test]
    fn trigger_attacks_and_isnt_blocked_uses_unblocked_mode_and_combat_duration() {
        let def = parse_trigger_line(
            "Whenever Murk Dwellers attacks and isn't blocked, it gets +2/+0 until end of combat.",
            "Murk Dwellers",
        );
        assert_eq!(def.mode, TriggerMode::AttackerUnblocked);
        let execute = def.execute.as_ref().expect("trigger should have effect");
        assert_eq!(execute.duration, Some(Duration::UntilEndOfCombat));
        assert!(
            matches!(
                execute.effect.as_ref(),
                Effect::Pump {
                    power: PtValue::Fixed(2),
                    toughness: PtValue::Fixed(0),
                    ..
                }
            ),
            "expected self pump, got {:?}",
            execute.effect
        );
    }

    #[test]
    fn trigger_attacks_you_or_planeswalker_you_control() {
        let def = parse_trigger_line(
            "Whenever a creature attacks you or a planeswalker you control, that creature's controller loses 1 life.",
            "Marchesa's Decree",
        );
        assert_eq!(def.mode, TriggerMode::Attacks);
        assert_eq!(
            def.attack_target_filter,
            Some(AttackTargetFilter::PlayerOrPlaneswalker)
        );
        assert_eq!(def.valid_target, Some(TargetFilter::Controller));
    }

    #[test]
    fn opponent_attacks_that_player_library_binds_to_triggering_player() {
        let def = parse_trigger_line(
            "Whenever an opponent attacks you and/or one or more planeswalkers you control, exile the top card of that player's library.",
            "Cunning Rhetoric",
        );
        assert_eq!(def.mode, TriggerMode::Attacks);
        assert_eq!(
            def.attack_target_filter,
            Some(AttackTargetFilter::PlayerOrPlaneswalker)
        );
        assert_eq!(def.valid_target, Some(TargetFilter::Controller));
        let execute = def.execute.as_ref().expect("trigger should have effect");
        assert!(
            matches!(
                execute.effect.as_ref(),
                Effect::ExileTop {
                    player: TargetFilter::TriggeringPlayer,
                    count: QuantityExpr::Fixed { value: 1 },
                    face_down: false,
                }
            ),
            "expected ExileTop to bind to TriggeringPlayer, got {:?}",
            execute.effect
        );
    }

    #[test]
    fn trigger_battalion() {
        let def = parse_trigger_line(
            "Whenever Boros Elite and at least two other creatures attack, Boros Elite gets +2/+2 until end of turn.",
            "Boros Elite",
        );
        assert_eq!(def.mode, TriggerMode::Attacks);
        assert!(def.condition.is_some());
        if let Some(TriggerCondition::MinCoAttackers { minimum }) = &def.condition {
            assert_eq!(*minimum, 2);
        } else {
            panic!("Expected MinCoAttackers");
        }
    }

    #[test]
    fn trigger_pack_tactics() {
        let def = parse_trigger_line(
            "Whenever Werewolf Pack Leader attacks, if the total power of creatures you control is 6 or greater, draw a card.",
            "Werewolf Pack Leader",
        );
        // Pack tactics is a different pattern (if-condition), not battalion
        assert_eq!(def.mode, TriggerMode::Attacks);
    }

    #[test]
    fn trigger_exploits_a_creature() {
        let def = parse_trigger_line(
            "When Sidisi's Faithful exploits a creature, return target creature to its owner's hand.",
            "Sidisi's Faithful",
        );
        assert_eq!(def.mode, TriggerMode::Exploited);
    }

    // --- Subject decomposition tests ---

    #[test]
    fn trigger_another_creature_you_control_enters() {
        let def = parse_trigger_line(
            "Whenever another creature you control enters, put a +1/+1 counter on Hinterland Sanctifier.",
            "Hinterland Sanctifier",
        );
        assert_eq!(def.mode, TriggerMode::ChangesZone);
        assert_eq!(def.destination, Some(Zone::Battlefield));
        assert_eq!(
            def.valid_card,
            Some(TargetFilter::Typed(
                TypedFilter::creature()
                    .controller(crate::types::ability::ControllerRef::You)
                    .properties(vec![FilterProp::Another])
            ))
        );
    }

    #[test]
    fn trigger_another_creature_enters_no_controller() {
        let def = parse_trigger_line(
            "Whenever another creature enters, draw a card.",
            "Some Card",
        );
        assert_eq!(def.mode, TriggerMode::ChangesZone);
        assert_eq!(def.destination, Some(Zone::Battlefield));
        match &def.valid_card {
            Some(TargetFilter::Typed(TypedFilter { properties, .. })) => {
                assert!(properties.contains(&FilterProp::Another));
            }
            other => panic!("Expected Typed filter with Another, got {:?}", other),
        }
    }

    #[test]
    fn trigger_a_creature_enters() {
        let def = parse_trigger_line(
            "Whenever a creature enters, you gain 1 life.",
            "Soul Warden",
        );
        assert_eq!(def.mode, TriggerMode::ChangesZone);
        assert_eq!(def.destination, Some(Zone::Battlefield));
        assert_eq!(
            def.valid_card,
            Some(TargetFilter::Typed(TypedFilter::creature()))
        );
    }

    #[test]
    fn trigger_gain_life_and_get_energy_chains_both_effects() {
        let def = parse_trigger_line(
            "Whenever another creature you control enters, you gain 1 life and get {E} (an energy counter).",
            "Guide of Souls",
        );
        assert_eq!(def.mode, TriggerMode::ChangesZone);
        assert_eq!(def.destination, Some(Zone::Battlefield));
        assert_eq!(
            def.valid_card,
            Some(TargetFilter::Typed(
                TypedFilter::creature()
                    .controller(ControllerRef::You)
                    .properties(vec![FilterProp::Another])
            ))
        );

        let execute = def.execute.expect("trigger should have execute ability");
        assert!(matches!(*execute.effect, Effect::GainLife { .. }));
        let sub_ability = execute
            .sub_ability
            .expect("energy gain should be chained after life gain");
        assert_eq!(
            *sub_ability.effect,
            Effect::GainEnergy {
                amount: QuantityExpr::Fixed { value: 1 }
            }
        );
    }

    #[test]
    fn trigger_counter_put_on_self() {
        let def = parse_trigger_line(
            "Whenever a +1/+1 counter is put on ~, draw a card.",
            "Fathom Mage",
        );
        assert_eq!(def.mode, TriggerMode::CounterAdded);
        assert_eq!(def.valid_card, Some(TargetFilter::SelfRef));
    }

    #[test]
    fn trigger_ordinal_counter_threshold_on_self() {
        let def = parse_trigger_line(
            "When the twelfth hour counter is put on ~, draw seven cards.",
            "Midnight Clock",
        );
        assert_eq!(def.mode, TriggerMode::CounterAdded);
        assert_eq!(def.valid_card, Some(TargetFilter::SelfRef));
        assert_eq!(
            def.counter_filter,
            Some(crate::types::ability::CounterTriggerFilter {
                counter_type: crate::types::counter::CounterType::Generic("hour".to_string()),
                threshold: Some(12),
            })
        );
    }

    #[test]
    fn trigger_ordinal_counter_threshold_does_not_emit_subject_fallback() {
        let mut ctx = ParseContext::default();
        let def = parse_trigger_line_with_index(
            "When the fifth plan counter is put on ~, sacrifice it.",
            "Doom Reigns Supreme",
            None,
            &mut ctx,
        );
        assert_eq!(def.mode, TriggerMode::CounterAdded);
        assert_eq!(def.valid_card, Some(TargetFilter::SelfRef));
        assert!(
            ctx.diagnostics.iter().all(|diagnostic| !matches!(
                diagnostic,
                OracleDiagnostic::TargetFallback { context, text, .. }
                    if context == "trigger subject parse fell back to Any"
                        && text == "the fifth plan counter is put on ~"
            )),
            "ordinal counter trigger should parse before generic subject fallback, got {:?}",
            ctx.diagnostics
        );
    }

    #[test]
    fn trigger_one_or_more_counters_on_self() {
        let def = parse_trigger_line(
            "Whenever one or more counters are put on ~, you gain 1 life.",
            "Some Card",
        );
        assert_eq!(def.mode, TriggerMode::CounterAdded);
        assert_eq!(def.valid_card, Some(TargetFilter::SelfRef));
    }

    // --- Constraint parsing tests ---

    #[test]
    fn trigger_once_each_turn_constraint() {
        let def = parse_trigger_line(
            "Whenever you gain life, put a +1/+1 counter on Exemplar of Light. This ability triggers only once each turn.",
            "Exemplar of Light",
        );
        assert_eq!(def.mode, TriggerMode::LifeGained);
        assert_eq!(
            def.constraint,
            Some(crate::types::ability::TriggerConstraint::OncePerTurn)
        );
    }

    #[test]
    fn trigger_no_constraint_by_default() {
        let def = parse_trigger_line(
            "Whenever you gain life, put a +1/+1 counter on this creature.",
            "Ajani's Pridemate",
        );
        assert_eq!(def.mode, TriggerMode::LifeGained);
        assert_eq!(def.constraint, None);
    }

    // CR 119.3 + CR 603.2: "Whenever you gain life" must restrict the trigger to
    // the source's controller. Regression for Vito, Thorn of the Dusk Rose and
    // every other "you gain life" trigger that previously fired on opponent
    // life-gain because `valid_target` was None.
    #[test]
    fn trigger_you_gain_life_scopes_to_controller() {
        let def = parse_trigger_line(
            "Whenever you gain life, target opponent loses that much life.",
            "Vito, Thorn of the Dusk Rose",
        );
        assert_eq!(def.mode, TriggerMode::LifeGained);
        assert_eq!(def.valid_target, Some(TargetFilter::Controller));
    }

    #[test]
    fn trigger_you_gain_life_pridemate_scopes_to_controller() {
        let def = parse_trigger_line(
            "Whenever you gain life, put a +1/+1 counter on this creature.",
            "Ajani's Pridemate",
        );
        assert_eq!(def.mode, TriggerMode::LifeGained);
        assert_eq!(def.valid_target, Some(TargetFilter::Controller));
    }

    // Negative test: "an opponent gains life" must remain opponent-scoped, not
    // pick up `Controller` from the "you gain life" fast-path.
    #[test]
    fn trigger_opponent_gains_life_scopes_to_opponent() {
        let def = parse_trigger_line(
            "Whenever an opponent gains life, you gain that much life.",
            "Some Card",
        );
        assert_eq!(def.mode, TriggerMode::LifeGained);
        assert_eq!(
            def.valid_target,
            Some(TargetFilter::Typed(
                TypedFilter::default().controller(ControllerRef::Opponent)
            ))
        );
    }

    // Negative test: "a player gains life" (no scope qualifier) must accept any
    // player. The subject-bearing handler stores the parsed subject filter, which
    // for "a player" is the unscoped player filter.
    #[test]
    fn trigger_a_player_gains_life_unscoped() {
        let def = parse_trigger_line("Whenever a player gains life, draw a card.", "Some Card");
        assert_eq!(def.mode, TriggerMode::LifeGained);
        // Whatever filter the subject parser produces for "a player", the key
        // invariant is that it is NOT scoped to Controller (which would silently
        // restrict to the source's controller).
        assert_ne!(def.valid_target, Some(TargetFilter::Controller));
    }

    #[test]
    fn trigger_only_during_your_turn() {
        let def = parse_trigger_line(
            "Whenever a creature enters, draw a card. This ability triggers only during your turn.",
            "Some Card",
        );
        assert_eq!(
            def.constraint,
            Some(crate::types::ability::TriggerConstraint::OnlyDuringYourTurn)
        );
    }

    // --- Compound subject tests ---

    #[test]
    fn trigger_self_or_another_creature_or_artifact_you_control() {
        use crate::types::ability::{ControllerRef, TypeFilter};
        let def = parse_trigger_line(
            "Whenever Haliya or another creature or artifact you control enters, you gain 1 life.",
            "Haliya, Guided by Light",
        );
        assert_eq!(def.mode, TriggerMode::ChangesZone);
        assert_eq!(def.destination, Some(Zone::Battlefield));
        match &def.valid_card {
            Some(TargetFilter::Or { filters }) => {
                assert_eq!(filters.len(), 3);
                assert_eq!(filters[0], TargetFilter::SelfRef);
                // Both branches should have Another + You controller
                assert_eq!(
                    filters[1],
                    TargetFilter::Typed(
                        TypedFilter::creature()
                            .controller(ControllerRef::You)
                            .properties(vec![FilterProp::Another])
                    )
                );
                assert_eq!(
                    filters[2],
                    TargetFilter::Typed(
                        TypedFilter::new(TypeFilter::Artifact)
                            .controller(ControllerRef::You)
                            .properties(vec![FilterProp::Another])
                    )
                );
            }
            other => panic!("Expected Or filter with 3 branches, got {:?}", other),
        }
    }

    #[test]
    fn normalize_legendary_short_name() {
        let result = normalize_self_refs(
            "Whenever Haliya or another creature enters",
            "Haliya, Guided by Light",
        );
        assert_eq!(result, "Whenever ~ or another creature enters");
    }

    /// CR 700.6: Arbaaz Mir's "Whenever ~ or another nontoken historic
    /// permanent you control enters" must parse cleanly into a ChangesZone
    /// trigger whose `valid_card` is an `Or { SelfRef, Typed[Permanent,
    /// controller=You, [NonToken, Historic, Another]] }`. Regression for
    /// the previously-Unknown trigger phrase.
    #[test]
    fn trigger_self_or_another_nontoken_historic_permanent_arbaaz() {
        use crate::types::ability::{FilterProp, TypeFilter};
        let def = parse_trigger_line(
            "Whenever Arbaaz Mir or another nontoken historic permanent you control enters, Arbaaz Mir deals 1 damage to each opponent and you gain 1 life.",
            "Arbaaz Mir",
        );
        assert_eq!(def.mode, TriggerMode::ChangesZone);
        assert_eq!(def.destination, Some(Zone::Battlefield));
        let TargetFilter::Or { ref filters } = def.valid_card.as_ref().expect("valid_card present")
        else {
            panic!("Expected Or filter, got {:?}", def.valid_card);
        };
        assert_eq!(filters.len(), 2, "expected 2-leg Or, got {filters:#?}");
        assert_eq!(filters[0], TargetFilter::SelfRef);
        let TargetFilter::Typed(ref tf) = filters[1] else {
            panic!("Expected Typed second leg, got {:?}", filters[1]);
        };
        assert_eq!(tf.controller, Some(ControllerRef::You));
        assert!(
            tf.type_filters.contains(&TypeFilter::Permanent),
            "expected Permanent in {:?}",
            tf.type_filters,
        );
        assert!(
            tf.properties.contains(&FilterProp::NonToken),
            "expected NonToken in {:?}",
            tf.properties,
        );
        assert!(
            tf.properties.contains(&FilterProp::Historic),
            "expected Historic in {:?}",
            tf.properties,
        );
        assert!(
            tf.properties.contains(&FilterProp::Another),
            "expected Another in {:?}",
            tf.properties,
        );
    }

    #[test]
    fn trigger_first_word_short_name_enters() {
        let def = parse_trigger_line(
            "When Sharuum enters, you may return target artifact card from your graveyard to the battlefield.",
            "Sharuum the Hegemon",
        );
        assert_eq!(def.mode, TriggerMode::ChangesZone);
        assert!(def.optional);
    }

    #[test]
    fn trigger_a_prefix_card_enters() {
        let def = parse_trigger_line(
            "When Sprouting Goblin enters, search your library for a land card with a basic land type, reveal it, put it into your hand, then shuffle.",
            "A-Sprouting Goblin",
        );
        assert_eq!(def.mode, TriggerMode::ChangesZone);
    }

    #[test]
    fn trigger_self_or_another_creature_enters() {
        let def = parse_trigger_line(
            "Whenever Some Card or another creature enters, draw a card.",
            "Some Card",
        );
        assert_eq!(def.mode, TriggerMode::ChangesZone);
        match &def.valid_card {
            Some(TargetFilter::Or { filters }) => {
                assert_eq!(filters.len(), 2);
                assert_eq!(filters[0], TargetFilter::SelfRef);
                match &filters[1] {
                    TargetFilter::Typed(TypedFilter { properties, .. }) => {
                        assert!(properties.contains(&FilterProp::Another));
                    }
                    other => panic!("Expected Typed with Another, got {:?}", other),
                }
            }
            other => panic!("Expected Or filter, got {:?}", other),
        }
    }

    // --- Intervening-if condition tests ---

    /// CR 608.2h: Haliya, Guided by Light — "draw a card if you've gained 3 or
    /// more life this turn" is a post-effect `if`. It is NOT a CR 603.4
    /// intervening-`if` (which must immediately follow the trigger condition),
    /// so it is re-homed onto the clause-level `execute.condition`, checked
    /// once at resolution — not hoisted to `TriggerDefinition.condition`.
    #[test]
    fn trigger_haliya_end_step_with_life_condition() {
        let def = parse_trigger_line(
            "At the beginning of your end step, draw a card if you've gained 3 or more life this turn.",
            "Haliya, Guided by Light",
        );
        assert_eq!(def.mode, TriggerMode::Phase);
        assert_eq!(def.phase, Some(Phase::End));
        assert!(
            def.condition.is_none(),
            "a post-effect `if` is not a CR 603.4 intervening-if and must not \
             hoist to the trigger condition, got {:?}",
            def.condition,
        );
        let execute = def.execute.expect("Haliya trigger must have an execute");
        assert_eq!(
            execute.condition,
            Some(AbilityCondition::QuantityCheck {
                lhs: QuantityExpr::Ref {
                    qty: QuantityRef::LifeGainedThisTurn {
                        player: PlayerScope::Controller,
                    },
                },
                comparator: Comparator::GE,
                rhs: QuantityExpr::Fixed { value: 3 },
            }),
            "the post-effect `if` must re-home onto the clause-level condition",
        );
    }

    /// CR 608.2h: A post-effect `if` with no explicit count ("if you gained
    /// life this turn") re-homes onto `execute.condition` as a `QuantityCheck`
    /// defaulting to `>= 1`, not onto the trigger-level condition.
    #[test]
    fn trigger_if_gained_life_no_number() {
        let def = parse_trigger_line(
            "At the beginning of your end step, create a Blood token if you gained life this turn.",
            "Some Card",
        );
        assert!(
            def.condition.is_none(),
            "a post-effect `if` must not hoist to the trigger condition, got {:?}",
            def.condition,
        );
        let execute = def.execute.expect("trigger must have an execute");
        assert_eq!(
            execute.condition,
            Some(AbilityCondition::QuantityCheck {
                lhs: QuantityExpr::Ref {
                    qty: QuantityRef::LifeGainedThisTurn {
                        player: PlayerScope::Controller,
                    },
                },
                comparator: Comparator::GE,
                rhs: QuantityExpr::Fixed { value: 1 },
            }),
            "the post-effect `if` must re-home onto the clause-level condition",
        );
    }

    #[test]
    fn trigger_if_descended_this_turn() {
        let def = parse_trigger_line(
            "At the beginning of your end step, if you descended this turn, scry 1.",
            "Ruin-Lurker Bat",
        );
        assert_eq!(def.mode, TriggerMode::Phase);
        assert_eq!(def.phase, Some(Phase::End));
        assert_eq!(
            def.condition,
            Some(TriggerCondition::QuantityComparison {
                lhs: QuantityExpr::Ref {
                    qty: QuantityRef::DescendedThisTurn,
                },
                comparator: Comparator::GE,
                rhs: QuantityExpr::Fixed { value: 1 },
            })
        );
        assert!(def.execute.is_some());
    }

    #[test]
    fn trigger_if_gained_5_or_more_life() {
        let def = parse_trigger_line(
            "At the beginning of each end step, if you gained 5 or more life this turn, create a 4/4 white Angel creature token with flying and vigilance.",
            "Resplendent Angel",
        );
        assert_eq!(
            def.condition,
            Some(TriggerCondition::QuantityComparison {
                lhs: QuantityExpr::Ref {
                    qty: QuantityRef::LifeGainedThisTurn {
                        player: PlayerScope::Controller,
                    },
                },
                comparator: Comparator::GE,
                rhs: QuantityExpr::Fixed { value: 5 },
            })
        );
        // Regression: execute must not be None — the effect text after the condition
        // must be preserved and parsed (previously the condition clause consumed the
        // entire text, leaving execute as None).
        assert!(
            def.execute.is_some(),
            "execute must be Some — effect text after 'if you gained N or more life this turn' was dropped"
        );
    }

    #[test]
    fn trigger_if_gained_4_or_more_life_angelic_accord() {
        // Angelic Accord: condition at start of effect text
        let def = parse_trigger_line(
            "At the beginning of each end step, if you gained 4 or more life this turn, create a 4/4 white Angel creature token with flying.",
            "Angelic Accord",
        );
        assert_eq!(
            def.condition,
            Some(TriggerCondition::QuantityComparison {
                lhs: QuantityExpr::Ref {
                    qty: QuantityRef::LifeGainedThisTurn {
                        player: PlayerScope::Controller,
                    },
                },
                comparator: Comparator::GE,
                rhs: QuantityExpr::Fixed { value: 4 },
            })
        );
        assert!(
            def.execute.is_some(),
            "execute must be Some — token creation effect was dropped"
        );
    }

    #[test]
    fn trigger_if_gained_life_this_turn_no_minimum() {
        // Ocelot Pride: "if you gained life this turn" (no number)
        let def = parse_trigger_line(
            "At the beginning of your end step, if you gained life this turn, create a 1/1 white Cat creature token.",
            "Ocelot Pride",
        );
        assert_eq!(
            def.condition,
            Some(TriggerCondition::QuantityComparison {
                lhs: QuantityExpr::Ref {
                    qty: QuantityRef::LifeGainedThisTurn {
                        player: PlayerScope::Controller,
                    },
                },
                comparator: Comparator::GE,
                rhs: QuantityExpr::Fixed { value: 1 },
            })
        );
        assert!(
            def.execute.is_some(),
            "execute must be Some — token creation effect was dropped"
        );
    }

    #[test]
    fn trigger_ocelot_pride_copy_each_entered_token_not_source() {
        let def = parse_trigger_line(
            "At the beginning of your end step, if you gained life this turn, create a 1/1 white Cat creature token. Then if you have the city's blessing, for each token you control that entered the battlefield this turn, create a token that's a copy of it.",
            "Ocelot Pride",
        );
        let create_cat = def.execute.expect("token creation execute");
        let copy_each = create_cat.sub_ability.expect("city blessing copy clause");
        assert!(matches!(
            copy_each.condition,
            Some(AbilityCondition::HasCityBlessing)
        ));
        assert!(copy_each.repeat_for.is_none());
        match &*copy_each.effect {
            Effect::CopyTokenOf {
                target,
                source_filter: Some(TargetFilter::Typed(filter)),
                ..
            } => {
                assert_eq!(*target, TargetFilter::None);
                assert_eq!(filter.controller, Some(ControllerRef::You));
                assert!(filter
                    .properties
                    .iter()
                    .any(|prop| matches!(prop, FilterProp::Token)));
                assert!(filter
                    .properties
                    .iter()
                    .any(|prop| matches!(prop, FilterProp::EnteredThisTurn)));
            }
            other => panic!("expected source-filtered CopyTokenOf, got {other:?}"),
        }
    }

    /// CR 603.4: A post-effect `if` ("draw a card if Y") is NOT an
    /// intervening-`if` — that rule applies only to an `if` that immediately
    /// follows the trigger condition ("When X, if Y, Z"). Such a trailing `if`
    /// must be left in the effect text so per-clause parsing
    /// (`strip_suffix_conditional`) attaches it as a clause-level
    /// `AbilityCondition`, rather than being hoisted to the trigger level.
    #[test]
    fn extract_if_does_not_hoist_post_effect_condition() {
        let (cleaned, cond) =
            extract_if_condition("draw a card if you've gained 3 or more life this turn.");
        assert_eq!(
            cond, None,
            "a post-effect `if` is not an intervening-if (CR 603.4) and must not hoist",
        );
        assert_eq!(
            cleaned, "draw a card if you've gained 3 or more life this turn.",
            "effect text must be unchanged when the `if` is not a true intervening-if",
        );
    }

    /// CR 603.4: "effect. Then if Y, effect2" — the `if` is introduced by "then"
    /// and scopes only to the second clause's sub_ability. `extract_if_condition`
    /// must NOT hoist this to the trigger-level condition.
    #[test]
    fn extract_if_skips_then_if_clause() {
        let (cleaned, cond) = extract_if_condition(
            "create a 1/1 black ninja creature token. then if you control five or more ninjas, that player loses half their life, rounded up.",
        );
        assert_eq!(
            cond, None,
            "then-if conditions must not be hoisted to trigger level",
        );
        assert_eq!(
            cleaned,
            "create a 1/1 black ninja creature token. then if you control five or more ninjas, that player loses half their life, rounded up.",
            "effect text must be returned unchanged when the if belongs to a then-clause",
        );
    }

    /// CR 603.4: Genuine leading intervening-if ("When X, if Y, Z" — here
    /// `extract_if_condition` receives only the effect portion "if Y, Z") must
    /// still be hoisted even if a later "then if" appears.
    #[test]
    fn extract_if_preserves_leading_intervening_if_with_later_then() {
        // Only the FIRST `if ` is considered for the then-clause guard; a
        // leading intervening-if (no preceding "then") is correctly hoisted.
        let (_, cond) = extract_if_condition("if you control a creature, draw a card");
        assert!(
            cond.is_some(),
            "leading intervening-if must still be hoisted, got {cond:?}",
        );
    }

    /// CR 603.4: Inline "then if" without a sentence boundary ("X then if Y,
    /// Z") — the condition still scopes to the then-clause sub_ability and
    /// must not be hoisted. Covers punctuation-free variants of the pattern.
    #[test]
    fn extract_if_skips_inline_then_if_clause() {
        let (_, cond) =
            extract_if_condition("draw a card then if you control a creature, gain 1 life");
        assert_eq!(
            cond, None,
            "inline `then if` (no sentence boundary) must not be hoisted",
        );
    }

    /// CR 603.4: "effect. Then, if Y, ..." (with comma after "Then") — the
    /// condition still belongs to the "then" clause and must not be hoisted.
    /// Regression: A Good Thing ("double your life total. Then, if you have
    /// 1,000 or more life, you lose the game.").
    #[test]
    fn extract_if_skips_then_comma_if_clause() {
        let (_, cond) = extract_if_condition(
            "double your life total. then, if you have 1,000 or more life, you lose the game.",
        );
        assert_eq!(
            cond, None,
            "\"then, if\" conditions must not be hoisted to trigger level",
        );
    }

    /// CR 608.2k + CR 603.4: Full Dark Leo & Shredder parse — the if-condition
    /// must attach to the sub_ability (not the trigger), the sub_ability target
    /// must be TriggeringPlayer (not a new Player target), and the sub_ability
    /// amount must resolve "half their life, rounded up".
    #[test]
    fn parse_dark_leo_trigger_structure() {
        use crate::types::ability::{AbilityCondition, Effect, RoundingMode};

        let def = parse_trigger_line(
            "Whenever ~ deals combat damage to a player, create a 1/1 black Ninja creature token. Then if you control five or more Ninjas, that player loses half their life, rounded up.",
            "Dark Leo & Shredder",
        );

        // Trigger-level condition must be None — the `if you control five or more`
        // scopes only to the sub_ability.
        assert_eq!(def.condition, None, "trigger.condition must be None");

        // Outer effect is the token creation.
        let execute = def.execute.as_ref().expect("execute must be Some");
        assert!(
            matches!(*execute.effect, Effect::Token { .. }),
            "outer execute must be Token, got {:?}",
            execute.effect,
        );

        // Sub-ability holds the conditional life-loss.
        let sub = execute
            .sub_ability
            .as_deref()
            .expect("sub_ability must be Some");
        // Sub-ability condition is the Ninja count check.
        assert!(
            matches!(
                &sub.condition,
                Some(AbilityCondition::QuantityCheck {
                    comparator: Comparator::GE,
                    rhs: QuantityExpr::Fixed { value: 5 },
                    ..
                })
            ),
            "sub_ability.condition must be QuantityCheck ≥ 5, got {:?}",
            sub.condition,
        );
        // Sub-ability effect is LoseLife targeting TriggeringPlayer with DivideRounded amount.
        match &*sub.effect {
            Effect::LoseLife { amount, target } => {
                assert_eq!(
                    target.as_ref(),
                    Some(&TargetFilter::TriggeringPlayer),
                    "sub_ability LoseLife.target must be TriggeringPlayer",
                );
                assert!(
                    matches!(
                        amount,
                        QuantityExpr::DivideRounded {
                            rounding: RoundingMode::Up,
                            ..
                        }
                    ),
                    "amount must be DivideRounded(Up), got {amount:?}",
                );
            }
            other => panic!("sub_ability effect must be LoseLife, got {other:?}"),
        }
    }

    /// CR 208.1 + CR 603.4: Cloud, Ex-SOLDIER — attack trigger with a "Then if
    /// ~ has power 7 or greater, …" sub-ability gate. Before the `~ has power N`
    /// grammar branch was added to `parse_source_power_toughness_condition`,
    /// the condition silently dropped and the Treasure sub-ability fired on
    /// every attack regardless of Cloud's power.
    ///
    /// Asserted shape:
    /// - Trigger-level `condition` is None (the gate scopes to the sub_ability).
    /// - Outer effect is the draw clause.
    /// - Sub-ability effect is Token creation, and its `condition` is a
    ///   `QuantityCheck` on `Power { scope: Source } >= 7`.
    #[test]
    fn parse_cloud_ex_soldier_attack_trigger_structure() {
        use crate::types::ability::{AbilityCondition, Effect};

        let def = parse_trigger_line(
            "Whenever ~ attacks, draw a card for each equipped attacking creature you control. Then if ~ has power 7 or greater, create two Treasure tokens.",
            "Cloud, Ex-SOLDIER",
        );

        // Trigger-level condition must be None — the "Then if ~ has power 7 or
        // greater" gate scopes only to the sub_ability.
        assert_eq!(
            def.condition, None,
            "trigger.condition must be None (gate scopes to sub_ability)"
        );

        let execute = def.execute.as_ref().expect("execute must be Some");

        // Outer effect: draw clause.
        assert!(
            matches!(*execute.effect, Effect::Draw { .. }),
            "outer execute must be Draw, got {:?}",
            execute.effect,
        );

        // Sub-ability: Treasure token creation gated on Power >= 7.
        let sub = execute
            .sub_ability
            .as_deref()
            .expect("sub_ability must be Some (Treasure clause)");
        match &sub.condition {
            Some(AbilityCondition::QuantityCheck {
                lhs:
                    QuantityExpr::Ref {
                        qty:
                            QuantityRef::Power {
                                scope: crate::types::ability::ObjectScope::Source,
                            },
                    },
                comparator: Comparator::GE,
                rhs: QuantityExpr::Fixed { value: 7 },
            }) => {}
            other => panic!(
                "sub_ability.condition must be QuantityCheck Power(Source) >= 7, got {other:?}",
            ),
        }
        assert!(
            matches!(*sub.effect, Effect::Token { .. }),
            "sub_ability effect must be Token, got {:?}",
            sub.effect,
        );
    }

    /// CR 208.1 + CR 107.3e + CR 603.4: Betor, Kin to All — three-tier end-step
    /// trigger where each tier is gated on a "creatures you control have total
    /// toughness N or greater" predicate. Before this regression test was
    /// added, all three gates dropped silently because no parser path
    /// recognized `Aggregate{Sum, Toughness, creatures-you-control}` in
    /// condition position; the trigger fired unconditionally and the third
    /// clause's "each opponent" subject collapsed onto the controller (so
    /// "each opponent loses half their life, rounded up" hit the source
    /// player instead).
    ///
    /// Asserted shape:
    /// - Trigger-level intervening-if at `total toughness ≥ 10` (CR 603.4 —
    ///   checked at trigger creation AND at resolution per the published
    ///   ruling).
    /// - First effect (Draw) is unconditional under the trigger gate.
    /// - Second sub_ability (UntapAll) carries `QuantityCheck ≥ 20`.
    /// - Third sub_ability (LoseLife) carries `QuantityCheck ≥ 40` and its
    ///   target is NOT the controller — it must address each opponent.
    #[test]
    fn parse_betor_kin_to_all_trigger_structure() {
        use crate::types::ability::{
            AbilityCondition, AggregateFunction, Effect, ObjectProperty, RoundingMode,
        };

        let def = parse_trigger_line(
            "At the beginning of your end step, if creatures you control have total toughness 10 or greater, draw a card. Then if creatures you control have total toughness 20 or greater, untap each creature you control. Then if creatures you control have total toughness 40 or greater, each opponent loses half their life, rounded up.",
            "Betor, Kin to All",
        );

        // -- Trigger-level: intervening-if `Aggregate{Sum, Toughness, creatures-you-control} >= 10`.
        let trigger_cond = def
            .condition
            .as_ref()
            .expect("trigger.condition must be Some (intervening-if hoisted)");
        match trigger_cond {
            TriggerCondition::QuantityComparison {
                lhs,
                comparator,
                rhs,
            } => {
                assert_eq!(*comparator, Comparator::GE);
                assert_eq!(*rhs, QuantityExpr::Fixed { value: 10 });
                match lhs {
                    QuantityExpr::Ref {
                        qty:
                            QuantityRef::Aggregate {
                                function,
                                property,
                                filter,
                            },
                    } => {
                        assert_eq!(*function, AggregateFunction::Sum);
                        assert_eq!(*property, ObjectProperty::Toughness);
                        match filter {
                            TargetFilter::Typed(t) => {
                                assert_eq!(t.controller, Some(ControllerRef::You));
                                assert!(t.type_filters.contains(&TypeFilter::Creature));
                            }
                            other => panic!("expected Typed(Creature, you) filter, got {other:?}"),
                        }
                    }
                    other => panic!("trigger lhs must be Aggregate Ref, got {other:?}"),
                }
            }
            other => panic!("trigger.condition must be QuantityComparison, got {other:?}"),
        }

        // -- Walk the sub_ability chain.
        let execute = def.execute.as_ref().expect("execute must be Some");

        // First effect: Draw (no per-clause condition; gate is the trigger-level intervening-if).
        assert!(
            matches!(*execute.effect, Effect::Draw { .. }),
            "first effect must be Draw, got {:?}",
            execute.effect,
        );

        // Second tier: UntapAll under "Then if ... ≥ 20".
        let untap_sub = execute
            .sub_ability
            .as_deref()
            .expect("first sub_ability (UntapAll) must be Some");
        match &untap_sub.condition {
            Some(AbilityCondition::QuantityCheck {
                comparator: Comparator::GE,
                rhs: QuantityExpr::Fixed { value: 20 },
                lhs,
            }) => {
                assert!(
                    matches!(
                        lhs,
                        QuantityExpr::Ref {
                            qty: QuantityRef::Aggregate {
                                function: AggregateFunction::Sum,
                                property: ObjectProperty::Toughness,
                                ..
                            },
                        }
                    ),
                    "untap sub_ability lhs must be Aggregate Sum/Toughness, got {lhs:?}",
                );
            }
            other => panic!(
                "untap sub_ability.condition must be QuantityCheck >= 20 over Aggregate Sum/Toughness, got {other:?}",
            ),
        }
        assert!(
            matches!(
                *untap_sub.effect,
                Effect::UntapAll { .. } | Effect::Untap { .. }
            ),
            "second tier effect must be UntapAll/Untap, got {:?}",
            untap_sub.effect,
        );

        // Third tier: LoseLife under "Then if ... ≥ 40", targeting each opponent.
        let lose_sub = untap_sub
            .sub_ability
            .as_deref()
            .expect("second sub_ability (LoseLife) must be Some");
        match &lose_sub.condition {
            Some(AbilityCondition::QuantityCheck {
                comparator: Comparator::GE,
                rhs: QuantityExpr::Fixed { value: 40 },
                lhs,
            }) => {
                assert!(
                    matches!(
                        lhs,
                        QuantityExpr::Ref {
                            qty: QuantityRef::Aggregate {
                                function: AggregateFunction::Sum,
                                property: ObjectProperty::Toughness,
                                ..
                            },
                        }
                    ),
                    "lose-life sub_ability lhs must be Aggregate Sum/Toughness, got {lhs:?}",
                );
            }
            other => panic!(
                "lose-life sub_ability.condition must be QuantityCheck >= 40 over Aggregate Sum/Toughness, got {other:?}",
            ),
        }
        match &*lose_sub.effect {
            Effect::LoseLife { amount, target } => {
                // CR 107.1a: amount must be DivideRounded by 2, rounding Up
                // ("half ... rounded up"). The inner ref resolves per the
                // active player binding (CR 608.2c) when the outer
                // player_scope iterates over each opponent.
                assert!(
                    matches!(
                        amount,
                        QuantityExpr::DivideRounded {
                            divisor: 2,
                            rounding: RoundingMode::Up,
                            ..
                        }
                    ),
                    "amount must be DivideRounded(2, Up), got {amount:?}",
                );

                // Critical regression: the third clause's "each opponent" subject
                // must NOT collapse onto the controller. The bug before the fix
                // produced a degenerate empty Typed filter (`type_filters: []`,
                // `controller: null`, `properties: []`) that the runtime resolved
                // to the controller, draining the source player's own life. Any
                // such empty filter must be rejected outright.
                if let Some(TargetFilter::Typed(t)) = target {
                    let degenerate_empty = t.type_filters.is_empty()
                        && t.controller.is_none()
                        && t.properties.is_empty();
                    assert!(
                        !degenerate_empty,
                        "regression: LoseLife.target collapsed to the bug-shape degenerate Typed filter (would drain the controller): {t:?}",
                    );
                }
            }
            other => {
                panic!("third tier effect must be LoseLife addressing each opponent, got {other:?}",)
            }
        }

        // The each-opponent dispatch lives on the sub_ability's `player_scope`,
        // not on `LoseLife.target`. The parser's `strip_each_player_subject`
        // strips the "each opponent " prefix and lifts `PlayerFilter::Opponent`
        // onto the surrounding spell-execute wrapper; the runtime iterates the
        // life-loss across opponents (CR 608.2c). This is the same encoding
        // emitted today by Night Market Lookout's "each opponent loses 1 life"
        // trigger.
        assert_eq!(
            lose_sub.player_scope,
            Some(PlayerFilter::Opponent),
            "third tier sub_ability.player_scope must be Some(Opponent) — each-opponent dispatch must be lifted to the wrapper, not collapsed onto the controller",
        );
    }

    /// CR 608.2k: "that player discards a card" in a trigger effect must target
    /// the triggering player (damaged player), not surface a fresh target prompt.
    /// Abyssal-Specter-class regression test.
    #[test]
    fn parse_abyssal_specter_that_player_discard() {
        use crate::types::ability::Effect;

        let def = parse_trigger_line(
            "Whenever ~ deals damage to a player, that player discards a card.",
            "Abyssal Specter",
        );
        let execute = def.execute.as_ref().expect("execute must be Some");
        match &*execute.effect {
            Effect::Discard { target, .. } => {
                assert_eq!(
                    target,
                    &TargetFilter::TriggeringPlayer,
                    "Discard.target must be TriggeringPlayer",
                );
            }
            other => panic!("execute effect must be Discard, got {other:?}"),
        }
    }

    #[test]
    fn trigger_if_gained_and_lost_life_compound() {
        // CR 119: "you gained and lost life this turn" is a compound-verb condition
        // with shared object — two event verbs joined by "and" sharing "life this turn".
        let def = parse_trigger_line(
            "At the beginning of your end step, if you gained and lost life this turn, create a 1/1 black Bat creature token with flying.",
            "Some Card",
        );
        assert!(
            matches!(
                &def.condition,
                Some(TriggerCondition::And { conditions }) if conditions.len() == 2
            ),
            "Expected And with 2 conditions, got {:?}",
            def.condition
        );
        assert!(def.execute.is_some());
    }

    #[test]
    fn trigger_attacker_it_gets_is_single_target_pump() {
        // CR 608.2c: "Whenever a creature you control attacks, it gets +2/+0 until end of turn."
        // "it" refers to the triggering attacker → single-object TriggeringSource,
        // which must lower to Effect::Pump (single target), NOT Effect::PumpAll.
        let def = parse_trigger_line(
            "Whenever a creature you control attacks, it gets +2/+2 until end of turn.",
            "Fervent Charge",
        );
        assert_eq!(def.mode, TriggerMode::Attacks);
        let exec = def.execute.as_ref().expect("execute must be Some");
        match &*exec.effect {
            Effect::Pump { target, .. } => {
                assert_eq!(*target, TargetFilter::TriggeringSource);
            }
            other => panic!(
                "expected Effect::Pump with TriggeringSource, got {:?}",
                other
            ),
        }
    }

    #[test]
    fn necroduality_copies_entering_zombie_not_source() {
        // CR 608.2k: "Whenever a nontoken Zombie you control enters, create a
        // token that's a copy of that creature." "That creature" is the
        // entering Zombie (the event source), NOT Necroduality itself. The
        // untargeted ChangesZone trigger lifts CopyTokenOf's ParentTarget to
        // TriggeringSource. (#596)
        let def = parse_trigger_line(
            "Whenever a nontoken Zombie you control enters, create a token that's a copy of that creature.",
            "Necroduality",
        );
        assert_eq!(def.mode, TriggerMode::ChangesZone);
        let exec = def.execute.as_ref().expect("execute must be Some");
        match &*exec.effect {
            Effect::CopyTokenOf { target, .. } => {
                assert_eq!(
                    *target,
                    TargetFilter::TriggeringSource,
                    "copy source must be the entering Zombie, not the trigger's own source"
                );
            }
            other => panic!("expected Effect::CopyTokenOf, got {other:?}"),
        }
    }

    #[test]
    fn flicker_enters_trigger_keeps_chosen_target_anaphor() {
        // CR 608.2c: "When ~ enters, you may exile another target permanent you
        // control, then return that card …" — "that card" refers to the CHOSEN
        // exiled permanent (ParentTarget), NOT the trigger event. The enters
        // event-source lift must STOP at the first chosen target so the
        // sub-ability stays ParentTarget (regression guard for #596 — Felidar
        // Guardian / Restoration Angel flicker class).
        let def = parse_trigger_line(
            "When this creature enters, you may exile another target permanent you control, then return that card to the battlefield under your control.",
            "Felidar Guardian",
        );
        assert_eq!(def.mode, TriggerMode::ChangesZone);
        let exec = def.execute.as_ref().expect("execute must be Some");
        // Top link targets the chosen permanent.
        assert!(
            matches!(exec.effect.target_filter(), Some(TargetFilter::Typed(_))),
            "top-level exile must keep its chosen Typed target, got {:?}",
            exec.effect.target_filter()
        );
        // Sub-ability "that card" must remain ParentTarget, not be lifted.
        let sub = exec
            .sub_ability
            .as_ref()
            .expect("flicker chains to a return");
        assert_eq!(
            sub.effect.target_filter(),
            Some(&TargetFilter::ParentTarget),
            "return-that-card must bind to the chosen permanent (ParentTarget), not TriggeringSource"
        );
    }

    #[test]
    fn counter_added_trigger_captures_explicit_type() {
        // CR 122.1: Hapatra — "Whenever you put one or more -1/-1 counters on a
        // creature, create a Snake" must fire ONLY on -1/-1 counters, not any
        // counter. The type-prefix extractor sets counter_filter. (#589)
        let def = parse_trigger_line(
            "Whenever you put one or more -1/-1 counters on a creature, create a 1/1 green Snake creature token with deathtouch.",
            "Hapatra, Vizier of Poisons",
        );
        assert_eq!(def.mode, TriggerMode::CounterAdded);
        let filter = def
            .counter_filter
            .as_ref()
            .expect("explicit -1/-1 counter type must be captured");
        assert_eq!(
            filter.counter_type,
            crate::types::counter::CounterType::Minus1Minus1
        );
        assert_eq!(filter.threshold, None, "no threshold form here");
    }

    #[test]
    fn counter_added_trigger_generic_has_no_type_filter() {
        // "one or more counters" (no type word) keeps firing on any counter
        // kind — the extractor must leave counter_filter unset.
        let def = parse_trigger_line(
            "Whenever you put one or more counters on a creature, draw a card.",
            "Test Card",
        );
        assert_eq!(def.mode, TriggerMode::CounterAdded);
        assert!(
            def.counter_filter.is_none(),
            "generic counter trigger must not set a type filter"
        );
    }

    #[test]
    fn counter_added_trigger_rejects_non_type_remainder() {
        // Regression (#589 review): the prefix strip is loose, so a non-"you"
        // subject ("an opponent puts one or more counters") leaves leftover
        // subject text in the type slot. try_parse_counter_type must reject that
        // multi-word junk → counter_filter stays None → the any-counter trigger
        // (Bold Plagiarist class) keeps firing on every counter, not none.
        let def = parse_trigger_line(
            "Whenever an opponent puts one or more counters on a creature they control, create a 1/1 colorless Thopter artifact creature token with flying.",
            "Test Card",
        );
        assert_eq!(def.mode, TriggerMode::CounterAdded);
        assert!(
            def.counter_filter.is_none(),
            "loose prefix strip must not manufacture a bogus typed filter from subject text"
        );
    }

    #[test]
    fn doran_attack_block_pump_resolves_pt_difference() {
        // Issue #417 — Doran, Besieged by Time.
        // "Whenever a creature you control attacks or blocks, it gets +X/+X
        //  until end of turn, where X is the difference between its power and
        //  toughness." The `where X is` tail must lower to a typed
        //  QuantityExpr::Difference, NOT leak the clause text into a
        //  PtValue::Variable (the `unwrap_or_else` fallback in
        //  oracle_effect/mod.rs). CR 208.1 / CR 611.2d. ("The difference
        //  between A and B" is an unsigned Oracle templating convention with
        //  no dedicated CR number — the resolver takes `.abs()`.)
        let def = parse_trigger_line(
            "Whenever a creature you control attacks or blocks, it gets +X/+X until end of turn, where X is the difference between its power and toughness.",
            "Doran, Besieged by Time",
        );
        let exec = def.execute.as_ref().expect("execute must be Some");
        let expected = QuantityExpr::Difference {
            left: Box::new(QuantityExpr::Ref {
                qty: QuantityRef::Power {
                    scope: ObjectScope::Recipient,
                },
            }),
            right: Box::new(QuantityExpr::Ref {
                qty: QuantityRef::Toughness {
                    scope: ObjectScope::Recipient,
                },
            }),
        };
        match &*exec.effect {
            Effect::Pump {
                power,
                toughness,
                target,
            } => {
                assert_eq!(
                    *power,
                    PtValue::Quantity(expected.clone()),
                    "power must be a typed Difference, not a Variable leak"
                );
                assert_eq!(
                    *toughness,
                    PtValue::Quantity(expected),
                    "toughness must be a typed Difference, not a Variable leak"
                );
                assert_eq!(*target, TargetFilter::TriggeringSource);
            }
            other => panic!("expected Effect::Pump, got {other:?}"),
        }
    }

    #[test]
    fn trigger_execute_pump_all_creatures() {
        // Regression: trigger bodies with "creatures you control get +1/+1 until end of turn"
        // must produce a PumpAll execute effect, not null.
        let def = parse_trigger_line(
            "Whenever another creature you control enters, creatures you control get +1/+1 until end of turn.",
            "Goldnight Commander",
        );
        assert!(
            def.execute.is_some(),
            "execute should be Some (PumpAll), got None"
        );
        let exec = def.execute.as_ref().unwrap();
        assert!(
            matches!(*exec.effect, Effect::PumpAll { .. }),
            "execute effect should be PumpAll, got {:?}",
            exec.effect
        );
    }

    #[test]
    fn extract_if_graveyard_threshold() {
        let (cleaned, cond) = extract_if_condition(
            "if there are seven or more cards in your graveyard, exile a card at random from your graveyard.",
        );
        assert!(
            matches!(cond, Some(TriggerCondition::QuantityComparison { .. })),
            "Expected QuantityComparison, got {:?}",
            cond
        );
        assert!(
            cleaned.contains("exile"),
            "Effect text should remain: {cleaned}"
        );
    }

    #[test]
    fn trigger_graveyard_threshold_tersa() {
        let def = parse_trigger_line(
            "Whenever ~ attacks, if there are seven or more cards in your graveyard, exile a card at random from your graveyard. You may play that card this turn.",
            "Tersa Lightshatter",
        );
        assert_eq!(def.mode, TriggerMode::Attacks);
        assert!(
            matches!(
                def.condition,
                Some(TriggerCondition::QuantityComparison { .. })
            ),
            "Expected graveyard threshold condition, got {:?}",
            def.condition
        );
    }

    // --- Counter placement with "you put" pattern ---

    #[test]
    fn trigger_you_put_counters_on_self() {
        let def = parse_trigger_line(
            "Whenever you put one or more +1/+1 counters on this creature, draw a card. This ability triggers only once each turn.",
            "Exemplar of Light",
        );
        assert_eq!(def.mode, TriggerMode::CounterAdded);
        assert_eq!(def.valid_card, Some(TargetFilter::SelfRef));
        assert_eq!(
            def.constraint,
            Some(crate::types::ability::TriggerConstraint::OncePerTurn)
        );
        // Constraint sentence should NOT leak as a sub-ability
        if let Some(ref exec) = def.execute {
            assert!(
                !matches!(
                    *exec.effect,
                    crate::types::ability::Effect::Unimplemented { .. }
                ),
                "Effect should be Draw, not Unimplemented"
            );
            assert!(
                exec.sub_ability.is_none(),
                "No spurious sub-ability from constraint text"
            );
        }
    }

    #[test]
    fn trigger_counters_put_on_another_creature_you_control() {
        use crate::types::ability::ControllerRef;
        let def = parse_trigger_line(
            "Whenever one or more +1/+1 counters are put on another creature you control, put a +1/+1 counter on this creature.",
            "Enduring Scalelord",
        );
        assert_eq!(def.mode, TriggerMode::CounterAdded);
        assert_eq!(
            def.valid_card,
            Some(TargetFilter::Typed(
                TypedFilter::creature()
                    .controller(ControllerRef::You)
                    .properties(vec![FilterProp::Another])
            ))
        );
    }

    #[test]
    fn trigger_you_put_counters_on_creature_you_control() {
        use crate::types::ability::ControllerRef;
        let def = parse_trigger_line(
            "Whenever you put one or more +1/+1 counters on a creature you control, draw a card.",
            "The Powerful Dragon",
        );
        assert_eq!(def.mode, TriggerMode::CounterAdded);
        assert_eq!(
            def.valid_card,
            Some(TargetFilter::Typed(
                TypedFilter::creature().controller(ControllerRef::You)
            ))
        );
    }

    #[test]
    fn strip_constraint_does_not_affect_effect() {
        let result =
            strip_constraint_sentences("draw a card. this ability triggers only once each turn.");
        assert_eq!(result, "draw a card");
    }

    #[test]
    fn strip_constraint_preserves_plain_effect() {
        let result = strip_constraint_sentences("put a +1/+1 counter on ~");
        assert_eq!(result, "put a +1/+1 counter on ~");
    }

    // --- Color-filtered trigger subjects ---

    #[test]
    fn trigger_white_creature_you_control_attacks() {
        let def = parse_trigger_line(
            "Whenever a white creature you control attacks, you gain 1 life.",
            "Linden, the Steadfast Queen",
        );
        assert_eq!(def.mode, TriggerMode::Attacks);
        assert_eq!(
            def.valid_card,
            Some(TargetFilter::Typed(
                TypedFilter::creature()
                    .controller(crate::types::ability::ControllerRef::You)
                    .properties(vec![FilterProp::HasColor {
                        color: crate::types::mana::ManaColor::White
                    }])
            ))
        );
    }

    // --- New trigger mode tests ---

    #[test]
    fn trigger_land_enters() {
        let def = parse_trigger_line("When this land enters, you gain 1 life.", "Bloodfell Caves");
        assert_eq!(def.mode, TriggerMode::ChangesZone);
        assert_eq!(def.destination, Some(Zone::Battlefield));
        assert_eq!(def.valid_card, Some(TargetFilter::SelfRef));
    }

    #[test]
    fn trigger_evelyn_exiles_each_library_with_collection_counter_and_permission() {
        let def = parse_trigger_line(
            "Whenever Evelyn or another Vampire you control enters, exile the top card of each player's library with a collection counter on it.",
            "Evelyn, the Covetous",
        );
        assert_eq!(def.mode, TriggerMode::ChangesZone);
        assert_eq!(def.destination, Some(Zone::Battlefield));

        let execute = def.execute.as_deref().expect("execute ability");
        assert_eq!(execute.player_scope, Some(PlayerFilter::All));
        assert!(matches!(
            execute.effect.as_ref(),
            Effect::ExileTop {
                player: TargetFilter::Controller,
                count: QuantityExpr::Fixed { value: 1 },
                face_down: false,
            }
        ));

        let counter = execute.sub_ability.as_deref().expect("counter rider");
        assert!(matches!(
            counter.effect.as_ref(),
            Effect::PutCounterAll {
                counter_type: CounterType::Generic(name),
                target: TargetFilter::TrackedSet { .. },
                ..
            } if name == "collection"
        ));

        let grant = counter.sub_ability.as_deref().expect("permission rider");
        let Effect::GrantCastingPermission {
            permission, target, ..
        } = grant.effect.as_ref()
        else {
            panic!("expected GrantCastingPermission, got {:?}", grant.effect);
        };
        assert!(matches!(target, TargetFilter::TrackedSet { .. }));
        assert!(matches!(
            permission,
            CastingPermission::PlayFromExile {
                frequency: CastFrequency::OncePerTurn,
                mana_spend_permission: Some(ManaSpendPermission::AnyTypeOrColor),
                ..
            }
        ));
    }

    #[test]
    fn trigger_aura_enters() {
        let def = parse_trigger_line(
            "When this Aura enters, tap target creature an opponent controls.",
            "Glaring Aegis",
        );
        assert_eq!(def.mode, TriggerMode::ChangesZone);
        assert_eq!(def.destination, Some(Zone::Battlefield));
        assert_eq!(def.valid_card, Some(TargetFilter::SelfRef));
    }

    #[test]
    fn trigger_equipment_enters() {
        let def = parse_trigger_line(
            "When this Equipment enters, attach it to target creature you control.",
            "Shining Armor",
        );
        assert_eq!(def.mode, TriggerMode::ChangesZone);
        assert_eq!(def.destination, Some(Zone::Battlefield));
        assert_eq!(def.valid_card, Some(TargetFilter::SelfRef));
    }

    #[test]
    fn trigger_vehicle_enters() {
        let def = parse_trigger_line(
            "When this Vehicle enters, create a 1/1 white Pilot creature token.",
            "Some Vehicle",
        );
        assert_eq!(def.mode, TriggerMode::ChangesZone);
        assert_eq!(def.destination, Some(Zone::Battlefield));
        assert_eq!(def.valid_card, Some(TargetFilter::SelfRef));
    }

    #[test]
    fn trigger_leaves_battlefield() {
        let def = parse_trigger_line(
            "When Oblivion Ring leaves the battlefield, return the exiled card to the battlefield.",
            "Oblivion Ring",
        );
        assert_eq!(def.mode, TriggerMode::LeavesBattlefield);
        assert_eq!(def.valid_card, Some(TargetFilter::SelfRef));
        assert!(def.trigger_zones.contains(&Zone::Graveyard));
        assert!(def.trigger_zones.contains(&Zone::Exile));
    }

    #[test]
    fn trigger_skyclave_apparition_leaves_battlefield_uses_linked_exile_owner_scope() {
        let def = parse_trigger_line(
            "When this creature leaves the battlefield, the exiled card's owner creates an X/X blue Illusion creature token, where X is the mana value of the exiled card.",
            "Skyclave Apparition",
        );
        assert_eq!(def.mode, TriggerMode::LeavesBattlefield);

        let execute = def.execute.as_deref().expect("execute ability");
        assert_eq!(
            execute.player_scope,
            Some(PlayerFilter::OwnersOfCardsExiledBySource)
        );

        match execute.effect.as_ref() {
            Effect::Token {
                name,
                power,
                toughness,
                ..
            } => {
                assert_eq!(name, "Illusion");
                let expected = QuantityExpr::Ref {
                    qty: QuantityRef::Aggregate {
                        function: crate::types::ability::AggregateFunction::Sum,
                        property: crate::types::ability::ObjectProperty::ManaValue,
                        filter: TargetFilter::And {
                            filters: vec![
                                TargetFilter::ExiledBySource,
                                TargetFilter::Typed(TypedFilter::default().properties(vec![
                                    FilterProp::Owned {
                                        controller: ControllerRef::You,
                                    },
                                ])),
                            ],
                        },
                    },
                };
                assert_eq!(power, &PtValue::Quantity(expected.clone()));
                assert_eq!(toughness, &PtValue::Quantity(expected));
            }
            other => panic!("expected Skyclave leaves trigger to create a token, got {other:?}"),
        }
    }

    /// CR 113.6k: A non-self-referential LTB trigger (source stays on the
    /// battlefield while some other object leaves) must NOT extend its
    /// `trigger_zones` into graveyard/exile — otherwise the trigger would
    /// continue to fire even after its source permanent was removed.
    #[test]
    fn trigger_leaves_battlefield_non_self_ref_keeps_default_zones() {
        let def = parse_trigger_line(
            "Whenever a creature you control leaves the battlefield, each opponent loses 1 life.",
            "Ninja Teen",
        );
        assert_eq!(def.mode, TriggerMode::LeavesBattlefield);
        assert!(
            !def.trigger_zones.contains(&Zone::Graveyard),
            "non-self-ref LTB must not extend to graveyard"
        );
        assert!(
            !def.trigger_zones.contains(&Zone::Exile),
            "non-self-ref LTB must not extend to exile"
        );
    }

    #[test]
    fn trigger_becomes_blocked() {
        let def = parse_trigger_line(
            "Whenever Gustcloak Cavalier becomes blocked, you may untap it and remove it from combat.",
            "Gustcloak Cavalier",
        );
        assert_eq!(def.mode, TriggerMode::BecomesBlocked);
        assert_eq!(def.valid_card, Some(TargetFilter::SelfRef));
    }

    #[test]
    fn trigger_becomes_blocked_pump_scales_with_creatures_blocking_it() {
        let def = parse_trigger_line(
            "Whenever this creature becomes blocked, it gets +2/+2 until end of turn for each creature blocking it.",
            "Gang of Elk",
        );

        assert_eq!(def.mode, TriggerMode::BecomesBlocked);
        assert_eq!(def.valid_card, Some(TargetFilter::SelfRef));
        let execute = def.execute.as_ref().expect("trigger should have effect");
        assert_eq!(execute.duration, Some(Duration::UntilEndOfTurn));
        match execute.effect.as_ref() {
            Effect::Pump {
                power,
                toughness,
                target,
            } => {
                assert_eq!(target, &TargetFilter::SelfRef);
                for value in [power, toughness] {
                    match value {
                        PtValue::Quantity(QuantityExpr::Multiply { factor, inner }) => {
                            assert_eq!(*factor, 2);
                            match inner.as_ref() {
                                QuantityExpr::Ref {
                                    qty: QuantityRef::ObjectCount { filter },
                                } => match filter {
                                    TargetFilter::Typed(tf) => {
                                        assert_eq!(tf.type_filters, vec![TypeFilter::Creature]);
                                        assert_eq!(tf.properties, vec![FilterProp::BlockingSource]);
                                    }
                                    other => panic!("expected Typed filter, got {other:?}"),
                                },
                                other => panic!("expected ObjectCount ref, got {other:?}"),
                            }
                        }
                        other => panic!("expected scaled dynamic P/T value, got {other:?}"),
                    }
                }
            }
            other => panic!("expected Pump, got {other:?}"),
        }
    }

    #[test]
    fn trigger_becomes_blocked_pump_scales_with_blockers_beyond_first() {
        let def = parse_trigger_line(
            "Whenever this creature becomes blocked, it gets -2/-1 until end of turn for each creature blocking it beyond the first.",
            "Johtull Wurm",
        );

        assert_eq!(def.mode, TriggerMode::BecomesBlocked);
        assert_eq!(def.valid_card, Some(TargetFilter::SelfRef));
        let execute = def.execute.as_ref().expect("trigger should have effect");
        assert_eq!(execute.duration, Some(Duration::UntilEndOfTurn));
        match execute.effect.as_ref() {
            Effect::Pump {
                power,
                toughness,
                target,
            } => {
                assert_eq!(target, &TargetFilter::SelfRef);
                assert_eq!(
                    power,
                    &PtValue::Quantity(QuantityExpr::Multiply {
                        factor: -2,
                        inner: Box::new(blocking_source_beyond_first_expr()),
                    })
                );
                assert_eq!(
                    toughness,
                    &PtValue::Quantity(QuantityExpr::Multiply {
                        factor: -1,
                        inner: Box::new(blocking_source_beyond_first_expr()),
                    })
                );
            }
            other => panic!("expected Pump, got {other:?}"),
        }
    }

    #[test]
    fn trigger_is_dealt_damage() {
        let def = parse_trigger_line(
            "Whenever Spitemare is dealt damage, it deals that much damage to any target.",
            "Spitemare",
        );
        assert_eq!(def.mode, TriggerMode::DamageReceived);
        assert_eq!(def.damage_kind, DamageKindFilter::Any);
    }

    #[test]
    fn trigger_is_dealt_combat_damage() {
        let def = parse_trigger_line(
            "Whenever ~ is dealt combat damage, draw a card.",
            "Some Card",
        );
        assert_eq!(def.mode, TriggerMode::DamageReceived);
        assert_eq!(def.damage_kind, DamageKindFilter::CombatOnly);
    }

    #[test]
    fn trigger_you_attack() {
        let def = parse_trigger_line(
            "Whenever you attack, create a 1/1 white Soldier creature token.",
            "Some Card",
        );
        assert_eq!(def.mode, TriggerMode::YouAttack);
    }

    // CR 508.1 + CR 603.7c: a delayed "Whenever you attack this turn" trigger is
    // prefix-stripped to the bare condition "you attack" before reaching
    // `parse_trigger_condition`. Bare "you attack" must resolve to YouAttack, not
    // Unknown — the #433 root cause (Dalkovan Encampment).
    #[test]
    fn trigger_condition_bare_you_attack() {
        let mut ctx = ParseContext::default();
        let (mode, _def) = parse_trigger_condition("you attack", &mut ctx);
        assert_eq!(mode, TriggerMode::YouAttack);
    }

    // The word-boundary guard: "you attacked this turn" is a condition, not a
    // trigger, and must NOT be recognized as a YouAttack trigger.
    #[test]
    fn trigger_condition_you_attacked_is_not_a_trigger() {
        let mut ctx = ParseContext::default();
        let (mode, _def) = parse_trigger_condition("you attacked this turn", &mut ctx);
        assert_ne!(mode, TriggerMode::YouAttack);
    }

    // CR 603.7c: the full Dalkovan Encampment activated ability — the inner
    // "Whenever you attack this turn, ..." clause is an effect-body delayed
    // trigger, so it builds a CreateDelayedTrigger whose WheneverEvent trigger
    // has mode YouAttack (previously Unknown — the #433 bug).
    #[test]
    fn trigger_dalkovan_encampment_delayed_you_attack() {
        use crate::parser::oracle::parse_oracle_text;

        let parsed = parse_oracle_text(
            "{2}{W}, {T}: Whenever you attack this turn, create two 1/1 red \
             Warrior creature tokens that are tapped and attacking. Sacrifice \
             them at the beginning of the next end step.",
            "Dalkovan Encampment",
            &[],
            &["Land".to_string()],
            &[],
        );
        assert_eq!(parsed.abilities.len(), 1);

        let delayed_effect = parsed.abilities[0].effect.as_ref();
        let Effect::CreateDelayedTrigger { condition, .. } = delayed_effect else {
            panic!("expected CreateDelayedTrigger, got {delayed_effect:?}");
        };
        let DelayedTriggerCondition::WheneverEvent { trigger } = condition else {
            panic!("expected WheneverEvent, got {condition:?}");
        };
        assert_eq!(trigger.mode, TriggerMode::YouAttack);
    }

    #[test]
    fn trigger_becomes_tapped() {
        let def = parse_trigger_line(
            "Whenever Night Market Lookout becomes tapped, each opponent loses 1 life and you gain 1 life.",
            "Night Market Lookout",
        );
        assert_eq!(def.mode, TriggerMode::Taps);
        assert_eq!(def.valid_card, Some(TargetFilter::SelfRef));
    }

    #[test]
    fn trigger_you_cast_this_spell() {
        let def = parse_trigger_line(
            "When you cast this spell, draw cards equal to the greatest power among creatures you control.",
            "Hydroid Krasis",
        );
        assert_eq!(def.mode, TriggerMode::SpellCast);
        assert_eq!(def.valid_card, Some(TargetFilter::SelfRef));
        assert!(def.trigger_zones.contains(&Zone::Stack));
    }

    /// CR 601.2i + CR 603.2: A self-cast trigger phrased as
    /// "When you cast <CARDNAME>" must produce the same `TriggerMode::SpellCast`
    /// shape (with `valid_card = SelfRef`) as the canonical "When you cast this
    /// spell" templating. After `normalize_card_name_refs` (CR 201.5), the card
    /// name becomes `~`, so the parser sees "when you cast ~" — a class-level
    /// pattern covering every aura/permanent whose cast trigger references
    /// itself by name.
    #[test]
    fn trigger_you_cast_self_by_name() {
        let def = parse_trigger_line(
            "When you cast Taught by Surrak, draw a card.",
            "Taught by Surrak",
        );
        assert_eq!(def.mode, TriggerMode::SpellCast);
        assert_eq!(def.valid_card, Some(TargetFilter::SelfRef));
        assert!(def.trigger_zones.contains(&Zone::Stack));
    }

    /// CR 601.2i + CR 603.2: Mirror of `trigger_you_cast_self_by_name` for the
    /// "Whenever" prefix. Both keywords are valid cast-time trigger phrasings
    /// (CR 603.1) and must produce identical SpellCast definitions.
    #[test]
    fn trigger_whenever_you_cast_self_by_name() {
        let def = parse_trigger_line("Whenever you cast Test Card, draw a card.", "Test Card");
        assert_eq!(def.mode, TriggerMode::SpellCast);
        assert_eq!(def.valid_card, Some(TargetFilter::SelfRef));
        assert!(def.trigger_zones.contains(&Zone::Stack));
    }

    /// CR 601.2i + CR 603.2: Passive "Whenever ~ is cast" phrasing — the third
    /// cell of the {when, whenever} × {you cast this spell, you cast ~, ~ is
    /// cast} cross-product. Exercises the composed `alt × alt` combinator path.
    #[test]
    fn trigger_whenever_self_is_cast() {
        let def = parse_trigger_line("Whenever Test Card is cast, draw a card.", "Test Card");
        assert_eq!(def.mode, TriggerMode::SpellCast);
        assert_eq!(def.valid_card, Some(TargetFilter::SelfRef));
        assert!(def.trigger_zones.contains(&Zone::Stack));
    }

    #[test]
    fn trigger_opponent_casts_multicolored_spell() {
        let def = parse_trigger_line(
            "Whenever an opponent casts a multicolored spell, you gain 1 life.",
            "Soldier of the Pantheon",
        );
        assert_eq!(def.mode, TriggerMode::SpellCast);
        assert_eq!(
            def.valid_target,
            Some(TargetFilter::Typed(
                TypedFilter::default().controller(ControllerRef::Opponent)
            ))
        );
        assert_eq!(
            def.valid_card,
            Some(TargetFilter::Typed(TypedFilter::default().properties(
                vec![FilterProp::ColorCount {
                    comparator: Comparator::GE,
                    count: 2,
                }]
            )))
        );
    }

    #[test]
    fn trigger_mana_cannons_uses_triggering_spell_color_count() {
        let def = parse_trigger_line(
            "Whenever you cast a multicolored spell, this enchantment deals X damage to any target, where X is the number of colors that spell is.",
            "Mana Cannons",
        );
        assert_eq!(def.mode, TriggerMode::SpellCast);
        assert_eq!(
            def.valid_card,
            Some(TargetFilter::Typed(
                TypedFilter::new(TypeFilter::Card).properties(vec![FilterProp::ColorCount {
                    comparator: Comparator::GE,
                    count: 2,
                }])
            ))
        );
        let execute = def.execute.as_deref().expect("trigger should execute");
        match execute.effect.as_ref() {
            Effect::DealDamage { amount, target, .. } => {
                assert_eq!(*target, TargetFilter::Any);
                assert_eq!(
                    *amount,
                    QuantityExpr::Ref {
                        qty: QuantityRef::ObjectColorCount {
                            scope: ObjectScope::EventSource
                        }
                    }
                );
            }
            other => panic!("expected DealDamage, got {other:?}"),
        }
    }

    #[test]
    fn trigger_you_cast_aura_spell() {
        let def = parse_trigger_line(
            "Whenever you cast an Aura spell, you may draw a card.",
            "Kor Spiritdancer",
        );
        assert_eq!(def.mode, TriggerMode::SpellCast);
        // Must restrict to Aura subtype
        assert_eq!(
            def.valid_card,
            Some(TargetFilter::Typed(
                TypedFilter::default().subtype("Aura".to_string())
            ))
        );
        // Must restrict to controller's spells
        assert_eq!(def.valid_target, Some(TargetFilter::Controller));
    }

    #[test]
    fn trigger_you_cast_creature_spell() {
        let def = parse_trigger_line(
            "Whenever you cast a creature spell, draw a card.",
            "Beast Whisperer",
        );
        assert_eq!(def.mode, TriggerMode::SpellCast);
        assert_eq!(
            def.valid_card,
            Some(TargetFilter::Typed(TypedFilter::new(TypeFilter::Creature)))
        );
        assert_eq!(def.valid_target, Some(TargetFilter::Controller));
    }

    #[test]
    fn trigger_you_cast_a_spell_no_type() {
        let def = parse_trigger_line("Whenever you cast a spell, add {C}.", "Conduit of Ruin");
        assert_eq!(def.mode, TriggerMode::SpellCast);
        // No type restriction
        assert!(def.valid_card.is_none());
        // But still restricted to controller
        assert_eq!(def.valid_target, Some(TargetFilter::Controller));
        // No origin clause → no zone restriction (CR 601.2a).
        assert_eq!(def.spell_cast_origin, OriginConstraint::Any);
    }

    /// CR 601.2a + #538: Ghostly Pilferer. The cast-origin discriminator
    /// "from anywhere other than their hand" must survive parsing as
    /// `NotEquals(Hand)`; without it the trigger fires on every opponent
    /// hand cast.
    #[test]
    fn trigger_opponent_casts_from_anywhere_other_than_hand() {
        let def = parse_trigger_line(
            "Whenever an opponent casts a spell from anywhere other than their hand, draw a card.",
            "Ghostly Pilferer",
        );
        assert_eq!(def.mode, TriggerMode::SpellCast);
        assert_eq!(
            def.valid_target,
            Some(TargetFilter::Typed(
                TypedFilter::default().controller(ControllerRef::Opponent)
            ))
        );
        assert_eq!(
            def.spell_cast_origin,
            OriginConstraint::NotEquals(Zone::Hand)
        );
    }

    /// CR 601.2a — positive cast-origin form. Snapcaster-class triggers
    /// must parse as `Equals(Graveyard)`.
    #[test]
    fn trigger_you_cast_a_spell_from_your_graveyard() {
        let def = parse_trigger_line(
            "Whenever you cast a spell from your graveyard, draw a card.",
            "Test Card",
        );
        assert_eq!(def.mode, TriggerMode::SpellCast);
        assert_eq!(def.valid_target, Some(TargetFilter::Controller));
        assert_eq!(
            def.spell_cast_origin,
            OriginConstraint::Equals(Zone::Graveyard)
        );
    }

    /// CR 205.2a + CR 601.2: "whenever you cast an artifact creature spell" must
    /// AND both core types into `valid_card`, so a non-creature artifact spell
    /// does NOT fire the trigger. Regression for Lux Artillery, whose spell-cast
    /// trigger incorrectly accepted any artifact spell.
    #[test]
    fn trigger_you_cast_artifact_creature_spell() {
        let def = parse_trigger_line(
            "Whenever you cast an artifact creature spell, it gains sunburst.",
            "Lux Artillery",
        );
        assert_eq!(def.mode, TriggerMode::SpellCast);
        let Some(TargetFilter::Typed(tf)) = &def.valid_card else {
            panic!("expected Typed valid_card, got {:?}", def.valid_card);
        };
        assert!(
            tf.type_filters.contains(&TypeFilter::Artifact),
            "expected Artifact in {:?}",
            tf.type_filters
        );
        assert!(
            tf.type_filters.contains(&TypeFilter::Creature),
            "expected Creature in {:?}",
            tf.type_filters
        );
    }

    /// CR 205.2a + CR 205.4a + CR 601.2: "whenever you cast a legendary creature
    /// spell" — supertype lives in properties, not type_filters.
    #[test]
    fn trigger_you_cast_legendary_creature_spell() {
        let def = parse_trigger_line(
            "Whenever you cast a legendary creature spell, draw a card.",
            "Test Card",
        );
        assert_eq!(def.mode, TriggerMode::SpellCast);
        let Some(TargetFilter::Typed(tf)) = &def.valid_card else {
            panic!("expected Typed valid_card, got {:?}", def.valid_card);
        };
        assert!(tf.type_filters.contains(&TypeFilter::Creature));
        assert!(
            tf.properties.iter().any(|prop| matches!(
                prop,
                FilterProp::HasSupertype {
                    value: crate::types::card_type::Supertype::Legendary
                }
            )),
            "expected HasSupertype(Legendary) in {:?}",
            tf.properties
        );
    }

    /// CR 205.2a + CR 205.4b + CR 601.2: "whenever you cast a noncreature
    /// artifact spell" — Non(Creature) + Artifact conjunction.
    #[test]
    fn trigger_you_cast_noncreature_artifact_spell() {
        let def = parse_trigger_line(
            "Whenever you cast a noncreature artifact spell, draw a card.",
            "Test Card",
        );
        assert_eq!(def.mode, TriggerMode::SpellCast);
        let Some(TargetFilter::Typed(tf)) = &def.valid_card else {
            panic!("expected Typed valid_card, got {:?}", def.valid_card);
        };
        assert!(tf.type_filters.contains(&TypeFilter::Artifact));
        assert!(
            tf.type_filters
                .contains(&TypeFilter::Non(Box::new(TypeFilter::Creature))),
            "expected Non(Creature) in {:?}",
            tf.type_filters
        );
    }

    /// CR 603.4 + CR 122.1: "at the beginning of your end step, if there are
    /// thirty or more counters among artifacts and creatures you control, ..."
    /// — intervening-if with counter-count condition that sums across every
    /// counter type on the matching permanents. Regression for Lux Artillery's
    /// second trigger, which previously produced `condition: null` and fired
    /// every end step unconditionally.
    #[test]
    fn trigger_intervening_if_counters_among_filter() {
        let def = parse_trigger_line(
            "At the beginning of your end step, if there are thirty or more counters among artifacts and creatures you control, this artifact deals 10 damage to each opponent.",
            "Lux Artillery",
        );
        assert_eq!(def.mode, TriggerMode::Phase);
        assert_eq!(def.phase, Some(Phase::End));
        let Some(TriggerCondition::QuantityComparison {
            lhs,
            comparator,
            rhs,
        }) = &def.condition
        else {
            panic!(
                "expected QuantityComparison intervening-if, got {:?}",
                def.condition
            );
        };
        assert_eq!(*comparator, Comparator::GE);
        assert_eq!(*rhs, QuantityExpr::Fixed { value: 30 });
        let QuantityExpr::Ref {
            qty:
                QuantityRef::CountersOnObjects {
                    counter_type,
                    filter,
                },
        } = lhs
        else {
            panic!("expected CountersOnObjects lhs, got {lhs:?}");
        };
        assert!(
            counter_type.is_none(),
            "expected any-counter-type (None), got {counter_type:?}"
        );
        // Filter should be an Or of (artifact you control) ∪ (creature you control).
        let TargetFilter::Or { filters } = filter else {
            panic!("expected Or filter for 'artifacts and creatures you control', got {filter:?}");
        };
        assert_eq!(filters.len(), 2);
        assert!(filters.iter().any(|f| matches!(
            f,
            TargetFilter::Typed(tf)
                if tf.type_filters.contains(&TypeFilter::Artifact)
                    && tf.controller == Some(ControllerRef::You)
        )));
        assert!(filters.iter().any(|f| matches!(
            f,
            TargetFilter::Typed(tf)
                if tf.type_filters.contains(&TypeFilter::Creature)
                    && tf.controller == Some(ControllerRef::You)
        )));
    }

    /// CR 603.4 + CR 701.9: "at the beginning of each end step, if an opponent
    /// discarded a card this turn, ..." — intervening-if must be hoisted as a
    /// scoped `CardsDiscardedThisTurn` quantity comparison. Regression
    /// for Tinybones, Trinket Thief (previously `condition: null`).
    #[test]
    fn trigger_intervening_if_opponent_discarded_this_turn() {
        let def = parse_trigger_line(
            "At the beginning of each end step, if an opponent discarded a card this turn, you draw a card and you lose 1 life.",
            "Tinybones, Trinket Thief",
        );
        assert_eq!(def.mode, TriggerMode::Phase);
        assert_eq!(def.phase, Some(Phase::End));
        let Some(TriggerCondition::QuantityComparison {
            lhs,
            comparator,
            rhs,
        }) = &def.condition
        else {
            panic!(
                "expected QuantityComparison intervening-if, got {:?}",
                def.condition
            );
        };
        assert_eq!(*comparator, Comparator::GE);
        assert_eq!(*rhs, QuantityExpr::Fixed { value: 1 });
        assert_eq!(
            *lhs,
            QuantityExpr::Ref {
                qty: QuantityRef::CardsDiscardedThisTurn {
                    player: PlayerScope::Opponent {
                        aggregate: AggregateFunction::Sum,
                    },
                },
            }
        );
    }

    #[test]
    fn trigger_intervening_if_card_left_your_graveyard_this_turn() {
        let def = parse_trigger_line(
            "At the beginning of your end step, if a card left your graveyard this turn, draw a card.",
            "Primary Research",
        );
        assert_eq!(def.mode, TriggerMode::Phase);
        assert_eq!(def.phase, Some(Phase::End));
        let Some(TriggerCondition::QuantityComparison {
            lhs,
            comparator,
            rhs,
        }) = &def.condition
        else {
            panic!(
                "expected QuantityComparison intervening-if, got {:?}",
                def.condition
            );
        };
        assert_eq!(*comparator, Comparator::GE);
        assert_eq!(*rhs, QuantityExpr::Fixed { value: 1 });
        assert!(matches!(
            lhs,
            QuantityExpr::Ref {
                qty: QuantityRef::ZoneChangeCountThisTurn {
                    from: Some(Zone::Graveyard),
                    to: None,
                    ..
                }
            }
        ));
    }

    #[test]
    fn trigger_intervening_if_permanent_was_put_into_your_hand_from_battlefield() {
        let def = parse_trigger_line(
            "At the beginning of your end step, if a permanent was put into your hand from the battlefield this turn, draw a card.",
            "Barrin, Tolarian Archmage",
        );
        let Some(TriggerCondition::QuantityComparison {
            lhs,
            comparator,
            rhs,
        }) = &def.condition
        else {
            panic!(
                "expected QuantityComparison intervening-if, got {:?}",
                def.condition
            );
        };
        assert_eq!(*comparator, Comparator::GE);
        assert_eq!(*rhs, QuantityExpr::Fixed { value: 1 });
        assert!(matches!(
            lhs,
            QuantityExpr::Ref {
                qty: QuantityRef::ZoneChangeCountThisTurn {
                    from: Some(Zone::Battlefield),
                    to: Some(Zone::Hand),
                    ..
                }
            }
        ));
    }

    #[test]
    fn trigger_intervening_if_artifact_or_creature_was_put_into_graveyard_from_battlefield() {
        let def = parse_trigger_line(
            "At the beginning of your end step, if an artifact or creature was put into a graveyard from the battlefield this turn, put a +1/+1 counter on this creature.",
            "Ichor Shade",
        );
        assert!(matches!(
            def.condition,
            Some(TriggerCondition::QuantityComparison {
                lhs: QuantityExpr::Ref {
                    qty: QuantityRef::ZoneChangeCountThisTurn {
                        from: Some(Zone::Battlefield),
                        to: Some(Zone::Graveyard),
                        filter: TargetFilter::Or { .. },
                    },
                },
                comparator: Comparator::GE,
                rhs: QuantityExpr::Fixed { value: 1 },
            })
        ));
    }

    #[test]
    fn trigger_intervening_if_source_dealt_damage_to_opponent_this_turn() {
        let def = parse_trigger_line(
            "At the beginning of each end step, if this creature dealt damage to an opponent this turn, put a +1/+1 counter on it.",
            "Dunerider Outlaw",
        );
        let Some(TriggerCondition::QuantityComparison {
            lhs,
            comparator,
            rhs,
        }) = &def.condition
        else {
            panic!(
                "expected QuantityComparison intervening-if, got {:?}",
                def.condition
            );
        };
        assert_eq!(*comparator, Comparator::GE);
        assert_eq!(*rhs, QuantityExpr::Fixed { value: 1 });
        assert!(matches!(
            lhs,
            QuantityExpr::Ref {
                qty: QuantityRef::DamageDealtThisTurn {
                    source,
                    ..
                }
            } if **source == TargetFilter::SelfRef
        ));
    }

    #[test]
    fn trigger_intervening_if_source_was_dealt_damage_this_turn() {
        let def = parse_trigger_line(
            "At the beginning of each end step, if this creature was dealt damage this turn, put a +0/+1 counter on it.",
            "Wall of Resistance",
        );
        assert!(matches!(
            def.condition,
            Some(TriggerCondition::QuantityComparison {
                lhs: QuantityExpr::Ref {
                    qty: QuantityRef::DamageDealtThisTurn {
                        source,
                        target,
                        ..
                    },
                },
                comparator: Comparator::GE,
                rhs: QuantityExpr::Fixed { value: 1 },
            }) if *source == TargetFilter::Any && *target == TargetFilter::SelfRef
        ));
    }

    #[test]
    fn trigger_intervening_if_counter_was_put_on_owned_permanent_this_turn() {
        let def = parse_trigger_line(
            "At the beginning of each end step, if a +1/+1 counter was put on a permanent under your control this turn, put a +1/+1 counter on this creature.",
            "Fairgrounds Trumpeter",
        );
        assert!(matches!(
            def.condition,
            Some(TriggerCondition::QuantityComparison {
                lhs: QuantityExpr::Ref {
                    qty: QuantityRef::CounterAddedThisTurn {
                        actor: CountScope::All,
                        counters: CounterMatch::OfType(CounterType::Plus1Plus1),
                        ..
                    },
                },
                comparator: Comparator::GE,
                rhs: QuantityExpr::Fixed { value: 1 },
            })
        ));
    }

    // --- ControlCount condition tests ---

    #[test]
    fn trigger_leonin_vanguard_control_creature_count() {
        let def = parse_trigger_line(
            "At the beginning of combat on your turn, if you control three or more creatures, this creature gets +1/+1 until end of turn and you gain 1 life.",
            "Leonin Vanguard",
        );
        assert_eq!(def.mode, TriggerMode::Phase);
        assert_eq!(def.phase, Some(Phase::BeginCombat));
        assert_eq!(def.constraint, Some(TriggerConstraint::OnlyDuringYourTurn));
        assert!(
            matches!(
                def.condition,
                Some(TriggerCondition::QuantityComparison {
                    lhs: QuantityExpr::Ref {
                        qty: QuantityRef::ObjectCount { .. },
                    },
                    comparator: Comparator::GE,
                    rhs: QuantityExpr::Fixed { value: 3 },
                })
            ),
            "Expected QuantityComparison with ObjectCount >= 3, got {:?}",
            def.condition
        );
        // Effect: pump self +1/+1 with life gain sub_ability
        let exec = def.execute.as_ref().expect("should have execute");
        assert!(matches!(
            *exec.effect,
            Effect::Pump {
                power: PtValue::Fixed(1),
                toughness: PtValue::Fixed(1),
                target: TargetFilter::SelfRef,
            }
        ));
        assert_eq!(exec.duration, Some(Duration::UntilEndOfTurn));
        // Sub-ability: gain 1 life
        let sub = exec.sub_ability.as_ref().expect("should have sub_ability");
        assert!(matches!(
            *sub.effect,
            Effect::GainLife {
                amount: QuantityExpr::Fixed { value: 1 },
                ..
            }
        ));
    }

    #[test]
    fn extract_if_control_creature_count() {
        let (cleaned, cond) = extract_if_condition(
            "if you control three or more creatures, ~ gets +1/+1 until end of turn",
        );
        assert_eq!(cleaned, "~ gets +1/+1 until end of turn");
        // The canonical combinator produces QuantityComparison with ObjectCount.
        let cond = cond.expect("should have condition");
        assert!(
            matches!(
                cond,
                TriggerCondition::QuantityComparison {
                    lhs: QuantityExpr::Ref {
                        qty: QuantityRef::ObjectCount { .. },
                    },
                    comparator: Comparator::GE,
                    rhs: QuantityExpr::Fixed { value: 3 },
                }
            ),
            "Expected QuantityComparison with ObjectCount >= 3, got {cond:?}"
        );
    }

    // --- Equipment / Aura subject filter tests ---

    #[test]
    fn trigger_equipped_creature_attacks() {
        let def = parse_trigger_line(
            "Whenever equipped creature attacks, put a +1/+1 counter on it.",
            "Blackblade Reforged",
        );
        assert_eq!(def.mode, TriggerMode::Attacks);
        assert_eq!(def.valid_card, Some(TargetFilter::AttachedTo));
    }

    #[test]
    fn trigger_equipped_creature_deals_combat_damage() {
        let def = parse_trigger_line(
            "Whenever equipped creature deals combat damage to a player, draw a card.",
            "Shadowspear",
        );
        assert_eq!(def.mode, TriggerMode::DamageDone);
        assert_eq!(def.damage_kind, DamageKindFilter::CombatOnly);
        assert_eq!(def.valid_source, Some(TargetFilter::AttachedTo));
        assert_eq!(def.valid_target, Some(TargetFilter::Player));
    }

    #[test]
    fn trigger_equipped_creature_dies() {
        let def = parse_trigger_line(
            "Whenever equipped creature dies, you gain 2 life.",
            "Strider Harness",
        );
        assert_eq!(def.mode, TriggerMode::ChangesZone);
        assert_eq!(def.origin, Some(Zone::Battlefield));
        assert_eq!(def.destination, Some(Zone::Graveyard));
        assert_eq!(def.valid_card, Some(TargetFilter::AttachedTo));
    }

    #[test]
    fn trigger_enchanted_creature_attacks() {
        let def = parse_trigger_line(
            "Whenever enchanted creature attacks, draw a card.",
            "Curiosity",
        );
        assert_eq!(def.mode, TriggerMode::Attacks);
        assert_eq!(def.valid_card, Some(TargetFilter::AttachedTo));
    }

    #[test]
    fn trigger_enchanted_creature_dies() {
        let def = parse_trigger_line(
            "Whenever enchanted creature dies, return ~ to its owner's hand.",
            "Angelic Destiny",
        );
        assert_eq!(def.mode, TriggerMode::ChangesZone);
        assert_eq!(def.origin, Some(Zone::Battlefield));
        assert_eq!(def.destination, Some(Zone::Graveyard));
        assert_eq!(def.valid_card, Some(TargetFilter::AttachedTo));
    }

    // CR 303.4 + CR 603.10a: "Whenever an enchanted creature dies" with the
    // indefinite article is NON-source-relative (the source isn't the Aura).
    // The subject filter must be a typed creature with `EnchantedBy` so runtime
    // interprets it as "has any Aura attached" (Hateful Eidolon).
    #[test]
    fn trigger_an_enchanted_creature_dies_hateful_eidolon() {
        let def = parse_trigger_line(
            "Whenever an enchanted creature dies, draw a card for each Aura you controlled that was attached to it.",
            "Hateful Eidolon",
        );
        assert_eq!(def.mode, TriggerMode::ChangesZone);
        assert_eq!(def.origin, Some(Zone::Battlefield));
        assert_eq!(def.destination, Some(Zone::Graveyard));
        let expected =
            TargetFilter::Typed(TypedFilter::creature().properties(vec![FilterProp::EnchantedBy]));
        assert_eq!(def.valid_card, Some(expected));
    }

    // CR 303.4: "Creatures that are enchanted by an Aura you control" subject filter.
    #[test]
    fn trigger_one_or_more_enchanted_creatures_attack_killian() {
        let def = parse_trigger_line(
            "Whenever one or more creatures that are enchanted by an Aura you control attack, draw a card.",
            "Killian, Decisive Mentor",
        );
        assert_eq!(def.mode, TriggerMode::YouAttack);
        let filter = def.valid_card.as_ref().expect("valid_card set");
        match filter {
            TargetFilter::Typed(tf) => {
                assert!(tf.type_filters.contains(&TypeFilter::Creature));
                let has_attachment = tf.properties.iter().any(|p| {
                    matches!(
                        p,
                        FilterProp::HasAttachment {
                            kind: AttachmentKind::Aura,
                            controller: Some(ControllerRef::You),
                        }
                    )
                });
                assert!(
                    has_attachment,
                    "expected HasAttachment(Aura, You); got {:?}",
                    tf.properties
                );
            }
            other => panic!("expected Typed filter, got {other:?}"),
        }
    }

    #[test]
    fn trigger_cycle_this_card() {
        let def = parse_trigger_line(
            "When you cycle this card, draw a card.",
            "Decree of Justice",
        );
        assert_eq!(def.mode, TriggerMode::Cycled);
        assert_eq!(def.valid_card, Some(TargetFilter::SelfRef));
        assert!(def.trigger_zones.contains(&Zone::Graveyard));
    }

    #[test]
    fn trigger_cycle_self_ref() {
        let def = parse_trigger_line(
            "When you cycle ~, you may draw a card.",
            "Decree of Justice",
        );
        assert_eq!(def.mode, TriggerMode::Cycled);
        assert_eq!(def.valid_card, Some(TargetFilter::SelfRef));
        assert!(def.trigger_zones.contains(&Zone::Graveyard));
        assert!(def.optional);
    }

    #[test]
    fn trigger_cycle_another_card() {
        // CR 702.29: "Whenever you cycle another card" — Drannith Stinger
        let def = parse_trigger_line(
            "Whenever you cycle another card, this creature deals 1 damage to each opponent.",
            "Drannith Stinger",
        );
        assert_eq!(def.mode, TriggerMode::Cycled);
        assert_eq!(def.valid_target, Some(TargetFilter::Controller));
        assert!(matches!(
            &def.valid_card,
            Some(TargetFilter::Typed(tf)) if tf.properties.contains(&FilterProp::Another)
        ));
    }

    #[test]
    fn trigger_cycle_or_discard_a_card() {
        // CR 702.29d: "Whenever you cycle or discard a card" — Drake Haven
        let def = parse_trigger_line(
            "Whenever you cycle or discard a card, you may pay {1}. If you do, create a 2/2 blue Drake creature token with flying.",
            "Drake Haven",
        );
        assert_eq!(def.mode, TriggerMode::CycledOrDiscarded);
        assert_eq!(def.valid_target, Some(TargetFilter::Controller));
    }

    #[test]
    fn trigger_cycle_or_discard_another_card() {
        // CR 702.29d: "Whenever you cycle or discard another card" — Horror of the Broken Lands
        let def = parse_trigger_line(
            "Whenever you cycle or discard another card, this creature gets +2/+1 until end of turn.",
            "Horror of the Broken Lands",
        );
        assert_eq!(def.mode, TriggerMode::CycledOrDiscarded);
        assert_eq!(def.valid_target, Some(TargetFilter::Controller));
        assert!(matches!(
            &def.valid_card,
            Some(TargetFilter::Typed(tf)) if tf.properties.contains(&FilterProp::Another)
        ));
    }

    #[test]
    fn trigger_when_you_cast_this_spell_if_youve_cast_another_spell_this_turn() {
        let def = parse_trigger_line(
            "When you cast this spell, if you've cast another spell this turn, copy it.",
            "Sage of the Skies",
        );
        assert_eq!(def.mode, TriggerMode::SpellCast);
        assert_eq!(def.valid_card, Some(TargetFilter::SelfRef));
        assert_eq!(def.trigger_zones, vec![Zone::Stack]);
        assert_eq!(
            def.condition,
            Some(TriggerCondition::QuantityComparison {
                lhs: QuantityExpr::Ref {
                    qty: QuantityRef::SpellsCastThisTurn {
                        scope: crate::types::ability::CountScope::Controller,
                        filter: None
                    },
                },
                comparator: Comparator::GE,
                rhs: QuantityExpr::Fixed { value: 2 },
            })
        );
    }

    #[test]
    fn trigger_attack_if_cast_spell_with_mana_value_this_turn() {
        let def = parse_trigger_line(
            "Whenever this creature attacks, if you've cast a spell with mana value 4 or greater this turn, draw a card.",
            "Rhino, Barreling Brute",
        );
        assert_eq!(def.mode, TriggerMode::Attacks);
        assert_eq!(
            def.condition,
            Some(TriggerCondition::QuantityComparison {
                lhs: QuantityExpr::Ref {
                    qty: QuantityRef::SpellsCastThisTurn {
                        scope: crate::types::ability::CountScope::Controller,
                        filter: Some(TargetFilter::Typed(TypedFilter::card().properties(vec![
                            FilterProp::Cmc {
                                comparator: Comparator::GE,
                                value: QuantityExpr::Fixed { value: 4 },
                            }
                        ]))),
                    },
                },
                comparator: Comparator::GE,
                rhs: QuantityExpr::Fixed { value: 1 },
            })
        );
    }

    #[test]
    fn trigger_end_step_if_cast_both_creature_and_noncreature_this_turn() {
        let def = parse_trigger_line(
            "At the beginning of each end step, if you've cast both a creature spell and a noncreature spell this turn, create a Clue token.",
            "Fae Offering",
        );
        assert_eq!(def.mode, TriggerMode::Phase);
        match def.condition {
            Some(TriggerCondition::And { conditions }) => {
                assert_eq!(conditions.len(), 2);
                assert!(conditions.iter().any(|condition| matches!(
                    condition,
                    TriggerCondition::QuantityComparison {
                        lhs: QuantityExpr::Ref {
                            qty: QuantityRef::SpellsCastThisTurn {
                                scope: crate::types::ability::CountScope::Controller,
                                filter: Some(TargetFilter::Typed(TypedFilter { type_filters, .. })),
                            },
                        },
                        comparator: Comparator::GE,
                        rhs: QuantityExpr::Fixed { value: 1 },
                    } if type_filters == &vec![TypeFilter::Creature]
                )));
                assert!(conditions.iter().any(|condition| matches!(
                    condition,
                    TriggerCondition::QuantityComparison {
                        lhs: QuantityExpr::Ref {
                            qty: QuantityRef::SpellsCastThisTurn {
                                scope: crate::types::ability::CountScope::Controller,
                                filter: Some(TargetFilter::Typed(TypedFilter { type_filters, .. })),
                            },
                        },
                        comparator: Comparator::GE,
                        rhs: QuantityExpr::Fixed { value: 1 },
                    } if type_filters == &vec![TypeFilter::Non(Box::new(TypeFilter::Creature))]
                )));
            }
            other => panic!("expected compound trigger condition, got {other:?}"),
        }
    }

    #[test]
    fn trigger_opponent_draws_a_card() {
        let def = parse_trigger_line(
            "Whenever an opponent draws a card, you gain 1 life.",
            "Underworld Dreams",
        );
        assert_eq!(def.mode, TriggerMode::Drawn);
        assert_eq!(
            def.valid_target,
            Some(TargetFilter::Typed(
                TypedFilter::default().controller(ControllerRef::Opponent)
            ))
        );
    }

    /// CR 603.4 + CR 102.1: "draws a card during their turn" — the trailing
    /// timing tail restricts the trigger to the drawing player's own turn
    /// (issue #403 defect 2a).
    #[test]
    fn trigger_draws_a_card_during_their_turn_attaches_actors_turn_timing() {
        let def = parse_trigger_line(
            "Whenever a player draws a card during their turn, you draw a card.",
            "Test Card",
        );
        assert_eq!(def.mode, TriggerMode::Drawn);
        assert_eq!(
            def.condition,
            Some(TriggerCondition::DuringPlayersTurn {
                player: PlayerFilter::TriggeringPlayer,
            }),
            "the 'during their turn' tail must become a DuringPlayersTurn intervening-if"
        );
    }

    /// CR 603.4 + CR 102.1: "gains life during their turn" — same timing-tail
    /// composition for the life-gain predicate (issue #403 defect 2a).
    #[test]
    fn trigger_gains_life_during_their_turn_attaches_actors_turn_timing() {
        let def = parse_trigger_line(
            "Whenever a player gains life during their turn, you draw a card.",
            "Test Card",
        );
        assert_eq!(def.mode, TriggerMode::LifeGained);
        assert_eq!(
            def.condition,
            Some(TriggerCondition::DuringPlayersTurn {
                player: PlayerFilter::TriggeringPlayer,
            })
        );
    }

    /// Regression guard: a plain draw trigger with no timing tail must remain
    /// unconditional.
    #[test]
    fn trigger_draws_a_card_without_timing_tail_has_no_condition() {
        let def = parse_trigger_line(
            "Whenever a player draws a card, you draw a card.",
            "Test Card",
        );
        assert_eq!(def.mode, TriggerMode::Drawn);
        assert_eq!(def.condition, None);
    }

    #[test]
    fn trigger_you_draw_a_card_scopes_to_controller() {
        // CR 121.1 + CR 603.2: Sheoldred's first trigger must scope to the
        // controller so it does not fire on opponent draws.
        let def = parse_trigger_line(
            "Whenever you draw a card, you gain 2 life.",
            "Sheoldred, the Apocalypse",
        );
        assert_eq!(def.mode, TriggerMode::Drawn);
        assert_eq!(def.valid_target, Some(TargetFilter::Controller));
    }

    #[test]
    fn trigger_opponent_loses_life_exquisite_blood() {
        // CR 119.3 + CR 603.2 + CR 603.7c: Exquisite Blood — opponent-scoped
        // life-loss trigger whose effect reads "that much" from the event.
        let def = parse_trigger_line(
            "Whenever an opponent loses life, you gain that much life.",
            "Exquisite Blood",
        );
        assert_eq!(def.mode, TriggerMode::LifeLost);
        assert_eq!(
            def.valid_target,
            Some(TargetFilter::Typed(
                TypedFilter::default().controller(ControllerRef::Opponent)
            ))
        );
        // Effect amount should be the triggering event's amount, not Fixed 1.
        let execute = def.execute.as_ref().expect("execute ability present");
        match &*execute.effect {
            crate::types::ability::Effect::GainLife { amount, player } => {
                assert_eq!(
                    amount,
                    &QuantityExpr::Ref {
                        qty: QuantityRef::EventContextAmount,
                    },
                    "Exquisite Blood: gain amount must reference event's life-loss amount"
                );
                // CR 608.2k: "you gain" — effect-level subject is the ability
                // controller. Must NOT be rerouted to the trigger's opponent
                // subject by a post-hoc `player_scope` override.
                assert_eq!(
                    player,
                    &crate::types::ability::GainLifePlayer::Controller,
                    "Exquisite Blood: 'you gain' recipient must be the ability controller"
                );
            }
            other => panic!("expected GainLife effect, got {other:?}"),
        }
        // CR 113.3c + CR 608.2k: The effect's subject ("you") is the single
        // authority for who gains life. There must be NO `player_scope` on the
        // execute ability — that field was historically (mis)used by a
        // post-hoc rewire to redirect "you gain" effects to the triggering
        // opponent, which is exactly the regression this guards against.
        assert!(
            execute.player_scope.is_none(),
            "Exquisite Blood: execute.player_scope must be None — the effect-level \
             subject is authoritative. Found {:?}",
            execute.player_scope,
        );
    }

    #[test]
    fn trigger_you_cycle_a_card() {
        let def = parse_trigger_line("Whenever you cycle a card, draw a card.", "Drake Haven");
        assert_eq!(def.mode, TriggerMode::Cycled);
        assert_eq!(def.valid_target, Some(TargetFilter::Controller));
    }

    #[test]
    fn trigger_you_lose_life() {
        let def = parse_trigger_line(
            "Whenever you lose life, create a 1/1 token.",
            "Unholy Annex",
        );
        assert_eq!(def.mode, TriggerMode::LifeLost);
        assert_eq!(def.valid_target, Some(TargetFilter::Controller));
    }

    #[test]
    fn trigger_you_lose_life_during_your_turn() {
        let def = parse_trigger_line(
            "Whenever you lose life during your turn, draw a card.",
            "Bloodtracker",
        );
        assert_eq!(def.mode, TriggerMode::LifeLost);
        assert_eq!(def.valid_target, Some(TargetFilter::Controller));
        assert_eq!(def.constraint, Some(TriggerConstraint::OnlyDuringYourTurn));
    }

    #[test]
    fn trigger_you_sacrifice_a_creature() {
        let def = parse_trigger_line(
            "Whenever you sacrifice a creature, draw a card.",
            "Morbid Opportunist",
        );
        assert_eq!(def.mode, TriggerMode::Sacrificed);
        assert_eq!(
            def.valid_card,
            Some(TargetFilter::Typed(
                TypedFilter::creature().controller(ControllerRef::You)
            ))
        );
    }

    #[test]
    fn trigger_a_player_sacrifices_a_permanent() {
        // CR 603 + CR 701.21: "a player sacrifices" → any-player scope (no controller filter).
        let def = parse_trigger_line(
            "Whenever a player sacrifices a permanent, put a +1/+1 counter on this creature.",
            "Merchant of Venom",
        );
        assert_eq!(def.mode, TriggerMode::Sacrificed);
        assert_eq!(
            def.valid_card,
            Some(TargetFilter::Typed(TypedFilter::new(TypeFilter::Permanent)))
        );
    }

    #[test]
    fn trigger_a_player_sacrifices_another_permanent() {
        // CR 603 + CR 701.21: Mazirek — "another permanent" carries FilterProp::Another,
        // which excludes the trigger source from matching its own sacrifice.
        let def = parse_trigger_line(
            "Whenever a player sacrifices another permanent, put a +1/+1 counter on each creature you control.",
            "Mazirek, Kraul Death Priest",
        );
        assert_eq!(def.mode, TriggerMode::Sacrificed);
        assert_eq!(
            def.valid_card,
            Some(TargetFilter::Typed(
                TypedFilter::new(TypeFilter::Permanent).properties(vec![FilterProp::Another])
            ))
        );
    }

    #[test]
    fn trigger_an_opponent_sacrifices_a_creature() {
        // CR 603 + CR 701.21: opponent-actor sacrifice dispatch.
        let def = parse_trigger_line(
            "Whenever an opponent sacrifices a creature, you gain 1 life.",
            "Test Card",
        );
        assert_eq!(def.mode, TriggerMode::Sacrificed);
        assert_eq!(
            def.valid_card,
            Some(TargetFilter::Typed(
                TypedFilter::creature().controller(ControllerRef::Opponent)
            ))
        );
    }

    #[test]
    fn trigger_sacrifice_with_during_your_turn_constraint() {
        // CR 603.2 + CR 603.7: Szarel, Genesis Shepherd — sacrifice trigger
        // with a trailing turn constraint. The parser must extract the
        // constraint rather than reject the whole line because the subject
        // wasn't the final token.
        use crate::types::ability::TriggerConstraint;
        let def = parse_trigger_line(
            "Whenever you sacrifice another nontoken permanent during your turn, you gain 1 life.",
            "Szarel, Genesis Shepherd",
        );
        assert_eq!(def.mode, TriggerMode::Sacrificed);
        assert_eq!(def.constraint, Some(TriggerConstraint::OnlyDuringYourTurn));
    }

    #[test]
    fn trigger_you_tap_a_land_for_mana() {
        let def = parse_trigger_line("Whenever you tap a land for mana, add {G}.", "Mana Flare");
        assert_eq!(def.mode, TriggerMode::TapsForMana);
        // CR 603.2 + CR 106.12a: "you tap a land" scopes the source filter to
        // the trigger source's controller.
        assert_eq!(
            def.valid_card,
            Some(TargetFilter::Typed(
                TypedFilter::new(TypeFilter::Land).controller(ControllerRef::You)
            ))
        );
        assert_eq!(def.valid_target, Some(TargetFilter::Controller));
    }

    #[test]
    fn trigger_forbidden_orchard_targets_opponent_token_owner() {
        let def = parse_trigger_line(
            "Whenever you tap Forbidden Orchard for mana, target opponent creates a 1/1 colorless Spirit creature token.",
            "Forbidden Orchard",
        );
        assert_eq!(def.mode, TriggerMode::TapsForMana);
        assert_eq!(def.valid_card, Some(TargetFilter::SelfRef));
        assert_eq!(def.valid_target, Some(TargetFilter::Controller));

        let execute = def.execute.as_deref().expect("trigger should execute");
        let Effect::Token { owner, .. } = execute.effect.as_ref() else {
            panic!("expected Token effect, got {:?}", execute.effect);
        };
        assert_eq!(
            *owner,
            TargetFilter::Typed(TypedFilter::default().controller(ControllerRef::Opponent))
        );
    }

    #[test]
    fn trigger_player_taps_land_for_mana_adds_to_that_player() {
        let def = parse_trigger_line(
            "Whenever a player taps a land for mana, that player adds one mana of any type that land produced.",
            "Mana Flare",
        );
        assert_eq!(def.mode, TriggerMode::TapsForMana);
        assert_eq!(
            def.valid_card,
            Some(TargetFilter::Typed(TypedFilter::new(TypeFilter::Land)))
        );
        let execute = def.execute.as_deref().unwrap();
        assert_eq!(execute.player_scope, Some(PlayerFilter::TriggeringPlayer));
        assert!(matches!(
            execute.effect.as_ref(),
            crate::types::ability::Effect::Mana {
                produced: crate::types::ability::ManaProduction::TriggerEventManaType,
                ..
            }
        ));
    }

    #[test]
    fn trigger_enchanted_land_is_tapped_for_mana() {
        let def = parse_trigger_line(
            "Whenever enchanted land is tapped for mana, its controller adds an additional {G}.",
            "Wild Growth",
        );
        assert_eq!(def.mode, TriggerMode::TapsForMana);
        assert_eq!(def.valid_card, Some(TargetFilter::AttachedTo));
        // CR 109.5 + CR 605.1b: "its controller" antecedent is the enchanted
        // land — the player who tapped it for mana. `PlayerFilter::TriggeringPlayer`
        // rebinds the resolving ability's controller to the ManaAdded event's
        // `player_id` so the bonus mana routes to the land's controller even
        // when the Aura is opponent-controlled.
        let execute = def.execute.as_deref().unwrap();
        assert_eq!(execute.player_scope, Some(PlayerFilter::TriggeringPlayer));
    }

    #[test]
    fn trigger_enchanted_forest_is_tapped_for_mana_utopia_sprawl() {
        // CR 205.3i + CR 605.1b: "Whenever enchanted Forest is tapped for mana …"
        // The basic land type token ("Forest") must resolve to `AttachedTo`; the
        // Enchant keyword already constrains the aura's attach target to Forests.
        let def = parse_trigger_line(
            "Whenever enchanted Forest is tapped for mana, its controller adds an additional one mana of the chosen color.",
            "Utopia Sprawl",
        );
        assert_eq!(def.mode, TriggerMode::TapsForMana);
        assert_eq!(def.valid_card, Some(TargetFilter::AttachedTo));
        let execute = def.execute.as_deref().unwrap();
        assert_eq!(execute.player_scope, Some(PlayerFilter::TriggeringPlayer));
    }

    #[test]
    fn trigger_fertile_ground_its_controller_adds_routes_to_triggering_player() {
        // CR 109.5 + CR 605.1b regression for the "its controller adds …"
        // Aura class (Fertile Ground, Wild Growth, Utopia Sprawl, Verdant Haven,
        // Trace of Abundance, Market Festival, Weirding Wood, Overgrowth):
        // bonus mana must go to the enchanted land's controller, not the
        // aura's controller, when an opponent ends up attaching the aura.
        let def = parse_trigger_line(
            "Whenever enchanted land is tapped for mana, its controller adds an additional one mana of any color.",
            "Fertile Ground",
        );
        assert_eq!(def.mode, TriggerMode::TapsForMana);
        assert_eq!(def.valid_card, Some(TargetFilter::AttachedTo));
        let execute = def.execute.as_deref().unwrap();
        assert_eq!(execute.player_scope, Some(PlayerFilter::TriggeringPlayer));
        assert!(matches!(
            execute.effect.as_ref(),
            crate::types::ability::Effect::Mana { .. }
        ));
    }

    #[test]
    fn trigger_opponent_taps_land_for_mana_vorinclex() {
        // CR 603.2 + CR 605.1a: "Whenever an opponent taps <subject> for mana" —
        // source must be opponent-controlled.
        let def = parse_trigger_line(
            "Whenever an opponent taps a land for mana, that land doesn't untap during its controller's next untap step.",
            "Vorinclex, Voice of Hunger",
        );
        assert_eq!(def.mode, TriggerMode::TapsForMana);
        match def.valid_card {
            Some(TargetFilter::Typed(ref tf)) => {
                assert_eq!(tf.type_filters, vec![TypeFilter::Land]);
                assert_eq!(tf.controller, Some(ControllerRef::Opponent));
            }
            other => panic!("expected Typed(Land) with Opponent controller, got {other:?}"),
        }
    }

    #[test]
    fn trigger_you_tap_land_for_mana_vorinclex_first_half() {
        // CR 603.2 + CR 106.12a: "you tap a land" arm scopes the source filter to
        // ControllerRef::You — the trigger fires only on lands the controller taps.
        let def = parse_trigger_line(
            "Whenever you tap a land for mana, add one mana of any type that land produced.",
            "Vorinclex, Voice of Hunger",
        );
        assert_eq!(def.mode, TriggerMode::TapsForMana);
        assert_eq!(def.valid_target, Some(TargetFilter::Controller));
        match def.valid_card {
            Some(TargetFilter::Typed(ref tf)) => {
                assert_eq!(tf.type_filters, vec![TypeFilter::Land]);
                assert_eq!(tf.controller, Some(ControllerRef::You));
            }
            other => panic!("expected Typed(Land) with You controller, got {other:?}"),
        }
    }

    #[test]
    fn trigger_nth_spell_second() {
        let def = parse_trigger_line(
            "Whenever you cast your second spell each turn, draw a card.",
            "Spectral Sailor",
        );
        assert_eq!(def.mode, TriggerMode::SpellCast);
        assert_eq!(
            def.constraint,
            Some(TriggerConstraint::NthSpellThisTurn { n: 2, filter: None })
        );
    }

    #[test]
    fn trigger_nth_spell_third() {
        let def = parse_trigger_line(
            "Whenever you cast your third spell each turn, create a 1/1 token.",
            "Some Card",
        );
        assert_eq!(def.mode, TriggerMode::SpellCast);
        assert_eq!(
            def.constraint,
            Some(TriggerConstraint::NthSpellThisTurn { n: 3, filter: None })
        );
    }

    /// CR 603.2: "you cast your Nth spell" must be gated on caster=you so the
    /// trigger does not fire for opponents' casts. Mirrors the symmetric
    /// `controller(Opponent)` scoping on the "an opponent casts their Nth"
    /// branch (Alphinaud Leveilleur class).
    #[test]
    fn trigger_nth_spell_you_scopes_to_controller() {
        let def = parse_trigger_line(
            "Whenever you cast your second spell each turn, scry 1.",
            "Alphinaud Leveilleur",
        );
        assert_eq!(def.mode, TriggerMode::SpellCast);
        assert_eq!(
            def.valid_target,
            Some(TargetFilter::Typed(
                TypedFilter::default().controller(ControllerRef::You),
            ))
        );
    }

    #[test]
    fn trigger_nth_draw_second() {
        let def = parse_trigger_line(
            "Whenever you draw your second card each turn, you gain 1 life.",
            "Some Card",
        );
        assert_eq!(def.mode, TriggerMode::Drawn);
        // CR 603.2: "you draw" scopes the trigger to the controller's draws.
        assert_eq!(
            def.valid_target,
            Some(TargetFilter::Typed(
                TypedFilter::default().controller(ControllerRef::You),
            ))
        );
        assert_eq!(
            def.constraint,
            Some(TriggerConstraint::NthDrawThisTurn { n: 2 })
        );
    }

    #[test]
    fn trigger_nth_draw_you_in_a_turn_phrasing() {
        // CR 603.2: "When you draw your Nth card in a turn" (Sneaky Snacker phrasing)
        // must scope to the controller's draws, not any player's.
        let def = parse_trigger_line(
            "When you draw your third card in a turn, return this card from your graveyard to the battlefield tapped.",
            "Sneaky Snacker",
        );
        assert_eq!(def.mode, TriggerMode::Drawn);
        assert_eq!(
            def.valid_target,
            Some(TargetFilter::Typed(
                TypedFilter::default().controller(ControllerRef::You),
            ))
        );
        assert_eq!(
            def.constraint,
            Some(TriggerConstraint::NthDrawThisTurn { n: 3 })
        );
    }

    #[test]
    fn trigger_nth_draw_opponent_second() {
        let def = parse_trigger_line(
            "Whenever an opponent draws their second card each turn, you draw two cards.",
            "The Unagi of Kyoshi Island",
        );
        assert_eq!(def.mode, TriggerMode::Drawn);
        assert_eq!(
            def.valid_target,
            Some(TargetFilter::Typed(
                TypedFilter::default().controller(ControllerRef::Opponent),
            ))
        );
        assert_eq!(
            def.constraint,
            Some(TriggerConstraint::NthDrawThisTurn { n: 2 })
        );
    }

    #[test]
    fn trigger_nth_draw_any_player() {
        let def = parse_trigger_line(
            "Whenever a player draws their third card each turn, you gain 1 life.",
            "Some Card",
        );
        assert_eq!(def.mode, TriggerMode::Drawn);
        assert_eq!(def.valid_target, None);
        assert_eq!(
            def.constraint,
            Some(TriggerConstraint::NthDrawThisTurn { n: 3 })
        );
    }

    /// SHAPE test (#413): The Council of Four's draw trigger. "during their
    /// turn" restricts the trigger to draws on the drawer's own turn, mapped
    /// to a `DuringPlayersTurn { TriggeringPlayer }` intervening-if. Does not
    /// substitute for the runtime test in `triggers.rs`.
    #[test]
    fn trigger_nth_draw_any_player_during_their_turn() {
        let def = parse_trigger_line(
            "Whenever a player draws their second card during their turn, you draw a card.",
            "The Council of Four",
        );
        assert_eq!(def.mode, TriggerMode::Drawn);
        assert_eq!(def.valid_target, None);
        assert_eq!(
            def.constraint,
            Some(TriggerConstraint::NthDrawThisTurn { n: 2 })
        );
        assert_eq!(
            def.condition,
            Some(TriggerCondition::DuringPlayersTurn {
                player: PlayerFilter::TriggeringPlayer,
            })
        );
        assert!(def.execute.is_some());
    }

    /// SHAPE test (#413): The Council of Four's spell trigger. "casts their
    /// second spell during their turn" → `NthSpellThisTurn { n: 2 }` constraint
    /// plus a `DuringPlayersTurn { TriggeringPlayer }` intervening-if.
    #[test]
    fn trigger_nth_spell_any_player_during_their_turn() {
        let def = parse_trigger_line(
            "Whenever a player casts their second spell during their turn, you create a 2/2 white Knight creature token.",
            "The Council of Four",
        );
        assert_eq!(def.mode, TriggerMode::SpellCast);
        assert_eq!(def.valid_target, None);
        assert_eq!(
            def.constraint,
            Some(TriggerConstraint::NthSpellThisTurn { n: 2, filter: None })
        );
        assert_eq!(
            def.condition,
            Some(TriggerCondition::DuringPlayersTurn {
                player: PlayerFilter::TriggeringPlayer,
            })
        );
        assert!(def.execute.is_some());
    }

    /// SHAPE test (#413): Ledger Shredder — cluster spot-check. "casts their
    /// second spell each turn" (no turn restriction) must still parse to the
    /// unrestricted form with no condition.
    #[test]
    fn trigger_nth_spell_any_player_each_turn_no_condition() {
        let def = parse_trigger_line(
            "Whenever a player casts their second spell each turn, ~ explores.",
            "Ledger Shredder",
        );
        assert_eq!(def.mode, TriggerMode::SpellCast);
        assert_eq!(
            def.constraint,
            Some(TriggerConstraint::NthSpellThisTurn { n: 2, filter: None })
        );
        assert_eq!(def.condition, None);
    }

    #[test]
    fn trigger_you_search_your_library() {
        let def = parse_trigger_line(
            "Whenever you search your library, scry 1.",
            "Search Elemental",
        );
        assert_eq!(def.mode, TriggerMode::SearchedLibrary);
        assert_eq!(def.valid_target, Some(TargetFilter::Controller));
    }

    #[test]
    fn trigger_opponent_searches_their_library() {
        let def = parse_trigger_line(
            "Whenever an opponent searches their library, you gain 1 life and draw a card.",
            "Archivist of Oghma",
        );
        assert_eq!(def.mode, TriggerMode::SearchedLibrary);
        assert_eq!(
            def.valid_target,
            Some(TargetFilter::Typed(
                TypedFilter::default().controller(ControllerRef::Opponent),
            ))
        );
    }

    #[test]
    fn trigger_you_scry() {
        let def = parse_trigger_line(
            "Whenever you scry, put a +1/+1 counter on this creature.",
            "Thoughtbound Phantasm",
        );
        assert_eq!(def.mode, TriggerMode::Scry);
        assert_eq!(def.valid_target, Some(TargetFilter::Controller));
    }

    #[test]
    fn trigger_you_surveil() {
        let def = parse_trigger_line(
            "Whenever you surveil, put a +1/+1 counter on Mirko.",
            "Mirko, Obsessive Theorist",
        );
        assert_eq!(def.mode, TriggerMode::Surveil);
        assert_eq!(def.valid_target, Some(TargetFilter::Controller));
    }

    #[test]
    fn trigger_you_scry_or_surveil() {
        let def = parse_trigger_line(
            "Whenever you scry or surveil, draw a card.",
            "Matoya, Archon Elder",
        );
        assert_eq!(def.mode, TriggerMode::PlayerPerformedAction);
        assert_eq!(def.valid_target, Some(TargetFilter::Controller));
        assert_eq!(
            def.player_actions,
            Some(vec![PlayerActionKind::Scry, PlayerActionKind::Surveil])
        );
    }

    #[test]
    fn trigger_opponent_scries_surveils_or_searches() {
        let def = parse_trigger_line(
            "Whenever an opponent scries, surveils, or searches their library, put a +1/+1 counter on River Song. Then River Song deals damage to that player equal to its power.",
            "River Song",
        );
        assert_eq!(def.mode, TriggerMode::PlayerPerformedAction);
        assert_eq!(
            def.valid_target,
            Some(TargetFilter::Typed(
                TypedFilter::default().controller(ControllerRef::Opponent),
            ))
        );
        assert_eq!(
            def.player_actions,
            Some(vec![
                PlayerActionKind::Scry,
                PlayerActionKind::Surveil,
                PlayerActionKind::SearchedLibrary,
            ])
        );
    }

    #[test]
    fn trigger_nth_spell_opponent_noncreature() {
        let def = parse_trigger_line(
            "Whenever an opponent casts their first noncreature spell each turn, draw a card.",
            "Esper Sentinel",
        );
        assert_eq!(def.mode, TriggerMode::SpellCast);
        // parse_type_phrase("noncreature") produces [Non(Creature)] without a redundant
        // Card base type — Non(Creature) alone is sufficient for spell-history filtering.
        assert_eq!(
            def.constraint,
            Some(TriggerConstraint::NthSpellThisTurn {
                n: 1,
                filter: Some(TargetFilter::Typed(TypedFilter {
                    type_filters: vec![TypeFilter::Non(Box::new(TypeFilter::Creature))],
                    controller: None,
                    properties: vec![],
                })),
            })
        );
        assert_eq!(
            def.valid_target,
            Some(TargetFilter::Typed(
                TypedFilter::default().controller(ControllerRef::Opponent)
            ))
        );
    }

    #[test]
    fn trigger_esper_sentinel_unless_pay() {
        let def = parse_trigger_line(
            "Whenever an opponent casts their first noncreature spell each turn, draw a card unless that player pays {X}, where X is this creature's power.",
            "Esper Sentinel",
        );
        assert_eq!(def.mode, TriggerMode::SpellCast);
        // Effect should be Draw, not Unimplemented
        let execute = def.execute.as_ref().expect("should have execute");
        assert!(
            matches!(*execute.effect, Effect::Draw { .. }),
            "execute effect should be Draw, got {:?}",
            execute.effect
        );
        // Unless pay should be DynamicGeneric with SelfPower
        let unless_pay = def.unless_pay.as_ref().expect("should have unless_pay");
        assert_eq!(
            unless_pay.cost,
            AbilityCost::ManaDynamic {
                quantity: QuantityExpr::Ref {
                    qty: QuantityRef::Power {
                        scope: crate::types::ability::ObjectScope::Source
                    }
                }
            }
        );
        assert_eq!(unless_pay.payer, TargetFilter::TriggeringPlayer);
    }

    #[test]
    fn trigger_unless_you_pay_mana() {
        // "sacrifice this creature unless you pay {G}{G}" — "you pay" payer variant
        let def = parse_trigger_line(
            "At the beginning of your upkeep, sacrifice this creature unless you pay {G}{G}.",
            "Test Card",
        );
        let unless_pay = def.unless_pay.as_ref().expect("should have unless_pay");
        assert_eq!(unless_pay.payer, TargetFilter::Controller);
        assert!(
            matches!(unless_pay.cost, AbilityCost::Mana { .. }),
            "cost should be Fixed mana, got {:?}",
            unless_pay.cost
        );
        // The effect text should be stripped of the unless clause
        let execute = def.execute.as_ref().expect("should have execute");
        assert!(
            matches!(*execute.effect, Effect::Sacrifice { .. }),
            "execute should be Sacrifice, got {:?}",
            execute.effect
        );
    }

    #[test]
    fn trigger_unless_you_pay_energy() {
        let def = parse_trigger_line(
            "At the beginning of your end step, sacrifice this creature unless you pay {E}{E}.",
            "Lathnu Hellion",
        );
        let unless_pay = def.unless_pay.as_ref().expect("should have unless_pay");
        assert_eq!(unless_pay.payer, TargetFilter::Controller);
        assert_eq!(
            unless_pay.cost,
            AbilityCost::PayEnergy {
                amount: QuantityExpr::Fixed { value: 2 }
            }
        );
        let execute = def.execute.as_ref().expect("should have execute");
        assert!(
            matches!(*execute.effect, Effect::Sacrifice { .. }),
            "execute should be Sacrifice, got {:?}",
            execute.effect
        );
    }

    /// CR 107.14 + CR 202.3: Volatile Stormdrake's dynamic energy unless-cost —
    /// "sacrifice that creature unless you pay an amount of {E} equal to its
    /// mana value" must surface a `PayEnergy` whose amount references the
    /// taxed object's mana value (`Recipient` scope).
    #[test]
    fn trigger_unless_you_pay_dynamic_energy() {
        let def = parse_trigger_line(
            "When this creature enters, sacrifice that creature unless you pay an amount of {E} equal to its mana value.",
            "Volatile Stormdrake",
        );
        let unless_pay = def.unless_pay.as_ref().expect("should have unless_pay");
        assert_eq!(unless_pay.payer, TargetFilter::Controller);
        assert_eq!(
            unless_pay.cost,
            AbilityCost::PayEnergy {
                amount: QuantityExpr::Ref {
                    qty: QuantityRef::ObjectManaValue {
                        scope: crate::types::ability::ObjectScope::Recipient,
                    },
                },
            }
        );
        let execute = def.execute.as_ref().expect("should have execute");
        assert!(
            matches!(*execute.effect, Effect::Sacrifice { .. }),
            "execute should be Sacrifice, got {:?}",
            execute.effect
        );
    }

    /// CR 608.2k + CR 603.7c: Self-ETB "sacrifice it" anaphor — Azorius
    /// Herald, Balduvian Horde, Glint Hawk, Faerie Impostor, Phlage. The
    /// bare object pronoun "it" in a `SelfRef`-subject trigger sub-effect
    /// must resolve to `TargetFilter::SelfRef` (the source itself), NOT to
    /// `ParentTarget` — the trigger introduces no parent-target chain.
    ///
    /// This locks in the divergent behavior between `resolve_it_pronoun`
    /// (which returns `SelfRef` for `Some(SelfRef)` subjects) and
    /// `resolve_pronoun_target` (which returns `ParentTarget` for the same
    /// subject case to support parent-target chains like
    /// "tap target creature. exile it"). The two helpers serve different
    /// anaphor contexts; the sacrifice imperative path delegates to
    /// `resolve_it_pronoun` because no parent target is in scope for the
    /// self-ETB trigger sub-effect.
    #[test]
    fn self_etb_sacrifice_it_anaphor_binds_to_self_ref() {
        let def = parse_trigger_line(
            "When ~ enters, sacrifice it unless {U} was spent to cast it.",
            "Azorius Herald",
        );
        let execute = def.execute.as_ref().expect("should have execute");
        let Effect::Sacrifice { target, .. } = &*execute.effect else {
            panic!("expected Sacrifice, got {:?}", execute.effect);
        };
        assert_eq!(
            *target,
            TargetFilter::SelfRef,
            "sacrifice target must bind to the self (entering source), not ParentTarget"
        );
    }

    #[test]
    fn trigger_unless_you_discard_a_card() {
        // CR 608.2c: Balduvian Horde — "sacrifice it unless you discard a card at random".
        // The "at random" suffix is currently sub-fidelity (player-chosen via WardDiscardChoice);
        // the cost-gate itself is captured.
        let def = parse_trigger_line(
            "When ~ enters, sacrifice it unless you discard a card at random.",
            "Balduvian Horde",
        );
        let unless_pay = def.unless_pay.as_ref().expect("should have unless_pay");
        assert_eq!(unless_pay.payer, TargetFilter::Controller);
        assert!(
            matches!(
                unless_pay.cost,
                AbilityCost::Discard {
                    count: QuantityExpr::Fixed { value: 1 },
                    filter: None,
                    random: false,
                    self_ref: false
                }
            ),
            "cost should be DiscardCard, got {:?}",
            unless_pay.cost
        );
        let execute = def.execute.as_ref().expect("should have execute");
        assert!(
            matches!(*execute.effect, Effect::Sacrifice { .. }),
            "execute should be Sacrifice, got {:?}",
            execute.effect
        );
    }

    #[test]
    fn trigger_unless_you_sacrifice_filter() {
        // Bog Elemental — "sacrifice this creature unless you sacrifice a land".
        let def = parse_trigger_line(
            "At the beginning of your upkeep, sacrifice this creature unless you sacrifice a land.",
            "Bog Elemental",
        );
        let unless_pay = def.unless_pay.as_ref().expect("should have unless_pay");
        assert_eq!(unless_pay.payer, TargetFilter::Controller);
        match &unless_pay.cost {
            AbilityCost::Sacrifice {
                count,
                target: filter,
            } => {
                assert_eq!(*count, 1);
                match filter {
                    TargetFilter::Typed(typed) => {
                        assert!(
                            typed
                                .type_filters
                                .iter()
                                .any(|t| matches!(t, TypeFilter::Land)),
                            "filter should include Land, got {:?}",
                            typed.type_filters,
                        );
                    }
                    other => panic!("expected Typed filter, got {:?}", other),
                }
            }
            other => panic!("cost should be Sacrifice, got {:?}", other),
        }
    }

    #[test]
    fn trigger_unless_you_pay_n_life() {
        // Carnophage — "tap this creature unless you pay 1 life".
        let def = parse_trigger_line(
            "At the beginning of your upkeep, tap this creature unless you pay 1 life.",
            "Carnophage",
        );
        let unless_pay = def.unless_pay.as_ref().expect("should have unless_pay");
        assert_eq!(unless_pay.payer, TargetFilter::Controller);
        assert!(
            matches!(
                unless_pay.cost,
                AbilityCost::PayLife {
                    amount: QuantityExpr::Fixed { value: 1 }
                }
            ),
            "cost should be PayLife {{ amount: 1 }}, got {:?}",
            unless_pay.cost
        );
    }

    #[test]
    fn trigger_unless_they_pay_life_binds_phase_that_player_to_scoped_player() {
        // Blood Clock — phase-scoped "that player" is the active player whose
        // upkeep began, not an event player from a spell/action trigger.
        let def = parse_trigger_line(
            "At the beginning of each player's upkeep, that player returns a permanent they control to its owner's hand unless they pay 2 life.",
            "Blood Clock",
        );

        let unless_pay = def.unless_pay.as_ref().expect("should have unless_pay");
        assert_eq!(unless_pay.payer, TargetFilter::Controller);
        assert!(
            matches!(
                unless_pay.cost,
                AbilityCost::PayLife {
                    amount: QuantityExpr::Fixed { value: 2 }
                }
            ),
            "cost should be PayLife {{ amount: 2 }}, got {:?}",
            unless_pay.cost
        );
        let execute = def.execute.as_ref().expect("should have execute");
        match execute.effect.as_ref() {
            Effect::Bounce { target, .. } => match target {
                TargetFilter::Typed(filter) => {
                    assert_eq!(filter.controller, Some(ControllerRef::ScopedPlayer));
                    assert!(
                        filter.type_filters.contains(&TypeFilter::Permanent),
                        "filter should include Permanent, got {:?}",
                        filter.type_filters
                    );
                }
                other => panic!("expected typed bounce filter, got {other:?}"),
            },
            other => panic!("expected Bounce effect, got {other:?}"),
        }
    }

    #[test]
    fn trigger_unless_they_pay_life_keeps_event_that_player_as_triggering_player() {
        let def = parse_trigger_line(
            "Whenever an opponent casts a spell, that player loses 5 life unless they pay 2 life.",
            "Test Card",
        );

        let unless_pay = def.unless_pay.as_ref().expect("should have unless_pay");
        assert_eq!(unless_pay.payer, TargetFilter::TriggeringPlayer);
        assert!(
            matches!(
                unless_pay.cost,
                AbilityCost::PayLife {
                    amount: QuantityExpr::Fixed { value: 2 }
                }
            ),
            "cost should be PayLife {{ amount: 2 }}, got {:?}",
            unless_pay.cost
        );
        let execute = def.execute.as_ref().expect("should have execute");
        match execute.effect.as_ref() {
            Effect::LoseLife { target, amount } => {
                assert_eq!(*target, Some(TargetFilter::TriggeringPlayer));
                assert_eq!(*amount, QuantityExpr::Fixed { value: 5 });
            }
            other => panic!("expected LoseLife effect, got {other:?}"),
        }
    }

    #[test]
    fn trigger_unless_you_discard_typed_preserves_filter() {
        // Drekavac — "unless you discard a noncreature card".
        let def = parse_trigger_line(
            "When ~ enters, sacrifice it unless you discard a noncreature card.",
            "Drekavac",
        );
        let unless_pay = def.unless_pay.as_ref().expect("should have unless_pay");
        match &unless_pay.cost {
            AbilityCost::Discard {
                filter: Some(TargetFilter::Typed(typed)),
                ..
            } => assert!(
                typed
                    .type_filters
                    .iter()
                    .any(|t| matches!(t, TypeFilter::Non(inner) if matches!(**inner, TypeFilter::Creature))),
                "filter should include noncreature, got {:?}",
                typed.type_filters
            ),
            other => panic!("cost should be filtered Discard, got {:?}", other),
        }
    }

    #[test]
    fn trigger_unless_you_return_artifact_to_hand() {
        // CR 118.12: Glint Hawk — "sacrifice it unless you return an artifact
        // you control to its owner's hand."
        let def = parse_trigger_line(
            "When ~ enters, sacrifice it unless you return an artifact you control to its owner's hand.",
            "Glint Hawk",
        );
        let unless_pay = def.unless_pay.as_ref().expect("should have unless_pay");
        assert_eq!(unless_pay.payer, TargetFilter::Controller);
        match &unless_pay.cost {
            AbilityCost::ReturnToHand {
                count,
                filter: Some(filter),
                from_zone,
            } => {
                assert_eq!(*count, 1);
                assert!(
                    from_zone.is_none(),
                    "battlefield source should have no from_zone"
                );
                let has_artifact = match filter {
                    TargetFilter::And { filters } => filters.iter().any(|f| matches!(f,
                        TargetFilter::Typed(tf) if tf.type_filters.contains(&TypeFilter::Artifact)
                    )),
                    TargetFilter::Typed(tf) => tf.type_filters.contains(&TypeFilter::Artifact),
                    _ => false,
                };
                assert!(
                    has_artifact,
                    "filter should include Artifact, got {:?}",
                    filter
                );
            }
            other => panic!("cost should be ReturnToHand, got {:?}", other),
        }
        let execute = def.execute.as_ref().expect("should have execute");
        assert!(
            matches!(*execute.effect, Effect::Sacrifice { .. }),
            "execute should be Sacrifice, got {:?}",
            execute.effect
        );
    }

    #[test]
    fn trigger_unless_you_return_another_creature_to_hand() {
        // CR 118.12: Faerie Impostor / Quickling — "sacrifice it unless you
        // return another creature you control to its owner's hand."
        let def = parse_trigger_line(
            "When ~ enters, sacrifice it unless you return another creature you control to its owner's hand.",
            "Faerie Impostor",
        );
        let unless_pay = def.unless_pay.as_ref().expect("should have unless_pay");
        match &unless_pay.cost {
            AbilityCost::ReturnToHand {
                count,
                filter: Some(filter),
                ..
            } => {
                assert_eq!(*count, 1);
                let has_another_creature = match filter {
                    TargetFilter::And { filters } => filters.iter().any(|f| {
                        matches!(f,
                            TargetFilter::Typed(tf) if tf.properties.contains(&FilterProp::Another)
                                && tf.type_filters.contains(&TypeFilter::Creature)
                        )
                    }),
                    TargetFilter::Typed(tf) => {
                        tf.properties.contains(&FilterProp::Another)
                            && tf.type_filters.contains(&TypeFilter::Creature)
                    }
                    _ => false,
                };
                assert!(
                    has_another_creature,
                    "filter should include Another+Creature, got {:?}",
                    filter
                );
            }
            other => panic!("cost should be ReturnToHand, got {:?}", other),
        }
    }

    #[test]
    fn trigger_unless_you_return_two_forests_to_hand() {
        // CR 118.12: Bull Elephant — "sacrifice it unless you return two
        // Forests you control to their owner's hand."
        let def = parse_trigger_line(
            "When ~ enters, sacrifice it unless you return two Forests you control to their owner's hand.",
            "Bull Elephant",
        );
        let unless_pay = def.unless_pay.as_ref().expect("should have unless_pay");
        match &unless_pay.cost {
            AbilityCost::ReturnToHand {
                count, from_zone, ..
            } => {
                assert_eq!(*count, 2);
                assert!(from_zone.is_none());
            }
            other => panic!("cost should be ReturnToHand, got {:?}", other),
        }
    }

    #[test]
    fn trigger_unless_you_return_non_lair_land_to_hand() {
        // CR 118.12: Crosis's Catacombs — "sacrifice it unless you return a
        // non-Lair land you control to its owner's hand."
        let def = parse_trigger_line(
            "When ~ enters, sacrifice it unless you return a non-Lair land you control to its owner's hand.",
            "Crosis's Catacombs",
        );
        let unless_pay = def.unless_pay.as_ref().expect("should have unless_pay");
        match &unless_pay.cost {
            AbilityCost::ReturnToHand {
                count, from_zone, ..
            } => {
                assert_eq!(*count, 1);
                assert!(from_zone.is_none());
            }
            other => panic!("cost should be ReturnToHand, got {:?}", other),
        }
    }

    #[test]
    fn trigger_unless_you_return_from_graveyard() {
        // CR 118.12: Harvest Wurm — "sacrifice it unless you return a basic
        // land card from your graveyard to your hand."
        let def = parse_trigger_line(
            "When ~ enters, sacrifice it unless you return a basic land card from your graveyard to your hand.",
            "Harvest Wurm",
        );
        let unless_pay = def.unless_pay.as_ref().expect("should have unless_pay");
        match &unless_pay.cost {
            AbilityCost::ReturnToHand {
                count,
                from_zone,
                filter: Some(filter),
            } => {
                assert_eq!(*count, 1);
                assert_eq!(
                    *from_zone,
                    Some(crate::types::zones::Zone::Graveyard),
                    "should be from graveyard"
                );
                let has_land = match filter {
                    TargetFilter::And { filters } => filters.iter().any(|f| {
                        matches!(f,
                            TargetFilter::Typed(tf) if tf.type_filters.contains(&TypeFilter::Land)
                        )
                    }),
                    TargetFilter::Typed(tf) => tf.type_filters.contains(&TypeFilter::Land),
                    _ => false,
                };
                assert!(has_land, "filter should include Land, got {:?}", filter);
            }
            other => panic!("cost should be ReturnToHand, got {:?}", other),
        }
    }

    #[test]
    fn trigger_unless_intervening_attacked_this_turn() {
        // Bellowing Saddlebrute (Raid) — "When this creature enters, you lose 4
        // life unless you attacked this turn." The trailing intervening-unless
        // wraps `AttackedThisTurn >= 1` in `Not`, leaving "you lose 4 life" as
        // the effect.
        let def = parse_trigger_line(
            "When ~ enters, you lose 4 life unless you attacked this turn.",
            "Bellowing Saddlebrute",
        );
        let cond = def
            .condition
            .expect("intervening unless should set condition");
        match cond {
            TriggerCondition::Not { condition } => match *condition {
                TriggerCondition::QuantityComparison {
                    lhs:
                        QuantityExpr::Ref {
                            qty: QuantityRef::AttackedThisTurn,
                        },
                    comparator: Comparator::GE,
                    rhs: QuantityExpr::Fixed { value: 1 },
                } => {}
                other => panic!("inner should be AttackedThisTurn >= 1, got {:?}", other),
            },
            other => panic!("expected TriggerCondition::Not, got {:?}", other),
        }
    }

    #[test]
    fn trigger_unless_intervening_controls_creature() {
        // Generic shape: "deals 3 damage to target player unless you control a
        // creature" — IsPresent(Creature) wrapped in Not.
        let def = parse_trigger_line(
            "When ~ enters, ~ deals 3 damage to target player unless you control a creature.",
            "Test",
        );
        match def.condition {
            Some(TriggerCondition::Not { condition }) => match *condition {
                TriggerCondition::ControlsType { .. } => {}
                other => panic!("inner should be ControlsType, got {:?}", other),
            },
            other => panic!("expected Some(Not), got {:?}", other),
        }
    }

    #[test]
    fn trigger_unless_intervening_does_not_swallow_unless_pay() {
        // Sanity: "unless you pay {2}" must remain captured by
        // extract_unless_pay_modifier — NOT routed through the new
        // intervening-condition combinator. If parse_inner_condition somehow
        // matched a cost phrase, the unless_pay slot would be lost.
        let def = parse_trigger_line("When ~ enters, draw a card unless you pay {2}.", "Test");
        assert!(
            def.unless_pay.is_some(),
            "unless-pay must be captured as alt-cost, not as intervening condition"
        );
        assert!(
            def.condition.is_none(),
            "no intervening condition should be set when unless-pay handled it"
        );
    }

    #[test]
    fn trigger_unless_they_pay_binds_that_player_to_triggering_player() {
        let def = parse_trigger_line(
            "Whenever an opponent casts a creature spell, that player loses 2 life unless they pay {2}.",
            "Isolation Cell",
        );

        let unless_pay = def.unless_pay.as_ref().expect("should have unless_pay");
        assert_eq!(unless_pay.payer, TargetFilter::TriggeringPlayer);
        assert!(
            matches!(unless_pay.cost, AbilityCost::Mana { .. }),
            "cost should be Fixed mana, got {:?}",
            unless_pay.cost
        );
    }

    #[test]
    fn trigger_unless_they_pay_binds_to_that_player_damage_target() {
        let def = parse_trigger_line(
            "Whenever an opponent casts a creature spell, this enchantment deals 2 damage to that player unless they pay {2}.",
            "Soul Barrier",
        );

        let unless_pay = def.unless_pay.as_ref().expect("should have unless_pay");
        assert_eq!(unless_pay.payer, TargetFilter::TriggeringPlayer);
        assert!(
            matches!(unless_pay.cost, AbilityCost::Mana { .. }),
            "cost should be Fixed mana, got {:?}",
            unless_pay.cost
        );
    }

    #[test]
    fn trigger_unless_they_pay_binds_each_opponent_to_scoped_controller() {
        let def = parse_trigger_line(
            "When this creature enters, each opponent sacrifices a permanent of their choice unless they pay {2}.",
            "Rishadan Footpad",
        );

        let unless_pay = def.unless_pay.as_ref().expect("should have unless_pay");
        assert_eq!(unless_pay.payer, TargetFilter::Controller);
        let execute = def.execute.as_ref().expect("should have execute");
        assert_eq!(execute.player_scope, Some(PlayerFilter::Opponent));
    }

    #[test]
    fn trigger_unless_they_pay_binds_creature_controller_to_parent_target_controller() {
        let def = parse_trigger_line(
            "Whenever this creature deals combat damage to a creature, that creature's controller loses 2 life unless they pay {2}.",
            "Death Charmer",
        );

        let unless_pay = def.unless_pay.as_ref().expect("should have unless_pay");
        assert_eq!(unless_pay.payer, TargetFilter::ParentTargetController);
    }

    #[test]
    fn trigger_unless_they_discard_binds_that_player_to_triggering_player() {
        let def = parse_trigger_line(
            "Whenever an opponent casts a spell, that player loses 5 life unless they discard a card.",
            "Painful Quandary",
        );

        let unless_pay = def.unless_pay.as_ref().expect("should have unless_pay");
        assert_eq!(unless_pay.payer, TargetFilter::TriggeringPlayer);
        assert!(
            matches!(
                unless_pay.cost,
                AbilityCost::Discard {
                    count: QuantityExpr::Fixed { value: 1 },
                    filter: None,
                    random: false,
                    self_ref: false
                }
            ),
            "cost should be DiscardCard, got {:?}",
            unless_pay.cost
        );
    }

    #[test]
    fn trigger_unless_they_discard_multi_sentence_branch_not_terminal_cost() {
        let def = parse_trigger_line(
            "At the beginning of your upkeep, each opponent loses 3 life unless they discard a card. If you're the monarch, instead each opponent loses 6 life unless they discard two cards.",
            "Court of Ambition",
        );

        assert!(
            def.unless_pay.is_none(),
            "multi-sentence branch should not be stripped as one terminal unless cost, got {:?}",
            def.unless_pay
        );
        let execute = def.execute.as_ref().expect("should have execute");
        assert!(
            execute.sub_ability.is_some(),
            "monarch branch should remain available for downstream parsing"
        );
    }

    #[test]
    fn trigger_unless_pay_for_each_uses_dynamic_generic_cost() {
        let def = parse_trigger_line(
            "At the beginning of your upkeep, sacrifice this creature unless you pay {1} for each card in your hand.",
            "Extravagant Spirit",
        );

        let unless_pay = def.unless_pay.as_ref().expect("should have unless_pay");
        assert_eq!(unless_pay.payer, TargetFilter::Controller);
        assert!(
            matches!(
                unless_pay.cost,
                AbilityCost::ManaDynamic {
                    quantity: QuantityExpr::Ref { .. }
                }
            ),
            "unless payment should use dynamic generic cost, got {:?}",
            unless_pay.cost
        );
    }

    #[test]
    fn trigger_unrecognized_unless_payment_preserves_clause_as_gap() {
        let def = parse_trigger_line(
            "When this creature enters, draw a card unless the active player compliments your hat.",
            "Test Card",
        );

        assert!(def.unless_pay.is_none());
        let execute = def.execute.as_ref().expect("should have execute");
        assert!(
            matches!(*execute.effect, Effect::Unimplemented { .. }),
            "unrecognized unless clause must remain visible as Unimplemented, got {:?}",
            execute.effect
        );
    }

    #[test]
    fn trigger_put_into_graveyard_from_battlefield_self() {
        // CR 700.4: "Is put into a graveyard from the battlefield" is a synonym for "dies."
        let def = parse_trigger_line(
            "When ~ is put into a graveyard from the battlefield, return ~ to its owner's hand.",
            "Rancor",
        );
        assert_eq!(def.mode, TriggerMode::ChangesZone);
        assert_eq!(def.origin, Some(Zone::Battlefield));
        assert_eq!(def.destination, Some(Zone::Graveyard));
        assert_eq!(def.valid_card, Some(TargetFilter::SelfRef));
    }

    #[test]
    fn trigger_put_into_graveyard_from_battlefield_another_creature() {
        // plural "are put into a graveyard from the battlefield"
        let def = parse_trigger_line(
            "Whenever a creature you control is put into a graveyard from the battlefield, you gain 1 life.",
            "Some Card",
        );
        assert_eq!(def.mode, TriggerMode::ChangesZone);
        assert_eq!(def.origin, Some(Zone::Battlefield));
        assert_eq!(def.destination, Some(Zone::Graveyard));
    }

    #[test]
    fn trigger_blocks_self() {
        let def = parse_trigger_line(
            "Whenever Sustainer of the Realm blocks, it gains +0/+2 until end of turn.",
            "Sustainer of the Realm",
        );
        assert_eq!(def.mode, TriggerMode::Blocks);
        assert_eq!(def.valid_card, Some(TargetFilter::SelfRef));
    }

    #[test]
    fn trigger_blocks_when_prefix() {
        let def = parse_trigger_line(
            "When Stoic Ephemera blocks, it deals 5 damage to each creature blocking or blocked by it.",
            "Stoic Ephemera",
        );
        assert_eq!(def.mode, TriggerMode::Blocks);
        assert_eq!(def.valid_card, Some(TargetFilter::SelfRef));
    }

    #[test]
    fn trigger_blocks_a_creature() {
        let def = parse_trigger_line(
            "Whenever Wall of Frost blocks a creature, that creature doesn't untap during its controller's next untap step.",
            "Wall of Frost",
        );
        assert_eq!(def.mode, TriggerMode::Blocks);
        assert_eq!(def.valid_card, Some(TargetFilter::SelfRef));
    }

    #[test]
    fn trigger_blocks_or_becomes_blocked() {
        // "blocks or becomes blocked" — parsed as Blocks (blocker side)
        let def = parse_trigger_line(
            "Whenever Karn, Silver Golem blocks or becomes blocked, it gets -4/+4 until end of turn.",
            "Karn, Silver Golem",
        );
        assert_eq!(def.mode, TriggerMode::Blocks);
        assert_eq!(def.valid_card, Some(TargetFilter::SelfRef));
    }

    #[test]
    fn trigger_creature_you_control_blocks() {
        let def = parse_trigger_line(
            "Whenever a creature you control blocks, draw a card.",
            "Some Card",
        );
        assert_eq!(def.mode, TriggerMode::Blocks);
    }

    #[test]
    fn trigger_chaos_ensues_mode() {
        let def = parse_trigger_line("Whenever chaos ensues, draw a card.", "Plane");
        assert_eq!(def.mode, TriggerMode::ChaosEnsues);
    }

    #[test]
    fn trigger_set_in_motion_mode() {
        let def = parse_trigger_line("When you set this scheme in motion, draw a card.", "Scheme");
        assert_eq!(def.mode, TriggerMode::SetInMotion);
    }

    #[test]
    fn trigger_crank_contraption_mode() {
        let def = parse_trigger_line(
            "Whenever you crank this Contraption, create a token.",
            "Contraption",
        );
        assert_eq!(def.mode, TriggerMode::CrankContraption);
    }

    #[test]
    fn trigger_turn_face_up_mode() {
        let def = parse_trigger_line(
            "When this creature is turned face up, draw a card.",
            "Morphling",
        );
        assert_eq!(def.mode, TriggerMode::TurnFaceUp);
        assert_eq!(def.valid_card, Some(TargetFilter::SelfRef));
    }

    #[test]
    fn trigger_commit_crime_mode() {
        let def = parse_trigger_line("Whenever you commit a crime, draw a card.", "At Knifepoint");
        assert_eq!(def.mode, TriggerMode::CommitCrime);
    }

    // CR 701.62: "Whenever you manifest dread" — actor-side Manifest Dread
    // trigger, gated on controller via TargetFilter::Controller.
    #[test]
    fn trigger_manifest_dread_actor_side() {
        let def = parse_trigger_line(
            "Whenever you manifest dread, put a card you put into your graveyard this way into your hand.",
            "Paranormal Analyst",
        );
        assert_eq!(def.mode, TriggerMode::ManifestDread);
        assert_eq!(def.valid_target, Some(TargetFilter::Controller));
    }

    // CR 708 + CR 701.40b: "Whenever you turn a permanent face up" — actor-side
    // TurnFaceUp trigger. `valid_card` records the subject, `valid_target`
    // gates on the turning player being the trigger controller.
    #[test]
    fn trigger_turn_permanent_face_up_actor_side() {
        let def = parse_trigger_line(
            "Whenever you turn a permanent face up, put a +1/+1 counter on it.",
            "Growing Dread",
        );
        assert_eq!(def.mode, TriggerMode::TurnFaceUp);
        assert_eq!(
            def.valid_card,
            Some(TargetFilter::Typed(TypedFilter::new(TypeFilter::Permanent)))
        );
        assert_eq!(def.valid_target, Some(TargetFilter::Controller));
    }

    // CR 708 + CR 701.40b: creature-subject variant of the actor-side trigger.
    #[test]
    fn trigger_turn_creature_face_up_actor_side() {
        let def = parse_trigger_line(
            "Whenever you turn a creature face up, draw a card.",
            "Hypothetical Morph Payoff",
        );
        assert_eq!(def.mode, TriggerMode::TurnFaceUp);
        assert_eq!(
            def.valid_card,
            Some(TargetFilter::Typed(TypedFilter::new(TypeFilter::Creature)))
        );
        assert_eq!(def.valid_target, Some(TargetFilter::Controller));
    }

    #[test]
    fn trigger_commit_crime_returns_this_card_from_graveyard_sets_graveyard_zone() {
        let def = parse_trigger_line(
            "Whenever you commit a crime, you may pay {B}. If you do, return this card from your graveyard to the battlefield.",
            "Forsaken Miner",
        );
        assert_eq!(def.mode, TriggerMode::CommitCrime);
        assert_eq!(def.trigger_zones, vec![Zone::Graveyard]);
        let execute = def.execute.expect("should have execute");
        let if_you_do = execute
            .sub_ability
            .expect("should have if-you-do sub ability");
        assert!(matches!(
            *if_you_do.effect,
            crate::types::ability::Effect::ChangeZone {
                origin: Some(Zone::Graveyard),
                destination: Zone::Battlefield,
                target: TargetFilter::SelfRef,
                ..
            }
        ));
    }

    #[test]
    fn trigger_day_night_changes_mode() {
        let def = parse_trigger_line(
            "Whenever day becomes night or night becomes day, draw a card.",
            "Firmament Sage",
        );
        assert_eq!(def.mode, TriggerMode::DayTimeChanges);
    }

    #[test]
    fn trigger_end_of_combat_phase() {
        let def = parse_trigger_line(
            "At end of combat, sacrifice this creature.",
            "Ball Lightning",
        );
        assert_eq!(def.mode, TriggerMode::Phase);
        assert_eq!(def.phase, Some(Phase::EndCombat));
    }

    #[test]
    fn trigger_becomes_target_mode() {
        let def = parse_trigger_line(
            "When this creature becomes the target of a spell or ability, sacrifice it.",
            "Frost Walker",
        );
        assert_eq!(def.mode, TriggerMode::BecomesTarget);
        assert_eq!(def.valid_card, Some(TargetFilter::SelfRef));
        assert_eq!(def.valid_source, None); // spell OR ability — no source filter
    }

    #[test]
    fn trigger_becomes_target_of_spell_only() {
        let def = parse_trigger_line(
            "Whenever this creature becomes the target of a spell, this creature deals 2 damage to that spell's controller.",
            "Bonecrusher Giant",
        );
        assert_eq!(def.mode, TriggerMode::BecomesTarget);
        assert_eq!(def.valid_card, Some(TargetFilter::SelfRef));
        assert_eq!(def.valid_source, Some(TargetFilter::StackSpell));
    }

    #[test]
    fn trigger_put_into_graveyard_from_anywhere() {
        let def = parse_trigger_line(
            "When this card is put into a graveyard from anywhere, draw a card.",
            "Dread",
        );
        assert_eq!(def.mode, TriggerMode::ChangesZone);
        assert_eq!(def.destination, Some(Zone::Graveyard));
        assert_eq!(def.valid_card, Some(TargetFilter::SelfRef));
        assert_eq!(def.origin, None);
    }

    #[test]
    fn trigger_you_discard_a_card() {
        let def = parse_trigger_line(
            "Whenever you discard a card, draw a card.",
            "Bag of Holding",
        );
        assert_eq!(def.mode, TriggerMode::Discarded);
        assert_eq!(
            def.valid_card,
            Some(TargetFilter::Typed(
                TypedFilter::new(TypeFilter::Card).controller(ControllerRef::You)
            ))
        );
    }

    #[test]
    fn trigger_opponent_discards_a_card() {
        let def = parse_trigger_line(
            "Whenever an opponent discards a card, draw a card.",
            "Geth's Grimoire",
        );
        assert_eq!(def.mode, TriggerMode::Discarded);
        assert_eq!(
            def.valid_card,
            Some(TargetFilter::Typed(
                TypedFilter::new(TypeFilter::Card).controller(ControllerRef::Opponent)
            ))
        );
    }

    /// CR 701.9 + CR 603.7c + CR 406.1: Necropotence's on-discard trigger
    /// exiles the just-discarded card from the graveyard. The "that card"
    /// anaphor must lift from `ParentTarget` to `TriggeringSource` so the
    /// `ChangeZone { origin: Some(Graveyard), destination: Exile }` resolves
    /// against the discarded object, and the controller filter must be
    /// `You`. Locks the parse output for the Necropotence punisher-class
    /// (Necropotence, Yawgmoth's Bargain-ish self-discard punishers).
    #[test]
    fn trigger_necropotence_you_discard_exile_from_graveyard() {
        let def = parse_trigger_line(
            "Whenever you discard a card, exile that card from your graveyard.",
            "Necropotence",
        );
        assert_eq!(def.mode, TriggerMode::Discarded);
        match def.valid_card.as_ref().expect("valid_card must be set") {
            TargetFilter::Typed(tf) => {
                assert_eq!(tf.controller, Some(ControllerRef::You));
            }
            other => panic!("expected Typed filter, got {other:?}"),
        }
        let execute = def
            .execute
            .as_deref()
            .expect("on-discard trigger must have an execute body");
        match execute.effect.as_ref() {
            Effect::ChangeZone {
                origin,
                destination,
                target,
                ..
            } => {
                assert_eq!(*origin, Some(Zone::Graveyard));
                assert_eq!(*destination, Zone::Exile);
                assert!(
                    matches!(target, TargetFilter::TriggeringSource),
                    "expected TriggeringSource after lift, got {target:?}",
                );
            }
            other => panic!("expected ChangeZone (graveyard → exile), got {other:?}"),
        }
    }

    /// CR 701.9a + CR 603.2c: type qualifier on the discarded card must be
    /// preserved so a "discards a land card" trigger fires only on land
    /// discards (Aclazotz, Deepest Betrayal class).
    #[test]
    fn trigger_opponent_discards_a_land_card() {
        let def = parse_trigger_line(
            "Whenever an opponent discards a land card, create a 1/1 black Bat creature token with flying.",
            "Aclazotz, Deepest Betrayal",
        );
        assert_eq!(def.mode, TriggerMode::Discarded);
        match def.valid_card.as_ref().expect("valid_card must be set") {
            TargetFilter::Typed(tf) => {
                assert!(
                    tf.type_filters.contains(&TypeFilter::Land),
                    "expected Land in type_filters, got {:?}",
                    tf.type_filters
                );
                assert_eq!(tf.controller, Some(ControllerRef::Opponent));
            }
            other => panic!("expected Typed filter, got {other:?}"),
        }
    }

    #[test]
    fn trigger_you_sacrifice_another_permanent() {
        let def = parse_trigger_line(
            "Whenever you sacrifice another permanent, draw a card.",
            "Furnace Celebration",
        );
        assert_eq!(def.mode, TriggerMode::Sacrificed);
        assert_eq!(
            def.valid_card,
            Some(TargetFilter::Typed(
                TypedFilter::new(TypeFilter::Permanent)
                    .controller(ControllerRef::You)
                    .properties(vec![FilterProp::Another])
            ))
        );
    }

    #[test]
    fn trigger_player_cycles_a_card() {
        let def = parse_trigger_line(
            "Whenever a player cycles a card, draw a card.",
            "Astral Slide",
        );
        assert_eq!(def.mode, TriggerMode::Cycled);
    }

    #[test]
    fn trigger_spell_cast_or_copy_mode() {
        let def = parse_trigger_line(
            "Whenever you cast or copy an instant or sorcery spell, create a Treasure token.",
            "Storm-Kiln Artist",
        );
        assert_eq!(def.mode, TriggerMode::SpellCastOrCopy);
        assert_eq!(def.valid_target, Some(TargetFilter::Controller));
    }

    #[test]
    fn trigger_unlock_door_mode() {
        let def = parse_trigger_line("When you unlock this door, draw a card.", "Door");
        assert_eq!(def.mode, TriggerMode::UnlockDoor);
    }

    #[test]
    fn trigger_you_fully_unlock_a_room_mode() {
        let def = parse_trigger_line(
            "Whenever you fully unlock a Room, draw a card.",
            "Entity Tracker",
        );
        assert_eq!(def.mode, TriggerMode::FullyUnlock);
        assert_eq!(def.valid_target, Some(TargetFilter::Controller));
        assert_eq!(
            def.valid_card,
            Some(TargetFilter::Typed(
                TypedFilter::default().subtype("Room".to_string())
            ))
        );
        assert!(matches!(
            def.execute
                .as_deref()
                .map(|ability| ability.effect.as_ref()),
            Some(Effect::Draw { .. })
        ));
    }

    #[test]
    fn trigger_you_fully_unlock_room_self_return_uses_graveyard_zone() {
        let def = parse_trigger_line(
            "Whenever you fully unlock a Room, you may return this card from your graveyard to your hand.",
            "Fear of Infinity",
        );
        assert_eq!(def.mode, TriggerMode::FullyUnlock);
        assert_eq!(def.trigger_zones, vec![Zone::Graveyard]);
        assert!(def.optional);
        assert!(matches!(
            def.execute
                .as_deref()
                .map(|ability| ability.effect.as_ref()),
            Some(Effect::Bounce {
                target: TargetFilter::SelfRef,
                destination: None,
            })
        ));
    }

    #[test]
    fn trigger_this_card_becomes_plotted_uses_exile_zone() {
        let def = parse_trigger_line(
            "When this card becomes plotted, it deals 2 damage to any target.",
            "Aloe Alchemist",
        );
        assert_eq!(def.mode, TriggerMode::BecomesPlotted);
        assert_eq!(def.trigger_zones, vec![Zone::Exile]);
        assert!(matches!(
            def.execute
                .as_deref()
                .map(|ability| ability.effect.as_ref()),
            Some(Effect::DealDamage { .. })
        ));
    }

    #[test]
    fn trigger_mutates_mode() {
        let def = parse_trigger_line("Whenever this creature mutates, draw a card.", "Gemrazer");
        assert_eq!(def.mode, TriggerMode::Mutates);
        assert_eq!(def.valid_card, Some(TargetFilter::SelfRef));
    }

    #[test]
    fn trigger_becomes_untapped_mode() {
        let def = parse_trigger_line(
            "Whenever this creature becomes untapped, draw a card.",
            "Arbiter of the Ideal",
        );
        assert_eq!(def.mode, TriggerMode::Untaps);
        assert_eq!(def.valid_card, Some(TargetFilter::SelfRef));
    }

    #[test]
    fn trigger_self_or_another_ally_enters() {
        let def = parse_trigger_line(
            "Whenever this creature or another Ally you control enters, you gain 1 life.",
            "Hada Freeblade",
        );
        assert_eq!(def.mode, TriggerMode::ChangesZone);
        assert!(matches!(def.valid_card, Some(TargetFilter::Or { .. })));
        assert_eq!(def.destination, Some(Zone::Battlefield));
    }

    #[test]
    fn trigger_may_have_self_become_named_equipment_if_you_do() {
        let def = parse_trigger_line(
            "Whenever a legendary creature you control enters, you may have The Irencrag become a legendary Equipment artifact named Everflame, Heroes' Legacy. If you do, it gains equip {3} and \"Equipped creature gets +3/+3\" and loses all other abilities.",
            "The Irencrag",
        );

        assert!(def.optional);
        let execute = def.execute.as_ref().expect("trigger execute ability");
        assert!(execute.optional);

        let Effect::GenericEffect {
            static_abilities, ..
        } = execute.effect.as_ref()
        else {
            panic!("expected GenericEffect, got {:?}", execute.effect);
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

        let sub_ability = execute.sub_ability.as_ref().expect("If you do sub-ability");
        assert!(matches!(
            sub_ability.condition,
            Some(AbilityCondition::IfYouDo)
        ));
        let Effect::GenericEffect {
            static_abilities, ..
        } = sub_ability.effect.as_ref()
        else {
            panic!(
                "expected GenericEffect sub-ability, got {:?}",
                sub_ability.effect
            );
        };
        assert!(matches!(
            static_abilities[0].affected,
            Some(TargetFilter::SelfRef)
        ));
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
    fn trigger_another_human_you_control_enters() {
        let def = parse_trigger_line(
            "Whenever another Human you control enters, draw a card.",
            "Welcoming Vampire",
        );
        assert_eq!(def.mode, TriggerMode::ChangesZone);
        assert_eq!(def.destination, Some(Zone::Battlefield));
        assert_eq!(
            def.valid_card,
            Some(TargetFilter::Typed(
                TypedFilter::default()
                    .subtype("Human".to_string())
                    .controller(ControllerRef::You)
                    .properties(vec![FilterProp::Another])
            ))
        );
    }

    #[test]
    fn trigger_dragon_you_control_attacks() {
        let def = parse_trigger_line(
            "Whenever a Dragon you control attacks, create a Treasure token.",
            "Ganax, Astral Hunter",
        );
        assert_eq!(def.mode, TriggerMode::Attacks);
        assert_eq!(
            def.valid_card,
            Some(TargetFilter::Typed(
                TypedFilter::default()
                    .subtype("Dragon".to_string())
                    .controller(ControllerRef::You)
            ))
        );
    }

    #[test]
    fn trigger_samurai_or_warrior_attacks_alone() {
        let def = parse_trigger_line(
            "Whenever a Samurai or Warrior you control attacks alone, draw a card.",
            "Raiyuu, Storm's Edge",
        );
        // Now that parse_type_phrase recognizes subtypes ("Samurai", "Warrior"),
        // the trigger parser correctly identifies this as an Attacks trigger.
        assert!(matches!(def.mode, TriggerMode::Attacks));
    }

    #[test]
    fn trigger_this_siege_enters_is_self_etb() {
        let def = parse_trigger_line("When this Siege enters, draw a card.", "Invasion");
        assert_eq!(def.mode, TriggerMode::ChangesZone);
        assert_eq!(def.destination, Some(Zone::Battlefield));
        assert_eq!(def.valid_card, Some(TargetFilter::SelfRef));
    }

    // --- Phase trigger possessive qualifier tests ---

    #[test]
    fn phase_trigger_your_upkeep() {
        let def = parse_trigger_line("At the beginning of your upkeep, draw a card.", "Test Card");
        assert_eq!(def.mode, TriggerMode::Phase);
        assert_eq!(def.phase, Some(Phase::Upkeep));
        assert_eq!(def.constraint, Some(TriggerConstraint::OnlyDuringYourTurn));
    }

    #[test]
    fn hope_estheim_end_step_mill_keeps_you_quantity_controller_scoped() {
        let def = parse_trigger_line(
            "At the beginning of your end step, each opponent mills X cards, where X is the amount of life you gained this turn.",
            "Hope Estheim",
        );
        let execute = def.execute.as_ref().expect("trigger execute ability");

        assert_eq!(execute.player_scope, Some(PlayerFilter::Opponent));
        match &*execute.effect {
            Effect::Mill { target, count, .. } => {
                assert_eq!(*target, TargetFilter::Controller);
                assert!(matches!(
                    count,
                    QuantityExpr::Ref {
                        qty: QuantityRef::LifeGainedThisTurn {
                            player: PlayerScope::Controller
                        }
                    }
                ));
            }
            other => panic!("expected Mill effect, got {other:?}"),
        }
    }

    #[test]
    fn phase_trigger_combat_on_your_turn() {
        let def = parse_trigger_line(
            "At the beginning of combat on your turn, target creature gets +1/+1 until end of turn.",
            "Test Card",
        );
        assert_eq!(def.mode, TriggerMode::Phase);
        assert_eq!(def.phase, Some(Phase::BeginCombat));
        assert_eq!(def.constraint, Some(TriggerConstraint::OnlyDuringYourTurn));
    }

    #[test]
    fn phase_trigger_each_players_upkeep_no_constraint() {
        let def = parse_trigger_line(
            "At the beginning of each player's upkeep, that player draws a card.",
            "Test Card",
        );
        assert_eq!(def.mode, TriggerMode::Phase);
        assert_eq!(def.phase, Some(Phase::Upkeep));
        assert_eq!(def.constraint, None);
    }

    /// CR 603.2b + CR 102.1: Dictate of Kruphix / Kami of the Crescent Moon —
    /// "At the beginning of each player's draw step, that player draws an
    /// additional card." `target` must be `ScopedPlayer`; the runtime then
    /// binds it to the active player at trigger fire time
    /// (`triggers::build_triggered_ability` for Phase triggers).
    #[test]
    fn phase_trigger_each_players_draw_step_that_player_draws() {
        let def = parse_trigger_line(
            "At the beginning of each player's draw step, that player draws an additional card.",
            "Dictate of Kruphix",
        );
        assert_eq!(def.mode, TriggerMode::Phase);
        assert_eq!(def.phase, Some(Phase::Draw));
        assert_eq!(def.constraint, None);
        match def.execute.as_ref().map(|ability| ability.effect.as_ref()) {
            Some(Effect::Draw { target, .. }) => {
                assert_eq!(target, &TargetFilter::ScopedPlayer);
            }
            other => panic!("expected Draw effect with ScopedPlayer target, got {other:?}"),
        }
    }

    /// CR 603.2b + CR 603.4 + CR 102.1: Ghirapur Orrery — the intervening-if
    /// "if that player has no cards in hand" must hoist onto the trigger
    /// definition as a `QuantityComparison` against `HandSize { ScopedPlayer }`,
    /// not be silently dropped. Without this, every player would draw three
    /// cards on every upkeep regardless of hand size.
    #[test]
    fn phase_trigger_intervening_if_that_player_no_cards_in_hand() {
        let def = parse_trigger_line(
            "At the beginning of each player's upkeep, if that player has no cards in hand, that player draws three cards.",
            "Ghirapur Orrery",
        );
        assert_eq!(def.mode, TriggerMode::Phase);
        assert_eq!(def.phase, Some(Phase::Upkeep));
        match def.condition.as_ref() {
            Some(TriggerCondition::QuantityComparison {
                lhs,
                comparator,
                rhs,
            }) => {
                assert_eq!(
                    lhs,
                    &QuantityExpr::Ref {
                        qty: QuantityRef::HandSize {
                            player: PlayerScope::ScopedPlayer,
                        },
                    }
                );
                assert_eq!(*comparator, Comparator::EQ);
                assert_eq!(rhs, &QuantityExpr::Fixed { value: 0 });
            }
            other => panic!("expected QuantityComparison condition, got {other:?}"),
        }
        match def.execute.as_ref().map(|ability| ability.effect.as_ref()) {
            Some(Effect::Draw { target, .. }) => {
                assert_eq!(target, &TargetFilter::ScopedPlayer);
            }
            other => panic!("expected Draw effect with ScopedPlayer target, got {other:?}"),
        }
    }

    #[test]
    fn phase_trigger_that_player_sacrifices_uses_scoped_player_not_target_player() {
        let def = parse_trigger_line(
            "At the beginning of each player's upkeep, that player sacrifices a non-Elf creature of their choice.",
            "Ruthless Winnower",
        );

        assert_eq!(def.mode, TriggerMode::Phase);
        assert_eq!(def.phase, Some(Phase::Upkeep));
        assert_eq!(def.valid_target, None);
        match def.execute.as_ref().map(|ability| ability.effect.as_ref()) {
            Some(Effect::Sacrifice { target, .. }) => match target {
                TargetFilter::Typed(filter) => {
                    assert_eq!(filter.controller, Some(ControllerRef::ScopedPlayer));
                }
                other => panic!("expected typed sacrifice filter, got {other:?}"),
            },
            other => panic!("expected Sacrifice effect, got {other:?}"),
        }
    }

    #[test]
    fn phase_trigger_enchanted_players_first_upkeep() {
        let def = parse_trigger_line(
            "At the beginning of enchanted player's first upkeep each turn, that player gets an additional upkeep step after this step.",
            "Paradox Haze",
        );
        assert_eq!(def.mode, TriggerMode::Phase);
        assert_eq!(def.phase, Some(Phase::Upkeep));
        assert_eq!(def.valid_target, Some(TargetFilter::AttachedTo));
        assert_eq!(
            def.constraint,
            Some(TriggerConstraint::MaxTimesPerTurn { max: 1 })
        );
        assert!(matches!(
            def.execute.as_ref().map(|ability| ability.effect.as_ref()),
            Some(Effect::AdditionalPhase {
                target: TargetFilter::TriggeringPlayer,
                phase: Phase::Upkeep,
                after: Phase::Upkeep,
                followed_by,
            }) if followed_by.is_empty()
        ));
    }

    #[test]
    fn phase_trigger_each_opponents_upkeep() {
        let def = parse_trigger_line(
            "At the beginning of each opponent's upkeep, this creature deals 1 damage to that player.",
            "Test Card",
        );
        assert_eq!(def.mode, TriggerMode::Phase);
        assert_eq!(def.phase, Some(Phase::Upkeep));
        assert_eq!(
            def.constraint,
            Some(TriggerConstraint::OnlyDuringOpponentsTurn)
        );
    }

    #[test]
    fn phase_trigger_each_combat_no_constraint() {
        let def = parse_trigger_line(
            "At the beginning of each combat, create a 1/1 white Soldier creature token.",
            "Test Card",
        );
        assert_eq!(def.mode, TriggerMode::Phase);
        assert_eq!(def.phase, Some(Phase::BeginCombat));
        assert_eq!(def.constraint, None);
    }

    #[test]
    fn trigger_optional_sub_ability_not_optional() {
        // "you may" applies to the first sentence only; the sub-ability
        // should not inherit optional.
        let def = parse_trigger_line(
            "When this creature enters, you may draw a card. Create a 1/1 white Soldier creature token.",
            "Some Card",
        );
        assert!(def.optional);
        let execute = def.execute.as_ref().unwrap();
        assert!(execute.optional, "root ability should be optional");
        let sub = execute
            .sub_ability
            .as_ref()
            .expect("should have sub_ability");
        assert!(!sub.optional, "sub-ability should NOT be optional");
    }

    #[test]
    fn trigger_you_may_mid_chain_not_trigger_optional() {
        // "you may" is in the second sentence — trigger-level optional is false,
        // but the second sentence's ability should have optional = true.
        let def = parse_trigger_line(
            "When this creature enters, draw a card. You may discard a card.",
            "Some Card",
        );
        assert!(!def.optional, "trigger-level optional should be false");
        let execute = def.execute.as_ref().unwrap();
        assert!(!execute.optional, "root ability should NOT be optional");
        let sub = execute
            .sub_ability
            .as_ref()
            .expect("should have sub_ability");
        assert!(sub.optional, "second sentence ability should be optional");
    }

    // ── Work Item 1: Leaves-Graveyard Batch Triggers ──────────────

    #[test]
    fn trigger_one_or_more_creature_cards_leave_graveyard() {
        let def = parse_trigger_line(
            "Whenever one or more creature cards leave your graveyard, create a 1/1 green and black Insect creature token.",
            "Insidious Roots",
        );
        assert_eq!(def.mode, TriggerMode::ChangesZoneAll);
        assert_eq!(def.origin, Some(Zone::Graveyard));
        assert!(def.batched);
        assert!(def.valid_card.is_some());
    }

    #[test]
    fn trigger_one_or_more_cards_leave_graveyard() {
        let def = parse_trigger_line(
            "Whenever one or more cards leave your graveyard, put a +1/+1 counter on this creature.",
            "Chalk Outline",
        );
        assert_eq!(def.mode, TriggerMode::ChangesZoneAll);
        assert_eq!(def.origin, Some(Zone::Graveyard));
        assert!(def.batched);
        assert_eq!(def.valid_card, None); // no type filter — "cards"
    }

    #[test]
    fn trigger_one_or_more_cards_leave_graveyard_during_your_turn() {
        let def = parse_trigger_line(
            "Whenever one or more cards leave your graveyard during your turn, you gain 1 life.",
            "Soul Enervation",
        );
        assert_eq!(def.mode, TriggerMode::ChangesZoneAll);
        assert_eq!(def.origin, Some(Zone::Graveyard));
        assert!(def.batched);
        assert_eq!(def.constraint, Some(TriggerConstraint::OnlyDuringYourTurn));
    }

    #[test]
    fn trigger_one_or_more_cards_put_into_exile_from_library_or_graveyard() {
        // CR 603.2c + CR 603.10a: Laelia, the Blade Reforged — batched
        // zone-change trigger with disjunctive source zones.
        let def = parse_trigger_line(
            "Whenever one or more cards are put into exile from your library and/or your graveyard, put a +1/+1 counter on Laelia.",
            "Laelia, the Blade Reforged",
        );
        assert_eq!(def.mode, TriggerMode::ChangesZoneAll);
        assert_eq!(def.destination, Some(Zone::Exile));
        assert_eq!(def.origin_zones, vec![Zone::Library, Zone::Graveyard]);
        assert!(def.batched);
    }

    #[test]
    fn trigger_one_or_more_cards_put_into_exile_from_library_only() {
        // Single-zone source variant should still parse.
        let def = parse_trigger_line(
            "Whenever one or more cards are put into exile from your library, put a +1/+1 counter on this creature.",
            "Test Card",
        );
        assert_eq!(def.mode, TriggerMode::ChangesZoneAll);
        assert_eq!(def.destination, Some(Zone::Exile));
        assert_eq!(def.origin_zones, vec![Zone::Library]);
        assert!(def.batched);
    }

    #[test]
    fn trigger_one_or_more_artifact_or_creature_cards_leave_graveyard() {
        let def = parse_trigger_line(
            "Whenever one or more artifact and/or creature cards leave your graveyard, put a +1/+1 counter on this creature.",
            "Attuned Hunter",
        );
        assert_eq!(def.mode, TriggerMode::ChangesZoneAll);
        assert_eq!(def.origin, Some(Zone::Graveyard));
        assert!(def.batched);
        assert!(matches!(def.valid_card, Some(TargetFilter::Or { .. })));
    }

    // ── Work Item 2: Discard Batch Triggers ───────────────────────

    #[test]
    fn trigger_you_discard_one_or_more_cards() {
        let def = parse_trigger_line(
            "Whenever you discard one or more cards, this creature gets +1/+0 until end of turn.",
            "Magmakin Artillerist",
        );
        assert_eq!(def.mode, TriggerMode::DiscardedAll);
        assert!(def.batched);
        assert_eq!(def.valid_target, Some(TargetFilter::Controller));
    }

    #[test]
    fn trigger_one_or_more_players_discard() {
        let def = parse_trigger_line(
            "Whenever one or more players discard one or more cards, put a +1/+1 counter on this creature.",
            "Waste Not",
        );
        assert_eq!(def.mode, TriggerMode::DiscardedAll);
        assert!(def.batched);
        assert_eq!(def.valid_target, None); // any player
    }

    // ── Work Item 3: Noncombat Damage to Opponent ─────────────────

    #[test]
    fn trigger_noncombat_damage_to_opponent() {
        let def = parse_trigger_line(
            "Whenever a source you control deals noncombat damage to an opponent, create a 1/1 red Elemental creature token.",
            "Virtue of Courage",
        );
        assert_eq!(def.mode, TriggerMode::DamageDone);
        assert_eq!(def.damage_kind, DamageKindFilter::NoncombatOnly);
        assert!(matches!(
            def.valid_source,
            Some(TargetFilter::Typed(TypedFilter {
                controller: Some(ControllerRef::You),
                ..
            }))
        ));
        assert!(matches!(
            def.valid_target,
            Some(TargetFilter::Typed(TypedFilter {
                controller: Some(ControllerRef::Opponent),
                ..
            }))
        ));
    }

    // ── Work Item 4: Transforms Into Self ─────────────────────────

    #[test]
    fn trigger_transforms_into_self() {
        let def = parse_trigger_line(
            "When this creature transforms into Trystan, Penitent Culler, you gain 3 life.",
            "Trystan, Penitent Culler",
        );
        assert_eq!(def.mode, TriggerMode::Transformed);
        assert_eq!(def.valid_source, Some(TargetFilter::SelfRef));
    }

    // ── Work Item 5: Tap Opponent's Creature ──────────────────────

    #[test]
    fn trigger_you_tap_opponent_creature() {
        let def = parse_trigger_line(
            "Whenever you tap an untapped creature an opponent controls, you gain 1 life.",
            "Hylda of the Icy Crown",
        );
        assert_eq!(def.mode, TriggerMode::Taps);
        assert!(matches!(
            def.valid_card,
            Some(TargetFilter::Typed(TypedFilter {
                controller: Some(ControllerRef::Opponent),
                ..
            }))
        ));
    }

    // ── Work Item 6: Expend Triggers ──────────────────────────────

    #[test]
    fn trigger_expend_4() {
        let def = parse_trigger_line(
            "Whenever you expend 4, put a +1/+1 counter on this creature.",
            "Roughshod Duo",
        );
        assert_eq!(def.mode, TriggerMode::ManaExpend);
        assert_eq!(def.expend_threshold, Some(4));
    }

    #[test]
    fn trigger_expend_8() {
        let def = parse_trigger_line("Whenever you expend 8, draw a card.", "Wandertale Mentor");
        assert_eq!(def.mode, TriggerMode::ManaExpend);
        assert_eq!(def.expend_threshold, Some(8));
    }

    #[test]
    fn trigger_plural_deal_combat_damage() {
        // CR 120.1: Plural "deal" for &-names after ~ normalization
        let def = parse_trigger_line(
            "Whenever Dark Leo & Shredder deal combat damage to a player, create a 1/1 black Ninja creature token.",
            "Dark Leo & Shredder",
        );
        assert_eq!(def.mode, TriggerMode::DamageDone);
        assert_eq!(def.damage_kind, DamageKindFilter::CombatOnly);
    }

    #[test]
    fn trigger_singular_deals_combat_damage_regression() {
        // Ensure singular "deals" still works
        let def = parse_trigger_line(
            "Whenever Ninja of the Deep Hours deals combat damage to a player, you may draw a card.",
            "Ninja of the Deep Hours",
        );
        assert_eq!(def.mode, TriggerMode::DamageDone);
        assert_eq!(def.damage_kind, DamageKindFilter::CombatOnly);
        assert_eq!(def.valid_target, Some(TargetFilter::Player));
    }

    #[test]
    fn trigger_one_or_more_ninja_or_rogue_combat_damage() {
        // CR 205.3m + CR 603.2c: Compound subtype in "one or more" batched damage trigger
        let result = try_parse_one_or_more_combat_damage_to_player(
            "whenever one or more ninja or rogue creatures you control deal combat damage to a player",
        );
        assert!(
            result.is_some(),
            "should parse one-or-more compound trigger"
        );
        let (mode, def) = result.unwrap();
        assert_eq!(mode, TriggerMode::DamageDoneOnceByController);
        assert_eq!(def.damage_kind, DamageKindFilter::CombatOnly);
        assert!(
            matches!(&def.valid_source, Some(TargetFilter::Or { filters }) if filters.len() == 2)
        );
    }

    #[test]
    fn trigger_etb_from_hand_if_attacking() {
        // Thousand-Faced Shadow: "When this creature enters from your hand, if it's attacking, ..."
        let def = parse_trigger_line(
            "When this creature enters from your hand, if it's attacking, create a token that's a copy of another target attacking creature. The token enters tapped and attacking.",
            "Thousand-Faced Shadow",
        );
        assert_eq!(def.mode, TriggerMode::ChangesZone);
        assert_eq!(def.destination, Some(Zone::Battlefield));
        assert_eq!(def.origin, Some(Zone::Hand));
        assert_eq!(def.valid_card, Some(TargetFilter::SelfRef));
        assert_eq!(def.condition, Some(TriggerCondition::SourceIsAttacking));
        // Effect should be CopyTokenOf
        assert!(def.execute.is_some());
        let exec = def.execute.as_ref().unwrap();
        assert!(matches!(*exec.effect, Effect::CopyTokenOf { .. }));
    }

    /// CR 603.6 + CR 603.6a — Flayer of the Hatebound: "enters from your
    /// graveyard" must set `origin = Some(Graveyard)` on the ChangesZone
    /// trigger. Issue #396 — previously the origin-zone qualifier was
    /// dropped, so the trigger fired on any creature entering from any zone
    /// (including commanders cast from the command zone).
    #[test]
    fn trigger_etb_from_graveyard_flayer() {
        let def = parse_trigger_line(
            "Whenever this creature or another creature enters from your graveyard, that creature deals damage equal to its power to any target.",
            "Flayer of the Hatebound",
        );
        assert_eq!(def.mode, TriggerMode::ChangesZone);
        assert_eq!(def.destination, Some(Zone::Battlefield));
        assert_eq!(def.origin, Some(Zone::Graveyard));
    }

    /// CR 603.6 + CR 603.6a — origin extraction for "enters from exile"
    /// triggers (e.g. cards that fire when a creature comes back from exile).
    #[test]
    fn trigger_etb_from_exile_origin() {
        let def = parse_trigger_line(
            "Whenever another creature enters from exile, draw a card.",
            "Test Source",
        );
        assert_eq!(def.mode, TriggerMode::ChangesZone);
        assert_eq!(def.destination, Some(Zone::Battlefield));
        assert_eq!(def.origin, Some(Zone::Exile));
    }

    /// CR 508.4 + CR 614.1 — Kaalia of the Vast: the inline-tail patcher in
    /// `try_parse_put_zone_change` must lift "tapped and attacking that
    /// opponent" onto the produced `Effect::ChangeZone`, setting both
    /// `enter_tapped` and `enters_attacking`.
    #[test]
    fn trigger_attacks_inline_tail_kaalia_tapped_and_attacking() {
        let def = parse_trigger_line(
            "Whenever Kaalia attacks an opponent, you may put an Angel, Demon, or Dragon creature card from your hand onto the battlefield tapped and attacking that opponent.",
            "Kaalia of the Vast",
        );
        assert_eq!(def.mode, TriggerMode::Attacks);
        let exec = def.execute.as_ref().expect("expected execute");
        match &*exec.effect {
            Effect::ChangeZone {
                destination,
                enter_tapped,
                enters_attacking,
                ..
            } => {
                assert_eq!(*destination, Zone::Battlefield);
                assert!(*enter_tapped, "expected enter_tapped");
                assert!(*enters_attacking, "expected enters_attacking");
            }
            other => panic!("expected ChangeZone, got {other:?}"),
        }
    }

    /// CR 508.4 — Ilharg / Preeminent Captain bare form: tail without a
    /// trailing player phrase. Both flags must still be set.
    #[test]
    fn trigger_attacks_inline_tail_ilharg_bare_tapped_and_attacking() {
        let def = parse_trigger_line(
            "Whenever Ilharg attacks, you may put a creature card from your hand onto the battlefield tapped and attacking.",
            "Ilharg, the Raze-Boar",
        );
        assert_eq!(def.mode, TriggerMode::Attacks);
        let exec = def.execute.as_ref().expect("expected execute");
        match &*exec.effect {
            Effect::ChangeZone {
                destination,
                enter_tapped,
                enters_attacking,
                ..
            } => {
                assert_eq!(*destination, Zone::Battlefield);
                assert!(*enter_tapped);
                assert!(*enters_attacking);
            }
            other => panic!("expected ChangeZone, got {other:?}"),
        }
    }

    /// CR 508.4 — Negative regression for the existing separate-sentence
    /// patcher (`ContinuationAst::EntersTappedAttacking`). Stangg / Shark
    /// Shredder / Thousand-Faced Shadow style "It enters tapped and attacking"
    /// in a follow-on sentence must continue to set both flags on the prior
    /// effect — the inline-tail patcher must not interfere.
    #[test]
    fn trigger_separate_sentence_patcher_still_sets_both_flags() {
        // Synthetic Stangg-style: a token effect followed by a separate
        // "It enters tapped and attacking" sentence patcher.
        let def = parse_trigger_line(
            "When this creature enters, create a 3/3 red Cat creature token. It enters tapped and attacking.",
            "Stangg-Style Test",
        );
        assert_eq!(def.mode, TriggerMode::ChangesZone);
        let exec = def.execute.as_ref().expect("expected execute");
        match &*exec.effect {
            Effect::Token {
                tapped,
                enters_attacking,
                ..
            } => {
                assert!(*tapped, "separate-sentence patcher must set tapped");
                assert!(
                    *enters_attacking,
                    "separate-sentence patcher must set enters_attacking"
                );
            }
            other => panic!("expected Token, got {other:?}"),
        }
    }

    #[test]
    fn cast_variant_paid_sneak_condition() {
        // CR 702.190a: "if its sneak cost was paid" → CastVariantPaid { variant: Sneak }
        let def = parse_trigger_line(
            "When this creature enters, if its sneak cost was paid, draw a card.",
            "Test Ninja",
        );
        assert_eq!(def.mode, TriggerMode::ChangesZone);
        assert_eq!(def.destination, Some(Zone::Battlefield));
        assert_eq!(
            def.condition,
            Some(TriggerCondition::CastVariantPaid {
                variant: CastVariantPaid::Sneak,
            })
        );
    }

    #[test]
    fn cast_variant_paid_ninjutsu_condition() {
        // CR 702.49: "if its ninjutsu cost was paid" → CastVariantPaid { variant: Ninjutsu }
        let def = parse_trigger_line(
            "When this creature enters, if its ninjutsu cost was paid, target opponent discards a card.",
            "Test Ninja",
        );
        assert_eq!(
            def.condition,
            Some(TriggerCondition::CastVariantPaid {
                variant: CastVariantPaid::Ninjutsu,
            })
        );
    }

    // CR 702.138b + CR 603.4: Phlage, Titan of Fire's Fury — "sacrifice it
    // unless it escaped" must (a) resolve "it" to SelfRef via pronoun context,
    // not ParentTarget, and (b) attach a negated CastVariantPaid { Escape }
    // intervening-if so the sacrifice fires on hard-casts and reanimation but
    // not on escape casts.
    #[test]
    fn phlage_unless_it_escaped_attaches_negated_escape_condition() {
        let def = parse_trigger_line(
            "When ~ enters, sacrifice it unless it escaped.",
            "Phlage, Titan of Fire's Fury",
        );
        assert_eq!(def.mode, TriggerMode::ChangesZone);
        assert_eq!(def.destination, Some(Zone::Battlefield));
        assert_eq!(def.valid_card, Some(TargetFilter::SelfRef));
        assert_eq!(
            def.condition,
            Some(TriggerCondition::Not {
                condition: Box::new(TriggerCondition::CastVariantPaid {
                    variant: CastVariantPaid::Escape,
                }),
            })
        );
        let execute = def.execute.as_deref().expect("execute ability");
        match &*execute.effect {
            Effect::Sacrifice { target, .. } => {
                assert_eq!(
                    *target,
                    TargetFilter::SelfRef,
                    "`sacrifice it` in an ETB-self trigger should resolve to SelfRef"
                );
            }
            other => panic!("expected Sacrifice, got {other:?}"),
        }
    }

    #[test]
    fn grave_pact_scopes_sacrifice_to_other_players() {
        let def = parse_trigger_line(
            "Whenever a creature you control dies, each other player sacrifices a creature of their choice.",
            "Grave Pact",
        );
        assert_eq!(def.mode, TriggerMode::ChangesZone);
        assert_eq!(def.origin, Some(Zone::Battlefield));
        assert_eq!(def.destination, Some(Zone::Graveyard));
        assert_eq!(
            def.valid_card,
            Some(TargetFilter::Typed(
                TypedFilter::creature().controller(ControllerRef::You)
            ))
        );

        let execute = def.execute.as_deref().expect("execute ability");
        match &*execute.effect {
            Effect::Sacrifice { target, count, .. } => {
                assert_eq!(*count, QuantityExpr::Fixed { value: 1 });
                assert_eq!(
                    *target,
                    TargetFilter::Typed(TypedFilter::creature().controller(ControllerRef::You))
                );
            }
            other => panic!("expected Sacrifice, got {other:?}"),
        }
    }

    #[test]
    fn portal_to_phyrexia_etb_scopes_each_opponent_sacrifice_to_scoped_player() {
        let def = parse_trigger_line(
            "When this artifact enters, each opponent sacrifices three creatures of their choice.",
            "Portal to Phyrexia",
        );
        let execute = def.execute.as_deref().expect("execute ability");
        assert_eq!(execute.player_scope, Some(PlayerFilter::Opponent));
        match &*execute.effect {
            Effect::Sacrifice { target, count, .. } => {
                assert_eq!(*count, QuantityExpr::Fixed { value: 3 });
                assert_eq!(
                    *target,
                    TargetFilter::Typed(TypedFilter::creature().controller(ControllerRef::You))
                );
            }
            other => panic!("expected Sacrifice, got {other:?}"),
        }
    }

    #[test]
    fn balefire_dragon_damages_creatures_controlled_by_damaged_player() {
        let def = parse_trigger_line(
            "Whenever this creature deals combat damage to a player, it deals that much damage to each creature controlled by that player.",
            "Balefire Dragon",
        );
        assert_eq!(def.mode, TriggerMode::DamageDone);
        assert_eq!(def.damage_kind, DamageKindFilter::CombatOnly);
        assert_eq!(def.valid_target, Some(TargetFilter::Player));

        let execute = def.execute.as_deref().expect("execute ability");
        match &*execute.effect {
            Effect::DamageAll { amount, target, .. } => {
                assert_eq!(
                    *amount,
                    QuantityExpr::Ref {
                        qty: QuantityRef::EventContextAmount,
                    }
                );
                assert_eq!(
                    *target,
                    TargetFilter::Typed(
                        TypedFilter::creature().controller(ControllerRef::TargetPlayer)
                    )
                );
            }
            other => panic!("expected DamageAll, got {other:?}"),
        }
    }

    #[test]
    fn ninjutsu_activation_trigger() {
        // CR 702.49a: "Whenever you activate a ninjutsu ability" → NinjutsuActivated
        let def = parse_trigger_line(
            "Whenever you activate a ninjutsu ability, look at the top three cards of your library.",
            "Satoru Umezawa",
        );
        assert_eq!(def.mode, TriggerMode::NinjutsuActivated);
    }

    #[test]
    fn ninjutsu_activation_trigger_with_once_per_turn() {
        // CR 702.49a: Ninjutsu activation with once-per-turn constraint
        let triggers = parse_trigger_lines(
            "Whenever you activate a ninjutsu ability, look at the top three cards of your library. Put one of them into your hand and the rest on the bottom of your library in any order. This ability triggers only once each turn.",
            "Satoru Umezawa",
        );
        assert_eq!(triggers.len(), 1);
        assert_eq!(triggers[0].mode, TriggerMode::NinjutsuActivated);
        assert_eq!(triggers[0].constraint, Some(TriggerConstraint::OncePerTurn));
    }

    #[test]
    fn exhaust_activation_trigger() {
        let def = parse_trigger_line(
            "Whenever you activate an exhaust ability, draw a card.",
            "Rangers' Aetherhive",
        );
        assert_eq!(def.mode, TriggerMode::ExhaustAbilityActivated);
        assert_eq!(def.condition, None);
    }

    #[test]
    fn exhaust_activation_trigger_non_mana_qualifier() {
        let def = parse_trigger_line(
            "Whenever you activate an exhaust ability that isn't a mana ability, draw a card.",
            "Sala, Deck Boss",
        );
        assert_eq!(def.mode, TriggerMode::ExhaustAbilityActivated);
        assert_eq!(
            def.condition,
            Some(TriggerCondition::ActivatedAbilityIsNonMana)
        );
    }

    // --- CR 115.9c: "that targets only [X]" trigger tests ---

    #[test]
    fn trigger_zada_targets_only_self() {
        let def = parse_trigger_line(
            "Whenever you cast an instant or sorcery spell that targets only Zada, copy that spell for each other creature you control.",
            "Zada, Hedron Grinder",
        );
        assert_eq!(def.mode, TriggerMode::SpellCast);
        // valid_card should be Or(Instant, Sorcery) with TargetsOnly { SelfRef } on each
        let valid_card = def.valid_card.expect("should have valid_card");
        if let TargetFilter::Or { filters } = &valid_card {
            assert_eq!(filters.len(), 2, "expected 2 branches for instant/sorcery");
            for f in filters {
                if let TargetFilter::Typed(tf) = f {
                    assert!(
                        tf.properties.iter().any(|p| matches!(p, FilterProp::TargetsOnly { filter } if **filter == TargetFilter::SelfRef)),
                        "expected TargetsOnly(SelfRef) in {tf:?}"
                    );
                } else {
                    panic!("expected Typed filter, got {f:?}");
                }
            }
        } else {
            panic!("expected Or filter, got {valid_card:?}");
        }
    }

    #[test]
    fn trigger_leyline_of_resonance_targets_only_single_creature_you_control() {
        let def = parse_trigger_line(
            "Whenever you cast an instant or sorcery spell that targets only a single creature you control, copy that spell.",
            "Leyline of Resonance",
        );
        assert_eq!(def.mode, TriggerMode::SpellCast);
        let valid_card = def.valid_card.expect("should have valid_card");
        if let TargetFilter::Or { filters } = &valid_card {
            assert_eq!(filters.len(), 2);
            for f in filters {
                if let TargetFilter::Typed(tf) = f {
                    assert!(
                        tf.properties
                            .iter()
                            .any(|p| matches!(p, FilterProp::TargetsOnly { .. })),
                        "expected TargetsOnly in {tf:?}"
                    );
                    assert!(
                        tf.properties
                            .iter()
                            .any(|p| matches!(p, FilterProp::HasSingleTarget)),
                        "expected HasSingleTarget in {tf:?}"
                    );
                } else {
                    panic!("expected Typed filter, got {f:?}");
                }
            }
        } else {
            panic!("expected Or filter, got {valid_card:?}");
        }
    }

    #[test]
    fn enters_tapped_and_attacking_patches_change_zone() {
        // CR 508.4: Shark Shredder — "put ... onto the battlefield under your control.
        // It enters tapped and attacking that player."
        let def = parse_trigger_line(
            "Whenever Shark Shredder deals combat damage to a player, put up to one target creature card from that player's graveyard onto the battlefield under your control. It enters tapped and attacking that player.",
            "Shark Shredder, Killer Clone",
        );
        assert_eq!(def.mode, TriggerMode::DamageDone);
        let exec = def.execute.as_ref().unwrap();
        // The primary effect should be ChangeZone with enter_tapped + enters_attacking.
        match &*exec.effect {
            Effect::ChangeZone {
                destination: Zone::Battlefield,
                under_your_control: true,
                enter_tapped: true,
                enters_attacking: true,
                ..
            } => {} // expected
            other => panic!(
                "expected ChangeZone with enter_tapped + enters_attacking, got {:?}",
                other
            ),
        }
        // The sub_ability should NOT be Unimplemented.
        if let Some(sub) = &exec.sub_ability {
            assert!(
                !matches!(*sub.effect, Effect::Unimplemented { .. }),
                "sub_ability should not be Unimplemented, got {:?}",
                sub.effect,
            );
        }
    }

    #[test]
    fn enters_tapped_and_attacking_patches_token() {
        // CR 508.4: Stangg — "create ... token. It enters tapped and attacking."
        let def = parse_trigger_line(
            "Whenever Stangg attacks, create Stangg Twin, a legendary 3/4 red and green Human Warrior creature token. It enters tapped and attacking.",
            "Stangg, Echo Warrior",
        );
        assert_eq!(def.mode, TriggerMode::Attacks);
        let exec = def.execute.as_ref().unwrap();
        match &*exec.effect {
            Effect::Token {
                tapped: true,
                enters_attacking: true,
                ..
            } => {} // expected
            other => panic!(
                "expected Token with tapped + enters_attacking, got {:?}",
                other
            ),
        }
    }

    // -----------------------------------------------------------------------
    // ChangesZone "put into graveyard" sub-pattern tests (Phase 35-01)
    // -----------------------------------------------------------------------

    #[test]
    fn trigger_put_into_graveyard_from_battlefield() {
        // CR 700.4: "is put into a graveyard from the battlefield" == "dies"
        let def = parse_trigger_line(
            "Whenever a creature is put into a graveyard from the battlefield, draw a card.",
            "Some Card",
        );
        assert_eq!(def.mode, TriggerMode::ChangesZone);
        assert_eq!(def.origin, Some(Zone::Battlefield));
        assert_eq!(def.destination, Some(Zone::Graveyard));
        assert!(def.valid_card.is_some());
        assert!(def.execute.is_some());
    }

    #[test]
    fn trigger_creature_card_put_into_graveyard_from_anywhere() {
        // "from anywhere" means no origin restriction (typed subject)
        let def = parse_trigger_line(
            "Whenever a creature card is put into a graveyard from anywhere, draw a card.",
            "Some Card",
        );
        assert_eq!(def.mode, TriggerMode::ChangesZone);
        assert_eq!(def.origin, None);
        assert_eq!(def.destination, Some(Zone::Graveyard));
        assert!(def.valid_card.is_some());
    }

    #[test]
    fn trigger_put_into_opponents_graveyard() {
        let def = parse_trigger_line(
            "Whenever a card is put into an opponent's graveyard from anywhere, draw a card.",
            "Some Card",
        );
        assert_eq!(def.mode, TriggerMode::ChangesZone);
        assert_eq!(def.origin, None);
        assert_eq!(def.destination, Some(Zone::Graveyard));
        assert_eq!(
            def.valid_target,
            Some(TargetFilter::Typed(
                TypedFilter::default().controller(ControllerRef::Opponent)
            ))
        );
    }

    // -----------------------------------------------------------------------
    // Phase trigger variant tests (35-02)
    // -----------------------------------------------------------------------

    #[test]
    fn trigger_end_of_combat_your_turn() {
        // CR 511.2: "At end of combat on your turn" restricts to controller's turn.
        let def = parse_trigger_line(
            "At end of combat on your turn, exile target creature you control, then return it to the battlefield under your control.",
            "Thassa, Deep-Dwelling",
        );
        assert_eq!(def.mode, TriggerMode::Phase);
        assert_eq!(def.phase, Some(Phase::EndCombat));
        assert_eq!(def.constraint, Some(TriggerConstraint::OnlyDuringYourTurn));
    }

    #[test]
    fn trigger_the_end_of_combat_your_turn() {
        // CR 511.2: Alternate phrasing "at the end of combat on your turn".
        let def = parse_trigger_line(
            "At the end of combat on your turn, put a +1/+1 counter on each creature that attacked this turn.",
            "Test Card",
        );
        assert_eq!(def.mode, TriggerMode::Phase);
        assert_eq!(def.phase, Some(Phase::EndCombat));
        assert_eq!(def.constraint, Some(TriggerConstraint::OnlyDuringYourTurn));
    }

    #[test]
    fn trigger_end_of_combat_no_constraint() {
        // CR 511.2: Bare "at end of combat" with no turn qualifier has no constraint.
        let def = parse_trigger_line(
            "At end of combat, sacrifice this creature.",
            "Ball Lightning",
        );
        assert_eq!(def.mode, TriggerMode::Phase);
        assert_eq!(def.phase, Some(Phase::EndCombat));
        assert_eq!(def.constraint, None);
    }

    #[test]
    fn trigger_each_end_step() {
        // CR 513.1: "each end step" fires every turn with no controller constraint.
        let def = parse_trigger_line(
            "At the beginning of each end step, each player draws a card.",
            "Howling Mine",
        );
        assert_eq!(def.mode, TriggerMode::Phase);
        assert_eq!(def.phase, Some(Phase::End));
        assert_eq!(def.constraint, None);
    }

    #[test]
    fn trigger_the_end_step() {
        // CR 513.1: "the end step" with no possessive — fires each turn.
        let def = parse_trigger_line(
            "At the beginning of the end step, sacrifice this creature.",
            "Test Card",
        );
        assert_eq!(def.mode, TriggerMode::Phase);
        assert_eq!(def.phase, Some(Phase::End));
        assert_eq!(def.constraint, None);
    }

    #[test]
    fn trigger_each_upkeep() {
        // CR 503.1a: "each upkeep" fires every turn with no controller constraint.
        let def = parse_trigger_line(
            "At the beginning of each upkeep, each player loses 1 life.",
            "Sulfuric Vortex",
        );
        assert_eq!(def.mode, TriggerMode::Phase);
        assert_eq!(def.phase, Some(Phase::Upkeep));
        assert_eq!(def.constraint, None);
    }

    #[test]
    fn trigger_phase_with_if_condition() {
        // Intervening-if condition is extracted by extract_if_condition upstream.
        let def = parse_trigger_line(
            "At the beginning of your end step, if you gained life this turn, draw a card.",
            "Dawn of Hope",
        );
        assert_eq!(def.mode, TriggerMode::Phase);
        assert_eq!(def.phase, Some(Phase::End));
        assert_eq!(def.constraint, Some(TriggerConstraint::OnlyDuringYourTurn));
        assert_eq!(
            def.condition,
            Some(TriggerCondition::QuantityComparison {
                lhs: QuantityExpr::Ref {
                    qty: QuantityRef::LifeGainedThisTurn {
                        player: PlayerScope::Controller,
                    },
                },
                comparator: Comparator::GE,
                rhs: QuantityExpr::Fixed { value: 1 },
            })
        );
    }

    #[test]
    fn trigger_put_into_your_graveyard_from_library() {
        let def = parse_trigger_line(
            "Whenever a creature card is put into your graveyard from your library, draw a card.",
            "Some Card",
        );
        assert_eq!(def.mode, TriggerMode::ChangesZone);
        assert_eq!(def.origin, Some(Zone::Library));
        assert_eq!(def.destination, Some(Zone::Graveyard));
        // CR 109.5: "your graveyard" narrows valid_card.controller (the field
        // match_changes_zone actually consults) — not just valid_target.
        if let Some(TargetFilter::Typed(tf)) = &def.valid_card {
            assert_eq!(tf.controller, Some(ControllerRef::You));
        } else {
            panic!(
                "Expected Typed valid_card with controller=You, got {:?}",
                def.valid_card
            );
        }
        assert_eq!(
            def.valid_target,
            Some(TargetFilter::Typed(
                TypedFilter::default().controller(ControllerRef::You)
            ))
        );
    }

    /// Regression for issue #311: Undead Alchemist class. "Whenever a creature
    /// card is put into an opponent's graveyard from their library" must:
    ///   - set origin = Library (CR 603.6c: from-library zone constraint)
    ///   - set valid_card.controller = Opponent (CR 109.5: "their" anaphor
    ///     binds back to the previously named opponent; valid_card is the field
    ///     match_changes_zone consults).
    ///     Without these, the trigger fires when the controller's own creatures go
    ///     from battlefield to graveyard — the user-reported softlock chain.
    #[test]
    fn trigger_put_into_opponent_graveyard_from_their_library() {
        let def = parse_trigger_line(
            "Whenever a creature card is put into an opponent's graveyard from their library, exile that card.",
            "Undead Alchemist",
        );
        assert_eq!(def.mode, TriggerMode::ChangesZone);
        assert_eq!(def.origin, Some(Zone::Library));
        assert_eq!(def.destination, Some(Zone::Graveyard));
        if let Some(TargetFilter::Typed(tf)) = &def.valid_card {
            assert_eq!(
                tf.controller,
                Some(ControllerRef::Opponent),
                "valid_card.controller must be Opponent so the trigger does not \
                 fire on the source's own death"
            );
            assert!(
                tf.type_filters.contains(&TypeFilter::Creature),
                "Expected Creature type filter, got {:?}",
                tf.type_filters
            );
        } else {
            panic!(
                "Expected Typed valid_card with controller=Opponent, got {:?}",
                def.valid_card
            );
        }
        assert_eq!(
            def.valid_target,
            Some(TargetFilter::Typed(
                TypedFilter::default().controller(ControllerRef::Opponent)
            ))
        );
    }

    /// CR 109.5 + CR 700.4: Bloodchief Ascension class — "an opponent's
    /// graveyard from anywhere". `origin = None` (no zone restriction) but
    /// `valid_card.controller = Opponent` so the trigger only fires for cards
    /// owned by an opponent.
    #[test]
    fn trigger_put_into_opponent_graveyard_from_anywhere() {
        let def = parse_trigger_line(
            "Whenever a card is put into an opponent's graveyard from anywhere, you gain 1 life.",
            "Bloodchief Ascension",
        );
        assert_eq!(def.mode, TriggerMode::ChangesZone);
        assert_eq!(def.origin, None);
        assert_eq!(def.destination, Some(Zone::Graveyard));
        if let Some(TargetFilter::Typed(tf)) = &def.valid_card {
            assert_eq!(tf.controller, Some(ControllerRef::Opponent));
        } else {
            panic!(
                "Expected Typed valid_card with controller=Opponent, got {:?}",
                def.valid_card
            );
        }
    }

    /// CR 109.5: "a player's graveyard from any library" — owner-unscoped form.
    /// Both possessives leave `valid_card.controller` and `origin` constrained
    /// only on the side that matters: origin=Library (CR 603.6c) but no
    /// controller filter on the card (since "a player" / "any" cover all).
    #[test]
    fn trigger_put_into_any_player_graveyard_from_any_library() {
        let def = parse_trigger_line(
            "Whenever a creature card is put into a player's graveyard from any library, draw a card.",
            "Spelltracker",
        );
        assert_eq!(def.mode, TriggerMode::ChangesZone);
        assert_eq!(def.origin, Some(Zone::Library));
        assert_eq!(def.destination, Some(Zone::Graveyard));
        if let Some(TargetFilter::Typed(tf)) = &def.valid_card {
            assert_eq!(tf.controller, None, "Expected no controller scope");
            assert!(
                tf.type_filters.contains(&TypeFilter::Creature),
                "Expected Creature type filter, got {:?}",
                tf.type_filters
            );
        } else {
            panic!(
                "Expected Typed valid_card with controller=None, got {:?}",
                def.valid_card
            );
        }
    }

    #[test]
    fn trigger_one_or_more_creature_cards_put_into_graveyard_from_library() {
        // CR 603.2c: "One or more" triggers fire once per batch
        let def = parse_trigger_line(
            "Whenever one or more creature cards are put into your graveyard from your library, draw a card.",
            "Some Card",
        );
        assert_eq!(def.mode, TriggerMode::ChangesZoneAll);
        assert_eq!(def.origin, Some(Zone::Library));
        assert_eq!(def.destination, Some(Zone::Graveyard));
        assert!(def.batched);
        assert_eq!(
            def.valid_target,
            Some(TargetFilter::Typed(
                TypedFilter::default().controller(ControllerRef::You)
            ))
        );
        // CR 109.5: Subject filter must carry both the creature type AND the
        // owner-narrowed controller scope so the matcher fires only for the
        // controller's cards.
        if let Some(TargetFilter::Typed(tf)) = &def.valid_card {
            assert!(
                tf.type_filters.contains(&TypeFilter::Creature),
                "Expected Creature in type_filters, got {:?}",
                tf.type_filters
            );
            assert_eq!(
                tf.controller,
                Some(ControllerRef::You),
                "Expected controller=You merged from 'your graveyard', got {:?}",
                tf.controller
            );
        } else {
            panic!("Expected Typed creature filter, got {:?}", def.valid_card);
        }
    }

    #[test]
    fn trigger_nontoken_creature_put_into_graveyard() {
        let def = parse_trigger_line(
            "Whenever a nontoken creature is put into your graveyard, draw a card.",
            "Some Card",
        );
        assert_eq!(def.mode, TriggerMode::ChangesZone);
        assert_eq!(def.destination, Some(Zone::Graveyard));
        // CR 111.1: "nontoken" is token identity, not a fake subtype.
        if let Some(TargetFilter::Typed(tf)) = &def.valid_card {
            assert!(
                tf.properties.contains(&FilterProp::NonToken),
                "Expected NonToken in properties, got {:?}",
                tf.properties
            );
        } else {
            panic!(
                "Expected Typed filter with NonToken, got {:?}",
                def.valid_card
            );
        }
    }

    #[test]
    fn trigger_creature_with_power_4_or_greater_enters() {
        let def = parse_trigger_line(
            "Whenever a creature with power 4 or greater enters the battlefield under your control, draw a card.",
            "Some Card",
        );
        assert_eq!(def.mode, TriggerMode::ChangesZone);
        assert_eq!(def.destination, Some(Zone::Battlefield));
        // Should have PowerGE { value: 4 } in the filter props
        if let Some(TargetFilter::Typed(tf)) = &def.valid_card {
            assert!(
                tf.properties.iter().any(|p| matches!(
                    p,
                    FilterProp::PowerGE {
                        value: QuantityExpr::Fixed { value: 4 }
                    }
                )),
                "Expected PowerGE(4) in properties, got {:?}",
                tf.properties
            );
        } else {
            panic!(
                "Expected Typed filter with PowerGE, got {:?}",
                def.valid_card
            );
        }
    }

    #[test]
    fn trigger_face_down_creature_dies() {
        let def = parse_trigger_line(
            "Whenever a face-down creature you control dies, draw a card.",
            "Some Card",
        );
        assert_eq!(def.mode, TriggerMode::ChangesZone);
        assert_eq!(def.origin, Some(Zone::Battlefield));
        assert_eq!(def.destination, Some(Zone::Graveyard));
        // Should have FaceDown in the filter props
        if let Some(TargetFilter::Typed(tf)) = &def.valid_card {
            assert!(
                tf.properties
                    .iter()
                    .any(|p| matches!(p, FilterProp::FaceDown)),
                "Expected FaceDown in properties, got {:?}",
                tf.properties
            );
            assert_eq!(tf.controller, Some(ControllerRef::You));
        } else {
            panic!(
                "Expected Typed filter with FaceDown, got {:?}",
                def.valid_card
            );
        }
    }

    #[test]
    fn trigger_put_into_your_graveyard_no_origin() {
        // "is put into your graveyard" without "from" clause
        let def = parse_trigger_line(
            "Whenever a creature is put into your graveyard, draw a card.",
            "Some Card",
        );
        assert_eq!(def.mode, TriggerMode::ChangesZone);
        assert_eq!(def.origin, None);
        assert_eq!(def.destination, Some(Zone::Graveyard));
        assert_eq!(
            def.valid_target,
            Some(TargetFilter::Typed(
                TypedFilter::default().controller(ControllerRef::You)
            ))
        );
    }

    #[test]
    fn trigger_one_or_more_cards_put_into_graveyard_from_anywhere() {
        let def = parse_trigger_line(
            "Whenever one or more cards are put into your graveyard from anywhere, draw a card.",
            "Some Card",
        );
        assert_eq!(def.mode, TriggerMode::ChangesZoneAll);
        assert_eq!(def.origin, None);
        assert_eq!(def.destination, Some(Zone::Graveyard));
        assert!(def.batched);
        // "your graveyard" scopes the affected cards to the controller even
        // when "cards" carries no additional type restriction.
        assert_eq!(
            def.valid_card,
            Some(TargetFilter::Typed(
                TypedFilter::default().controller(ControllerRef::You)
            ))
        );
    }

    #[test]
    fn trigger_precombat_main_phase() {
        // CR 505.1: "precombat main phase" maps to PreCombatMain.
        let def = parse_trigger_line(
            "At the beginning of your precombat main phase, add one mana of any color.",
            "Test Card",
        );
        assert_eq!(def.mode, TriggerMode::Phase);
        assert_eq!(def.phase, Some(Phase::PreCombatMain));
        assert_eq!(def.constraint, Some(TriggerConstraint::OnlyDuringYourTurn));
    }

    #[test]
    fn trigger_postcombat_main_phase() {
        // CR 505.1: "postcombat main phase" maps to PostCombatMain.
        let def = parse_trigger_line(
            "At the beginning of each player's postcombat main phase, that player may cast a spell.",
            "Test Card",
        );
        assert_eq!(def.mode, TriggerMode::Phase);
        assert_eq!(def.phase, Some(Phase::PostCombatMain));
        // "each player's" has no "your" or "opponent" → no constraint
        assert_eq!(def.constraint, None);
    }

    #[test]
    fn trigger_first_main_phase() {
        // CR 505.1: "first main phase" is an alias for precombat main phase.
        let def = parse_trigger_line(
            "At the beginning of your first main phase, add one mana of any color.",
            "Test Card",
        );
        assert_eq!(def.mode, TriggerMode::Phase);
        assert_eq!(def.phase, Some(Phase::PreCombatMain));
        assert_eq!(def.constraint, Some(TriggerConstraint::OnlyDuringYourTurn));
    }

    /// Coalition Relic, third ability — Future Sight artifact, issue #130.
    ///
    /// Oracle: "At the beginning of your precombat main phase, you may remove
    /// all charge counters from ~. If you do, add one mana of any color for
    /// each charge counter removed this way."
    ///
    /// This trigger composes four primitives that must all wire together:
    ///
    /// 1. CR 505.1: "precombat main phase" → `Phase::PreCombatMain`.
    /// 2. CR 603.5 + CR 118.12: "you may" → `optional: true` on both the
    ///    trigger def AND its execute ability, routing through
    ///    `WaitingFor::OptionalEffectChoice` at resolution; "If you do" checks
    ///    whether the player chose to pay the optional cost.
    /// 3. CR 122.1: "remove all charge counters from ~" →
    ///    `Effect::RemoveCounter { counter_type: "charge", count: -1, target:
    ///    SelfRef }` (count=-1 is the "remove all" sentinel).
    /// 4. CR 609.3 + CR 106.1 + CR 122.1: "If you do, add one mana of any
    ///    color for each charge counter removed this way" →
    ///    sub_ability with `condition: Some(IfYouDo)` and effect
    ///    `Effect::Mana { produced: AnyOneColor { count:
    ///    QuantityExpr::Ref { qty: PreviousEffectAmount }, color_options: <all
    ///    five>, .. }, .. }`. The runtime parent-effect-aware scan in
    ///    `effects/mod.rs` reads `GameEvent::CounterRemoved` for RemoveCounter
    ///    parents to populate `last_effect_amount`, which
    ///    `PreviousEffectAmount` reads.
    #[test]
    fn trigger_coalition_relic_charge_counter_drain() {
        use crate::types::ability::AbilityCondition;
        use crate::types::ability::ManaProduction;

        let def = parse_trigger_line(
            "At the beginning of your precombat main phase, you may remove all charge counters from ~. If you do, add one mana of any color for each charge counter removed this way.",
            "Coalition Relic",
        );

        // (1) Phase shape.
        assert_eq!(def.mode, TriggerMode::Phase);
        assert_eq!(def.phase, Some(Phase::PreCombatMain));
        assert_eq!(def.constraint, Some(TriggerConstraint::OnlyDuringYourTurn));

        // (2) Optional flag on the trigger def itself.
        assert!(def.optional, "trigger must carry optional: true");

        let execute = def
            .execute
            .as_ref()
            .expect("trigger must have an execute body");

        // (2 cont.) Optional propagated to the execute root so the resolver
        // routes through WaitingFor::OptionalEffectChoice.
        assert!(
            execute.optional,
            "execute root must carry optional: true so OptionalEffectChoice fires"
        );

        // (3) Root effect: remove all charge counters from self.
        match &*execute.effect {
            Effect::RemoveCounter {
                counter_type,
                count,
                target,
            } => {
                assert_eq!(
                    counter_type,
                    &Some(crate::types::counter::CounterType::Generic(
                        "charge".to_string()
                    ))
                );
                assert_eq!(*count, -1, "count=-1 is the remove-all sentinel");
                assert!(matches!(target, TargetFilter::SelfRef));
            }
            other => panic!("expected Effect::RemoveCounter, got {other:?}"),
        }

        // (4) Sub-ability: gated by IfYouDo, produces dynamic-count any-color
        // mana from the count of counters removed by the parent.
        let sub = execute
            .sub_ability
            .as_ref()
            .expect("execute must have an If-you-do sub-ability");

        assert_eq!(
            sub.condition,
            Some(AbilityCondition::IfYouDo),
            "sub-ability must be gated by IfYouDo so it only fires when the player accepted"
        );
        assert!(
            !sub.optional,
            "sub-ability is not its own optional choice — only the root prompts the player"
        );

        match &*sub.effect {
            Effect::Mana {
                produced,
                target: mana_target,
                ..
            } => {
                assert!(
                    mana_target.is_none(),
                    "no player target on this mana production"
                );
                match produced {
                    ManaProduction::AnyOneColor {
                        count,
                        color_options,
                        ..
                    } => {
                        assert_eq!(
                            *count,
                            QuantityExpr::Ref {
                                qty: QuantityRef::PreviousEffectAmount
                            },
                            "for-each tail must dispatch to PreviousEffectAmount"
                        );
                        assert_eq!(
                            color_options.len(),
                            5,
                            "any-color must offer all five colors"
                        );
                    }
                    other => panic!("expected AnyOneColor, got {other:?}"),
                }
            }
            other => panic!("expected Effect::Mana on sub-ability, got {other:?}"),
        }
    }

    #[test]
    fn trigger_second_main_phase() {
        // CR 505.1: "second main phase" is an alias for postcombat main phase.
        let def = parse_trigger_line(
            "At the beginning of each player's second main phase, that player draws a card.",
            "Test Card",
        );
        assert_eq!(def.mode, TriggerMode::Phase);
        assert_eq!(def.phase, Some(Phase::PostCombatMain));
        assert_eq!(def.constraint, None);
    }

    // --- Plan 03: Attacks trigger sub-patterns ---

    #[test]
    fn trigger_enchanted_player_attacked() {
        // CR 508.1a: "enchanted player is attacked" — AttachedTo as defending player.
        let def = parse_trigger_line(
            "Whenever enchanted player is attacked, create a 1/1 white Soldier creature token.",
            "Curse of the Forsaken",
        );
        assert_eq!(def.mode, TriggerMode::Attacks);
        assert_eq!(def.valid_target, Some(TargetFilter::AttachedTo));
        assert!(def.execute.is_some());
    }

    #[test]
    fn trigger_two_or_more_creatures_attack() {
        // CR 508.1a: "two or more" uses MinCoAttackers with minimum=1 (2-1).
        let def = parse_trigger_line(
            "Whenever two or more creatures you control attack a player, draw a card.",
            "Edric, Spymaster of Trest",
        );
        assert_eq!(def.mode, TriggerMode::YouAttack);
        assert_eq!(
            def.condition,
            Some(TriggerCondition::MinCoAttackers { minimum: 1 })
        );
        assert_eq!(def.valid_target, Some(TargetFilter::Player));
        assert!(def.execute.is_some());
    }

    // --- Plan 03: SpellCast trigger sub-patterns ---

    #[test]
    fn trigger_first_spell_opponents_turn() {
        // CR 601.2: "first spell during each opponent's turn"
        let def = parse_trigger_line(
            "Whenever you cast your first spell during each opponent's turn, draw a card.",
            "Faerie Mastermind",
        );
        assert_eq!(def.mode, TriggerMode::SpellCast);
        assert_eq!(
            def.constraint,
            Some(TriggerConstraint::NthSpellThisTurn { n: 1, filter: None })
        );
        assert_eq!(
            def.condition,
            Some(TriggerCondition::DuringPlayersTurn {
                player: PlayerFilter::Opponent,
            })
        );
    }

    /// CR 107.3 + CR 202.1: "whenever you cast your first spell with {X} in its
    /// mana cost each turn" — the "with {X}" qualifier lives AFTER "spell"
    /// (post-spell modifier), not before. Verifies `HasXInManaCost` filter
    /// emission on the per-turn SpellCast trigger. Target cards: Lattice
    /// Library, Nev the Practical Dean, Owlin Spiralmancer, Zimone Infinite
    /// Analyst.
    #[test]
    fn trigger_first_spell_with_x_in_cost() {
        use crate::types::ability::{FilterProp, TypedFilter};
        let def = parse_trigger_line(
            "Whenever you cast your first spell with {X} in its mana cost each turn, draw a card.",
            "Nev, the Practical Dean",
        );
        assert_eq!(def.mode, TriggerMode::SpellCast);
        let expected_filter = TargetFilter::Typed(
            TypedFilter::default().properties(vec![FilterProp::HasXInManaCost]),
        );
        assert_eq!(
            def.constraint,
            Some(TriggerConstraint::NthSpellThisTurn {
                n: 1,
                filter: Some(expected_filter),
            }),
            "first-spell-with-X trigger must carry HasXInManaCost filter"
        );
        assert!(def.execute.is_some());
    }

    /// CR 107.3 + CR 202.1: Combined type phrase + X-in-cost qualifier.
    /// "your first creature spell with {X} in its mana cost each turn" should
    /// produce an And-composed filter of (Creature) AND (HasXInManaCost).
    #[test]
    fn trigger_first_creature_spell_with_x_in_cost() {
        use crate::types::ability::{FilterProp, TypeFilter, TypedFilter};
        let def = parse_trigger_line(
            "Whenever you cast your first creature spell with {X} in its mana cost each turn, draw a card.",
            "Hypothetical",
        );
        assert_eq!(def.mode, TriggerMode::SpellCast);
        let TriggerConstraint::NthSpellThisTurn { n, ref filter } = def.constraint.unwrap() else {
            panic!("expected NthSpellThisTurn");
        };
        assert_eq!(n, 1);
        let filter = filter.as_ref().expect("filter must be set");
        // Shape: And { filters: [Creature typed, HasXInManaCost typed] }
        match filter {
            TargetFilter::And { filters } => {
                assert_eq!(filters.len(), 2, "expected 2-part AND filter");
                assert!(
                    filters
                        .iter()
                        .any(|f| matches!(f, TargetFilter::Typed(tf) if tf.type_filters.contains(&TypeFilter::Creature))),
                    "must include Creature type filter: {filters:?}"
                );
                assert!(
                    filters.iter().any(|f| matches!(
                        f,
                        TargetFilter::Typed(TypedFilter { properties, .. })
                            if properties.contains(&FilterProp::HasXInManaCost)
                    )),
                    "must include HasXInManaCost filter: {filters:?}"
                );
            }
            other => panic!("expected AND filter, got {other:?}"),
        }
    }

    /// Ensure the existing "first spell each turn" behavior (no qualifier) is
    /// preserved by the refactor — filter remains `None`.
    #[test]
    fn trigger_first_spell_no_qualifier_remains_none() {
        let def = parse_trigger_line(
            "Whenever you cast your first spell each turn, draw a card.",
            "Archmage Emeritus",
        );
        assert_eq!(def.mode, TriggerMode::SpellCast);
        assert_eq!(
            def.constraint,
            Some(TriggerConstraint::NthSpellThisTurn { n: 1, filter: None })
        );
    }

    #[test]
    fn trigger_copy_spell() {
        // CR 707.10: "you copy a spell" maps to SpellCopy.
        let def = parse_trigger_line(
            "Whenever you copy a spell, put a +1/+1 counter on ~.",
            "Ivy, Gleeful Spellthief",
        );
        assert_eq!(def.mode, TriggerMode::SpellCopy);
        assert_eq!(def.valid_target, Some(TargetFilter::Controller));
        assert!(def.execute.is_some());
    }

    // --- Plan 03: DamageDone trigger sub-patterns ---

    #[test]
    fn trigger_dealt_damage_by_source_dies() {
        // CR 700.4 + CR 120.1: "a creature dealt damage by ~ this turn dies"
        let def = parse_trigger_line(
            "Whenever a creature dealt damage by Syr Konrad, the Grim this turn dies, each opponent loses 1 life.",
            "Syr Konrad, the Grim",
        );
        assert_eq!(def.mode, TriggerMode::ChangesZone);
        assert_eq!(def.origin, Some(Zone::Battlefield));
        assert_eq!(def.destination, Some(Zone::Graveyard));
        assert_eq!(
            def.condition,
            Some(TriggerCondition::DealtDamageBySourceThisTurn)
        );
    }

    #[test]
    fn trigger_you_dealt_damage() {
        // CR 120.1: "whenever you're dealt damage" — player damage received.
        let def = parse_trigger_line(
            "Whenever you're dealt damage, put that many charge counters on ~.",
            "Stuffy Doll",
        );
        assert_eq!(def.mode, TriggerMode::DamageReceived);
        assert_eq!(def.damage_kind, DamageKindFilter::Any);
        assert_eq!(def.valid_target, Some(TargetFilter::Controller));
    }

    #[test]
    fn trigger_you_dealt_combat_damage() {
        // CR 120.1a: "whenever you're dealt combat damage" — combat-only variant.
        let def = parse_trigger_line(
            "Whenever you're dealt combat damage, draw a card.",
            "Test Card",
        );
        assert_eq!(def.mode, TriggerMode::DamageReceived);
        assert_eq!(def.damage_kind, DamageKindFilter::CombatOnly);
        assert_eq!(def.valid_target, Some(TargetFilter::Controller));
    }

    #[test]
    fn trigger_opponent_dealt_noncombat_damage() {
        // CR 120.2b: "whenever an opponent is dealt noncombat damage"
        let def = parse_trigger_line(
            "Whenever an opponent is dealt noncombat damage, you gain 1 life.",
            "Test Card",
        );
        assert_eq!(def.mode, TriggerMode::DamageReceived);
        assert_eq!(def.damage_kind, DamageKindFilter::NoncombatOnly);
    }

    // --- Plan 03: CounterRemoved trigger sub-patterns ---

    #[test]
    fn trigger_time_counter_removed_exile() {
        // CR 122.1: "a time counter is removed from ~ while it's exiled"
        let def = parse_trigger_line(
            "Whenever a time counter is removed from ~ while it's exiled, you may cast a copy of ~ without paying its mana cost.",
            "Rift Bolt",
        );
        assert_eq!(def.mode, TriggerMode::CounterRemoved);
        assert_eq!(def.valid_card, Some(TargetFilter::SelfRef));
        assert_eq!(def.trigger_zones, vec![Zone::Exile]);
    }

    #[test]
    fn trigger_counter_removed_no_zone_constraint() {
        // CR 122.1: "a time counter is removed from ~" without zone constraint.
        let def = parse_trigger_line(
            "Whenever a time counter is removed from ~, deal 1 damage to any target.",
            "Test Suspend Card",
        );
        assert_eq!(def.mode, TriggerMode::CounterRemoved);
        assert_eq!(def.valid_card, Some(TargetFilter::SelfRef));
        // No zone constraint — fires from default zones
        assert_eq!(def.trigger_zones, vec![Zone::Battlefield]);
    }

    // -----------------------------------------------------------------------
    // CR 608.2k: Trigger pronoun resolution — "it"/"its" context-dependent
    // -----------------------------------------------------------------------

    #[test]
    fn trigger_it_resolves_to_triggering_source_for_non_self_subject() {
        // "it" refers to the entering creature, not the enchantment
        let def = parse_trigger_line(
            "Whenever a creature you control enters, put a +1/+1 counter on it",
            "Test Enchantment",
        );
        let exec = def.execute.as_ref().expect("should have execute");
        match &*exec.effect {
            Effect::PutCounter { target, .. } => {
                assert_eq!(
                    *target,
                    TargetFilter::TriggeringSource,
                    "non-self trigger 'it' should resolve to TriggeringSource"
                );
            }
            other => panic!("Expected PutCounter, got {:?}", other),
        }
    }

    #[test]
    fn trigger_it_stays_self_ref_for_self_subject() {
        // "it" refers to ~ (the card itself entering)
        let def = parse_trigger_line(
            "When Test Card enters, put a +1/+1 counter on it",
            "Test Card",
        );
        let exec = def.execute.as_ref().expect("should have execute");
        match &*exec.effect {
            Effect::PutCounter { target, .. } => {
                assert_eq!(
                    *target,
                    TargetFilter::SelfRef,
                    "self-trigger 'it' should stay SelfRef"
                );
            }
            other => panic!("Expected PutCounter, got {:?}", other),
        }
    }

    #[test]
    fn trigger_tilde_stays_self_ref_with_non_self_subject() {
        // "~" always refers to the source permanent, even in non-self trigger
        let def = parse_trigger_line(
            "Whenever a creature you control enters, sacrifice ~",
            "Test Enchantment",
        );
        let exec = def.execute.as_ref().expect("should have execute");
        match &*exec.effect {
            Effect::Sacrifice { target, .. } => {
                assert_eq!(*target, TargetFilter::SelfRef, "~ should always be SelfRef");
            }
            other => panic!("Expected Sacrifice, got {:?}", other),
        }
    }

    #[test]
    fn trigger_otherwise_branch_preserves_context() {
        // Tribute to the World Tree pattern: else_ability "it" = triggering creature
        let def = parse_trigger_line(
            "Whenever a creature you control enters, draw a card if its power is 3 or greater. Otherwise, put two +1/+1 counters on it.",
            "Tribute to the World Tree",
        );
        let exec = def.execute.as_ref().expect("should have execute");
        let else_ab = exec
            .else_ability
            .as_ref()
            .expect("should have else_ability");
        match &*else_ab.effect {
            Effect::PutCounter { target, count, .. } => {
                assert_eq!(
                    *target,
                    TargetFilter::TriggeringSource,
                    "else_ability 'it' should be TriggeringSource"
                );
                assert_eq!(*count, QuantityExpr::Fixed { value: 2 });
            }
            other => panic!("Expected PutCounter in else_ability, got {:?}", other),
        }
    }

    #[test]
    fn trigger_copy_token_suffix_condition_attaches_otherwise() {
        let (effect_without_if, trigger_condition) = extract_if_condition(
            "create a tapped token that's a copy of ~ if seven or more land cards are in your graveyard. otherwise, create a tapped 1/1 black insect creature token with flying.",
        );
        assert!(
            trigger_condition.is_none(),
            "suffix condition before otherwise must stay in effect chain, got {trigger_condition:?}"
        );
        assert!(
            crate::parser::oracle_nom::primitives::scan_contains(
                &effect_without_if,
                "if seven or more land cards are in your graveyard"
            ),
            "effect text should preserve suffix condition, got {effect_without_if:?}"
        );
        let mut effect_ctx = ParseContext {
            subject: Some(TargetFilter::SelfRef),
            card_name: Some("Scouring Swarm".to_string()),
            ..Default::default()
        };
        let chain_ir =
            parse_effect_chain_ir(&effect_without_if, AbilityKind::Spell, &mut effect_ctx);
        let chain = lower_effect_chain_ir(&chain_ir);
        assert!(
            chain.condition.is_some(),
            "effect chain should carry suffix condition, got {chain:?}"
        );

        let def = parse_trigger_line(
            "Whenever you sacrifice a land, create a tapped token that's a copy of this creature if seven or more land cards are in your graveyard. Otherwise, create a tapped 1/1 black Insect creature token with flying.",
            "Scouring Swarm",
        );
        let exec = def.execute.as_ref().expect("should have execute");
        assert!(
            matches!(
                &*exec.effect,
                Effect::CopyTokenOf {
                    tapped: true,
                    enters_attacking: false,
                    ..
                }
            ),
            "expected tapped CopyTokenOf, got {:?}",
            exec.effect
        );
        assert!(
            exec.condition.is_some(),
            "copy branch should carry suffix condition"
        );
        let else_ab = exec
            .else_ability
            .as_ref()
            .expect("should have else_ability");
        assert!(
            matches!(&*else_ab.effect, Effect::Token { tapped: true, .. }),
            "expected tapped token in else_ability, got {:?}",
            else_ab.effect
        );
    }

    /// Issue #466 — CR 608.2c: a special (rider) clause occupies a chunk slot
    /// and carries its own trailing boundary. A normal clause that follows a
    /// special clause must stamp its `sub_link` from the boundary AFTER the
    /// special clause, not from the stale boundary that preceded it.
    ///
    /// Direct-IR construction tests the boundary-advance building block in
    /// isolation: clause 0 normal (`boundary: Comma`), clause 1
    /// `SpecialClause::Otherwise` (`boundary: Sentence`), clause 2 normal.
    /// With the bug, clause 2 inherits clause 0's `Comma` → `ContinuationStep`;
    /// with the fix it inherits clause 1's `Sentence` → `SequentialSibling`.
    #[test]
    fn lower_effect_chain_ir_advances_boundary_past_special_clause() {
        use crate::parser::oracle_ir::ast::{parsed_clause, ClauseBoundary};
        use crate::parser::oracle_ir::effect_chain::{ClauseIr, EffectChainIr, SpecialClause};
        use crate::types::ability::SubAbilityLink;

        fn make_clause(
            effect: Effect,
            boundary: Option<ClauseBoundary>,
            special: Option<SpecialClause>,
        ) -> ClauseIr {
            ClauseIr {
                parsed: parsed_clause(effect),
                boundary,
                condition: None,
                is_optional: false,
                opponent_may_scope: None,
                repeat_for: None,
                player_scope: None,
                starting_with: None,
                delayed_condition: None,
                prefix_delayed_condition: None,
                intrinsic_continuation: None,
                followup_continuation: None,
                absorbed_by_followup: false,
                multi_target: None,
                where_x_expression: None,
                is_otherwise: false,
                unless_pay: None,
                special,
                source_text: String::new(),
                target_selection_mode: Default::default(),
            }
        }

        let draw_one = || Effect::Draw {
            count: QuantityExpr::Fixed { value: 1 },
            target: TargetFilter::Controller,
        };
        // `SpecialClause::Otherwise` is a silent no-op when no prior conditional
        // def exists, so clause 0 needs no condition.
        let otherwise_def = Box::new(crate::types::ability::AbilityDefinition::new(
            AbilityKind::Spell,
            draw_one(),
        ));

        let ir = EffectChainIr {
            clauses: vec![
                // clause 0: normal, trailing boundary = Comma
                make_clause(draw_one(), Some(ClauseBoundary::Comma), None),
                // clause 1: SpecialClause::Otherwise, trailing boundary = Sentence
                make_clause(
                    draw_one(),
                    Some(ClauseBoundary::Sentence),
                    Some(SpecialClause::Otherwise(otherwise_def)),
                ),
                // clause 2: normal — must stamp sub_link from clause 1's Sentence
                make_clause(draw_one(), None, None),
            ],
            kind: AbilityKind::Spell,
            chain_rounding: None,
            actor: None,
            repeat_until: None,
        };

        let root = lower_effect_chain_ir(&ir);
        // clause 0 → root def; clause 1 special (no-op); clause 2 → trailing sibling.
        let trailing = root
            .sub_ability
            .as_ref()
            .expect("clause 2 should lower to a trailing sub_ability def");
        assert_eq!(
            trailing.sub_link,
            SubAbilityLink::SequentialSibling,
            "clause after a special clause must inherit the boundary AFTER the \
             special clause (Sentence → SequentialSibling)"
        );
        assert_ne!(
            trailing.sub_link,
            SubAbilityLink::ContinuationStep,
            "regression guard: must NOT inherit clause 0's stale Comma boundary"
        );
    }

    #[test]
    fn trigger_subject_predicate_it_gains() {
        // "it gains haste" — subject-predicate with "it" as subject.
        // The subject "it" resolves to TriggeringSource and lands in the
        // static_abilities[0].affected field (not the top-level `target`).
        let def = parse_trigger_line(
            "Whenever a creature you control enters, it gains haste until end of turn",
            "Test Enchantment",
        );
        let exec = def.execute.as_ref().expect("should have execute");
        match &*exec.effect {
            Effect::GenericEffect {
                static_abilities, ..
            } => {
                assert_eq!(
                    static_abilities[0].affected,
                    Some(TargetFilter::TriggeringSource),
                    "subject-predicate 'it' should produce TriggeringSource in affected"
                );
            }
            other => panic!("Expected GenericEffect, got {:?}", other),
        }
    }

    #[test]
    fn trigger_equipped_creature_it_resolves_to_triggering_source() {
        // "it" = equipped creature (AttachedTo subject → TriggeringSource)
        let def = parse_trigger_line(
            "Whenever equipped creature attacks, put a +1/+1 counter on it",
            "Test Equipment",
        );
        let exec = def.execute.as_ref().expect("should have execute");
        match &*exec.effect {
            Effect::PutCounter { target, .. } => {
                assert_eq!(
                    *target,
                    TargetFilter::TriggeringSource,
                    "equipped creature 'it' should be TriggeringSource"
                );
            }
            other => panic!("Expected PutCounter, got {:?}", other),
        }
    }

    // --- CR 115.9b: "that targets" trigger integration tests ---

    #[test]
    fn trigger_heroic_that_targets_self() {
        let def = parse_trigger_line(
            "Heroic — Whenever you cast a spell that targets this creature, put a +1/+1 counter on each creature you control.",
            "Phalanx Leader",
        );
        assert_eq!(def.mode, TriggerMode::SpellCast);
        assert_eq!(def.valid_target, Some(TargetFilter::Controller));
        // valid_card should have Targets { SelfRef } property
        let valid_card = def.valid_card.expect("should have valid_card");
        if let TargetFilter::Typed(tf) = &valid_card {
            assert!(
                tf.properties
                    .iter()
                    .any(|p| matches!(p, FilterProp::Targets { filter } if **filter == TargetFilter::SelfRef)),
                "expected Targets {{ SelfRef }} in properties: {:?}",
                tf.properties
            );
        } else {
            panic!("expected Typed filter, got {valid_card:?}");
        }
    }

    #[test]
    fn trigger_floodpits_etb_keeps_stun_counter_on_parent_target() {
        let def = parse_trigger_line(
            "When this creature enters, tap target creature an opponent controls and put a stun counter on it.",
            "Floodpits Drowner",
        );
        let exec = def.execute.as_ref().expect("should have execute ability");
        let sub = exec
            .sub_ability
            .as_ref()
            .expect("tap effect should chain into stun counter effect");
        match &*sub.effect {
            Effect::PutCounter { target, .. } => {
                assert!(
                    matches!(target, TargetFilter::ParentTarget),
                    "expected ParentTarget, got {target:?}"
                );
            }
            other => panic!("expected PutCounter sub-ability, got {other:?}"),
        }
    }

    /// CR 603.4 + CR 608.2h: A post-effect re-homeable `if` ("draw a card if
    /// you have 40 or more life") is NOT hoisted to the trigger-level condition
    /// by `extract_if_condition` — it is left intact for `strip_suffix_conditional`
    /// to re-home onto a clause-level `AbilityCondition` (`execute.condition`).
    #[test]
    fn extract_if_you_have_n_or_more_life() {
        let (cleaned, cond) = extract_if_condition("draw a card if you have 40 or more life");
        assert_eq!(
            cond, None,
            "a re-homeable post-effect `if` must not hoist to the trigger condition",
        );
        assert_eq!(
            cleaned, "draw a card if you have 40 or more life",
            "the post-effect `if` must be left in the effect text for re-homing",
        );
    }

    /// CR 603.4 + CR 608.2h: "you win the game if you have 40 or more life" —
    /// the `if` is post-effect (after the effect verb) and re-homeable, so
    /// `extract_if_condition` leaves it intact rather than hoisting it.
    #[test]
    fn extract_if_you_have_n_or_more_life_win() {
        let (cleaned, cond) = extract_if_condition("you win the game if you have 40 or more life");
        assert_eq!(
            cond, None,
            "a re-homeable post-effect `if` must not hoist to the trigger condition",
        );
        assert_eq!(
            cleaned, "you win the game if you have 40 or more life",
            "the post-effect `if` must be left in the effect text for re-homing",
        );
    }

    /// CR 603.4 + CR 608.2h: A post-effect re-homeable `if` is left intact by
    /// `extract_if_condition` — `strip_suffix_conditional` re-homes it later.
    #[test]
    fn extract_if_gained_life_regression() {
        let (cleaned, cond) = extract_if_condition("draw a card if you've gained life this turn");
        assert_eq!(
            cond, None,
            "a re-homeable post-effect `if` must not hoist to the trigger condition",
        );
        assert_eq!(
            cleaned, "draw a card if you've gained life this turn",
            "the post-effect `if` must be left in the effect text for re-homing",
        );
    }

    // --- Fix 1: find_effect_boundary comma splitter respects type-phrase lists ---

    #[test]
    fn split_trigger_compound_type_subject() {
        // "a creature, planeswalker, or battle enters" — comma is part of the subject
        let tp = TextPair::new(
            "whenever a creature, planeswalker, or battle enters the battlefield, draw a card",
            "whenever a creature, planeswalker, or battle enters the battlefield, draw a card",
        );
        let (condition, effect) = split_trigger(tp);
        assert!(
            condition.contains("enters"),
            "Condition should contain 'enters', got: '{condition}'"
        );
        assert_eq!(effect, "draw a card");
    }

    #[test]
    fn split_trigger_two_type_subject() {
        // "a creature or enchantment" — no comma in subject but "artifact, creature, or enchantment" has
        let tp = TextPair::new(
            "whenever an artifact, creature, or enchantment enters the battlefield, you gain 1 life",
            "whenever an artifact, creature, or enchantment enters the battlefield, you gain 1 life",
        );
        let (condition, effect) = split_trigger(tp);
        assert!(
            condition.contains("enchantment"),
            "Condition should contain full type list, got: '{condition}'"
        );
        assert_eq!(effect, "you gain 1 life");
    }

    #[test]
    fn continues_player_action_list_type_word() {
        // Bare type word after comma: "planeswalker, or battle enters"
        assert!(continues_player_action_list(
            "planeswalker, or battle enters"
        ));
        assert!(continues_player_action_list("or battle enters"));
        assert!(continues_player_action_list(
            "creature, or enchantment enters"
        ));
        // Non-type word should not match
        assert!(!continues_player_action_list("draw a card"));
        assert!(!continues_player_action_list("you gain 1 life"));
    }

    // --- Fix 2: missing event verbs ---

    #[test]
    fn trigger_is_exiled() {
        let def = parse_trigger_line(
            "Whenever a creature you control is exiled, draw a card.",
            "Test Card",
        );
        assert_eq!(def.mode, TriggerMode::Exiled);
        assert!(def.valid_card.is_some());
    }

    #[test]
    fn trigger_is_sacrificed() {
        let def = parse_trigger_line(
            "Whenever a creature is sacrificed, you gain 1 life.",
            "Test Card",
        );
        assert_eq!(def.mode, TriggerMode::Sacrificed);
        assert!(def.valid_card.is_some());
    }

    #[test]
    fn trigger_is_destroyed() {
        let def = parse_trigger_line(
            "Whenever a permanent you control is destroyed, draw a card.",
            "Test Card",
        );
        assert_eq!(def.mode, TriggerMode::Destroyed);
        assert!(def.valid_card.is_some());
    }

    // --- Milled triggers (CR 701.17a) ---

    #[test]
    fn trigger_passive_milled_batched() {
        // The Wise Mothman / Mirelurk Queen / Screeching Scorchbeast shape.
        let def = parse_trigger_line(
            "Whenever one or more nonland cards are milled, put a +1/+1 counter on it.",
            "Test Card",
        );
        assert_eq!(def.mode, TriggerMode::Milled);
        assert!(def.batched, "\"one or more\" subject must stamp batched");
        match def.valid_card {
            Some(TargetFilter::Typed(ref f)) => {
                assert!(
                    f.type_filters.iter().any(|t| matches!(t, TypeFilter::Card)),
                    "valid_card should be a card filter, got {:?}",
                    f.type_filters
                );
                assert!(
                    f.type_filters.iter().any(|t| matches!(
                        t,
                        TypeFilter::Non(inner) if matches!(**inner, TypeFilter::Land)
                    )),
                    "valid_card should carry Non(Land), got {:?}",
                    f.type_filters
                );
            }
            other => panic!("expected Typed nonland-card filter, got {other:?}"),
        }
    }

    #[test]
    fn trigger_passive_milled_single_card() {
        // Single-card passive form — no "one or more", so not batched.
        let def = parse_trigger_line("Whenever a card is milled, you gain 1 life.", "Test Card");
        assert_eq!(def.mode, TriggerMode::Milled);
        assert!(!def.batched);
        assert!(def.valid_card.is_some());
    }

    #[test]
    fn trigger_active_milled_a_player() {
        // Glowing One shape: "Whenever a player mills a nonland card, …"
        let def = parse_trigger_line(
            "Whenever a player mills a nonland card, you gain 1 life.",
            "Test Card",
        );
        assert_eq!(def.mode, TriggerMode::Milled);
        match def.valid_card {
            Some(TargetFilter::Typed(ref f)) => {
                assert!(
                    f.type_filters.iter().any(|t| matches!(t, TypeFilter::Card)),
                    "valid_card should be a card filter"
                );
                assert!(
                    f.controller.is_none(),
                    "\"a player\" milling has no controller scope, got {:?}",
                    f.controller
                );
            }
            other => panic!("expected Typed nonland-card filter, got {other:?}"),
        }
    }

    #[test]
    fn trigger_active_milled_an_opponent_scopes_controller() {
        // Infesting Radroach shape: the milled card lives in the opponent's library.
        let def = parse_trigger_line(
            "Whenever an opponent mills a nonland card, you gain 1 life.",
            "Test Card",
        );
        assert_eq!(def.mode, TriggerMode::Milled);
        match def.valid_card {
            Some(TargetFilter::Typed(ref f)) => assert_eq!(
                f.controller,
                Some(ControllerRef::Opponent),
                "opponent milling must scope the milled-card filter to Opponent"
            ),
            other => panic!("expected Typed opponent-scoped filter, got {other:?}"),
        }
    }

    #[test]
    fn trigger_active_milled_one_or_more() {
        // Zellix shape: "Whenever a player mills one or more creature cards, …"
        let def = parse_trigger_line(
            "Whenever a player mills one or more creature cards, create a 1/1 black Insect creature token.",
            "Test Card",
        );
        assert_eq!(def.mode, TriggerMode::Milled);
        assert!(def.batched, "\"one or more\" subject must stamp batched");
        match def.valid_card {
            Some(TargetFilter::Typed(ref f)) => assert!(
                f.type_filters
                    .iter()
                    .any(|t| matches!(t, TypeFilter::Creature)),
                "valid_card should be a creature-card filter, got {:?}",
                f.type_filters
            ),
            other => panic!("expected Typed creature-card filter, got {other:?}"),
        }
    }

    #[test]
    fn trigger_fights() {
        let def = parse_trigger_line(
            "Whenever a creature you control fights, put a +1/+1 counter on it.",
            "Test Card",
        );
        assert_eq!(def.mode, TriggerMode::Fight);
        assert!(def.valid_card.is_some());
    }

    // -- StaticCondition → TriggerCondition bridge tests --

    #[test]
    fn bridge_quantity_comparison() {
        let sc = StaticCondition::QuantityComparison {
            lhs: QuantityExpr::Ref {
                qty: QuantityRef::HandSize {
                    player: crate::types::ability::PlayerScope::Controller,
                },
            },
            comparator: Comparator::EQ,
            rhs: QuantityExpr::Fixed { value: 0 },
        };
        let tc = static_condition_to_trigger_condition(&sc).unwrap();
        assert!(matches!(
            tc,
            TriggerCondition::QuantityComparison {
                comparator: Comparator::EQ,
                ..
            }
        ));
    }

    #[test]
    fn bridge_is_present_to_controls_type() {
        let sc = StaticCondition::IsPresent {
            filter: Some(TargetFilter::Typed(
                TypedFilter::default().controller(ControllerRef::You),
            )),
        };
        let tc = static_condition_to_trigger_condition(&sc).unwrap();
        assert!(matches!(tc, TriggerCondition::ControlsType { .. }));
    }

    #[test]
    fn bridge_is_present_none_filter_returns_none() {
        let sc = StaticCondition::IsPresent { filter: None };
        assert!(static_condition_to_trigger_condition(&sc).is_none());
    }

    #[test]
    fn bridge_not_during_your_turn() {
        let sc = StaticCondition::Not {
            condition: Box::new(StaticCondition::DuringYourTurn),
        };
        let tc = static_condition_to_trigger_condition(&sc).unwrap();
        assert_eq!(
            tc,
            TriggerCondition::Not {
                condition: Box::new(TriggerCondition::DuringPlayersTurn {
                    player: PlayerFilter::Controller,
                }),
            }
        );
    }

    #[test]
    fn bridge_during_your_turn_maps_to_trigger() {
        assert_eq!(
            static_condition_to_trigger_condition(&StaticCondition::DuringYourTurn),
            Some(TriggerCondition::DuringPlayersTurn {
                player: PlayerFilter::Controller,
            }),
        );
    }

    #[test]
    fn bridge_not_is_present_to_quantity_eq_zero() {
        let sc = StaticCondition::Not {
            condition: Box::new(StaticCondition::IsPresent {
                filter: Some(TargetFilter::Typed(TypedFilter {
                    type_filters: vec![TypeFilter::Creature],
                    controller: Some(ControllerRef::You),
                    properties: Vec::new(),
                })),
            }),
        };
        let tc = static_condition_to_trigger_condition(&sc).unwrap();
        match tc {
            TriggerCondition::QuantityComparison {
                comparator,
                rhs: QuantityExpr::Fixed { value: 0 },
                ..
            } => assert_eq!(comparator, Comparator::EQ),
            other => panic!("expected QuantityComparison EQ 0, got {other:?}"),
        }
    }

    #[test]
    fn bridge_negated_quantity_comparison() {
        let sc = StaticCondition::Not {
            condition: Box::new(StaticCondition::QuantityComparison {
                lhs: QuantityExpr::Ref {
                    qty: QuantityRef::LifeTotal {
                        player: crate::types::ability::PlayerScope::Controller,
                    },
                },
                comparator: Comparator::GE,
                rhs: QuantityExpr::Fixed { value: 5 },
            }),
        };
        let tc = static_condition_to_trigger_condition(&sc).unwrap();
        match tc {
            TriggerCondition::QuantityComparison {
                comparator: Comparator::LT,
                ..
            } => {}
            other => panic!("expected negated GE→LT, got {other:?}"),
        }
    }

    #[test]
    fn bridge_has_max_speed() {
        let tc = static_condition_to_trigger_condition(&StaticCondition::HasMaxSpeed).unwrap();
        assert_eq!(tc, TriggerCondition::HasMaxSpeed);
    }

    #[test]
    fn bridge_class_level_ge() {
        let sc = StaticCondition::ClassLevelGE { level: 2 };
        let tc = static_condition_to_trigger_condition(&sc).unwrap();
        assert_eq!(tc, TriggerCondition::ClassLevelGE { level: 2 });
    }

    #[test]
    fn bridge_and_recursive() {
        let sc = StaticCondition::And {
            conditions: vec![
                StaticCondition::HasMaxSpeed,
                StaticCondition::QuantityComparison {
                    lhs: QuantityExpr::Ref {
                        qty: QuantityRef::HandSize {
                            player: crate::types::ability::PlayerScope::Controller,
                        },
                    },
                    comparator: Comparator::GE,
                    rhs: QuantityExpr::Fixed { value: 3 },
                },
            ],
        };
        let tc = static_condition_to_trigger_condition(&sc).unwrap();
        match tc {
            TriggerCondition::And { conditions } => assert_eq!(conditions.len(), 2),
            other => panic!("expected And, got {other:?}"),
        }
    }

    #[test]
    fn bridge_and_with_unmappable_returns_none() {
        let sc = StaticCondition::And {
            conditions: vec![
                StaticCondition::HasMaxSpeed,
                StaticCondition::IsRingBearer, // unmappable
            ],
        };
        assert!(static_condition_to_trigger_condition(&sc).is_none());
    }

    #[test]
    fn bridge_unmappable_variants_return_none() {
        assert!(
            static_condition_to_trigger_condition(&StaticCondition::SourceEnteredThisTurn)
                .is_none()
        );
        assert!(static_condition_to_trigger_condition(&StaticCondition::IsRingBearer).is_none());
    }

    #[test]
    fn bridge_monarch() {
        assert_eq!(
            static_condition_to_trigger_condition(&StaticCondition::IsMonarch),
            Some(TriggerCondition::IsMonarch),
        );
    }

    #[test]
    fn bridge_no_monarch() {
        assert_eq!(
            static_condition_to_trigger_condition(&StaticCondition::NoMonarch),
            Some(TriggerCondition::NoMonarch),
        );
    }

    #[test]
    fn bridge_city_blessing() {
        assert_eq!(
            static_condition_to_trigger_condition(&StaticCondition::HasCityBlessing),
            Some(TriggerCondition::HasCityBlessing),
        );
    }

    #[test]
    fn bridge_source_is_tapped() {
        assert_eq!(
            static_condition_to_trigger_condition(&StaticCondition::SourceIsTapped),
            Some(TriggerCondition::SourceIsTapped),
        );
    }

    #[test]
    fn bridge_source_in_zone() {
        use crate::types::zones::Zone;
        assert_eq!(
            static_condition_to_trigger_condition(&StaticCondition::SourceInZone {
                zone: Zone::Graveyard,
            }),
            Some(TriggerCondition::SourceInZone {
                zone: Zone::Graveyard,
            }),
        );
    }

    #[test]
    fn bridge_has_counters() {
        assert_eq!(
            static_condition_to_trigger_condition(&StaticCondition::HasCounters {
                counters: CounterMatch::OfType(CounterType::Time),
                minimum: 1,
                maximum: None,
            }),
            Some(TriggerCondition::HasCounters {
                counters: CounterMatch::OfType(CounterType::Time),
                minimum: 1,
                maximum: None,
            }),
        );
    }

    #[test]
    fn trigger_intervening_if_this_card_is_suspended() {
        let def = parse_trigger_line(
            "Whenever you cast a spell, if this card is suspended, remove a time counter from it.",
            "17-Year Cicadas",
        );

        assert_eq!(def.mode, TriggerMode::SpellCast);
        assert_eq!(def.trigger_zones, vec![Zone::Exile]);
        match def.condition {
            Some(TriggerCondition::And { conditions }) => {
                assert!(conditions.iter().any(|condition| matches!(
                    condition,
                    TriggerCondition::SourceInZone { zone: Zone::Exile }
                )));
                assert!(conditions.iter().any(|condition| matches!(
                    condition,
                    TriggerCondition::HasCounters {
                        counters: CounterMatch::OfType(CounterType::Time),
                        minimum: 1,
                        maximum: None,
                    }
                )));
            }
            other => panic!("expected suspended And condition, got {other:?}"),
        }
    }

    #[test]
    fn trigger_intervening_if_this_card_is_in_your_graveyard_sets_trigger_zone() {
        let def = parse_trigger_line(
            "At the beginning of your upkeep, if this card is in your graveyard, you gain 1 life.",
            "Graveyard Source",
        );

        assert_eq!(def.trigger_zones, vec![Zone::Graveyard]);
        assert_eq!(
            def.condition,
            Some(TriggerCondition::SourceInZone {
                zone: Zone::Graveyard,
            }),
        );
    }

    // -- Nom bridge fallback integration tests --

    // CR 603.4 + CR 102.1: "that player's turn" patterns bind to
    // the trigger event's player (drawer / tapper / damaged player / etc.), not
    // the trigger controller. Both the affirmative ("if it's") and negation
    // ("if it isn't" / "if it's not") forms are parsed via the source-referential
    // simple-pattern branch in `extract_if_condition`.
    #[test]
    fn extract_if_isnt_that_players_turn_yields_triggering_player_negation() {
        let (cleaned, cond) =
            extract_if_condition("if it isn't that player's turn, create a tapped Treasure token");
        assert_eq!(cleaned, "create a tapped Treasure token");
        assert_eq!(
            cond.unwrap(),
            TriggerCondition::Not {
                condition: Box::new(TriggerCondition::DuringPlayersTurn {
                    player: PlayerFilter::TriggeringPlayer,
                }),
            }
        );
    }

    #[test]
    fn extract_if_its_not_that_players_turn_yields_triggering_player_negation() {
        let (cleaned, cond) =
            extract_if_condition("if it's not that player's turn, destroy that land");
        assert_eq!(cleaned, "destroy that land");
        assert_eq!(
            cond.unwrap(),
            TriggerCondition::Not {
                condition: Box::new(TriggerCondition::DuringPlayersTurn {
                    player: PlayerFilter::TriggeringPlayer,
                }),
            }
        );
    }

    #[test]
    fn extract_if_its_that_players_turn_yields_triggering_player_affirmative() {
        let (cleaned, cond) = extract_if_condition("if it's that player's turn, you gain 1 life");
        assert_eq!(cleaned, "you gain 1 life");
        assert_eq!(
            cond.unwrap(),
            TriggerCondition::DuringPlayersTurn {
                player: PlayerFilter::TriggeringPlayer,
            }
        );
    }

    #[test]
    fn tataru_taru_scions_secretary_attaches_triggering_player_negation() {
        // CR 603.4 + CR 102.1: "Whenever an opponent draws a card, if it isn't
        // that player's turn, create a tapped Treasure token. This ability
        // triggers only once each turn."
        let def = parse_trigger_line(
            "Whenever an opponent draws a card, if it isn't that player's turn, create a tapped Treasure token. This ability triggers only once each turn.",
            "Tataru Taru",
        );
        assert_eq!(def.mode, TriggerMode::Drawn);
        assert_eq!(
            def.condition,
            Some(TriggerCondition::Not {
                condition: Box::new(TriggerCondition::DuringPlayersTurn {
                    player: PlayerFilter::TriggeringPlayer,
                }),
            }),
            "Tataru Taru must gate on it NOT being the drawing player's turn"
        );
    }

    #[test]
    fn price_of_glory_attaches_triggering_player_negation() {
        // CR 603.4 + CR 102.1: "Whenever a player taps a land for mana, if it's
        // not that player's turn, destroy that land."
        let def = parse_trigger_line(
            "Whenever a player taps a land for mana, if it's not that player's turn, destroy that land.",
            "Price of Glory",
        );
        assert_eq!(def.mode, TriggerMode::TapsForMana);
        assert_eq!(
            def.condition,
            Some(TriggerCondition::Not {
                condition: Box::new(TriggerCondition::DuringPlayersTurn {
                    player: PlayerFilter::TriggeringPlayer,
                }),
            }),
            "Price of Glory must gate on it NOT being the tapping player's turn"
        );
    }

    #[test]
    fn fallback_if_you_control_a_creature() {
        // "if you control a creature" is handled by the nom bridge fallback
        let (cleaned, cond) = extract_if_condition("if you control a creature, draw a card");
        assert_eq!(cleaned, "draw a card");
        assert!(cond.is_some());
        assert!(matches!(
            cond.unwrap(),
            TriggerCondition::ControlsType { .. }
        ));
    }

    #[test]
    fn fallback_if_hand_empty() {
        let (cleaned, cond) = extract_if_condition("if you have no cards in hand, draw a card");
        assert_eq!(cleaned, "draw a card");
        match cond.unwrap() {
            TriggerCondition::QuantityComparison {
                comparator: Comparator::EQ,
                rhs: QuantityExpr::Fixed { value: 0 },
                ..
            } => {}
            other => panic!("expected QuantityComparison EQ 0, got {other:?}"),
        }
    }

    #[test]
    fn combinator_handles_gained_life() {
        // "if you gained life this turn" routes through the nom combinator,
        // producing QuantityComparison with LifeGainedThisTurn.
        let (_, cond) = extract_if_condition("if you gained life this turn, draw a card");
        assert_eq!(
            cond.unwrap(),
            TriggerCondition::QuantityComparison {
                lhs: QuantityExpr::Ref {
                    qty: QuantityRef::LifeGainedThisTurn {
                        player: PlayerScope::Controller,
                    },
                },
                comparator: Comparator::GE,
                rhs: QuantityExpr::Fixed { value: 1 },
            }
        );
    }

    #[test]
    fn fallback_does_not_shadow_specific_not_your_turn() {
        let (_, cond) = extract_if_condition("if it's not your turn, draw a card");
        assert_eq!(
            cond.unwrap(),
            TriggerCondition::Not {
                condition: Box::new(TriggerCondition::DuringPlayersTurn {
                    player: PlayerFilter::Controller,
                }),
            }
        );
    }

    #[test]
    fn combinator_handles_controls_count() {
        // "if you control 3 or more creatures" routes through the nom combinator,
        // producing QuantityComparison with ObjectCount.
        let (_, cond) = extract_if_condition("if you control three or more creatures, draw a card");
        assert!(
            matches!(
                cond.unwrap(),
                TriggerCondition::QuantityComparison {
                    lhs: QuantityExpr::Ref {
                        qty: QuantityRef::ObjectCount { .. },
                    },
                    comparator: Comparator::GE,
                    rhs: QuantityExpr::Fixed { value: 3 },
                }
            ),
            "Expected QuantityComparison with ObjectCount >= 3"
        );
    }

    #[test]
    fn combinator_handles_life_total() {
        // "if you have 5 or more life" routes through the nom combinator,
        // producing QuantityComparison with LifeTotal.
        let (_, cond) = extract_if_condition("if you have five or more life, draw a card");
        assert_eq!(
            cond.unwrap(),
            TriggerCondition::QuantityComparison {
                lhs: QuantityExpr::Ref {
                    qty: QuantityRef::LifeTotal {
                        player: crate::types::ability::PlayerScope::Controller
                    },
                },
                comparator: Comparator::GE,
                rhs: QuantityExpr::Fixed { value: 5 },
            }
        );
    }

    // -- Source-referential condition extraction tests --

    #[test]
    fn extract_tribute_not_paid() {
        let (cleaned, cond) =
            extract_if_condition("put two +1/+1 counters on it if tribute wasn't paid");
        assert_eq!(cleaned, "put two +1/+1 counters on it");
        assert_eq!(cond.unwrap(), TriggerCondition::TributeNotPaid);
    }

    #[test]
    fn extract_addendum_main_phase() {
        let (cleaned, cond) =
            extract_if_condition("draw a card if you cast this spell during your main phase");
        assert_eq!(cleaned, "draw a card");
        assert_eq!(
            cond.unwrap(),
            TriggerCondition::CastDuringPhase {
                phases: vec![Phase::PreCombatMain, Phase::PostCombatMain],
            }
        );
    }

    #[test]
    fn extract_adamant_three_red() {
        let (cleaned, cond) = extract_if_condition(
            "it deals 4 damage instead if at least three red mana was spent to cast this spell",
        );
        assert_eq!(cleaned, "it deals 4 damage instead");
        assert_eq!(
            cond.unwrap(),
            TriggerCondition::ManaColorSpent {
                color: crate::types::mana::ManaColor::Red,
                minimum: 3,
            }
        );
    }

    // CR 400.7d + CR 601.2h: Incarnation / hybrid-ETB cycle — symbolic-form
    // spent-mana condition "if {C}{C} was spent to cast it".
    #[test]
    fn extract_symbolic_mana_spent_two_green() {
        let (cleaned, cond) = extract_if_condition(
            "if {G}{G} was spent to cast it, exile target artifact or enchantment an opponent controls",
        );
        assert_eq!(
            cleaned,
            "exile target artifact or enchantment an opponent controls"
        );
        assert_eq!(
            cond.unwrap(),
            TriggerCondition::ManaColorSpent {
                color: crate::types::mana::ManaColor::Green,
                minimum: 2,
            }
        );
    }

    #[test]
    fn extract_symbolic_mana_spent_two_blue_with_trailing_effect() {
        let (cleaned, cond) = extract_if_condition(
            "if {U}{U} was spent to cast it, draw two cards, then discard a card",
        );
        assert_eq!(cleaned, "draw two cards, then discard a card");
        assert_eq!(
            cond.unwrap(),
            TriggerCondition::ManaColorSpent {
                color: crate::types::mana::ManaColor::Blue,
                minimum: 2,
            }
        );
    }

    #[test]
    fn extract_symbolic_mana_spent_single_red_this_spell() {
        let (cleaned, cond) =
            extract_if_condition("draw a card if {R} was spent to cast this spell");
        assert_eq!(cleaned, "draw a card");
        assert_eq!(
            cond.unwrap(),
            TriggerCondition::ManaColorSpent {
                color: crate::types::mana::ManaColor::Red,
                minimum: 1,
            }
        );
    }

    #[test]
    fn extract_symbolic_unless_mana_spent_single_blue() {
        let (cleaned, cond) = extract_if_condition("sacrifice it unless {U} was spent to cast it");
        assert_eq!(cleaned, "sacrifice it");
        assert_eq!(
            cond.unwrap(),
            TriggerCondition::Not {
                condition: Box::new(TriggerCondition::ManaColorSpent {
                    color: crate::types::mana::ManaColor::Blue,
                    minimum: 1,
                }),
            }
        );
    }

    #[test]
    fn extract_symbolic_unless_mana_spent_two_black() {
        let (cleaned, cond) =
            extract_if_condition("sacrifice it unless {B}{B} was spent to cast it");
        assert_eq!(cleaned, "sacrifice it");
        assert_eq!(
            cond.unwrap(),
            TriggerCondition::Not {
                condition: Box::new(TriggerCondition::ManaColorSpent {
                    color: crate::types::mana::ManaColor::Black,
                    minimum: 2,
                }),
            }
        );
    }

    #[test]
    fn extract_no_mana_spent_condition() {
        let (cleaned, cond) =
            extract_if_condition("if no mana was spent to cast it, counter that spell");
        assert_eq!(cleaned, "counter that spell");
        assert_eq!(
            cond,
            Some(TriggerCondition::ManaSpentCondition {
                text: "no mana was spent to cast it".to_string(),
            })
        );
    }

    // The extractor uses `scan_split_at_phrase`, so the clause doesn't have to
    // be at the start of the text. Covers the same positional flexibility the
    // word-form Adamant extractor already relies on.
    #[test]
    fn extract_symbolic_mana_spent_mid_sentence() {
        let (cleaned, cond) = extract_if_condition(
            "it deals 4 damage instead if {R}{R}{R} was spent to cast this spell",
        );
        assert_eq!(cleaned, "it deals 4 damage instead");
        assert_eq!(
            cond.unwrap(),
            TriggerCondition::ManaColorSpent {
                color: crate::types::mana::ManaColor::Red,
                minimum: 3,
            }
        );
    }

    // Production path pre-lowercases effect text; verify the extractor matches
    // lowercase `{g}{g}` equivalently to uppercase. This is the case actually
    // exercised by Wistfulness/Vibrance/Deceit at card-data-export time.
    #[test]
    fn extract_symbolic_mana_spent_lowercase_input() {
        let (cleaned, cond) = extract_if_condition(
            "if {g}{g} was spent to cast it, exile target artifact or enchantment an opponent controls",
        );
        assert_eq!(
            cleaned,
            "exile target artifact or enchantment an opponent controls"
        );
        assert_eq!(
            cond.unwrap(),
            TriggerCondition::ManaColorSpent {
                color: crate::types::mana::ManaColor::Green,
                minimum: 2,
            }
        );
    }

    #[test]
    fn extract_symbolic_mana_spent_mixed_colors() {
        let (cleaned, cond) = extract_if_condition("do something if {G}{U} was spent to cast it");
        assert_eq!(cleaned, "do something");
        let TriggerCondition::And { conditions } =
            cond.expect("mixed color run should produce an And condition")
        else {
            panic!("expected And condition");
        };
        assert_eq!(
            conditions,
            vec![
                TriggerCondition::ManaColorSpent {
                    color: crate::types::mana::ManaColor::Green,
                    minimum: 1,
                },
                TriggerCondition::ManaColorSpent {
                    color: crate::types::mana::ManaColor::Blue,
                    minimum: 1,
                },
            ]
        );
    }

    // Hybrid pips should not match — CR 601.2h tracks the color actually paid,
    // not the hybrid pip symbol itself.
    #[test]
    fn extract_symbolic_mana_spent_rejects_hybrid() {
        let (_cleaned, cond) =
            extract_if_condition("do something if {G/U}{G/U} was spent to cast it");
        assert!(cond.is_none(), "hybrid pips should not match");
    }

    #[test]
    fn extract_had_counter_typed() {
        let (cleaned, cond) =
            extract_if_condition("return it to the battlefield if it had a +1/+1 counter on it");
        assert_eq!(cleaned, "return it to the battlefield");
        assert_eq!(
            cond.unwrap(),
            TriggerCondition::HadCounters {
                counter_type: Some(crate::types::counter::CounterType::Plus1Plus1),
            }
        );
    }

    #[test]
    fn extract_if_it_wasnt_blocking_as_zone_change_lookback() {
        let (cleaned, cond) = extract_if_condition("if it wasn't blocking, draw a card");
        assert_eq!(cleaned, "draw a card");
        assert_eq!(
            cond.unwrap(),
            TriggerCondition::Not {
                condition: Box::new(TriggerCondition::ZoneChangeObjectMatchesFilter {
                    origin: Some(Zone::Battlefield),
                    destination: Zone::Graveyard,
                    filter: TargetFilter::Typed(
                        TypedFilter::creature().properties(vec![FilterProp::Blocking])
                    ),
                }),
            }
        );
    }

    #[test]
    fn extract_if_it_was_enchanted_or_equipped_as_zone_change_lookback() {
        let (cleaned, cond) =
            extract_if_condition("if it was enchanted or equipped, return it to its owner's hand");
        assert_eq!(cleaned, "return it to its owner's hand");
        assert_eq!(
            cond.unwrap(),
            TriggerCondition::ZoneChangeObjectMatchesFilter {
                origin: Some(Zone::Battlefield),
                destination: Zone::Graveyard,
                filter: TargetFilter::Typed(TypedFilter::creature().properties(vec![
                    FilterProp::HasAnyAttachmentOf {
                        kinds: vec![AttachmentKind::Aura, AttachmentKind::Equipment],
                        controller: None,
                    },
                ])),
            }
        );
    }

    #[test]
    fn extract_had_counters_untyped() {
        let (cleaned, cond) = extract_if_condition("draw a card if it had counters on it");
        assert_eq!(cleaned, "draw a card");
        assert_eq!(
            cond.unwrap(),
            TriggerCondition::HadCounters { counter_type: None },
        );
    }

    /// CR 603.4 + CR 608.2h: "you're the monarch" is a re-homeable post-effect
    /// `if` — `extract_if_condition` leaves it for `strip_suffix_conditional`
    /// to attach as a clause-level `AbilityCondition` rather than hoisting it.
    #[test]
    fn bridge_monarch_from_trigger_text() {
        let (cleaned, cond) = extract_if_condition("draw a card if you're the monarch");
        assert_eq!(
            cond, None,
            "a re-homeable post-effect `if` must not hoist to the trigger condition",
        );
        assert_eq!(cleaned, "draw a card if you're the monarch");
    }

    /// CR 603.4 + CR 608.2h: "this land is tapped" is a re-homeable post-effect
    /// `if` — left intact by `extract_if_condition` for downstream re-homing.
    #[test]
    fn bridge_source_tapped_from_trigger_text() {
        let (cleaned, cond) =
            extract_if_condition("put a storage counter on it if this land is tapped");
        assert_eq!(
            cond, None,
            "a re-homeable post-effect `if` must not hoist to the trigger condition",
        );
        assert_eq!(
            cleaned,
            "put a storage counter on it if this land is tapped"
        );
    }

    #[test]
    fn cast_trigger_lowers_to_control_next_turn_effect() {
        let def = parse_trigger_line(
            "When you cast this spell, you gain control of target opponent during that player's next turn. After that turn, that player takes an extra turn.",
            "Emrakul, the Promised End",
        );
        assert_eq!(def.mode, TriggerMode::SpellCast);
        let execute = def.execute.expect("expected execute ability");
        match execute.effect.as_ref() {
            Effect::ControlNextTurn {
                target,
                grant_extra_turn_after,
            } => {
                assert!(*grant_extra_turn_after);
                assert_eq!(
                    target,
                    &TargetFilter::Typed(
                        TypedFilter::default().controller(ControllerRef::Opponent)
                    )
                );
            }
            other => panic!("expected ControlNextTurn effect, got {other:?}"),
        }
    }

    #[test]
    fn state_trigger_control_no_islands() {
        let def = parse_trigger_line(
            "When you control no Islands, sacrifice this creature.",
            "Dandân",
        );
        assert_eq!(def.mode, TriggerMode::StateCondition);
        if let Some(TriggerCondition::ControlsNone { filter }) = &def.condition {
            if let TargetFilter::Typed(tf) = filter {
                assert!(
                    tf.type_filters
                        .iter()
                        .any(|t| matches!(t, TypeFilter::Subtype(s) if s == "Island")),
                    "expected Island subtype in {:?}",
                    tf.type_filters,
                );
            } else {
                panic!("expected Typed filter, got {filter:?}");
            }
        } else {
            panic!("expected ControlsNone condition, got {:?}", def.condition,);
        }
        // Effect should be sacrifice self
        let execute = def.execute.as_ref().expect("should have execute");
        assert!(
            matches!(*execute.effect, Effect::Sacrifice { .. }),
            "expected Sacrifice, got {:?}",
            execute.effect,
        );
    }

    #[test]
    fn state_trigger_control_no_other_creatures() {
        let def = parse_trigger_line(
            "When you control no other creatures, sacrifice this creature.",
            "Emperor Crocodile",
        );
        assert_eq!(def.mode, TriggerMode::StateCondition);
        if let Some(TriggerCondition::ControlsNone { filter }) = &def.condition {
            if let TargetFilter::Typed(tf) = filter {
                assert!(tf.properties.contains(&FilterProp::Another));
                assert!(tf.type_filters.contains(&TypeFilter::Creature));
                assert_eq!(tf.controller, Some(ControllerRef::You));
            } else {
                panic!("expected Typed filter, got {filter:?}");
            }
        } else {
            panic!("expected ControlsNone condition, got {:?}", def.condition);
        }
    }

    #[test]
    fn state_trigger_control_no_artifacts() {
        let def = parse_trigger_line(
            "When you control no artifacts, sacrifice this creature.",
            "Covetous Dragon",
        );
        assert_eq!(def.mode, TriggerMode::StateCondition);
        if let Some(TriggerCondition::ControlsNone { filter }) = &def.condition {
            if let TargetFilter::Typed(tf) = filter {
                assert!(tf.type_filters.contains(&TypeFilter::Artifact));
            } else {
                panic!("expected Typed filter, got {filter:?}");
            }
        } else {
            panic!("expected ControlsNone condition, got {:?}", def.condition);
        }
    }

    // --- Compound trigger tests ---

    #[test]
    fn compound_and_when_cycle_and_dies() {
        // Jund Sojourners: "When you cycle ~ and when ~ dies, you may have it deal 1 damage to any target."
        let triggers = parse_trigger_lines(
            "When you cycle this card and when this creature dies, you may have it deal 1 damage to any target.",
            "Jund Sojourners",
        );
        assert_eq!(triggers.len(), 2);
        assert_eq!(triggers[0].mode, TriggerMode::Cycled);
        assert_eq!(triggers[1].mode, TriggerMode::ChangesZone);
        assert_eq!(triggers[1].origin, Some(Zone::Battlefield));
        assert_eq!(triggers[1].destination, Some(Zone::Graveyard));
        // Both should have the same execute effect
        assert!(triggers[0].execute.is_some());
        assert!(triggers[1].execute.is_some());
    }

    #[test]
    fn compound_and_when_enters_and_sacrifice() {
        // Heaped Harvest: "When this artifact enters and when you sacrifice it, ..."
        let triggers = parse_trigger_lines(
            "When this artifact enters and when you sacrifice it, you may search your library for a basic land card, put it onto the battlefield tapped, then shuffle.",
            "Heaped Harvest",
        );
        assert_eq!(triggers.len(), 2);
        assert_eq!(triggers[0].mode, TriggerMode::ChangesZone);
        assert_eq!(triggers[0].destination, Some(Zone::Battlefield));
        assert_eq!(triggers[1].mode, TriggerMode::Sacrificed);
    }

    #[test]
    fn compound_or_enters_or_deals_combat_damage() {
        // Aerial Extortionist: "Whenever this creature enters or deals combat damage to a player, ..."
        let triggers = parse_trigger_lines(
            "Whenever this creature enters or deals combat damage to a player, exile up to one target nonland permanent.",
            "Aerial Extortionist",
        );
        assert_eq!(triggers.len(), 2);
        assert_eq!(triggers[0].mode, TriggerMode::ChangesZone);
        assert_eq!(triggers[0].destination, Some(Zone::Battlefield));
        assert_eq!(triggers[1].mode, TriggerMode::DamageDone);
        assert_eq!(triggers[1].damage_kind, DamageKindFilter::CombatOnly);
    }

    #[test]
    fn compound_or_deals_combat_damage_or_dies() {
        // Park Heights Maverick: "Whenever this creature deals combat damage to a player or dies, proliferate."
        let triggers = parse_trigger_lines(
            "Whenever this creature deals combat damage to a player or dies, proliferate.",
            "Park Heights Maverick",
        );
        assert_eq!(triggers.len(), 2);
        assert_eq!(triggers[0].mode, TriggerMode::DamageDone);
        assert_eq!(triggers[0].damage_kind, DamageKindFilter::CombatOnly);
        assert_eq!(triggers[1].mode, TriggerMode::ChangesZone);
        assert_eq!(triggers[1].origin, Some(Zone::Battlefield));
        assert_eq!(triggers[1].destination, Some(Zone::Graveyard));
    }

    #[test]
    fn compound_and_whenever_enters_and_cast_spell() {
        // Salacinder and Soot: "When ~ enters and whenever you cast an Elemental spell, ..."
        let triggers = parse_trigger_lines(
            "When Salacinder and Soot enters and whenever you cast an Elemental spell, choose one —",
            "Salacinder and Soot",
        );
        assert_eq!(triggers.len(), 2);
        assert_eq!(triggers[0].mode, TriggerMode::ChangesZone);
        assert_eq!(triggers[1].mode, TriggerMode::SpellCast);
    }

    #[test]
    fn non_compound_trigger_returns_single() {
        // Normal trigger should produce exactly 1 result
        let triggers = parse_trigger_lines("When this creature enters, draw a card.", "Test Card");
        assert_eq!(triggers.len(), 1);
        assert_eq!(triggers[0].mode, TriggerMode::ChangesZone);
    }

    // ── "and/or" compound subject triggers ──

    #[test]
    fn trigger_self_and_or_other_nontoken_creatures_enter() {
        // CR 603.4 + CR 601.2: Satoru-style "~ and/or one or more other nontoken
        // creatures you control enter, if none of them were cast ..."
        let def = parse_trigger_line(
            "Whenever ~ and/or one or more other nontoken creatures you control enter, if none of them were cast or no mana was spent to cast them, draw a card.",
            "Satoru, the Infiltrator",
        );
        assert_eq!(def.mode, TriggerMode::ChangesZone);
        assert_eq!(def.destination, Some(Zone::Battlefield));
        assert!(def.batched);

        // Subject should be Or { SelfRef, Typed(nontoken creature you control, Another) }
        match &def.valid_card {
            Some(TargetFilter::Or { filters }) => {
                assert_eq!(
                    filters.len(),
                    2,
                    "Expected 2 filters in Or, got {filters:?}"
                );
                assert_eq!(filters[0], TargetFilter::SelfRef);
                // Second filter: nontoken creature you control with Another
                if let TargetFilter::Typed(tf) = &filters[1] {
                    assert!(
                        tf.properties.contains(&FilterProp::Another),
                        "Expected Another property, got {:?}",
                        tf.properties
                    );
                    assert_eq!(tf.controller, Some(ControllerRef::You));
                    assert!(
                        tf.type_filters.contains(&TypeFilter::Creature),
                        "Expected Creature type, got {:?}",
                        tf.type_filters
                    );
                } else {
                    panic!("Expected Typed filter, got {:?}", filters[1]);
                }
            }
            other => panic!("Expected Or filter, got {other:?}"),
        }

        // Condition: Or { Not(WasCast), ManaSpentCondition }
        match &def.condition {
            Some(TriggerCondition::Or { conditions }) => {
                assert_eq!(conditions.len(), 2);
                assert_eq!(
                    conditions[0],
                    TriggerCondition::Not {
                        condition: Box::new(TriggerCondition::WasCast),
                    }
                );
                assert!(
                    matches!(&conditions[1], TriggerCondition::ManaSpentCondition { .. }),
                    "Expected ManaSpentCondition, got {:?}",
                    conditions[1]
                );
            }
            other => panic!("Expected Or condition, got {other:?}"),
        }
    }

    #[test]
    fn trigger_if_it_wasnt_cast() {
        // CR 603.4 + CR 601.2: "if it wasn't cast" — negation of WasCast.
        let def = parse_trigger_line(
            "Whenever a creature enters under your control, if it wasn't cast, draw a card.",
            "Test Card",
        );
        assert_eq!(def.mode, TriggerMode::ChangesZone);
        assert_eq!(
            def.condition,
            Some(TriggerCondition::Not {
                condition: Box::new(TriggerCondition::WasCast),
            })
        );
    }

    #[test]
    fn trigger_subject_extracts_opponent_as_player() {
        // CR 608.2k: "an opponent" should be recognized as a player-type subject,
        // not fall through to parse_type_phrase returning Any.
        let (filter, rest) =
            parse_single_subject("an opponent draws a card", &mut ParseContext::default());
        assert!(
            matches!(
                &filter,
                TargetFilter::Typed(tf) if tf.type_filters.is_empty()
                    && tf.controller == Some(ControllerRef::Opponent)
            ),
            "expected opponent player filter, got: {filter:?}"
        );
        assert!(
            rest.starts_with("draws"),
            "rest should start with verb: {rest}"
        );
    }

    #[test]
    fn trigger_subject_extracts_player() {
        let (filter, rest) =
            parse_single_subject("a player casts a spell", &mut ParseContext::default());
        assert_eq!(filter, TargetFilter::Player);
        assert!(
            rest.starts_with("casts"),
            "rest should start with verb: {rest}"
        );
    }

    #[test]
    fn sheoldred_they_lose_life_has_triggering_player() {
        // Sheoldred: "Whenever an opponent draws a card, they lose 2 life."
        // The LoseLife effect should target TriggeringPlayer (the opponent who drew).
        let def = parse_trigger_line(
            "Whenever an opponent draws a card, they lose 2 life.",
            "Sheoldred, the Apocalypse",
        );
        assert_eq!(def.mode, TriggerMode::Drawn);
        let execute = def.execute.as_ref().expect("should have execute");
        match &*execute.effect {
            Effect::LoseLife { target, .. } => {
                assert_eq!(
                    *target,
                    Some(TargetFilter::TriggeringPlayer),
                    "LoseLife should target TriggeringPlayer"
                );
            }
            other => panic!("expected LoseLife, got: {other:?}"),
        }
    }

    #[test]
    fn smothering_tithe_that_player_pays_as_triggering_player() {
        let def = parse_trigger_line(
            "Whenever an opponent draws a card, that player may pay {2}. If the player doesn't, you create a Treasure token.",
            "Smothering Tithe",
        );

        assert_eq!(def.mode, TriggerMode::Drawn);
        let execute = def.execute.as_ref().expect("should have execute");
        match &*execute.effect {
            Effect::PayCost {
                payer,
                cost: crate::types::ability::PaymentCost::Mana { cost },
            } => {
                assert_eq!(payer, &TargetFilter::TriggeringPlayer);
                assert_eq!(cost, &crate::types::mana::ManaCost::generic(2));
            }
            other => panic!("expected PayCost, got: {other:?}"),
        }
        assert!(execute.optional, "that player may pay should be optional");

        let sub = execute
            .sub_ability
            .as_ref()
            .expect("Treasure creation should remain chained");
        assert_eq!(
            sub.condition,
            Some(AbilityCondition::Not {
                condition: Box::new(AbilityCondition::IfYouDo)
            })
        );
        match &*sub.effect {
            Effect::Token { name, owner, .. } => {
                assert_eq!(name, "Treasure");
                assert_eq!(owner, &TargetFilter::Controller);
            }
            other => panic!("expected Treasure Token sub_ability, got: {other:?}"),
        }
    }

    /// CR 603.4: Wedding Ring — "an opponent who controls F draws
    /// a card" parses the relative clause into an `ObjectCount >= 1`
    /// intervening-if scoped to the triggering player, ANDed with the
    /// during-turn timing condition.
    fn assert_wedding_ring_clause_condition(condition: &Option<TriggerCondition>) {
        let TriggerCondition::And { conditions } = condition
            .as_ref()
            .expect("trigger should carry a condition")
        else {
            panic!("expected And condition, got: {condition:?}");
        };
        assert_eq!(conditions.len(), 2, "during-turn AND clause-presence");
        // First conjunct: the "during their turn" timing restriction.
        assert_eq!(
            conditions[0],
            TriggerCondition::DuringPlayersTurn {
                player: PlayerFilter::TriggeringPlayer,
            },
        );
        // Second conjunct: the "who controls F" relative-clause intervening-if.
        match &conditions[1] {
            TriggerCondition::QuantityComparison {
                lhs:
                    QuantityExpr::Ref {
                        qty: QuantityRef::ObjectCount { filter },
                    },
                comparator: Comparator::GE,
                rhs: QuantityExpr::Fixed { value: 1 },
            } => {
                let TargetFilter::Typed(tf) = filter else {
                    panic!("expected Typed clause filter, got: {filter:?}");
                };
                assert_eq!(
                    tf.controller,
                    Some(ControllerRef::TriggeringPlayer),
                    "clause filter controller must be TriggeringPlayer",
                );
                assert!(
                    tf.type_filters.contains(&TypeFilter::Artifact),
                    "clause filter should be an artifact: {tf:?}",
                );
                // CR 201.2a: name comparison is case-insensitive at evaluation;
                // `parse_trigger_condition` lowercases the condition text, so the
                // parsed `Named` value is lowercase here.
                assert!(
                    tf.properties.iter().any(|p| matches!(
                        p,
                        FilterProp::Named { name } if name.eq_ignore_ascii_case("Wedding Ring")
                    )),
                    "clause filter should carry Named \"Wedding Ring\": {tf:?}",
                );
            }
            other => panic!("expected QuantityComparison clause, got: {other:?}"),
        }
    }

    #[test]
    fn wedding_ring_who_controls_clause_drawn() {
        // `parse_trigger_condition` receives the trigger CONDITION clause only
        // (the effect has already been split off by the IR pipeline at the
        // first comma — see `parse_trigger_line_with_index_ir`).
        let mut ctx = ParseContext::default();
        let (mode, def) = parse_trigger_condition(
            "Whenever an opponent who controls an artifact named Wedding Ring draws a card during their turn",
            &mut ctx,
        );
        assert_eq!(mode, TriggerMode::Drawn);
        assert_eq!(
            def.valid_target,
            Some(TargetFilter::Typed(
                TypedFilter::default().controller(ControllerRef::Opponent)
            )),
        );
        assert_wedding_ring_clause_condition(&def.condition);
    }

    #[test]
    fn wedding_ring_who_controls_clause_gains_life() {
        let mut ctx = ParseContext::default();
        let (mode, def) = parse_trigger_condition(
            "Whenever an opponent who controls an artifact named Wedding Ring gains life during their turn",
            &mut ctx,
        );
        assert_eq!(mode, TriggerMode::LifeGained);
        assert_eq!(
            def.valid_target,
            Some(TargetFilter::Typed(
                TypedFilter::default().controller(ControllerRef::Opponent)
            )),
        );
        assert_wedding_ring_clause_condition(&def.condition);
    }

    #[test]
    fn plain_opponent_draws_has_no_clause_condition() {
        // CR 603.4: A trigger with no "who controls" clause must not carry a
        // QuantityComparison intervening-if (unregressed baseline).
        let mut ctx = ParseContext::default();
        let (mode, def) = parse_trigger_condition("an opponent draws a card", &mut ctx);
        assert_eq!(mode, TriggerMode::Drawn);
        assert_eq!(def.condition, None);
    }

    #[test]
    fn parse_trigger_condition_entry_resets_stale_subject_clause() {
        // ROUND-3 CORRECTION #1: `parse_trigger_condition` unconditionally
        // resets `ctx.pending_trigger_subject_clause` at entry. This guards
        // against a clause set on a prior parse path (subject decomposition,
        // compound-`or` recursion, or a discarded-remainder caller such as
        // `extract_trigger_subject_for_context`) leaking into the next trigger
        // line — those paths set the clause but never reach the consuming
        // `.take()` in `parse_trigger_condition`.
        //
        // This test discriminates the entry-reset directly: a stale clause is
        // pre-seeded on the context, then a CLEAN trigger (no "who controls"
        // clause of its own) is parsed. With the entry-reset the stale clause
        // is discarded and the clean trigger carries NO condition. Without the
        // entry-reset the stale clause survives to the post-`try_parse_event`
        // `.take()` and is wrongly ANDed into the clean trigger's condition.
        let mut ctx = ParseContext {
            pending_trigger_subject_clause: Some(TargetFilter::Typed(TypedFilter {
                type_filters: vec![TypeFilter::Artifact],
                controller: None,
                properties: vec![FilterProp::Named {
                    name: "wedding ring".to_string(),
                }],
            })),
            ..ParseContext::default()
        };

        let (mode, def) = parse_trigger_condition("an opponent draws a card", &mut ctx);
        assert_eq!(mode, TriggerMode::Drawn);
        assert_eq!(
            def.condition, None,
            "entry-reset must discard the pre-seeded stale clause; \
             a clean trigger must not inherit a leaked QuantityComparison",
        );
        // The reset must also clear the field itself, not just ignore it.
        assert_eq!(ctx.pending_trigger_subject_clause, None);
    }

    #[test]
    fn who_controls_clause_does_not_leak_to_next_trigger_line() {
        // Sequential end-to-end check: a "who controls F" trigger followed by a
        // plain opponent-draw trigger on the SAME context. The first parse
        // consumes its own clause via `.take()`; the entry-reset additionally
        // guarantees the second line starts clean.
        let mut ctx = ParseContext::default();
        let (_, first) = parse_trigger_condition(
            "Whenever an opponent who controls an artifact named Wedding Ring draws a card during their turn",
            &mut ctx,
        );
        assert_wedding_ring_clause_condition(&first.condition);
        let (_, second) = parse_trigger_condition("an opponent draws a card", &mut ctx);
        assert_eq!(
            second.condition, None,
            "second trigger line must not inherit the first line's clause",
        );
    }

    /// CR 603.4: Assert `condition` is exactly the who-controls `ObjectCount
    /// >= 1` intervening-if (no during-turn AND-wrapper), with the clause
    /// filter's controller rewritten to `TriggeringPlayer` and carrying a
    /// type-filter matching `expect_type`.
    fn assert_who_controls_clause_only(
        condition: &Option<TriggerCondition>,
        expect_type: TypeFilter,
    ) {
        match condition
            .as_ref()
            .expect("trigger should carry a condition")
        {
            TriggerCondition::QuantityComparison {
                lhs:
                    QuantityExpr::Ref {
                        qty: QuantityRef::ObjectCount { filter },
                    },
                comparator: Comparator::GE,
                rhs: QuantityExpr::Fixed { value: 1 },
            } => {
                let TargetFilter::Typed(tf) = filter else {
                    panic!("expected Typed clause filter, got: {filter:?}");
                };
                assert_eq!(
                    tf.controller,
                    Some(ControllerRef::TriggeringPlayer),
                    "clause filter controller must be TriggeringPlayer",
                );
                assert!(
                    tf.type_filters.contains(&expect_type),
                    "clause filter should be {expect_type:?}: {tf:?}",
                );
            }
            other => panic!("expected QuantityComparison clause, got: {other:?}"),
        }
    }

    #[test]
    fn who_controls_clause_cast_lifts_condition() {
        // CR 603.4 + CR 601.2: BEFORE the cast-scan guard, `split_once_on`
        // matched " casts a" anywhere, so this line parsed to a clause-LESS
        // SpellCast (the who-controls clause silently dropped). AFTER, the
        // cast-scan guard declines, subject decomposition runs, and the
        // who-controls clause is lifted into `def.condition`.
        let mut ctx = ParseContext::default();
        let (mode, def) = parse_trigger_condition(
            "Whenever an opponent who controls a creature casts a spell",
            &mut ctx,
        );
        assert_eq!(mode, TriggerMode::SpellCast);
        assert_who_controls_clause_only(&def.condition, TypeFilter::Creature);
    }

    #[test]
    fn who_controls_clause_sacrifice_lifts_condition() {
        // CR 603.4 + CR 701.21: who-controls sacrifice line is purely additive
        // (today falls through to Unknown). Mode must be `Sacrificed`.
        let mut ctx = ParseContext::default();
        let (mode, def) = parse_trigger_condition(
            "Whenever an opponent who controls an artifact sacrifices a permanent",
            &mut ctx,
        );
        assert_eq!(mode, TriggerMode::Sacrificed);
        assert_who_controls_clause_only(&def.condition, TypeFilter::Artifact);
    }

    #[test]
    fn who_controls_clause_discard_lifts_condition() {
        // CR 603.4 + CR 701.9: who-controls discard line, purely additive.
        let mut ctx = ParseContext::default();
        let (mode, def) = parse_trigger_condition(
            "Whenever an opponent who controls a creature discards a card",
            &mut ctx,
        );
        assert_eq!(mode, TriggerMode::Discarded);
        assert_who_controls_clause_only(&def.condition, TypeFilter::Creature);
    }

    #[test]
    fn who_controls_clause_mills_lifts_condition() {
        // CR 603.4 + CR 701.17a: who-controls mill line — parsed by
        // `try_parse_event` (ViaEvent), purely additive.
        let mut ctx = ParseContext::default();
        let (mode, def) = parse_trigger_condition(
            "Whenever a player who controls a creature mills a nonland card",
            &mut ctx,
        );
        assert_eq!(mode, TriggerMode::Milled);
        assert_who_controls_clause_only(&def.condition, TypeFilter::Creature);
    }

    #[test]
    fn plain_opponent_cast_unchanged_by_guard() {
        // CR 603.4: Regression — the cast-scan guard must NOT over-fire on a
        // plain (no who-controls) cast line. `valid_target` stays `Opponent`,
        // no who-controls condition is added.
        let mut ctx = ParseContext::default();
        let (mode, def) = parse_trigger_condition("Whenever an opponent casts a spell", &mut ctx);
        assert_eq!(mode, TriggerMode::SpellCast);
        assert_eq!(
            def.valid_target,
            Some(TargetFilter::Typed(
                TypedFilter::default().controller(ControllerRef::Opponent)
            )),
        );
        assert_eq!(def.condition, None);
    }

    #[test]
    fn who_controls_cast_clause_does_not_leak_to_next_trigger_line() {
        // CR 603.4: a who-controls cast line followed by a plain trigger parse
        // independently on the same context — the clause does not leak.
        let mut ctx = ParseContext::default();
        let (_, first) = parse_trigger_condition(
            "Whenever an opponent who controls a creature casts a spell",
            &mut ctx,
        );
        assert_who_controls_clause_only(&first.condition, TypeFilter::Creature);
        let (_, second) = parse_trigger_condition("an opponent draws a card", &mut ctx);
        assert_eq!(
            second.condition, None,
            "second trigger line must not inherit the first line's clause",
        );
    }

    #[test]
    fn trigger_you_may_pay_remains_controller() {
        let def = parse_trigger_line(
            "Whenever you attack, you may pay {2}. If you do, draw a card.",
            "Test Card",
        );

        assert_eq!(def.mode, TriggerMode::YouAttack);
        let execute = def.execute.as_ref().expect("should have execute");
        match &*execute.effect {
            Effect::PayCost {
                payer,
                cost: crate::types::ability::PaymentCost::Mana { cost },
            } => {
                assert_eq!(payer, &TargetFilter::Controller);
                assert_eq!(cost, &crate::types::mana::ManaCost::generic(2));
            }
            other => panic!("expected PayCost, got: {other:?}"),
        }
        assert!(execute.optional, "you may pay should remain optional");
    }

    // -----------------------------------------------------------------------
    // Parts A–E: Station / Saddle / Crew triggers + OnlyDuringYourMainPhase
    // + condition-scoped OncePerTurn sweep.
    // -----------------------------------------------------------------------

    #[test]
    fn monoist_gravliner_stations_trigger_parses() {
        // CR 702.184a: "Whenever a creature stations this Spacecraft, ..."
        let def = parse_trigger_line(
            "Whenever a creature stations this Spacecraft, that creature perpetually gains deathtouch and lifelink.",
            "Monoist Gravliner",
        );
        assert_eq!(def.mode, TriggerMode::Stationed);
    }

    #[test]
    fn another_creature_stations_subject_threading() {
        // valid_source carries the actor subject (pronoun context).
        let def = parse_trigger_line(
            "Whenever another creature stations ~, draw a card.",
            "Test Spacecraft",
        );
        assert_eq!(def.mode, TriggerMode::Stationed);
        // Subject is a Typed(Creature) with FilterProp::Another.
        match &def.valid_source {
            Some(TargetFilter::Typed(tf)) => {
                use crate::types::ability::FilterProp;
                assert!(
                    tf.properties.contains(&FilterProp::Another),
                    "expected FilterProp::Another in subject, got {:?}",
                    tf.properties
                );
            }
            other => panic!("expected Typed subject, got {other:?}"),
        }
    }

    #[test]
    fn burrowfiend_becomes_saddled_parses_with_once_per_turn() {
        // CR 702.171b + Part D: BecomesSaddled mode + OncePerTurn from condition-scoped scan.
        let def = parse_trigger_line(
            "Whenever this creature becomes saddled for the first time each turn, mill two cards.",
            "Stubborn Burrowfiend",
        );
        assert_eq!(def.mode, TriggerMode::BecomesSaddled);
        assert_eq!(def.constraint, Some(TriggerConstraint::OncePerTurn));
    }

    #[test]
    fn gearshift_ace_crews_trigger_parses() {
        // CR 702.122: "Whenever ~ crews a Vehicle, ..."
        let def = parse_trigger_line(
            "Whenever Gearshift Ace crews a Vehicle, that Vehicle gains flying until end of turn.",
            "Gearshift Ace",
        );
        assert_eq!(def.mode, TriggerMode::Crews);
        assert_eq!(def.valid_card, Some(TargetFilter::SelfRef));
    }

    #[test]
    fn canyon_vaulter_compound_trigger_parses() {
        // CR 702.122 + CR 702.171c + CR 505.1: SaddlesOrCrews + OnlyDuringYourMainPhase.
        let def = parse_trigger_line(
            "Whenever Canyon Vaulter saddles a Mount or crews a Vehicle during your main phase, that Mount or Vehicle gains flying until end of turn.",
            "Canyon Vaulter",
        );
        assert_eq!(def.mode, TriggerMode::SaddlesOrCrews);
        assert_eq!(
            def.constraint,
            Some(TriggerConstraint::OnlyDuringYourMainPhase)
        );
    }

    #[test]
    fn saddles_a_mount_singular_parses() {
        // Pre-stage: no card prints this today without the compound; the arm must still fire.
        let def = parse_trigger_line(
            "Whenever ~ saddles a Mount, draw a card.",
            "Hypothetical Saddler",
        );
        assert_eq!(def.mode, TriggerMode::Saddles);
        assert_eq!(def.valid_card, Some(TargetFilter::SelfRef));
    }

    #[test]
    fn first_time_each_turn_in_condition_sets_once_per_turn() {
        // Part D: condition-scoped constraint assignment.
        let def = parse_trigger_line(
            "Whenever ~ attacks for the first time each turn, draw a card.",
            "Godo, Bandit Warlord",
        );
        assert_eq!(def.mode, TriggerMode::Attacks);
        assert_eq!(def.constraint, Some(TriggerConstraint::OncePerTurn));
    }

    #[test]
    fn first_time_each_turn_in_effect_only_does_not_set_constraint() {
        // Part D scope guard: the phrase in EFFECT text alone must not set the constraint.
        // Contrived input — no real card prints this, but the guard is important.
        let def = parse_trigger_line(
            "Whenever ~ attacks, for the first time each turn create a token.",
            "Contrived Card",
        );
        assert_eq!(def.mode, TriggerMode::Attacks);
        assert_eq!(def.constraint, None);
    }

    #[test]
    fn valiant_rescuer_regression() {
        // Part D: the removed hardcoded handler must be replaced by the generic path
        // + condition-scoped OncePerTurn. FilterProp::Another must still be present,
        // and `secondary` must NOT be set (the removed hack is the only writer).
        use crate::types::ability::FilterProp;
        let def = parse_trigger_line(
            "Whenever you cycle another card for the first time each turn, create a 2/2 red Dinosaur creature token.",
            "Valiant Rescuer",
        );
        assert_eq!(def.mode, TriggerMode::Cycled);
        assert_eq!(def.constraint, Some(TriggerConstraint::OncePerTurn));
        assert!(!def.secondary, "removed hack should not set secondary");
        match &def.valid_card {
            Some(TargetFilter::Typed(tf)) => {
                assert!(tf.properties.contains(&FilterProp::Another));
            }
            other => panic!("expected Typed filter with Another prop, got {other:?}"),
        }
    }

    #[test]
    fn aurelia_attacks_first_time_has_constraint() {
        // Regression guard: Aurelia was previously parsed as TriggerMode::Attacks
        // but without the OncePerTurn constraint (latent multi-card bug).
        let def = parse_trigger_line(
            "Whenever Aurelia, the Warleader attacks for the first time each turn, untap all attacking creatures.",
            "Aurelia, the Warleader",
        );
        assert_eq!(def.mode, TriggerMode::Attacks);
        assert_eq!(def.constraint, Some(TriggerConstraint::OncePerTurn));
    }

    #[test]
    fn during_your_main_phase_parser_arm_unit_test() {
        // Isolated: parse_trigger_constraint arm.
        assert_eq!(
            parse_trigger_constraint("whenever ~ attacks during your main phase"),
            Some(TriggerConstraint::OnlyDuringYourMainPhase)
        );
    }

    #[test]
    fn tiana_when_keyword_and_compound_subject_parses() {
        // M9 + M10 guard: Tiana uses "When" (not "Whenever") AND a compound subject
        // "Tiana, Angelic Mechanic or another legendary creature you control".
        // normalize_card_name_refs must replace the full name → ~, and the compound
        // subject parser must produce Or { SelfRef, Typed(Creature, Legendary, You, Another) }.
        let def = parse_trigger_line(
            "When Tiana, Angelic Mechanic or another legendary creature you control crews a Vehicle, that Vehicle perpetually gets +1/+0.",
            "Tiana, Angelic Mechanic",
        );
        assert_eq!(def.mode, TriggerMode::Crews);
        // valid_card must be an Or with both SelfRef and the Typed branch.
        match &def.valid_card {
            Some(TargetFilter::Or { filters }) => {
                let has_self = filters.iter().any(|f| matches!(f, TargetFilter::SelfRef));
                let has_typed_legendary = filters.iter().any(|f| {
                    matches!(
                        f,
                        TargetFilter::Typed(tf)
                        if tf.controller == Some(ControllerRef::You)
                            && tf.properties.contains(&FilterProp::HasSupertype {
                                value: crate::types::card_type::Supertype::Legendary,
                            })
                            && tf.properties.contains(&FilterProp::Another)
                    )
                });
                assert!(
                    has_self && has_typed_legendary,
                    "expected Or{{SelfRef, Typed(Legendary, You, Another)}}, got {filters:?}"
                );
            }
            other => panic!("expected Or filter, got {other:?}"),
        }
    }

    #[test]
    fn mighty_servant_becomes_crewed_parses_with_once_per_turn() {
        // M3 regression: "becomes crewed" was never recognized by parse_simple_event,
        // so Mighty Servant of Leuk-O and Mindlink Mech silently parsed as Unknown
        // despite carrying the OncePerTurn constraint. Part M3 adds the arm.
        let def = parse_trigger_line(
            "Whenever this Vehicle becomes crewed for the first time each turn, draw two cards.",
            "Mighty Servant of Leuk-O",
        );
        assert_eq!(def.mode, TriggerMode::BecomesCrewed);
        assert_eq!(def.constraint, Some(TriggerConstraint::OncePerTurn));
    }

    #[test]
    fn gourmand_talent_two_triggers_both_constrained() {
        // D5 #4: Gourmand's Talent has two separate life-gain triggers. Each must
        // carry OncePerTurn independently; runtime trig_idx (ordinal in the trigger
        // list) keys the OncePerTurn state distinctly, so independent parse →
        // independent runtime tracking.
        let first = parse_trigger_line(
            "Whenever you gain life for the first time each turn, draw a card.",
            "Gourmand's Talent",
        );
        let second = parse_trigger_line(
            "Whenever you gain life for the first time each turn, create a Food token.",
            "Gourmand's Talent",
        );
        assert_eq!(first.constraint, Some(TriggerConstraint::OncePerTurn));
        assert_eq!(second.constraint, Some(TriggerConstraint::OncePerTurn));
    }

    #[test]
    fn stensia_generic_damage_trigger_constrained() {
        // D5 #5 / M8: Stensia's "a creature deals damage to one or more players for
        // the first time each turn" — phrase modifies the EVENT, not per-creature
        // frequency. OncePerTurn keyed on Stensia's (obj_id, trig_idx) is
        // source-level — one firing per turn regardless of which creature triggered.
        let def = parse_trigger_line(
            "Whenever a creature deals damage to one or more players for the first time each turn, put a +1/+1 counter on it.",
            "Stensia, Condemner's Keep",
        );
        assert_eq!(def.constraint, Some(TriggerConstraint::OncePerTurn));
    }

    // SOC Tier 2.6: "Whenever you create one or more creature tokens" —
    // batched token-creation trigger with type + controller filters.
    #[test]
    fn trigger_one_or_more_creature_tokens_created() {
        let def = parse_trigger_line(
            "Whenever you create one or more creature tokens, put a story counter on this artifact.",
            "Staff of the Storyteller",
        );
        assert_eq!(def.mode, TriggerMode::TokenCreated);
        assert!(def.batched);
        assert_eq!(def.valid_target, Some(TargetFilter::Controller));
        assert!(
            def.valid_card.is_some(),
            "creature-type filter should be captured on valid_card"
        );
        assert!(def.execute.is_some());
    }

    #[test]
    fn trigger_one_or_more_tokens_created_bare() {
        let def = parse_trigger_line(
            "Whenever you create one or more tokens, draw a card.",
            "Test Card",
        );
        assert_eq!(def.mode, TriggerMode::TokenCreated);
        assert!(def.batched);
        assert_eq!(def.valid_card, None);
        assert_eq!(def.valid_target, Some(TargetFilter::Controller));
    }

    #[test]
    fn trigger_one_or_more_artifact_tokens_created() {
        let def = parse_trigger_line(
            "Whenever you create one or more artifact tokens, you gain 1 life.",
            "Test Card",
        );
        assert_eq!(def.mode, TriggerMode::TokenCreated);
        assert!(def.batched);
        assert!(def.valid_card.is_some());
    }

    // CR 508.1 + CR 603.2c + CR 603.4: Attacks-with-N-creatures trigger family
    // (Firemane Commando and analogous cards).

    /// Issue #610 — Kratos, Stoic Father. The actor-led count==1 "attack with one
    /// or more <TYPE>" form must populate `valid_card` with the attacker-type
    /// filter (and NO count condition; the matcher's "≥1 matching attacker" IS
    /// "one or more"). Pre-fix this text fell through to the bare "you attack"
    /// fallback, dropping the God filter entirely.
    #[test]
    fn you_attack_with_one_or_more_gods_populates_filter() {
        let def = parse_trigger_line(
            "Whenever you attack with one or more Gods, you get an experience counter.",
            "Kratos, Stoic Father",
        );
        assert_eq!(def.mode, TriggerMode::YouAttack);
        assert!(def.batched);
        // No count condition for the count==1 form.
        assert_eq!(def.condition, None);
        // valid_target carries the controller (You) axis.
        assert_eq!(
            def.valid_target,
            Some(TargetFilter::Typed(
                TypedFilter::default().controller(ControllerRef::You)
            ))
        );
        // valid_card carries the God attacker-type filter (load-bearing fix).
        match &def.valid_card {
            Some(TargetFilter::Typed(tf)) => assert!(
                tf.type_filters
                    .iter()
                    .any(|t| matches!(t, TypeFilter::Subtype(s) if s == "God")),
                "expected God subtype in valid_card, got {:?}",
                tf.type_filters,
            ),
            other => panic!("expected Typed valid_card with God subtype, got {other:?}"),
        }
    }

    /// Issue #610 (Anim Pakal class) — negated subtype head noun. "non-Gnome
    /// creatures" must yield a negated-Gnome filter on `valid_card`. `parse_type_phrase`
    /// already emits the negation; verify it survives onto `valid_card`.
    #[test]
    fn you_attack_with_one_or_more_non_gnome_creatures() {
        let def = parse_trigger_line(
            "Whenever you attack with one or more non-Gnome creatures, create a 1/1 Gnome.",
            "Anim Pakal, Thousandth Moon",
        );
        assert_eq!(def.mode, TriggerMode::YouAttack);
        assert_eq!(def.condition, None);
        match &def.valid_card {
            Some(TargetFilter::Typed(tf)) => assert!(
                tf.type_filters.iter().any(|t| matches!(
                    t,
                    TypeFilter::Non(inner) if matches!(&**inner, TypeFilter::Subtype(s) if s == "Gnome")
                )),
                "expected negated Gnome subtype in valid_card, got {:?}",
                tf.type_filters,
            ),
            other => panic!("expected Typed valid_card with negated Gnome, got {other:?}"),
        }
    }

    /// Issue #610 REV 2 GUARDRAIL (deferral lock). The actor-led count>1 form
    /// ("two or more creatures") must remain byte-identical to pre-fix behavior:
    /// `valid_card` UNSET and the count condition is the unchanged 2-field
    /// `AttackersDeclaredMin { scope, minimum }`. Populating `valid_card` for
    /// count>1 without a condition-level type axis would over-fire. This test
    /// locks the deferral so a future edit cannot silently regress it.
    #[test]
    fn you_attack_with_two_or_more_creatures_defers_filter() {
        let def = parse_trigger_line(
            "Whenever you attack with two or more creatures, draw a card.",
            "Firemane Commando",
        );
        assert_eq!(def.mode, TriggerMode::YouAttack);
        assert_eq!(
            def.valid_card, None,
            "count>1 must NOT set valid_card (REV 2 deferral)"
        );
        assert_eq!(
            def.condition,
            Some(TriggerCondition::AttackersDeclaredMin {
                scope: ControllerRef::You,
                minimum: 2,
            })
        );
    }

    #[test]
    fn trigger_you_attack_with_two_or_more_creatures() {
        let def = parse_trigger_line(
            "Whenever you attack with two or more creatures, draw a card.",
            "Firemane Commando",
        );
        assert_eq!(def.mode, TriggerMode::YouAttack);
        assert!(def.batched);
        assert_eq!(
            def.valid_target,
            Some(TargetFilter::Typed(
                TypedFilter::default().controller(ControllerRef::You)
            ))
        );
        assert_eq!(
            def.condition,
            Some(TriggerCondition::AttackersDeclaredMin {
                scope: ControllerRef::You,
                minimum: 2,
            })
        );
    }

    #[test]
    fn trigger_another_player_attacks_with_two_or_more_creatures_intervening_if() {
        let def = parse_trigger_line(
            "Whenever another player attacks with two or more creatures, they draw a card if none of those creatures attacked you.",
            "Firemane Commando",
        );
        assert_eq!(def.mode, TriggerMode::YouAttack);
        assert!(def.batched);
        assert_eq!(
            def.valid_target,
            Some(TargetFilter::Typed(
                TypedFilter::default().controller(ControllerRef::Opponent)
            ))
        );
        // Composed: batch-size AND none-of-those-attacked-you.
        match &def.condition {
            Some(TriggerCondition::And { conditions }) => {
                assert_eq!(conditions.len(), 2);
                assert!(matches!(
                    &conditions[0],
                    TriggerCondition::AttackersDeclaredMin {
                        scope: ControllerRef::Opponent,
                        minimum: 2,
                    }
                ));
                assert!(matches!(
                    &conditions[1],
                    TriggerCondition::NoneOfAttackersTargetedYou
                ));
            }
            other => panic!(
                "expected And(AttackersDeclaredMin, NoneOfAttackersTargetedYou), got {other:?}"
            ),
        }
        // CR 121.1 + CR 603.7c + CR 608.2k: "they draw a card" — the effect-level
        // subject ("they") must be encoded directly on the Draw target as
        // `TriggeringPlayer`, not via a post-hoc `player_scope` override on the
        // execute ability. The runtime auto-binds `target: TriggeringPlayer`
        // from `state.current_trigger_event` at resolution time
        // (`extract_event_context_filter` in `effects/mod.rs`).
        let execute = def.execute.as_ref().expect("execute");
        match &*execute.effect {
            crate::types::ability::Effect::Draw { target, .. } => {
                assert_eq!(
                    target,
                    &TargetFilter::TriggeringPlayer,
                    "Draw target must be TriggeringPlayer, not Controller"
                );
            }
            other => panic!("expected Draw effect, got {other:?}"),
        }
        assert!(
            execute.player_scope.is_none(),
            "player_scope must be None — the effect-level subject is the single \
             authority for routing. Found {:?}",
            execute.player_scope,
        );
    }

    #[test]
    fn trigger_an_opponent_attacks_with_two_or_more_creatures() {
        let def = parse_trigger_line(
            "Whenever an opponent attacks with two or more creatures, you gain 1 life.",
            "Test Card",
        );
        assert_eq!(def.mode, TriggerMode::YouAttack);
        assert_eq!(
            def.condition,
            Some(TriggerCondition::AttackersDeclaredMin {
                scope: ControllerRef::Opponent,
                minimum: 2,
            })
        );
        assert!(def.batched);
    }

    /// CR 109.4 + CR 115.1 + CR 506.2: Karazikar's first trigger introduces
    /// the attacked player in the condition; "that player controls" inside the
    /// effect must resolve to `ControllerRef::TargetPlayer` so the runtime
    /// auto-surfaces a Player target slot (the attacked player) rather than
    /// defaulting to "you" and offering the trigger controller's own creatures.
    #[test]
    fn karazikar_attack_a_player_uses_target_player_controller() {
        use crate::types::ability::Effect;

        let def = parse_trigger_line(
            "Whenever you attack a player, tap target creature that player controls and goad it.",
            "Karazikar, the Eye Tyrant",
        );
        assert_eq!(def.mode, TriggerMode::YouAttack);
        let execute = def.execute.as_deref().expect("execute ability");
        match execute.effect.as_ref() {
            Effect::Tap { target } => match target {
                TargetFilter::Typed(t) => assert_eq!(
                    t.controller,
                    Some(ControllerRef::TargetPlayer),
                    "tap target should reference attacked player",
                ),
                other => panic!("expected Typed filter, got {other:?}"),
            },
            other => panic!("expected Tap effect, got {other:?}"),
        }
    }

    /// Negative scope test — a non-attack-player trigger ("Whenever you draw a
    /// card") MUST NOT push the relative-player scope, so "that player controls"
    /// inside the effect (synthetic but exercising the parser) still defaults to
    /// `ControllerRef::You`. Guards against accidental scope leakage.
    #[test]
    fn non_attack_player_trigger_does_not_emit_target_player() {
        use crate::types::ability::Effect;

        let def = parse_trigger_line(
            "Whenever you draw a card, tap target creature that player controls.",
            "Test Card",
        );
        let execute = def.execute.as_deref().expect("execute ability");
        // If the parser doesn't classify the synthetic effect, the negative
        // assertion is vacuously satisfied — the karazikar test covers the
        // positive case. If it DOES classify, the controller must remain `You`.
        if let Effect::Tap {
            target: TargetFilter::Typed(t),
        } = execute.effect.as_ref()
        {
            assert_eq!(
                t.controller,
                Some(ControllerRef::You),
                "non-attack-player trigger should not emit TargetPlayer",
            );
        }
    }

    /// CR 107.3a + CR 601.2f: Wan Shi Tong's ETB trigger pays `{X}` at cast
    /// and must put X +1/+1 counters on himself. Verify the pronoun "him"
    /// routes through `resolve_it_pronoun` → `SelfRef` (not `ParentTarget`),
    /// and that `Variable{name:"X"}` is rewritten to `CostXPaid` on both the
    /// primary PutCounter count and the chained Draw's `DivideRounded` inner.
    #[test]
    fn wan_shi_tong_etb_cost_x_and_self_pronoun() {
        let def = parse_trigger_line(
            "When Wan Shi Tong enters, put X +1/+1 counters on him. Then draw half X cards, rounded down.",
            "Wan Shi Tong, Librarian",
        );
        let execute = def.execute.as_ref().expect("execute should exist");
        match execute.effect.as_ref() {
            Effect::PutCounter {
                count,
                target,
                counter_type,
            } => {
                assert_eq!(
                    counter_type,
                    &crate::types::counter::CounterType::Plus1Plus1
                );
                assert_eq!(
                    count,
                    &QuantityExpr::Ref {
                        qty: QuantityRef::CostXPaid
                    },
                    "PutCounter count should be CostXPaid, got {count:?}"
                );
                assert_eq!(
                    target,
                    &TargetFilter::SelfRef,
                    "'on him' should resolve to SelfRef for ETB-self trigger, got {target:?}"
                );
            }
            other => panic!("expected PutCounter, got {other:?}"),
        }
        let sub = execute.sub_ability.as_ref().expect("sub ability");
        match sub.effect.as_ref() {
            Effect::Draw { count, .. } => match count {
                QuantityExpr::DivideRounded { inner, .. } => {
                    assert_eq!(
                        **inner,
                        QuantityExpr::Ref {
                            qty: QuantityRef::CostXPaid
                        },
                        "DivideRounded inner should be CostXPaid, got {inner:?}"
                    );
                }
                other => panic!("expected DivideRounded, got {other:?}"),
            },
            other => panic!("expected Draw, got {other:?}"),
        }
    }

    /// CR 107.3a + CR 601.2f: The Meathook Massacre's ETB trigger applies
    /// -X/-X to each creature. Verify `PtValue::Variable("-X")` is rewritten
    /// to `PtValue::Quantity(Multiply{factor:-1, inner:Ref(CostXPaid)})`
    /// so the runtime pump handler's `PtValue::Quantity` branch evaluates
    /// the X paid at cast time instead of short-circuiting to zero.
    #[test]
    fn meathook_massacre_etb_cost_x_pumpall() {
        let def = parse_trigger_line(
            "When The Meathook Massacre enters, each creature gets -X/-X until end of turn.",
            "The Meathook Massacre",
        );
        let execute = def.execute.as_ref().expect("execute should exist");
        let expected = PtValue::Quantity(QuantityExpr::Multiply {
            factor: -1,
            inner: Box::new(QuantityExpr::Ref {
                qty: QuantityRef::CostXPaid,
            }),
        });
        match execute.effect.as_ref() {
            Effect::PumpAll {
                power, toughness, ..
            } => {
                assert_eq!(
                    power, &expected,
                    "PumpAll power should be Multiply(-1, CostXPaid), got {power:?}"
                );
                assert_eq!(
                    toughness, &expected,
                    "PumpAll toughness should be Multiply(-1, CostXPaid), got {toughness:?}"
                );
            }
            other => panic!("expected PumpAll, got {other:?}"),
        }
    }

    /// Regression: the cost-X rewrite must be scoped to ETB-self triggers.
    /// A dies / attacks / activated-ability trigger that mentions `X` must
    /// NOT be rewritten, because `cost_x_paid` on the dying permanent refers
    /// to a historical cast and the modern trigger body's X has a different
    /// meaning (or no meaning, which falls back to the existing Variable
    /// resolver). We spot-check a non-ETB dies trigger to ensure the
    /// Variable name survives.
    #[test]
    fn cost_x_rewrite_skips_non_etb_triggers() {
        // Pure dies trigger — not ETB, must not be rewritten.
        let def = parse_trigger_line(
            "When ~ dies, put X +1/+1 counters on target creature you control.",
            "Fake Card",
        );
        assert_ne!(
            def.destination,
            Some(Zone::Battlefield),
            "sanity: dies trigger is Graveyard destination, not Battlefield"
        );
        // `trigger_should_rewrite_cost_x` must return false.
        assert!(
            !trigger_should_rewrite_cost_x(&def),
            "dies trigger should NOT have X rewritten to CostXPaid"
        );
    }

    // ── BecomeCopy via triggered ability — CR 707.9 + CR 707.9a ───────────
    //
    // Class: "[Self] becomes a copy of <target>, except <body>" inside a
    // triggered ability body. Building blocks under test: shared
    // `parse_except_clause` (oracle_effect::become_copy_except), the new
    // `RetainPrintedTriggerFromSource` continuous modification (CR 707.9a),
    // and trigger-index threading via `ParseContext::current_trigger_index`.
    //
    // The tests intentionally span multiple cards / phrasings (Irma, plus
    // a synthetic gendered variant) so the building block is exercised
    // across the class — not just the named card.

    #[test]
    fn trigger_become_copy_with_set_name_and_retain_this_ability() {
        // Irma, Part-Time Mutant — the canonical card driving this work.
        // Use the indexed entry point to simulate `parse_oracle_text`'s
        // wiring of `current_trigger_index` (the trigger is the card's first
        // and only printed trigger → index 0).
        let def = parse_trigger_line_with_index(
            "At the beginning of combat on your turn, ~ becomes a copy of up to one other target creature you control, except her name is ~ and she has this ability. Then put a +1/+1 counter on her.",
            "Irma, Part-Time Mutant",
            Some(0),
            &mut ParseContext::default(),
        );
        // Phase + constraint: BoC on your turn.
        assert_eq!(def.mode, TriggerMode::Phase);
        assert_eq!(def.phase, Some(Phase::BeginCombat));
        assert_eq!(def.constraint, Some(TriggerConstraint::OnlyDuringYourTurn));

        let execute = def.execute.as_deref().expect("execute body must parse");
        // Primary effect: BecomeCopy with target = creature you control + Another.
        match execute.effect.as_ref() {
            Effect::BecomeCopy {
                target,
                additional_modifications,
                ..
            } => {
                match target {
                    TargetFilter::Typed(tf) => {
                        assert_eq!(tf.controller, Some(ControllerRef::You));
                        assert!(
                            tf.properties.contains(&FilterProp::Another),
                            "expected Another (other-target) property, got {:?}",
                            tf.properties
                        );
                    }
                    other => panic!("expected Typed creature target, got {other:?}"),
                }
                // Modifications must include SetName and RetainPrintedTriggerFromSource.
                assert!(
                    additional_modifications.iter().any(|m| matches!(
                        m,
                        crate::types::ability::ContinuousModification::SetName { name }
                            if name == "Irma, Part-Time Mutant"
                    )),
                    "expected SetName(Irma, Part-Time Mutant), got {additional_modifications:?}"
                );
                assert!(
                    additional_modifications.iter().any(|m| matches!(
                        m,
                        crate::types::ability::ContinuousModification::RetainPrintedTriggerFromSource {
                            source_trigger_index: 0,
                        }
                    )),
                    "expected RetainPrintedTriggerFromSource(0), got {additional_modifications:?}"
                );
            }
            other => panic!("expected BecomeCopy primary effect, got {other:?}"),
        }
        // Reflexive sub_ability: PutCounter on her (= SelfRef via is_it_pronoun).
        let sub = execute
            .sub_ability
            .as_deref()
            .expect("Then put a +1/+1 counter on her — sub_ability must chain");
        match sub.effect.as_ref() {
            Effect::PutCounter {
                counter_type,
                target,
                ..
            } => {
                assert_eq!(
                    counter_type,
                    &crate::types::counter::CounterType::Plus1Plus1
                );
                assert!(
                    matches!(target, TargetFilter::SelfRef),
                    "+1/+1 counter must land on the post-copy self, got {target:?}"
                );
            }
            other => panic!("expected PutCounter sub_ability, got {other:?}"),
        }
        // Optional targeting: "up to one" must surface optional_targeting=true.
        assert!(
            execute.optional_targeting,
            "up to one target must mark the execute as optional_targeting=true"
        );
    }

    #[test]
    fn trigger_become_copy_he_has_this_ability() {
        // Same class, gendered "he" variant — the building block must accept
        // every pronoun without per-card branching.
        let def = parse_trigger_line_with_index(
            "At the beginning of your upkeep, ~ becomes a copy of target creature you control, except his name is ~ and he has this ability.",
            "Test Mutant",
            Some(0),
            &mut ParseContext::default(),
        );
        let execute = def.execute.as_deref().unwrap();
        match execute.effect.as_ref() {
            Effect::BecomeCopy {
                additional_modifications,
                ..
            } => {
                assert!(additional_modifications.iter().any(|m| matches!(
                    m,
                    crate::types::ability::ContinuousModification::SetName { name }
                        if name == "Test Mutant"
                )));
                assert!(additional_modifications.iter().any(|m| matches!(
                    m,
                    crate::types::ability::ContinuousModification::RetainPrintedTriggerFromSource {
                        source_trigger_index: 0
                    }
                )));
            }
            other => panic!("expected BecomeCopy, got {other:?}"),
        }
    }

    #[test]
    fn trigger_become_copy_it_has_this_ability_neuter() {
        // Class: neuter "it" pronoun in the except clause. The trigger frame
        // here is the simple "[Self] becomes ..." form (Irma family); the
        // alternative "you may have ~ become ..." frame (Cryptoplasm proper)
        // is a distinct grammatical pattern handled by the replacement parser
        // — the building block under test is the except-clause arm regardless
        // of which trigger frame produced the BecomeCopy.
        let def = parse_trigger_line_with_index(
            "At the beginning of your upkeep, ~ becomes a copy of another target creature, except it has this ability.",
            "Test Cloner",
            Some(0),
            &mut ParseContext::default(),
        );
        let execute = def.execute.as_deref().unwrap();
        match execute.effect.as_ref() {
            Effect::BecomeCopy {
                additional_modifications,
                ..
            } => {
                assert!(
                    additional_modifications.iter().any(|m| matches!(
                        m,
                        crate::types::ability::ContinuousModification::RetainPrintedTriggerFromSource { source_trigger_index: 0 }
                    )),
                    "trigger with 'except it has this ability' must emit \
                     RetainPrintedTriggerFromSource(0); got {additional_modifications:?}"
                );
            }
            other => panic!("expected BecomeCopy, got {other:?}"),
        }
    }

    #[test]
    fn trigger_become_copy_without_this_ability_clause_emits_no_retain() {
        // The retain modification must only be emitted when the body contains
        // "<pronoun> has this ability". A copy trigger without that clause
        // must NOT spuriously retain the trigger.
        let def = parse_trigger_line(
            "When ~ enters, ~ becomes a copy of target creature you control.",
            "Vanilla Cloner",
        );
        let execute = def.execute.as_deref().unwrap();
        match execute.effect.as_ref() {
            Effect::BecomeCopy {
                additional_modifications,
                ..
            } => {
                assert!(
                    !additional_modifications.iter().any(|m| matches!(
                        m,
                        crate::types::ability::ContinuousModification::RetainPrintedTriggerFromSource { .. }
                    )),
                    "no 'has this ability' clause → no RetainPrintedTriggerFromSource"
                );
            }
            other => panic!("expected BecomeCopy, got {other:?}"),
        }
    }

    /// CR 208.1 + CR 603.4 + CR 109.3: Selvala, Heart of the Wilds — the ETB
    /// trigger fires unconditionally for every other creature, but the "may
    /// draw a card" effect is gated on the triggering creature's power being
    /// strictly greater than the max power of every other creature.
    /// Regression for the silently-dropped intervening-if condition (#333).
    #[test]
    fn trigger_intervening_if_selvala_power_greater_than_each_other() {
        let def = parse_trigger_line(
            "Whenever another creature enters, its controller may draw a card if its power is greater than each other creature's power.",
            "Selvala, Heart of the Wilds",
        );
        let Some(TriggerCondition::QuantityComparison {
            lhs,
            comparator,
            rhs,
        }) = &def.condition
        else {
            panic!(
                "expected QuantityComparison intervening-if, got {:?}",
                def.condition
            );
        };
        // LHS: the triggering creature's power.
        assert_eq!(*comparator, Comparator::GT);
        assert_eq!(
            *lhs,
            QuantityExpr::Ref {
                qty: QuantityRef::Power {
                    scope: ObjectScope::EventSource,
                },
            }
        );
        // RHS: Max(power) across creatures excluding the triggering object.
        let QuantityExpr::Ref {
            qty:
                QuantityRef::Aggregate {
                    function,
                    property,
                    filter,
                },
        } = rhs
        else {
            panic!("expected Aggregate Max Power rhs, got {rhs:?}");
        };
        assert_eq!(*function, AggregateFunction::Max);
        assert_eq!(*property, crate::types::ability::ObjectProperty::Power);
        let TargetFilter::Typed(tf) = filter else {
            panic!("expected Typed creature filter, got {filter:?}");
        };
        assert_eq!(tf.type_filters, vec![TypeFilter::Creature]);
        assert!(
            tf.properties.contains(&FilterProp::OtherThanTriggerObject),
            "expected OtherThanTriggerObject in aggregate filter, got {:?}",
            tf.properties
        );
        // CR 603.4: the source-referential condition is non-re-homeable, so it
        // hoists to the trigger-level condition — it must NOT also re-home onto
        // the clause-level `execute.condition`.
        let execute = def.execute.expect("Selvala trigger must have an execute");
        assert!(
            execute.condition.is_none(),
            "a hoisted non-re-homeable `if` must not also re-home onto the \
             clause-level condition, got {:?}",
            execute.condition,
        );
    }

    /// Issue #444 — Odric, Lunarch Marshal. The full trigger parses to a
    /// single `GenericEffect` carrying one conditioned `StaticDefinition` per
    /// keyword (first strike + the 11 from the "the same is true for"
    /// sentence). Each `StaticDefinition` grants exactly that keyword and is
    /// gated on `IsPresent` of a creature you control with that keyword.
    #[test]
    fn parse_odric_lunarch_marshal_conditional_keyword_grants() {
        use crate::types::keywords::Keyword;

        let def = parse_trigger_line(
            "At the beginning of combat on your turn, creatures you control gain first \
             strike until end of turn if a creature you control has first strike. The \
             same is true for flying, double strike, deathtouch, haste, hexproof, \
             indestructible, lifelink, menace, reach, trample, and vigilance.",
            "Odric, Lunarch Marshal",
        );
        let execute = def.execute.expect("Odric trigger must have an execute");
        let Effect::GenericEffect {
            static_abilities, ..
        } = &*execute.effect
        else {
            panic!("expected GenericEffect, got {:?}", execute.effect);
        };
        // First strike + 11 "same is true for" keywords = 12 definitions.
        assert_eq!(
            static_abilities.len(),
            12,
            "expected 12 conditioned StaticDefinitions, got {}",
            static_abilities.len()
        );
        // The continuation must NOT remain a separate Unimplemented sub_ability.
        assert!(
            execute.sub_ability.is_none(),
            "the 'same is true for' sentence must fold into static_abilities"
        );

        let expected = [
            Keyword::FirstStrike,
            Keyword::Flying,
            Keyword::DoubleStrike,
            Keyword::Deathtouch,
            Keyword::Haste,
            Keyword::Hexproof,
            Keyword::Indestructible,
            Keyword::Lifelink,
            Keyword::Menace,
            Keyword::Reach,
            Keyword::Trample,
            Keyword::Vigilance,
        ];
        for (sdef, keyword) in static_abilities.iter().zip(expected.iter()) {
            // Grant: one AddKeyword for this keyword.
            assert!(
                sdef.modifications.iter().any(|m| matches!(
                    m,
                    ContinuousModification::AddKeyword { keyword: k } if k == keyword
                )),
                "static def must grant {keyword:?}, mods: {:?}",
                sdef.modifications
            );
            // Gate: IsPresent of a creature with the SAME keyword.
            let Some(crate::types::ability::StaticCondition::IsPresent {
                filter: Some(TargetFilter::Typed(tf)),
            }) = &sdef.condition
            else {
                panic!(
                    "static def for {keyword:?} must be IsPresent-gated, got {:?}",
                    sdef.condition
                );
            };
            assert!(
                tf.properties.iter().any(|p| matches!(
                    p,
                    FilterProp::WithKeyword { value } if value == keyword
                )),
                "gate for {keyword:?} must check WithKeyword({keyword:?}), props: {:?}",
                tf.properties
            );
        }
    }
}

/// Snapshot tests locking current trigger parser output before the IR split.
/// These verify behavioral parity: identical snapshots before and after the
/// `parse_trigger_line_with_index_ir` / `lower_trigger_ir` refactor.
#[cfg(test)]
mod snapshot_tests {
    use super::*;

    fn parse_trigger_line(text: &str, card_name: &str) -> TriggerDefinition {
        parse_trigger_line_with_index(text, card_name, None, &mut ParseContext::default())
    }

    #[test]
    fn trigger_simple_etb_self() {
        let def = parse_trigger_line(
            "When Test Card enters the battlefield, draw a card.",
            "Test Card",
        );
        insta::assert_json_snapshot!(def);
    }

    #[test]
    fn trigger_conditional_upkeep() {
        let def = parse_trigger_line(
            "At the beginning of your upkeep, if you control no creatures, sacrifice Test Card.",
            "Test Card",
        );
        insta::assert_json_snapshot!(def);
    }

    #[test]
    fn trigger_compound_and_when_etb_half() {
        let defs = parse_trigger_lines_at_index(
            "When Test Card enters the battlefield and whenever a creature dies, draw a card.",
            "Test Card",
            None,
            &mut ParseContext::default(),
        );
        assert_eq!(defs.len(), 2, "compound trigger should split into 2");
        insta::assert_json_snapshot!(defs[0]);
    }

    #[test]
    fn trigger_compound_and_when_dies_half() {
        let defs = parse_trigger_lines_at_index(
            "When Test Card enters the battlefield and whenever a creature dies, draw a card.",
            "Test Card",
            None,
            &mut ParseContext::default(),
        );
        assert_eq!(defs.len(), 2, "compound trigger should split into 2");
        insta::assert_json_snapshot!(defs[1]);
    }

    #[test]
    fn trigger_optional_you_may() {
        let def = parse_trigger_line(
            "When Test Card enters the battlefield, you may draw a card.",
            "Test Card",
        );
        insta::assert_json_snapshot!(def);
    }

    #[test]
    fn trigger_once_per_turn() {
        let def = parse_trigger_line(
            "Whenever a creature enters the battlefield under your control for the first time each turn, draw a card.",
            "Test Card",
        );
        insta::assert_json_snapshot!(def);
    }

    #[test]
    fn trigger_unless_pay() {
        let def = parse_trigger_line(
            "At the beginning of your upkeep, sacrifice Test Card unless you pay {2}.",
            "Test Card",
        );
        insta::assert_json_snapshot!(def);
    }

    /// CR 608.2k + CR 603.7c + CR 701.9: Tergrid, God of Fright's discard
    /// branch. The compound trigger splitter produces a `Discarded` trigger
    /// (controller-scoped to `Opponent`), and the top-level
    /// `ChangeZone { target: ParentTarget }` produced by the effect parser
    /// is lifted to `TriggeringSource` by `lower_trigger_ir` so the runtime
    /// resolves "that card" against the just-discarded object.
    #[test]
    fn trigger_discarded_lifts_that_card_to_triggering_source() {
        let def = parse_trigger_line(
            "Whenever an opponent discards a permanent card, you may put that card from a graveyard onto the battlefield under your control.",
            "Tergrid, God of Fright",
        );
        assert_eq!(def.mode, TriggerMode::Discarded);
        let execute = def.execute.as_deref().expect("trigger has execute");
        match execute.effect.as_ref() {
            Effect::ChangeZone {
                origin,
                destination,
                target,
                under_your_control,
                ..
            } => {
                assert_eq!(*origin, Some(Zone::Graveyard));
                assert_eq!(*destination, Zone::Battlefield);
                assert!(matches!(target, TargetFilter::TriggeringSource));
                assert!(*under_your_control);
            }
            other => panic!("expected ChangeZone, got {other:?}"),
        }
    }

    /// CR 608.2k + CR 603.7c: The `ParentTarget` → `TriggeringSource` lift
    /// must descend through chained `sub_ability`s, not just the top-level
    /// effect. A Tergrid-shape trigger like "...exile that card, then
    /// create a token" carries the "that card" anaphor on the FIRST
    /// sub-ability's effect. Without sub_ability descent, the second link
    /// would silently bind to the trigger source instead of the
    /// just-discarded object.
    ///
    /// Synthetic Oracle string: no shipping card today has this exact
    /// shape, but the building block must work for the whole punisher-class.
    #[test]
    fn trigger_discarded_lift_descends_through_sub_ability() {
        let def = parse_trigger_line(
            "Whenever an opponent discards a permanent card, exile that card, then exile that card from a graveyard.",
            "Test Punisher",
        );
        assert_eq!(def.mode, TriggerMode::Discarded);
        let execute = def.execute.as_deref().expect("trigger has execute");

        // Walk the chain: collect every ChangeZone's target filter encountered
        // through the top-level effect and any sub_ability descent. Every
        // `ParentTarget` should have been lifted to `TriggeringSource`.
        let mut targets = Vec::new();
        if let Effect::ChangeZone { target, .. } = execute.effect.as_ref() {
            targets.push(target.clone());
        }
        let mut next = execute.sub_ability.as_deref();
        while let Some(child) = next {
            if let Effect::ChangeZone { target, .. } = child.effect.as_ref() {
                targets.push(target.clone());
            }
            next = child.sub_ability.as_deref();
        }

        assert!(
            !targets.is_empty(),
            "expected at least one ChangeZone in the chain (top + sub_ability), got none"
        );
        for (i, t) in targets.iter().enumerate() {
            assert!(
                !matches!(t, TargetFilter::ParentTarget),
                "chain link {i} still has ParentTarget — sub_ability lift did not descend: {t:?}",
            );
            assert!(
                matches!(t, TargetFilter::TriggeringSource),
                "chain link {i} should be TriggeringSource, got {t:?}",
            );
        }
    }

    // CR 701.3a: "Whenever ~ becomes attached to a creature" trigger
    // Covers Inchblade Companion, Assimilation Aegis, Enormous Energy Blade, Killer Cosplay.
    #[test]
    fn trigger_becomes_attached_to_a_creature() {
        let def = parse_trigger_line(
            "Whenever ~ becomes attached to a creature, that creature gets +1/+1 until end of turn.",
            "Inchblade Companion",
        );
        assert_eq!(
            def.mode,
            TriggerMode::Attached,
            "Expected TriggerMode::Attached, got {:?}",
            def.mode
        );
        assert_eq!(
            def.valid_card,
            Some(TargetFilter::SelfRef),
            "Expected valid_card = SelfRef (Equipment self-reference), got {:?}",
            def.valid_card
        );
        assert_eq!(
            def.valid_target,
            Some(TargetFilter::Typed(TypedFilter::creature())),
            "Expected valid_target = creature, got {:?}",
            def.valid_target
        );
    }

    // CR 701.3a: "becomes attached to a permanent" variant
    #[test]
    fn trigger_becomes_attached_to_a_permanent() {
        let def = parse_trigger_line(
            "Whenever ~ becomes attached to a permanent, draw a card.",
            "Assimilation Aegis",
        );
        assert_eq!(
            def.mode,
            TriggerMode::Attached,
            "Expected TriggerMode::Attached for 'attached to a permanent', got {:?}",
            def.mode
        );
        assert_eq!(
            def.valid_target,
            Some(TargetFilter::Typed(TypedFilter::permanent())),
            "Expected valid_target = permanent, got {:?}",
            def.valid_target
        );
    }

    // Regression: "Whenever ~ becomes attached to a creature" should NOT be TriggerMode::Unknown.
    #[test]
    fn trigger_becomes_attached_not_unknown() {
        let def = parse_trigger_line(
            "Whenever ~ becomes attached to a creature, that creature gets +2/+2 until end of turn.",
            "Enormous Energy Blade",
        );
        assert!(
            !matches!(def.mode, TriggerMode::Unknown(_)),
            "Trigger should not fall through to Unknown; got {:?}",
            def.mode
        );
    }

    // Regression: non-self attached subjects are not supported by
    // EffectResolved's source-only payload and must not parse as a broad
    // TriggerMode::Attached trigger.
    #[test]
    fn trigger_becomes_attached_non_self_subject_stays_unknown() {
        let def = parse_trigger_line(
            "Whenever an Aura you control becomes attached to a creature you control, draw a card.",
            "Siona, Captain of the Pyleas",
        );
        assert!(
            matches!(def.mode, TriggerMode::Unknown(_)),
            "non-self attachment trigger should stay Unknown, got {:?}",
            def.mode
        );
    }

    /// CR 701.43a: "Whenever you exert a creature" — actor-side exert trigger.
    /// Trueheart Twins, Battlefield Scavenger, Rohirrim Chargers, Vizier of the
    /// True, and Resolute Survivors all share this exact trigger phrasing.
    /// The parser must emit TriggerMode::Exerted with valid_card set to the
    /// subject (a creature the controller exerts, i.e. a generic creature filter
    /// scoped to the controller).
    #[test]
    fn trigger_you_exert_a_creature() {
        let def = parse_trigger_line(
            "Whenever you exert a creature, that creature gets +1/+0 and gains first strike until end of turn.",
            "Trueheart Twins",
        );
        assert_eq!(def.mode, TriggerMode::Exerted);
        assert_eq!(def.valid_target, Some(TargetFilter::Controller));
        let Some(TargetFilter::Typed(tf)) = def.valid_card else {
            panic!(
                "expected typed exerted creature filter, got {:?}",
                def.valid_card
            );
        };
        assert_eq!(tf.type_filters, vec![TypeFilter::Creature]);
    }

    /// CR 701.43a: Self-reference is the exerted permanent, not a degraded
    /// generic filter.
    #[test]
    fn trigger_you_exert_self_ref() {
        let def = parse_trigger_line("Whenever you exert ~, untap it.", "Vizier of the True");
        assert_eq!(def.mode, TriggerMode::Exerted);
        assert_eq!(def.valid_target, Some(TargetFilter::Controller));
        assert_eq!(def.valid_card, Some(TargetFilter::SelfRef));
    }

    /// CR 701.43a: Opponent actor scope must be recorded on valid_target while
    /// the exerted creature remains the valid_card filter.
    #[test]
    fn trigger_opponent_exerts_a_creature() {
        let def = parse_trigger_line(
            "Whenever an opponent exerts a creature, you gain 1 life.",
            "Exertion Watcher",
        );
        assert_eq!(def.mode, TriggerMode::Exerted);
        assert_eq!(
            def.valid_target,
            Some(TargetFilter::Typed(
                TypedFilter::default().controller(ControllerRef::Opponent)
            ))
        );
        let Some(TargetFilter::Typed(tf)) = def.valid_card else {
            panic!(
                "expected typed exerted creature filter, got {:?}",
                def.valid_card
            );
        };
        assert_eq!(tf.type_filters, vec![TypeFilter::Creature]);
    }

    /// CR 701.43a: Resolute Survivors — "Whenever you exert a creature" with a
    /// life-gain effect. Ensures the trigger body is parsed independently of
    /// the trigger condition.
    #[test]
    fn trigger_exert_resolute_survivors() {
        let def = parse_trigger_line(
            "Whenever you exert a creature, you gain 1 life.",
            "Resolute Survivors",
        );
        assert_eq!(def.mode, TriggerMode::Exerted);
        assert!(def.valid_card.is_some());
    }
}
