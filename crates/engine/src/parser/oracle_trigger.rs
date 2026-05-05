use nom::branch::alt;
use nom::bytes::complete::{tag, take_until};
use nom::combinator::{recognize, value};
use nom::multi::many1;
use nom::sequence::{delimited, pair, preceded, terminated};
use nom::Parser;
use nom_language::error::VerboseError;

use super::oracle_effect::{lower_effect_chain_ir, parse_effect_chain_ir};
use super::oracle_ir::context::ParseContext;
use super::oracle_ir::trigger::{TriggerBody, TriggerIr, TriggerModifiers};
use super::oracle_nom::condition::parse_inner_condition;
use super::oracle_nom::error::OracleResult;
use super::oracle_nom::primitives::{
    self as nom_primitives, scan_contains, scan_preceded, scan_split_at_phrase,
};
use super::oracle_nom::quantity as nom_quantity;
use super::oracle_target::{parse_type_phrase, starts_with_type_word};
use super::oracle_util::{
    canonicalize_subtype_name, is_core_type_name, is_non_subtype_subject_name, merge_or_filters,
    normalize_card_name_refs, parse_number, parse_ordinal, parse_subtype, strip_after,
    strip_reminder_text, TextPair, SELF_REF_PARSE_ONLY_PHRASES,
};
use crate::parser::oracle_ir::diagnostic::OracleDiagnostic;
use crate::types::ability::{
    AbilityKind, AttachmentKind, CastVariantPaid, Comparator, ControllerRef, CounterTriggerFilter,
    DamageKindFilter, FilterProp, PlayerFilter, QuantityExpr, QuantityRef, StaticCondition,
    TargetFilter, TriggerCondition, TriggerConstraint, TriggerDefinition, TypeFilter, TypedFilter,
    UnlessCost, UnlessPayModifier,
};
use crate::types::card_type::CoreType;
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
    value(
        (),
        pair(
            alt((
                tag::<_, _, VerboseError<&str>>("that player "),
                tag("that opponent "),
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
        | Effect::IncreaseSpeed { amount, .. }
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
        scan_preceded(&lower, |i| tag::<_, _, VerboseError<&str>>(PHRASE).parse(i))
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

    fn parse_actor(input: &str) -> Result<(&str, ()), nom::Err<VerboseError<&str>>> {
        alt((
            value((), tag::<_, _, VerboseError<&str>>("you ")),
            value((), tag("an opponent ")),
            value((), tag("a player ")),
            value((), tag("another player ")),
        ))
        .parse(input)
    }

    fn parse_attack_verb(input: &str) -> Result<(&str, ()), nom::Err<VerboseError<&str>>> {
        alt((
            value((), tag::<_, _, VerboseError<&str>>("attack ")),
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
    fn parse_damage_phrase(input: &str) -> Result<(&str, ()), nom::Err<VerboseError<&str>>> {
        alt((
            value(
                (),
                tag::<_, _, VerboseError<&str>>("deals combat damage to "),
            ),
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
                if tag::<_, _, VerboseError<&str>>("a player")
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
            if tag::<_, _, VerboseError<&str>>("a player")
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
    let (effect_without_if, if_condition) = extract_if_condition(&effect_lower);

    // CR 609.3: "You may" at the start of the effect text makes the triggered
    // effect optional at resolution.
    let starts_with_you_may =
        |s: &str| tag::<_, _, VerboseError<&str>>("you may ").parse(s).is_ok();
    let after_structural_if = effect_lower
        .strip_prefix("if ") // allow-noncombinator: structural if-clause skip when condition is unrecognized
        .and_then(|rest| rest.split_once(", "))
        .map(|(_cond, body)| body);
    let optional = starts_with_you_may(effect_lower.as_str())
        || starts_with_you_may(effect_without_if.trim_start())
        || after_structural_if.is_some_and(starts_with_you_may);

    // Strip constraint sentences so they don't leak into effect parsing as sub-abilities
    let effect_final = strip_constraint_sentences(&effect_without_if);

    // CR 118.12: Detect "unless [player] pays {cost}" in effect text.
    let (effect_for_parse, unless_pay) = extract_unless_pay_modifier(&effect_final);

    // CR 608.2k: Extract trigger subject for pronoun resolution in effect text.
    let trigger_subject = extract_trigger_subject_for_context(condition_text, ctx);
    let mut effect_ctx = ParseContext {
        subject: Some(trigger_subject.clone()),
        card_name: Some(card_name.to_string()),
        current_trigger_index,
        ..Default::default()
    };

    // CR 109.4 + CR 115.1 + CR 506.2: Set relative-player scope for
    // TargetPlayer resolution inside the trigger effect body.
    let cond_lower = condition_text.to_lowercase();
    if condition_introduces_target_player(&cond_lower) {
        effect_ctx.relative_player_scope = Some(ControllerRef::TargetPlayer);
    }

    // Parse the effect body
    let has_up_to = scan_contains(&effect_for_parse, "up to one");
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
        } else {
            let ir = parse_effect_chain_ir(&effect_for_parse, AbilityKind::Spell, &mut effect_ctx);
            Some(TriggerBody::EffectChain(ir))
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

    def
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
        tag::<_, _, VerboseError<&str>>("this ability triggers only the first ").parse(i)
    }) {
        let start = prefix_text.len();
        if let Ok((_, (_, after_each_turn))) =
            nom_primitives::split_once_on(matched_start, "each turn")
        {
            let end_pos = lower.len() - after_each_turn.len();
            let end_pos = if tag::<_, _, VerboseError<&str>>(".")
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
fn extract_unless_pay_modifier(text: &str) -> (String, Option<UnlessPayModifier>) {
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
    let primary_is_discard = tag::<_, _, VerboseError<&str>>("discard ")
        .parse(lower[..unless_pos].trim_start())
        .is_ok();
    if primary_is_discard
        && tag::<_, _, VerboseError<&str>>("you discard ")
            .parse(after_unless)
            .is_ok()
    {
        return (text.to_string(), None);
    }

    // CR 118.12 + CR 608.2c + CR 119.4: Non-mana alternative costs ("you discard
    // a card", "you sacrifice a [filter]", "you pay N life") map to the existing
    // `UnlessCost::{DiscardCard, Sacrifice, PayLife}` variants — the runtime
    // resolver in `engine_payment_choices.rs` already handles all four via
    // `WaitingFor::WardDiscardChoice` / `WaitingFor::WardSacrificeChoice`.
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

    // Parse payer + payment verb as a single combinator: "(payer) pay(s) " → (TargetFilter, &str).
    let payer_result: Result<(&str, TargetFilter), _> = alt((
        value(
            TargetFilter::Controller,
            tag::<_, _, VerboseError<&str>>("you pay "),
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

    let (cost_str, payer) = match payer_result {
        Ok((rest, p)) => (rest, p),
        Err(_) => {
            // No recognized payment pattern — strip the unless clause so the effect parses.
            let cleaned = text[..unless_pos].trim().to_string();
            return (cleaned, None);
        }
    };

    // Extract cost symbols
    let cost_end = cost_str
        .find(|c: char| c != '{' && c != '}' && !c.is_alphanumeric())
        .unwrap_or(cost_str.len());
    let cost_text = cost_str[..cost_end].trim();

    if cost_text.is_empty() || !cost_text.contains('{') {
        return (text.to_string(), None);
    }

    // Determine the cost type
    let cost = if cost_text == "{x}" || cost_text == "{X}" {
        // Check for "where X is" clause
        let remainder = &cost_str[cost_end..];
        if let Some(quantity) = parse_where_x_is_trigger(remainder) {
            UnlessCost::DynamicGeneric { quantity }
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
        UnlessCost::Fixed { cost: mana_cost }
    };

    // Payer was already determined by the combinator above.

    // Strip the unless clause from the effect text
    let cleaned = text[..unless_pos].trim().to_string();

    (cleaned, Some(UnlessPayModifier { cost, payer }))
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
/// This is a known sub-fidelity gap (Balduvian Horde class). Parameterizing
/// `UnlessCost::DiscardCard` with a `random` axis is deferred pending the
/// `UnlessCost`/`AbilityCost` unification refactor.
fn parse_unless_alt_cost(after_unless: &str) -> Option<UnlessCost> {
    // "you discard a card" — match prefix, accept any trailing modifiers
    // ("at random", trailing punctuation) since the caller strips the entire
    // unless-clause wholesale.
    if tag::<_, _, VerboseError<&str>>("you discard a card")
        .parse(after_unless)
        .is_ok()
    {
        return Some(UnlessCost::DiscardCard);
    }

    // "you pay N life" / "you pay N life." — life amount is bare integer.
    if let Ok((rest, _)) = tag::<_, _, VerboseError<&str>>("you pay ").parse(after_unless) {
        if let Some((amount, after_num)) = parse_number(rest) {
            if tag::<_, _, VerboseError<&str>>("life")
                .parse(after_num.trim_start())
                .is_ok()
            {
                return Some(UnlessCost::PayLife {
                    amount: amount as i32,
                });
            }
        }
    }

    // "you sacrifice [count] [filter]" — delegates filter parsing to the shared
    // `parse_target` building block (oracle_target). Count is optional and
    // defaults to 1; articles ("a"/"an") are absorbed by `parse_target` via
    // its "target {phrase}" entry point.
    if let Ok((rest, _)) = tag::<_, _, VerboseError<&str>>("you sacrifice ").parse(after_unless) {
        return parse_unless_sacrifice_filter(rest);
    }

    // CR 118.12: "you return [count] [filter] [you control] to its/their owner's hand"
    if let Ok((rest, _)) = tag::<_, _, VerboseError<&str>>("you return ").parse(after_unless) {
        return parse_unless_return_to_hand(rest);
    }

    None
}

/// Parse the tail of "you sacrifice ..." into an `UnlessCost::Sacrifice`.
/// Expects lowercased text. Accepts:
/// - `a creature` / `an artifact` / `a [type] you control`
/// - `two creatures` / `three lands`
/// - terminal sentence punctuation
fn parse_unless_sacrifice_filter(rest: &str) -> Option<UnlessCost> {
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
        let stripped = alt((tag::<_, _, VerboseError<&str>>("a "), tag("an ")))
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

    Some(UnlessCost::Sacrifice { count, filter })
}

/// CR 118.12: Parse "you return [count] [filter] [you control] to its/their
/// owner's hand" into `UnlessCost::ReturnToHand`. Expects lowercased text after
/// "you return ". Handles patterns like:
/// - "an artifact you control to its owner's hand"
/// - "two forests you control to their owner's hand"
/// - "another creature you control to its owner's hand"
/// - "an untapped island you control to its owner's hand"
/// - "a non-lair land you control to its owner's hand"
fn parse_unless_return_to_hand(rest: &str) -> Option<UnlessCost> {
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

    Some(UnlessCost::ReturnToHand {
        count,
        filter,
        from_zone,
    })
}

/// Parse "where X is ~'s power" / "where X is this creature's power" etc.
/// Delegates to `nom_quantity::parse_quantity_ref` for the value reference after
/// stripping the "where X is" prefix.
fn parse_where_x_is_trigger(text: &str) -> Option<QuantityExpr> {
    let trimmed = text.trim().trim_start_matches(',').trim();
    let (rest, ()) = alt((
        value((), tag::<_, _, VerboseError<&str>>("where x is ")),
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

/// CR 603.4: Rewrite `Another` inside any `ObjectCount` / `ObjectCountDistinctNames`
/// filter carried by a `QuantityExpr`. Leaves non-object-count refs untouched.
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
            qty: QuantityRef::ObjectCountDistinctNames { filter },
        } => QuantityExpr::Ref {
            qty: QuantityRef::ObjectCountDistinctNames {
                filter: substitute_another_in_filter(filter),
            },
        },
        QuantityExpr::Offset { inner, offset } => QuantityExpr::Offset {
            inner: Box::new(substitute_another_in_expr(inner)),
            offset: *offset,
        },
        QuantityExpr::HalfRounded { inner, rounding } => QuantityExpr::HalfRounded {
            inner: Box::new(substitute_another_in_expr(inner)),
            rounding: *rounding,
        },
        QuantityExpr::Multiply { factor, inner } => QuantityExpr::Multiply {
            factor: *factor,
            inner: Box::new(substitute_another_in_expr(inner)),
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
        StaticCondition::DuringYourTurn => Some(TriggerCondition::DuringYourTurn),
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
                condition: Box::new(TriggerCondition::DuringYourTurn),
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
            // CR 611.2b: Not(SourceIsTapped) → source is untapped.
            StaticCondition::SourceIsTapped => Some(TriggerCondition::Not {
                condition: Box::new(TriggerCondition::SourceIsTapped),
            }),
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
        // CR 702.131a: City's Blessing bridges directly.
        StaticCondition::HasCityBlessing => Some(TriggerCondition::HasCityBlessing),
        // CR 611.2b: Source tapped state bridges for trigger conditions like
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
        | StaticCondition::SourceMatchesFilter { .. }
        | StaticCondition::DefendingPlayerControls { .. }
        | StaticCondition::SourceAttackingAlone
        | StaticCondition::SourceIsAttacking
        | StaticCondition::SourceIsBlocking
        | StaticCondition::SourceIsBlocked
        | StaticCondition::SourceIsEquipped
        | StaticCondition::SourceIsMonstrous
        | StaticCondition::SourceAttachedToCreature
        | StaticCondition::OpponentPoisonAtLeast { .. }
        | StaticCondition::UnlessPay { .. }
        | StaticCondition::Unrecognized { .. }
        | StaticCondition::EnchantedIsFaceDown
        | StaticCondition::SourceControllerEquals { .. }
        | StaticCondition::None => None,

        // CR 309.7: Dungeon completion bridges directly.
        StaticCondition::CompletedADungeon => {
            Some(TriggerCondition::CompletedDungeon { specific: None })
        }

        // CR 903.3: Commander control bridges directly.
        StaticCondition::ControlsCommander => Some(TriggerCondition::ControlsCommander),
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
        tag::<_, _, VerboseError<&str>>("unless it escaped").parse(i)
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
        fn was_type_combinator(i: &str) -> nom::IResult<&str, CoreType, VerboseError<&str>> {
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

    // CR 509.1a + CR 603.4: "if defending player controls no [type]"
    // Nom combinator prefix dispatch + parse_type_phrase for the remainder.
    {
        fn def_prefix(i: &str) -> nom::IResult<&str, (), VerboseError<&str>> {
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

    // CR 603.4: Both `if` and trailing `unless` introduce intervening predicates.
    // `unless` is the negation of `if` — wrap the parsed predicate in `Not`.
    // Cost-form `unless` ("unless you pay {2}", "unless you sacrifice a creature")
    // is already stripped upstream by `extract_unless_pay_modifier`.
    if let Some(result) = try_extract_intervening(&tp, &lower, text, "if ", |c| c) {
        return result;
    }
    if let Some(result) =
        try_extract_intervening(&tp, &lower, text, " unless ", |c| TriggerCondition::Not {
            condition: Box::new(c),
        })
    {
        return result;
    }

    // CR 603.4 + CR 603.10a + CR 509.1g: Death-trigger look-back condition.
    // The subject is the dying event object, not the source in its current zone.
    if let Some((prefix, negated, rest)) = scan_preceded(&lower, |i| {
        preceded(
            tag::<_, _, VerboseError<&str>>("if it "),
            alt((
                value(
                    true,
                    alt((
                        tag::<_, _, VerboseError<&str>>("wasn't blocking"),
                        tag("was not blocking"),
                    )),
                ),
                value(false, tag("was blocking")),
            )),
        )
        .parse(i)
    }) {
        let filter =
            TargetFilter::Typed(TypedFilter::creature().properties(vec![FilterProp::Blocking]));
        let condition = TriggerCondition::ZoneChangeObjectMatchesFilter {
            origin: Some(Zone::Battlefield),
            destination: Zone::Graveyard,
            filter,
        };
        let condition = if negated {
            TriggerCondition::Not {
                condition: Box::new(condition),
            }
        } else {
            condition
        };
        let consumed = lower.len() - prefix.len() - rest.len();
        return (
            strip_condition_clause(text, prefix.len(), consumed),
            Some(condition),
        );
    }

    (text.to_string(), None)
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

/// Try to extract an intervening predicate introduced by `keyword`.
///
/// Runs `parse_inner_condition` on the fragment after `keyword`, accepts only
/// if it stops at a clause boundary with no dangling `otherwise` branch (which
/// would change the semantics from intervening-if to a conditional override
/// pair), then bridges to `TriggerCondition` via `static_condition_to_trigger_condition`
/// and applies `wrap`. Used for both `if X` (wrap = identity) and
/// `unless X` (wrap = `Not`).
fn try_extract_intervening(
    tp: &TextPair<'_>,
    lower: &str,
    text: &str,
    keyword: &str,
    wrap: impl FnOnce(TriggerCondition) -> TriggerCondition,
) -> Option<(String, Option<TriggerCondition>)> {
    let pos = tp.find(keyword)?;
    let cond_fragment = &lower[pos + keyword.len()..];
    let (rest, sc) = parse_inner_condition(cond_fragment).ok()?;
    let rest_trimmed = rest.trim();
    let after_dots = rest_trimmed.trim_start_matches('.').trim_start();
    let has_otherwise = tag::<_, _, VerboseError<&str>>("otherwise")
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

/// CR 702.49a: Parse "whenever you activate a ninjutsu ability" trigger.
/// Matches all ninjutsu-family activation patterns.
fn try_parse_ninjutsu_activation_trigger(lower: &str) -> Option<(TriggerMode, TriggerDefinition)> {
    for prefix in ["whenever you activate ", "when you activate "] {
        let Ok((rest, ())) = value((), tag::<_, _, VerboseError<&str>>(prefix)).parse(lower) else {
            continue;
        };
        // CR 702.49a: Match "a ninjutsu ability" — covers the ninjutsu-family keyword
        if tag::<_, _, VerboseError<&str>>("a ninjutsu ability")
            .parse(rest)
            .is_ok()
        {
            let mut def = make_base();
            def.mode = TriggerMode::NinjutsuActivated;
            return Some((TriggerMode::NinjutsuActivated, def));
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
    let (rest, counter_type_text) = take_until::<_, _, VerboseError<&str>>(" counter on it")
        .parse(after)
        .ok()?;
    let (rest, _) = tag::<_, _, VerboseError<&str>>(" counter on it")
        .parse(rest)
        .ok()?;
    let clause_len = prefix.len() + (after.len() - rest.len());
    Some((
        strip_condition_clause(text, pos, clause_len),
        Some(TriggerCondition::HadCounters {
            counter_type: Some(counter_type_text.to_string()),
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
        value(ManaColor::White, tag::<_, _, VerboseError<&str>>("white")),
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
    if alt((tag::<_, _, VerboseError<&str>>("then, "), tag("then ")))
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
        tag::<_, _, VerboseError<&str>>("then,"),
        tag::<_, _, VerboseError<&str>>("then"),
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
    fn is_event_verb_start(text: &str) -> bool {
        alt((
            value((), tag::<_, _, VerboseError<&str>>("dies")),
            value((), tag("die ")),
            value((), tag("deals ")),
            value((), tag("deal ")),
            value((), tag("enters")),
            value((), tag("enter ")),
            value((), tag("attacks")),
            value((), tag("attack ")),
            value((), tag("blocks")),
            value((), tag("block ")),
            value((), tag("is sacrificed")),
            value((), tag("are sacrificed")),
            value((), tag("is exiled")),
            value((), tag("are exiled")),
            value((), tag("leaves")),
            value((), tag("is put into")),
        ))
        .parse(text)
        .is_ok()
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
        value((), tag::<_, _, VerboseError<&str>>("whenever ")).parse(cond_lower)
    {
        ("Whenever", rest)
    } else if let Ok((rest, ())) =
        value((), tag::<_, _, VerboseError<&str>>("when ")).parse(cond_lower)
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
        alt((
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
            tag("is sacrificed"),
            tag("are sacrificed"),
            tag("is exiled"),
            tag("are exiled"),
            tag("leaves"),
            tag("is put into"),
        ))
        .parse(i)
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
        if !continues_player_action_list(after) {
            return Some(comma_pos);
        }
        search_start = comma_pos + 2;
    }
    None
}

fn continues_player_action_list(after_comma: &str) -> bool {
    let trimmed = after_comma.trim_start();
    let candidate = value((), tag::<_, _, VerboseError<&str>>("or "))
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
        value((), tag::<_, _, VerboseError<&str>>("or ")),
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

    // --- Subject + event decomposition ---
    // Strip leading "when"/"whenever" using nom alt()
    let after_keyword = alt((
        value((), tag::<_, _, VerboseError<&str>>("whenever ")),
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
        return (mode, def);
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
/// Destroy targeting "that player controls") and Ruthless Winnower
/// (Sacrifice with `TargetPlayer`-scoped filter) both trigger the companion
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
        value((), tag::<_, _, VerboseError<&str>>("whenever ")),
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
        tag::<_, _, VerboseError<&str>>("another player "),
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
        value((), tag::<_, _, VerboseError<&str>>("and/or ")),
        value((), tag::<_, _, VerboseError<&str>>("or ")),
    ))
    .parse(rest_trimmed)
    {
        let (second, final_rest) = parse_trigger_subject(after_sep, ctx);
        return (merge_or_filters(first, second), final_rest);
    }

    (first, rest)
}

/// Parse a single (non-compound) trigger subject.
fn parse_single_subject<'a>(text: &'a str, ctx: &mut ParseContext) -> (TargetFilter, &'a str) {
    // Self-reference: "~"
    if let Ok((rest, ())) = value((), tag::<_, _, VerboseError<&str>>("~ ")).parse(text) {
        return (TargetFilter::SelfRef, rest);
    }
    if text == "~" {
        return (TargetFilter::SelfRef, "");
    }

    if let Ok((rest, ())) = value((), tag::<_, _, VerboseError<&str>>("this ")).parse(text) {
        let noun_end = rest.find(' ').unwrap_or(rest.len());
        if noun_end > 0 {
            return (TargetFilter::SelfRef, rest[noun_end..].trim_start());
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
    if let Ok((after_another, ())) =
        value((), tag::<_, _, VerboseError<&str>>("another ")).parse(text)
    {
        let (filter, rest) = parse_type_phrase(after_another);
        let with_another = add_another_prop(filter);
        return (with_another, rest);
    }

    if let Ok((after_quantifier, ())) =
        value((), tag::<_, _, VerboseError<&str>>("one or more ")).parse(text)
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
        value((), tag::<_, _, VerboseError<&str>>("you put one or more ")).parse(text)
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
            alt((
                tag::<_, _, VerboseError<&str>>("an opponent"),
                tag("opponent"),
            )),
        ),
        value(
            TargetFilter::Typed(TypedFilter::default().controller(ControllerRef::Opponent)),
            tag("each opponent"),
        ),
        value(TargetFilter::Player, alt((tag("a player"), tag("player")))),
    ))
    .parse(text)
    {
        return (filter, rest.trim_start());
    }

    // "a "/"an " + type phrase (general subject)
    if let Ok((after, ())) = alt((
        value((), tag::<_, _, VerboseError<&str>>("a ")),
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

// ---------------------------------------------------------------------------
// Event verb parsing: matches the event after the subject
// ---------------------------------------------------------------------------

/// Parse the "to <target>" qualifier that follows a damage verb.
/// Returns a `TargetFilter` for the three common cases:
/// - "to a player"         → `Player`
/// - "to an opponent"      → opponent-controlled TypedFilter
/// - "to you"              → `Controller`
///
/// Other qualifiers (e.g. "to a player or planeswalker") are left as `None`
/// so the trigger fires for any target, matching current behaviour.
fn parse_damage_to_qualifier(after_verb: &str) -> Option<TargetFilter> {
    let (rest, ()) = value((), tag::<_, _, VerboseError<&str>>("to "))
        .parse(after_verb.trim_start())
        .ok()?;
    // Use nom alt() to match damage target qualifiers (input already lowercase)
    fn parse_damage_target(input: &str) -> OracleResult<'_, TargetFilter> {
        alt((
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

/// CR 603.6a + CR 611.2b: After consuming the `"enter"` prefix in a ChangesZone
/// trigger clause, recognize an optional tapped-state rider — `"enters tapped"`
/// or `"enters untapped"` — and produce the corresponding intervening-if
/// condition so the trigger only fires when the source's post-ETB tapped state
/// matches.
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
        tag::<_, _, VerboseError<&str>>("s "),
        alt((
            value(true, tag::<_, _, VerboseError<&str>>("untapped")),
            value(false, tag::<_, _, VerboseError<&str>>("tapped")),
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
            condition: Box::new(TriggerCondition::SourceIsTapped),
        }
    } else {
        TriggerCondition::SourceIsTapped
    })
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
    if tag::<_, _, VerboseError<&str>>("enters or attacks")
        .parse(rest)
        .is_ok()
        || tag::<_, _, VerboseError<&str>>("enters the battlefield or attacks")
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
    if tag::<_, _, VerboseError<&str>>("attacks or blocks")
        .parse(rest)
        .is_ok()
    {
        let mut def = make_base();
        def.mode = TriggerMode::AttacksOrBlocks;
        def.valid_card = Some(subject.clone());
        return Some((TriggerMode::AttacksOrBlocks, def));
    }

    // "enters [the battlefield]" / "enter [the battlefield]" (plural for "one or more" subjects)
    if let Ok((after_enter, ())) = value((), tag::<_, _, VerboseError<&str>>("enter")).parse(rest) {
        let mut def = make_base();
        def.mode = TriggerMode::ChangesZone;
        def.destination = Some(Zone::Battlefield);
        def.valid_card = Some(subject.clone());

        // CR 702.49c: "enters from your hand" — set origin zone.
        let rest_lower = rest.to_lowercase();
        if scan_contains(&rest_lower, "from your hand") {
            def.origin = Some(Zone::Hand);
        }

        // CR 603.6a + CR 611.2b: "enters untapped" / "enters tapped" — conditional
        // ETB trigger gated on the source's tapped state at resolution time. The
        // tapped-state check examines the object after ETB replacement effects
        // (e.g. "enters tapped unless you control three or more Forests") have
        // resolved, per CR 603.6a's "check at the moment the event fires". The
        // `SourceIsTapped` runtime evaluator (game/triggers.rs) inspects
        // `obj.tapped` on the triggering object, which by then reflects the
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
        value((), tag::<_, _, VerboseError<&str>>("deals combat damage")),
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
        value((), tag::<_, _, VerboseError<&str>>("deals damage")),
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
        value((), tag::<_, _, VerboseError<&str>>("and at least ")),
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
    let attacks_result = tag::<_, _, VerboseError<&str>>("attacks")
        .parse(rest)
        .map(|(r, _)| r)
        .ok()
        .or_else(|| {
            tag::<_, _, VerboseError<&str>>("attack")
                .parse(rest)
                .ok()
                .map(|(r, _)| r)
                .filter(|r| !r.starts_with("er") && !r.starts_with("ing"))
        });
    if let Some(after) = attacks_result {
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
        def.mode = TriggerMode::Attacks;
        def.valid_card = Some(subject.clone());
        def.attack_target_filter = attack_target_filter;
        if matches!(
            def.attack_target_filter,
            Some(AttackTargetFilter::PlayerOrPlaneswalker) | Some(AttackTargetFilter::Player)
        ) && tag::<_, _, VerboseError<&str>>(" you").parse(after).is_ok()
        {
            def.valid_target = Some(TargetFilter::Controller);
        }
        return Some((TriggerMode::Attacks, def));
    }

    // "blocks" — fires for the blocking creature.
    if tag::<_, _, VerboseError<&str>>("blocks")
        .parse(rest)
        .is_ok()
    {
        let mut def = make_base();
        def.mode = TriggerMode::Blocks;
        def.valid_card = Some(subject.clone());
        return Some((TriggerMode::Blocks, def));
    }

    // "leaves the battlefield" / "leaves"
    if alt((
        value(
            (),
            tag::<_, _, VerboseError<&str>>("leaves the battlefield"),
        ),
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
        value((), tag::<_, _, VerboseError<&str>>("is exiled")),
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
        value((), tag::<_, _, VerboseError<&str>>("is sacrificed")),
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
        value((), tag::<_, _, VerboseError<&str>>("is destroyed")),
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

    // CR 701.14: "fights" / "fight" — fight trigger
    // Guard against false-matching "fighting".
    {
        let fights_result = tag::<_, _, VerboseError<&str>>("fights")
            .parse(rest)
            .map(|(r, _)| r)
            .ok()
            .or_else(|| {
                tag::<_, _, VerboseError<&str>>("fight")
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
        )))
        .parse(input)
    }
    if let Ok((_, event)) = parse_simple_event.parse(rest) {
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
        }
        return Some((def.mode.clone(), def));
    }

    // Counter-related events: "a +1/+1 counter is put on ~" / "one or more counters are put on ~"
    if let Some(result) = try_parse_counter_trigger(full_lower) {
        return Some(result);
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
    if let Ok((_, mode)) = parse_life_verb.parse(rest) {
        let mut def = make_base();
        def.mode = mode.clone();
        def.valid_target = Some(subject.clone());
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
    if parse_draws_card.parse(rest).is_ok() {
        let mut def = make_base();
        def.mode = TriggerMode::Drawn;
        def.valid_target = Some(subject.clone());
        // CR 121.1 + CR 504.1 + CR 603.4: Detect Orcish Bowmasters' "except the
        // first one [you|they] draw in each of [your|their] draw steps" clause
        // (shape-compatible with Alhammarret's Archive's replacement variant —
        // shared combinator in `oracle_replacement`). When present, gate the
        // trigger so it does NOT fire on the active player's first draw of
        // the draw step.
        if super::oracle_replacement::has_except_first_draw_in_draw_step_clause(rest)
            || super::oracle_replacement::has_except_first_draw_in_draw_step_clause(full_lower)
        {
            def.condition = Some(TriggerCondition::ExceptFirstDrawInDrawStep);
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

    if matches!(
        lower,
        "when you unlock this door" | "whenever you unlock this door"
    ) {
        let mut def = make_base();
        def.mode = TriggerMode::UnlockDoor;
        return Some((TriggerMode::UnlockDoor, def));
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
        if tag::<_, _, VerboseError<&str>>(prefix).parse(lower).is_ok() {
            let mut def = make_base();
            def.mode = TriggerMode::Attacks;
            // AttachedTo here references the player the aura is attached to
            def.valid_target = Some(TargetFilter::AttachedTo);
            return Some((TriggerMode::Attacks, def));
        }
    }

    for prefix in ["whenever you cast or copy ", "when you cast or copy "] {
        if let Ok((rest, ())) = value((), tag::<_, _, VerboseError<&str>>(prefix)).parse(lower) {
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
        if tag::<_, _, VerboseError<&str>>(prefix).parse(lower).is_ok() {
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
        let Ok((rest, ())) = value((), tag::<_, _, VerboseError<&str>>(prefix)).parse(lower) else {
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
        value((), tag::<_, _, VerboseError<&str>>("whenever ")),
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
            tag::<_, _, VerboseError<&str>>("you attack"),
        ),
        value(ControllerRef::Opponent, tag("another player attacks")),
        value(ControllerRef::Opponent, tag("an opponent attacks")),
    ))
    .parse(after_prefix)
    .ok()?;

    // Required " with " separator.
    let (after_with, ()) = value((), tag::<_, _, VerboseError<&str>>(" with "))
        .parse(after_actor)
        .ok()?;

    // Parse "N or more creatures" — N is a positive integer word/digit.
    let (after_n, n) = nom_primitives::parse_number.parse(after_with).ok()?;
    let (after_or_more, ()) = value((), tag::<_, _, VerboseError<&str>>(" or more creatures"))
        .parse(after_n)
        .ok()?;
    // Accept optional trailing " each turn" / " this turn" qualifier (unused here,
    // but keeps the matcher permissive for CR 603.4 timing qualifiers). Must end
    // at the condition boundary — the caller already split the effect text off,
    // so `after_or_more` should be empty or punctuation-only.
    let (rest, _) = opt(alt((
        tag::<_, _, VerboseError<&str>>(" each turn"),
        tag(" this turn"),
    )))
    .parse(after_or_more)
    .ok()?;
    if !rest.trim().is_empty() {
        return None;
    }

    if n < 1 {
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
        let Ok((rest, ())) = value((), tag::<_, _, VerboseError<&str>>(prefix)).parse(lower) else {
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
            tag::<_, _, VerboseError<&str>>("whenever you create one or more "),
        ),
        value(
            (),
            tag::<_, _, VerboseError<&str>>("when you create one or more "),
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
        let Ok((rest, ())) = value((), tag::<_, _, VerboseError<&str>>(prefix)).parse(lower) else {
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
fn parse_your_zone_token(input: &str) -> nom::IResult<&str, Zone, VerboseError<&str>> {
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
fn parse_disjunctive_zone_set(input: &str) -> nom::IResult<&str, Vec<Zone>, VerboseError<&str>> {
    let (input, first) = parse_your_zone_token(input)?;
    // Optional second zone joined by "and/or" (canonical), "or", or "and".
    let rest_parser = |i| -> nom::IResult<&str, Zone, VerboseError<&str>> {
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
        let Ok((rest, ())) = value((), tag::<_, _, VerboseError<&str>>(prefix)).parse(lower) else {
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
        let Ok((rest, ())) = value((), tag::<_, _, VerboseError<&str>>(prefix)).parse(lower) else {
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
        let Ok((rest, ())) = value((), tag::<_, _, VerboseError<&str>>(prefix)).parse(lower) else {
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
        value((), tag::<_, _, VerboseError<&str>>("whenever a ")),
        value((), tag("whenever an ")),
        value((), tag("when a ")),
        value((), tag("when an ")),
    ))
    .parse(lower)
    .ok()?;

    let (after_subtype, subtype_text) =
        take_until::<_, _, VerboseError<&str>>(" you control enters")
            .parse(after_prefix)
            .ok()?;

    let (after_sentinel, ()) = value((), tag::<_, _, VerboseError<&str>>(" you control enters"))
        .parse(after_subtype)
        .ok()?;

    // Accept either bare "enters" or "enters the battlefield".
    let (tail, ()) = alt((
        value((), tag::<_, _, VerboseError<&str>>(" the battlefield")),
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
        let Ok((rest, ())) = value((), tag::<_, _, VerboseError<&str>>(prefix)).parse(lower) else {
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
        let Ok((rest, ())) = value((), tag::<_, _, VerboseError<&str>>(prefix)).parse(lower) else {
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
        value((), tag::<_, _, VerboseError<&str>>("at end of combat")),
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
            value((), tag::<_, _, VerboseError<&str>>("on your turn")),
            value((), tag("on each of your turns")),
        ))
        .parse(rest)
        .is_ok()
        {
            def.constraint = Some(TriggerConstraint::OnlyDuringYourTurn);
        }
        return Some((TriggerMode::Phase, def));
    }

    let (stripped, ()) = value((), tag::<_, _, VerboseError<&str>>("at the beginning of"))
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
    if let Some(result) = try_parse_ninjutsu_activation_trigger(lower) {
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
        if let Ok((rest, ())) = value((), tag::<_, _, VerboseError<&str>>(prefix)).parse(lower) {
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
        if let Ok((rest, ())) = value((), tag::<_, _, VerboseError<&str>>(prefix)).parse(lower) {
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
        let Ok((rest, ())) = value((), tag::<_, _, VerboseError<&str>>(prefix)).parse(lower) else {
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
        def.valid_card = Some(filter);
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
                if actor_controller.is_some() {
                    // Constrain subject to opponent-controlled permanents.
                    // Mirrors `set_opponent_controller` pattern used elsewhere
                    // (effects/mod.rs DamageAll).
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
                    set_opponent_controller(&mut filter);
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

            // First, try the post-spell-modifier-aware decomposition for shapes
            // that include "with {X} in its mana cost" etc.
            if let Some(filter) = parse_spell_qualifier_payload(payload) {
                def.valid_card = Some(filter);
                return Some((TriggerMode::SpellCast, def));
            }

            // Fall back to the classic type-phrase parser for bare type filters.
            // TypeFilter::Card alone means "spell" with no type restriction — skip it.
            let (filter, _rest) = parse_type_phrase(after);
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
        let after_article = value((), tag::<_, _, VerboseError<&str>>("n ")) // "an" → strip the trailing "n "
            .parse(after_casts)
            .map(|(rest, _)| rest)
            .unwrap_or(after_casts)
            .trim_start();
        let spell_clause = nom_primitives::split_once_on(after_article, ", ")
            .map(|(_, (before, _))| before)
            .unwrap_or(after_article);
        // Handle "with mana value equal to the chosen number" (Talion, the Kindly Lord)
        // CR 202.3: Mana value comparison against a dynamic reference quantity.
        if let Some(rest) = spell_clause
            .strip_suffix("with mana value equal to the chosen number")
            .or_else(|| spell_clause.strip_suffix("with mana value equal to that number"))
        {
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
            def.valid_card = Some(TargetFilter::Typed(
                TypedFilter::default().properties(vec![FilterProp::Multicolored]),
            ));
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

    if scan_contains(lower, "you draw a card") {
        // CR 121.1 + CR 603.2: "Whenever you draw a card" — scope to the trigger's
        // controller. Without this filter, `match_drawn` would fire for all players'
        // draws (Sheoldred's first trigger misfires on opponent draws).
        let mut def = make_base();
        def.mode = TriggerMode::Drawn;
        def.valid_target = Some(TargetFilter::Controller);
        return Some((TriggerMode::Drawn, def));
    }

    // "whenever you attack" — player-centric attack trigger
    if scan_contains(lower, "whenever you attack") || scan_contains(lower, "when you attack") {
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

    // "when you cast this spell" — self-cast trigger (fires from stack)
    if scan_contains(lower, "when you cast this spell") || scan_contains(lower, "when ~ is cast") {
        let mut def = make_base();
        def.mode = TriggerMode::SpellCast;
        def.valid_card = Some(TargetFilter::SelfRef);
        // Cast triggers fire while the spell is on the stack
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
        let Ok((rest, ())) = value((), tag::<_, _, VerboseError<&str>>(prefix)).parse(lower) else {
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

/// Timing-clause kind for nth-spell triggers.
/// CR 601.2 + CR 603.4: The trailing "each turn" / "in a turn" (unrestricted
/// timing) vs "during each opponent's turn" (restricted to opponent's turn).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NthSpellTimingKind {
    EachTurn,
    DuringOpponentsTurn,
}

/// Inspect the text after the ordinal to determine the timing clause kind.
/// Returns `None` when the text does not end with a recognized timing tail
/// (so the caller can reject the pattern). Uses `str::strip_suffix` —
/// structural suffix removal of fixed literals, not parser dispatch.
fn classify_nth_spell_timing(rest: &str) -> Option<NthSpellTimingKind> {
    let trimmed = rest.trim();
    if trimmed.strip_suffix(" each turn").is_some() || trimmed.strip_suffix(" in a turn").is_some()
    {
        Some(NthSpellTimingKind::EachTurn)
    } else if trimmed
        .strip_suffix(" during each opponent's turn")
        .or_else(|| trimmed.strip_suffix(" during each opponent\u{2019}s turn"))
        .is_some()
    {
        Some(NthSpellTimingKind::DuringOpponentsTurn)
    } else {
        None
    }
}

/// "you cast your <ordinal> [qualifier] spell [post-spell modifier] each turn"
/// Also handles "during each opponent's turn" variant (CR 601.2).
fn try_parse_nth_spell_you(lower: &str) -> Option<(TriggerMode, TriggerDefinition)> {
    let prefix = "you cast your ";
    let after = strip_after(lower, prefix)?;
    let (n, rest) = parse_ordinal(after)?;
    let timing = classify_nth_spell_timing(rest)?;
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
    if timing == NthSpellTimingKind::DuringOpponentsTurn {
        def.condition = Some(TriggerCondition::DuringOpponentsTurn);
    }
    Some((TriggerMode::SpellCast, def))
}

/// "an opponent casts their <ordinal> [qualifier] spell [post-spell modifier] each turn"
fn try_parse_nth_spell_opponent(lower: &str) -> Option<(TriggerMode, TriggerDefinition)> {
    let prefix = "an opponent casts their ";
    let after = strip_after(lower, prefix)?;
    let (n, rest) = parse_ordinal(after)?;
    // Opponents-path does not support "during each opponent's turn" (redundant wording).
    if !matches!(
        classify_nth_spell_timing(rest),
        Some(NthSpellTimingKind::EachTurn)
    ) {
        return None;
    }
    let filter = extract_spell_type_filter(rest);
    let mut def = make_base();
    def.mode = TriggerMode::SpellCast;
    def.valid_target = Some(TargetFilter::Typed(
        TypedFilter::default().controller(ControllerRef::Opponent),
    ));
    def.constraint = Some(TriggerConstraint::NthSpellThisTurn { n, filter });
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
    if !matches!(
        classify_nth_spell_timing(rest),
        Some(NthSpellTimingKind::EachTurn)
    ) {
        return None;
    }
    let filter = extract_spell_type_filter(rest);
    let mut def = make_base();
    def.mode = TriggerMode::SpellCast;
    def.constraint = Some(TriggerConstraint::NthSpellThisTurn { n, filter });
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
    // Strip the trailing timing clause to isolate the qualifier payload (the words
    // between the ordinal and the timing tail). Uses `str::strip_suffix` on literal
    // timing tails — not parser dispatch, structural suffix removal.
    let qualifier = trimmed
        .strip_suffix(" each turn")
        .or_else(|| trimmed.strip_suffix(" in a turn"))
        .or_else(|| trimmed.strip_suffix(" during each opponent's turn"))
        .or_else(|| trimmed.strip_suffix(" during each opponent\u{2019}s turn"))?;
    parse_spell_qualifier_payload(qualifier.trim())
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
            tag::<_, _, VerboseError<&str>>("with {x} in its mana cost"),
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
    // (static N, X-variable, EventContextSourceManaValue) is supported here for
    // free alongside the search filter and target filter call sites.
    if let Some((prop, consumed)) = super::oracle_target::parse_mana_value_suffix(modifier) {
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
    if alt((
        value((), tag::<_, _, VerboseError<&str>>("card each turn")),
        value((), tag("card in a turn")),
    ))
    .parse(rest)
    .is_ok()
    {
        let mut def = make_base();
        def.mode = TriggerMode::Drawn;
        def.valid_target = Some(TargetFilter::Typed(
            TypedFilter::default().controller(ControllerRef::You),
        ));
        def.constraint = Some(TriggerConstraint::NthDrawThisTurn { n });
        return Some((TriggerMode::Drawn, def));
    }
    None
}

/// "an opponent draws their <ordinal> card each turn"
fn try_parse_nth_draw_opponent(lower: &str) -> Option<(TriggerMode, TriggerDefinition)> {
    let prefix = "an opponent draws their ";
    let after = strip_after(lower, prefix)?;
    let (n, rest) = parse_ordinal(after)?;
    if alt((
        value((), tag::<_, _, VerboseError<&str>>("card each turn")),
        value((), tag("card in a turn")),
    ))
    .parse(rest)
    .is_ok()
    {
        let mut def = make_base();
        def.mode = TriggerMode::Drawn;
        def.valid_target = Some(TargetFilter::Typed(
            TypedFilter::default().controller(ControllerRef::Opponent),
        ));
        def.constraint = Some(TriggerConstraint::NthDrawThisTurn { n });
        return Some((TriggerMode::Drawn, def));
    }
    None
}

/// "a player draws their <ordinal> card each turn"
/// CR 121.2: No valid_target filter — fires for any player's draw.
fn try_parse_nth_draw_any_player(lower: &str) -> Option<(TriggerMode, TriggerDefinition)> {
    let prefix = "a player draws their ";
    let after = strip_after(lower, prefix)?;
    let (n, rest) = parse_ordinal(after)?;
    if alt((
        value((), tag::<_, _, VerboseError<&str>>("card each turn")),
        value((), tag("card in a turn")),
    ))
    .parse(rest)
    .is_ok()
    {
        let mut def = make_base();
        def.mode = TriggerMode::Drawn;
        def.constraint = Some(TriggerConstraint::NthDrawThisTurn { n });
        return Some((TriggerMode::Drawn, def));
    }
    None
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
    let (counter_prefix, counter_start) = scan_split_at_phrase(lower, |i| {
        tag::<_, _, VerboseError<&str>>("counter").parse(i)
    })?;
    let Ok((_, (_, subject_text))) = nom_primitives::split_once_on(counter_start, " on ") else {
        return None;
    };
    let subject_text = subject_text.trim();

    let mut def = make_base();
    def.mode = TriggerMode::CounterAdded;
    if let Some(filter) = parse_counter_threshold_prefix(counter_prefix) {
        def = def.counter_filter(filter);
    }

    // Parse the subject after "on "
    if tag::<_, _, VerboseError<&str>>("~")
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
    let (rest, _) = alt((tag::<_, _, VerboseError<&str>>("when "), tag("whenever ")))
        .parse(prefix.trim_start())
        .ok()?;
    let (rest, _) = tag::<_, _, VerboseError<&str>>("the ")
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

/// CR 122.1: Parse "a [type] counter is removed from [subject]" patterns.
/// Also handles zone constraints like "while it's exiled" (e.g. suspend cards).
fn try_parse_counter_removed(lower: &str) -> Option<(TriggerMode, TriggerDefinition)> {
    // Pattern: "a [type] counter is removed from [subject] [while ...]"
    let (after_a, ()) = alt((
        value((), tag::<_, _, VerboseError<&str>>("whenever a ")),
        value((), tag("when a ")),
    ))
    .parse(lower)
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

/// CR 700.4: Parse "is/are put into [possessive] graveyard [from zone]" patterns.
/// Handles all forms:
/// - "is put into a graveyard from anywhere" (no origin restriction)
/// - "is put into a graveyard from the battlefield" (equivalent to "dies")
/// - "is put into your graveyard [from your library]" (controller filter + optional origin)
/// - "is put into an opponent's graveyard from anywhere" (opponent controller filter)
/// - "are put into your graveyard from your library" (plural form for batched triggers)
fn try_parse_put_into_graveyard(
    subject: &TargetFilter,
    rest: &str,
) -> Option<(TriggerMode, TriggerDefinition)> {
    // Match the verb prefix: "is put into " or "are put into "
    let (after_verb, ()) = alt((
        value((), tag::<_, _, VerboseError<&str>>("is put into ")),
        value((), tag("are put into ")),
    ))
    .parse(rest)
    .ok()?;

    // Parse the graveyard possessive: "a graveyard", "your graveyard", "an opponent's graveyard"
    fn parse_graveyard_possessive(input: &str) -> OracleResult<'_, Option<TargetFilter>> {
        alt((
            value(None, tag("a graveyard")),
            value(
                Some(TargetFilter::Typed(
                    TypedFilter::default().controller(ControllerRef::You),
                )),
                tag("your graveyard"),
            ),
            value(
                Some(TargetFilter::Typed(
                    TypedFilter::default().controller(ControllerRef::Opponent),
                )),
                tag("an opponent's graveyard"),
            ),
        ))
        .parse(input)
    }
    let (after_gy, valid_target) = parse_graveyard_possessive.parse(after_verb).ok()?;

    // Parse optional "from [zone]" clause
    let after_gy = after_gy.trim_start();
    let origin = if let Ok((after_from, ())) =
        value((), tag::<_, _, VerboseError<&str>>("from ")).parse(after_gy)
    {
        let after_from = after_from.trim_start();
        // Use nom alt() for origin zone matching
        fn parse_origin_zone(input: &str) -> OracleResult<'_, Option<Zone>> {
            alt((
                value(Some(Zone::Battlefield), tag("the battlefield")),
                // CR 700.4: "from anywhere" means no origin restriction
                value(None, tag("anywhere")),
                value(Some(Zone::Library), tag("your library")),
                value(Some(Zone::Hand), tag("your hand")),
            ))
            .parse(input)
        }
        parse_origin_zone
            .parse(after_from)
            .ok()
            .map(|(_, z)| z)
            .unwrap_or(None)
    } else {
        // No "from" clause -- no origin restriction (any zone to graveyard)
        None
    };

    let mut def = make_base();
    def.mode = TriggerMode::ChangesZone;
    def.destination = Some(Zone::Graveyard);
    def.origin = origin;
    def.valid_card = Some(subject.clone());
    def.valid_target = valid_target;
    Some((TriggerMode::ChangesZone, def))
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
        value((), tag::<_, _, VerboseError<&str>>("is put into ")),
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
        value((), tag::<_, _, VerboseError<&str>>("from ")).parse(after_hand)
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
        value((), tag::<_, _, VerboseError<&str>>("is put into exile")),
        value((), tag("are put into exile")),
    ))
    .parse(rest)
    .ok()?;

    let after_verb = after_verb.trim_start();
    let origin = if let Ok((after_from, ())) =
        value((), tag::<_, _, VerboseError<&str>>("from ")).parse(after_verb)
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
        let Ok((rest, ())) = value((), tag::<_, _, VerboseError<&str>>(prefix)).parse(lower) else {
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

        // Parse the graveyard possessive using nom alt()
        // Reuse the same combinator as try_parse_put_into_graveyard
        fn parse_gy_possessive_batch(input: &str) -> OracleResult<'_, Option<TargetFilter>> {
            alt((
                value(None, tag("a graveyard")),
                value(
                    Some(TargetFilter::Typed(
                        TypedFilter::default().controller(ControllerRef::You),
                    )),
                    tag("your graveyard"),
                ),
                value(
                    Some(TargetFilter::Typed(
                        TypedFilter::default().controller(ControllerRef::Opponent),
                    )),
                    tag("an opponent's graveyard"),
                ),
            ))
            .parse(input)
        }
        let Ok((after_gy, valid_target)) = parse_gy_possessive_batch.parse(after_put) else {
            continue;
        };

        // Parse optional "from [zone]" clause using nom
        let after_gy = after_gy.trim_start();
        let origin = if let Ok((after_from, ())) =
            value((), tag::<_, _, VerboseError<&str>>("from ")).parse(after_gy)
        {
            let after_from = after_from.trim_start();
            fn parse_origin_zone_batch(input: &str) -> OracleResult<'_, Option<Zone>> {
                alt((
                    value(Some(Zone::Battlefield), tag("the battlefield")),
                    value(None, tag("anywhere")),
                    value(Some(Zone::Library), tag("your library")),
                ))
                .parse(input)
            }
            parse_origin_zone_batch
                .parse(after_from)
                .ok()
                .map(|(_, z)| z)
                .unwrap_or(None)
        } else {
            None
        };

        // Parse the subject type filter: "creature cards", "land cards", "cards"
        let filter = if subject_text == "cards" {
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

        let mut def = make_base();
        def.mode = TriggerMode::ChangesZoneAll;
        def.destination = Some(Zone::Graveyard);
        def.origin = origin;
        def.valid_card = filter;
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
        let Ok((rest, ())) = value((), tag::<_, _, VerboseError<&str>>(prefix)).parse(lower) else {
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
            value((), tag::<_, _, VerboseError<&str>>("from ")).parse(after_lib)
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
        value((), tag::<_, _, VerboseError<&str>>("whenever ")),
        value((), tag("when ")),
    ))
    .parse(lower)
    .ok()?;

    // CR 603.2c: Batched discard triggers — "one or more" fire once per batch.
    if tag::<_, _, VerboseError<&str>>("you discard one or more")
        .parse(event)
        .is_ok()
    {
        let mut def = make_base();
        def.mode = TriggerMode::DiscardedAll;
        def.valid_target = Some(TargetFilter::Controller);
        def.batched = true;
        return Some((TriggerMode::DiscardedAll, def));
    }
    if tag::<_, _, VerboseError<&str>>("one or more players discard one or more")
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
        value((), tag::<_, _, VerboseError<&str>>("whenever ")),
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
    let turn_constraint = tag::<_, _, VerboseError<&str>>("during ")
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
fn parse_phase_keyword(input: &str) -> nom::IResult<&str, Phase, VerboseError<&str>> {
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
        tag::<_, _, VerboseError<&str>>("each opponent's "),
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
    if tag::<_, _, VerboseError<&str>>("your ")
        .parse(phase_text)
        .is_ok()
    {
        return Some(TriggerConstraint::OnlyDuringYourTurn);
    }
    // Suffix-based: "combat on your turn", "each combat on your turn"
    let mut remaining = phase_text;
    while !remaining.is_empty() {
        if tag::<_, _, VerboseError<&str>>("on your turn")
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
        if let Ok((rest, ())) = value((), tag::<_, _, VerboseError<&str>>("other ")).parse(text) {
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
                        tag::<_, _, VerboseError<&str>>("~"),
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
        AbilityCondition, AbilityKind, Comparator, ControllerRef, DamageModification, Duration,
        Effect, FilterProp, PlayerFilter, PlayerScope, PtValue, QuantityExpr, QuantityRef,
        TargetFilter, TypeFilter, TypedFilter, UnlessCost,
    };
    use crate::types::counter::{CounterMatch, CounterType};
    use crate::types::replacements::ReplacementEvent;

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

    // CR 603.6a + CR 611.2b: "When this land enters untapped, ..." — Gingerbread
    // Cabin class. The trigger must carry `Not { Box::new(SourceIsTapped) }` so
    // it only fires when the ETB-tapped replacement did NOT apply.
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
                condition: Box::new(TriggerCondition::SourceIsTapped)
            })
        );
        assert!(def.execute.is_some());
    }

    // CR 603.6a + CR 611.2b: "Whenever a permanent you control enters tapped, ..." —
    // Amulet of Vigor class. The `enters tapped` rider must set
    // `SourceIsTapped` (fires only when entering tapped).
    #[test]
    fn trigger_etb_subject_enters_tapped_attaches_condition() {
        let def = parse_trigger_line(
            "Whenever a permanent you control enters tapped, untap it.",
            "Amulet of Vigor",
        );
        assert_eq!(def.mode, TriggerMode::ChangesZone);
        assert_eq!(def.destination, Some(Zone::Battlefield));
        assert_eq!(def.condition, Some(TriggerCondition::SourceIsTapped));
    }

    // Guard: a bare "enters" (no tapped-state rider) must NOT attach a
    // SourceIsTapped condition.
    #[test]
    fn trigger_etb_bare_enters_has_no_tapped_condition() {
        let def = parse_trigger_line(
            "When this creature enters, draw a card.",
            "Elvish Visionary",
        );
        assert_eq!(def.mode, TriggerMode::ChangesZone);
        assert!(
            !matches!(def.condition, Some(TriggerCondition::SourceIsTapped))
                && !matches!(
                    &def.condition,
                    Some(TriggerCondition::Not { condition })
                        if matches!(**condition, TriggerCondition::SourceIsTapped)
                ),
            "bare `enters` must not attach SourceIsTapped; got {:?}",
            def.condition
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
                condition: Box::new(TriggerCondition::SourceIsTapped)
            })
        );
        assert_eq!(
            parse_enters_tapped_state_rider("s untapped "),
            Some(TriggerCondition::Not {
                condition: Box::new(TriggerCondition::SourceIsTapped)
            })
        );
        assert_eq!(
            parse_enters_tapped_state_rider("s tapped,"),
            Some(TriggerCondition::SourceIsTapped)
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

    #[test]
    fn trigger_haliya_end_step_with_life_condition() {
        let def = parse_trigger_line(
            "At the beginning of your end step, draw a card if you've gained 3 or more life this turn.",
            "Haliya, Guided by Light",
        );
        assert_eq!(def.mode, TriggerMode::Phase);
        assert_eq!(def.phase, Some(Phase::End));
        assert_eq!(
            def.condition,
            Some(TriggerCondition::QuantityComparison {
                lhs: QuantityExpr::Ref {
                    qty: QuantityRef::LifeGainedThisTurn {
                        player: PlayerScope::Controller,
                    },
                },
                comparator: Comparator::GE,
                rhs: QuantityExpr::Fixed { value: 3 },
            })
        );
        // Effect should be just "draw a card" with condition stripped
        assert!(def.execute.is_some());
    }

    #[test]
    fn trigger_if_gained_life_no_number() {
        let def = parse_trigger_line(
            "At the beginning of your end step, create a Blood token if you gained life this turn.",
            "Some Card",
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
    fn extract_if_strips_condition_from_effect() {
        let (cleaned, cond) =
            extract_if_condition("draw a card if you've gained 3 or more life this turn.");
        assert_eq!(cleaned, "draw a card");
        assert_eq!(
            cond,
            Some(TriggerCondition::QuantityComparison {
                lhs: QuantityExpr::Ref {
                    qty: QuantityRef::LifeGainedThisTurn {
                        player: PlayerScope::Controller,
                    },
                },
                comparator: Comparator::GE,
                rhs: QuantityExpr::Fixed { value: 3 },
            })
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
        // Sub-ability effect is LoseLife targeting TriggeringPlayer with HalfRounded amount.
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
                        QuantityExpr::HalfRounded {
                            rounding: RoundingMode::Up,
                            ..
                        }
                    ),
                    "amount must be HalfRounded(Up), got {amount:?}",
                );
            }
            other => panic!("sub_ability effect must be LoseLife, got {other:?}"),
        }
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
            Some(TargetFilter::Typed(
                TypedFilter::default().properties(vec![FilterProp::Multicolored])
            ))
        );
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
    /// `QuantityComparison` against `OpponentDiscardedCardThisTurn`. Regression
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
                qty: QuantityRef::OpponentDiscardedCardThisTurn,
            }
        );
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
        assert_eq!(
            def.valid_card,
            Some(TargetFilter::Typed(TypedFilter::new(TypeFilter::Land)))
        );
        assert_eq!(def.valid_target, Some(TargetFilter::Controller));
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
        // CR 603.2 + CR 106.3: "you tap a land" arm must NOT carry ControllerRef::Opponent.
        let def = parse_trigger_line(
            "Whenever you tap a land for mana, add one mana of any type that land produced.",
            "Vorinclex, Voice of Hunger",
        );
        assert_eq!(def.mode, TriggerMode::TapsForMana);
        assert_eq!(def.valid_target, Some(TargetFilter::Controller));
        match def.valid_card {
            Some(TargetFilter::Typed(ref tf)) => {
                assert_eq!(tf.type_filters, vec![TypeFilter::Land]);
                assert_eq!(tf.controller, None);
            }
            other => panic!("expected Typed(Land) with no controller, got {other:?}"),
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
            UnlessCost::DynamicGeneric {
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
            matches!(unless_pay.cost, UnlessCost::Fixed { .. }),
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
            matches!(unless_pay.cost, UnlessCost::DiscardCard),
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
            UnlessCost::Sacrifice { count, filter } => {
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
            matches!(unless_pay.cost, UnlessCost::PayLife { amount: 1 }),
            "cost should be PayLife {{ amount: 1 }}, got {:?}",
            unless_pay.cost
        );
    }

    #[test]
    fn trigger_unless_you_discard_typed_does_not_match() {
        // Drekavac — "unless you discard a noncreature card". UnlessCost::DiscardCard
        // doesn't carry filter info, so we deliberately fall through (preserves
        // rules-correctness; player-chosen any-card discard would violate the
        // "noncreature card" restriction). Coverage waits, architecture wins.
        let def = parse_trigger_line(
            "When ~ enters, sacrifice it unless you discard a noncreature card.",
            "Drekavac",
        );
        assert!(
            def.unless_pay.is_none(),
            "typed discard should fall through (UnlessCost::DiscardCard has no filter), got {:?}",
            def.unless_pay
        );
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
            UnlessCost::ReturnToHand {
                count,
                filter,
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
            UnlessCost::ReturnToHand { count, filter, .. } => {
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
            UnlessCost::ReturnToHand {
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
            UnlessCost::ReturnToHand {
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
            UnlessCost::ReturnToHand {
                count,
                from_zone,
                filter,
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
        assert_eq!(
            def.valid_target,
            Some(TargetFilter::Typed(
                TypedFilter::default().controller(ControllerRef::You)
            ))
        );
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
        // Subject filter should be creature type
        if let Some(TargetFilter::Typed(tf)) = &def.valid_card {
            assert!(
                tf.type_filters.contains(&TypeFilter::Creature),
                "Expected Creature in type_filters, got {:?}",
                tf.type_filters
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
        // "cards" with no type restriction should have no valid_card filter
        assert_eq!(def.valid_card, None);
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
                assert_eq!(counter_type, "charge");
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
        assert_eq!(def.condition, Some(TriggerCondition::DuringOpponentsTurn));
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

    #[test]
    fn extract_if_you_have_n_or_more_life() {
        let (cleaned, cond) = extract_if_condition("draw a card if you have 40 or more life");
        assert_eq!(
            cond,
            Some(TriggerCondition::QuantityComparison {
                lhs: QuantityExpr::Ref {
                    qty: QuantityRef::LifeTotal {
                        player: crate::types::ability::PlayerScope::Controller
                    },
                },
                comparator: Comparator::GE,
                rhs: QuantityExpr::Fixed { value: 40 },
            })
        );
        assert_eq!(cleaned.trim(), "draw a card");
    }

    #[test]
    fn extract_if_you_have_n_or_more_life_win() {
        let (cleaned, cond) = extract_if_condition("you win the game if you have 40 or more life");
        assert!(
            matches!(
                cond,
                Some(TriggerCondition::QuantityComparison {
                    lhs: QuantityExpr::Ref {
                        qty: QuantityRef::LifeTotal {
                            player: crate::types::ability::PlayerScope::Controller
                        },
                    },
                    comparator: Comparator::GE,
                    ..
                })
            ),
            "Expected QuantityComparison with LifeTotal >= N, got: {cond:?}"
        );
        assert_eq!(cleaned.trim(), "you win the game");
    }

    #[test]
    fn extract_if_gained_life_regression() {
        // Existing pattern must still work — now produces QuantityComparison via combinator
        let (_, cond) = extract_if_condition("draw a card if you've gained life this turn");
        assert_eq!(
            cond,
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
                condition: Box::new(TriggerCondition::DuringYourTurn),
            }
        );
    }

    #[test]
    fn bridge_during_your_turn_maps_to_trigger() {
        assert_eq!(
            static_condition_to_trigger_condition(&StaticCondition::DuringYourTurn),
            Some(TriggerCondition::DuringYourTurn),
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
                condition: Box::new(TriggerCondition::DuringYourTurn),
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
                counter_type: Some("+1/+1".to_string()),
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
    fn extract_had_counters_untyped() {
        let (cleaned, cond) = extract_if_condition("draw a card if it had counters on it");
        assert_eq!(cleaned, "draw a card");
        assert_eq!(
            cond.unwrap(),
            TriggerCondition::HadCounters { counter_type: None },
        );
    }

    #[test]
    fn bridge_monarch_from_trigger_text() {
        let (cleaned, cond) = extract_if_condition("draw a card if you're the monarch");
        assert_eq!(cleaned, "draw a card");
        assert_eq!(cond.unwrap(), TriggerCondition::IsMonarch);
    }

    #[test]
    fn bridge_source_tapped_from_trigger_text() {
        let (cleaned, cond) =
            extract_if_condition("put a storage counter on it if this land is tapped");
        assert!(cleaned.contains("put a storage counter"));
        assert_eq!(cond.unwrap(), TriggerCondition::SourceIsTapped);
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
    /// primary PutCounter count and the chained Draw's `HalfRounded` inner.
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
                assert_eq!(counter_type, "P1P1");
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
                QuantityExpr::HalfRounded { inner, .. } => {
                    assert_eq!(
                        **inner,
                        QuantityExpr::Ref {
                            qty: QuantityRef::CostXPaid
                        },
                        "HalfRounded inner should be CostXPaid, got {inner:?}"
                    );
                }
                other => panic!("expected HalfRounded, got {other:?}"),
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
                // +1/+1 counters use the normalized "P1P1" canonical form
                // (see oracle_effect::counter::normalize_counter_type).
                assert_eq!(counter_type, "P1P1");
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
}
