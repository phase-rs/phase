use crate::parser::oracle_nom::error::OracleError;
use nom::branch::alt;
use nom::bytes::complete::{tag, take_until};
use nom::character::complete::{one_of, space1};
use nom::combinator::{all_consuming, eof, map, opt, peek, recognize, rest, value};
use nom::multi::{many1, separated_list1};
use nom::sequence::{delimited, pair, preceded, terminated};
use nom::Parser;

use super::oracle_effect::{
    condition_text_is_rehomeable, lower_effect_chain_ir, parse_effect_chain_ir,
    try_parse_each_player_copy_chosen, try_parse_exile_top_each_library_with_collection_counter,
    try_parse_grant_graveyard_keyword_to_target, try_parse_reanimator_aura_etb_effect,
};
use super::oracle_ir::context::ParseContext;
use super::oracle_ir::trigger::{FirstTimeLimit, TriggerBody, TriggerIr, TriggerModifiers};
use super::oracle_modal::try_parse_inline_modal;
use super::oracle_nom::condition::parse_inner_condition;
use super::oracle_nom::condition::parse_source_has_counters;
use super::oracle_nom::error::{oracle_err, OracleResult};
use super::oracle_nom::filter::{
    parse_color_property, parse_enters_origin_zone, parse_with_property,
};
use super::oracle_nom::primitives::{
    self as nom_primitives, scan_contains, scan_preceded, scan_split_at_phrase,
};
use super::oracle_nom::target::parse_type_phrase as parse_type_phrase_nom;
use super::oracle_static::parse_commander_subject_filter_prefix;
use super::oracle_target::{
    attachment_kinds_filter_prop, parse_attachment_kind_disjunction, parse_type_phrase,
    starts_with_type_list_continuation, starts_with_type_word,
};
use super::oracle_util::{
    canonicalize_subtype_name, is_core_type_name, is_non_subtype_subject_name, merge_or_filters,
    normalize_card_name_refs, parse_number, parse_ordinal, parse_subtype, strip_after,
    strip_reminder_text, TextPair, SELF_REF_PARSE_ONLY_PHRASES,
};
use crate::parser::oracle_ir::diagnostic::OracleDiagnostic;
use crate::types::ability::ManaProduction;
use crate::types::ability::{
    AbilityCost, AbilityDefinition, AbilityKind, AbilityTag, AggregateFunction, AttachmentKind,
    AttackersDeclaredCountSubject, CastManaObjectScope, CastManaSpentMetric, CastVariantPaid,
    CoinFlipResult, Comparator, ControllerRef, CountScope, CounterTriggerFilter, DamageKindFilter,
    DestinationConstraint, DieResultFilter, Effect, FilterProp, ObjectScope, OriginConstraint,
    ParsedCondition, PlayerFilter, PlayerScope, PtStat, PtValueScope, QuantityExpr, QuantityRef,
    RenownSubject, SacrificeAggregateStat, SacrificeCost, SacrificeRequirement, SharedQuality,
    StaticCondition, TapCreaturesRequirement, TargetFilter, TriggerCondition, TriggerConstraint,
    TriggerDefinition, TypeFilter, TypedFilter, UnlessPayModifier, ZoneChangeClause,
};
use crate::types::card_type::{is_land_subtype, CoreType};
use crate::types::counter::CounterType;
use crate::types::events::{ClashResult, PlayerActionKind};
use crate::types::keywords::KeywordKind;
use crate::types::mana::{ManaColor, ManaType};
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

/// CR 108.3 + CR 109.5: "Whenever you cast a spell you don't own" — the spell's
/// owner is an opponent even though its controller on the stack is you.
fn strip_spell_not_owned_qualifier(payload: &str) -> (&str, bool) {
    let mut parser = alt((
        terminated(
            take_until(" you don't own"),
            tag::<_, _, OracleError<'_>>(" you don't own"),
        ),
        terminated(take_until(" you do not own"), tag(" you do not own")),
    ));
    parser
        .parse(payload)
        .map(|(body, _)| (body.trim(), true))
        .unwrap_or((payload, false))
}

/// CR 603.7c: "Whenever a player casts a spell they don't own" — the casting
/// player is the trigger event's player; the spell must not be owned by them.
fn strip_spell_they_dont_own_qualifier(payload: &str) -> (&str, bool) {
    let mut parser = alt((
        terminated(
            take_until(" they don't own"),
            tag::<_, _, OracleError<'_>>(" they don't own"),
        ),
        terminated(take_until(" they do not own"), tag(" they do not own")),
    ));
    parser
        .parse(payload)
        .map(|(body, _)| (body.trim(), true))
        .unwrap_or((payload, false))
}

fn with_owner_scope(filter: TargetFilter, controller: ControllerRef) -> TargetFilter {
    match filter {
        TargetFilter::Typed(mut typed) => {
            if !typed
                .properties
                .iter()
                .any(|prop| matches!(prop, FilterProp::Owned { .. }))
            {
                typed.properties.push(FilterProp::Owned { controller });
            }
            TargetFilter::Typed(typed)
        }
        TargetFilter::Or { filters } => TargetFilter::Or {
            filters: filters
                .into_iter()
                .map(|filter| with_owner_scope(filter, controller.clone()))
                .collect(),
        },
        TargetFilter::And { filters } => TargetFilter::And {
            filters: filters
                .into_iter()
                .map(|filter| with_owner_scope(filter, controller.clone()))
                .collect(),
        },
        TargetFilter::Not { filter } => TargetFilter::Not {
            filter: Box::new(with_owner_scope(*filter, controller)),
        },
        TargetFilter::Any => TargetFilter::Typed(
            TypedFilter::card().properties(vec![FilterProp::Owned { controller }]),
        ),
        other => TargetFilter::And {
            filters: vec![
                other,
                TargetFilter::Typed(
                    TypedFilter::card().properties(vec![FilterProp::Owned { controller }]),
                ),
            ],
        },
    }
}

/// CR 115.1: A "becomes the target of a spell or ability you control / an
/// opponent controls" trigger (e.g. Valiant — Heartfire Hero, Emberheart
/// Challenger) restricts the targeting source's controller. Parse that optional
/// controller off the front of the post-event remainder; `None` leaves the
/// source unrestricted (bare "a spell or ability").
fn parse_target_source_controller(rest: &str) -> Option<ControllerRef> {
    parse_target_source_controller_tail(rest).0
}

/// CR 115.1: Parse an optional source-controller clause off the front of `rest`,
/// returning BOTH the recognized controller (if any) and the unconsumed tail. A
/// `None` controller paired with the original `rest` means no clause was present.
/// Exposing the tail lets the ability-only `BecomesTargetAbility` arm enforce a
/// remaining-empty guard (rejecting source restrictions it cannot model) without
/// changing `parse_target_source_controller`'s controller-only callers.
fn parse_target_source_controller_tail(rest: &str) -> (Option<ControllerRef>, &str) {
    let rest = rest.trim_start();
    match alt((
        value(
            ControllerRef::You,
            tag::<_, _, OracleError<'_>>("you control"),
        ),
        value(ControllerRef::Opponent, tag("an opponent controls")),
    ))
    .parse(rest)
    {
        Ok((tail, controller)) => (Some(controller), tail),
        Err(_) => (None, rest),
    }
}

/// CR 115.1: The targeting source of such a trigger is a stack spell OR a stack
/// ability controlled by `controller`. Mirrors the spell/ability split the
/// stack-entry matcher (`stack_entry_matches_filter`) enforces at runtime: the
/// spell branch pairs `StackSpell` with a controller-scoped `Typed`, the ability
/// branch uses `StackAbility`'s own controller dimension.
fn becomes_target_source_filter(controller: ControllerRef) -> TargetFilter {
    TargetFilter::Or {
        filters: vec![
            TargetFilter::And {
                filters: vec![
                    TargetFilter::StackSpell,
                    TargetFilter::Typed(TypedFilter::default().controller(controller.clone())),
                ],
            },
            TargetFilter::StackAbility {
                controller: Some(controller),
                tag: None,
                kind: None,
            },
        ],
    }
}

fn parse_self_return_origin_zone(lower: &str) -> Option<Zone> {
    nom_primitives::scan_preceded(lower, |input| {
        let (rest, _) = (
            alt((
                tag::<_, _, OracleError<'_>>("return this card "),
                tag("return ~ "),
                tag("return it "),
            )),
            tag("from "),
        )
            .parse(input)?;
        let (rest, zone) = parse_cast_origin_zone(rest)?;
        Ok((rest, zone))
    })
    .and_then(|(_, zone, _)| zone)
}

fn self_recursion_trigger_zone(
    ability: &crate::types::ability::AbilityDefinition,
    source_lower: &str,
) -> Option<Zone> {
    match ability.effect.as_ref() {
        crate::types::ability::Effect::ChangeZone {
            origin: Some(origin),
            target: TargetFilter::SelfRef,
            ..
        } if *origin != Zone::Battlefield => Some(*origin),
        crate::types::ability::Effect::Bounce {
            target: TargetFilter::SelfRef,
            destination,
            ..
        } if destination.is_none_or(|zone| zone == Zone::Hand) => {
            parse_self_return_origin_zone(source_lower)
        }
        // CR 603.10a + CR 603.7a: A trigger whose effect schedules a delayed
        // self-return ("return this card from your graveyard ... at the
        // beginning of the next end step", Prized Amalgam) fires while the
        // source is in that origin zone — so the origin zone of the delayed
        // trigger's inner self-return is the firing zone. Descend into the
        // carried effect to surface it.
        crate::types::ability::Effect::CreateDelayedTrigger { effect, .. } => {
            self_recursion_trigger_zone(effect, source_lower)
        }
        _ => ability
            .sub_ability
            .as_deref()
            .and_then(|ability| self_recursion_trigger_zone(ability, source_lower))
            .or_else(|| {
                ability
                    .else_ability
                    .as_deref()
                    .and_then(|ability| self_recursion_trigger_zone(ability, source_lower))
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

/// CR 113.6 + CR 113.6b: Collect every zone the trigger's
/// source must occupy for the condition to be satisfiable. Returns the
/// deduplicated union of `SourceInZone { zone }` references across
/// `And`/`Or` composites, in first-seen declaration order. Mirrors the
/// static-side analog `oracle_static::collect_source_in_zones`.
///
/// Two motivating shapes:
/// - Single zone (CR 113.6b): a graveyard-functioning trigger with
///   condition `SourceInZone { Graveyard }` returns `[Graveyard]`.
/// - Multi-zone disjunction (Eminence ability word per CR 207.2c — e.g.
///   Edgar Markov, The Ur-Dragon): "if ~ is in the
///   command zone or on the battlefield" parses to
///   `Or { [SourceInZone(Command), SourceInZone(Battlefield)] }` and
///   returns `[Command, Battlefield]`, so the runtime trigger scanner
///   considers both zones when locating the source.
///
/// `Not { SourceInZone(X) }` is deliberately NOT recursed: a "source is
/// NOT in X" condition would be the opposite signal (the runtime
/// condition check filters the trigger out in zone X), so adding X to
/// the scan-zones list would be backwards. No production card currently
/// constructs that shape — flagged for documentation.
///
/// Issue #817 — Edgar Markov: the previous `Option<Zone>` shape only
/// returned the first zone from `And` composites and ignored `Or`
/// entirely, so an Eminence trigger's `trigger_zones` stayed at the
/// `[Battlefield]` `make_base` default and the command-zone source was
/// never scanned.
fn trigger_condition_source_zones(condition: &TriggerCondition) -> Vec<Zone> {
    let mut out: Vec<Zone> = Vec::new();
    collect_trigger_condition_source_zones(condition, &mut out);
    out
}

fn collect_trigger_condition_source_zones(condition: &TriggerCondition, out: &mut Vec<Zone>) {
    match condition {
        TriggerCondition::SourceInZone { zone } if !out.contains(zone) => {
            out.push(*zone);
        }
        TriggerCondition::And { conditions } | TriggerCondition::Or { conditions } => {
            for inner in conditions {
                collect_trigger_condition_source_zones(inner, out);
            }
        }
        _ => {}
    }
}

/// CR 113.6b + CR 400.7: When a trigger's intervening-if pins the source to a
/// single zone ("if this card is in your graveyard") and the effect is an
/// implicit self-return ("return it to the battlefield …") with no "from …"
/// phrase, stamp that zone as `ChangeZone.origin` so the runtime can locate
/// the object (Jocasta, Automaton Avenger — issue #4566).
fn stamp_self_return_origin_from_trigger_condition(def: &mut TriggerDefinition) {
    let zones = def
        .condition
        .as_ref()
        .map(trigger_condition_source_zones)
        .unwrap_or_default();
    let Some(origin) = (zones.len() == 1).then(|| zones[0]) else {
        return;
    };
    if let Some(execute) = def.execute.as_deref_mut() {
        stamp_self_return_origin_in_effect(&mut execute.effect, origin);
    }
}

fn stamp_self_return_origin_in_effect(effect: &mut Effect, origin: Zone) {
    match effect {
        Effect::ChangeZone {
            origin: o,
            destination,
            target,
            ..
        } if matches!(
            target,
            TargetFilter::SelfRef | TargetFilter::TriggeringSource
        ) && matches!(destination, Zone::Battlefield | Zone::Hand) =>
        {
            if o.is_none() {
                *o = Some(origin);
            }
            if matches!(target, TargetFilter::TriggeringSource) {
                *target = TargetFilter::SelfRef;
            }
        }
        Effect::CreateDelayedTrigger { effect: inner, .. } => {
            stamp_self_return_origin_in_effect(&mut inner.effect, origin);
        }
        Effect::ChooseOneOf { branches, .. } => {
            for branch in branches.iter_mut() {
                stamp_self_return_origin_in_effect(&mut branch.effect, origin);
            }
        }
        _ => {}
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

/// Extract the `(lhs, rhs)` operands of a `QuantityComparison` trigger
/// condition, looking through a top-level `And` composite (intervening-if
/// conditions are composed onto any pre-existing condition via
/// `and_trigger_conditions`). Used to resolve the anaphoric "draw cards equal
/// to the difference" count against the hoisted condition's two operands.
fn quantity_comparison_operands(cond: &TriggerCondition) -> Option<(&QuantityExpr, &QuantityExpr)> {
    match cond {
        TriggerCondition::QuantityComparison { lhs, rhs, .. } => Some((lhs, rhs)),
        TriggerCondition::And { conditions } | TriggerCondition::Or { conditions } => {
            conditions.iter().find_map(quantity_comparison_operands)
        }
        _ => None,
    }
}

/// CR 122.1 + CR 603.4 + CR 603.10a: Resolve every deferred "the difference"
/// counter-anaphor placeholder anywhere in an ability's effect tree.
///
/// The put-counter parser emits a `Variable { "difference" }` count placeholder
/// for a bare "equal to the difference" because the two operands live on the
/// trigger's hoisted intervening-if, not the effect clause. `Effect::count_expr_mut`
/// only reaches the top-level effect, so a placeholder nested under a
/// clause-level conditional `sub_ability` (Conformer Shuriken: "tap target
/// creature …; if that creature has power greater than ~'s power, put a number
/// of +1/+1 counters on ~ equal to the difference") would otherwise escape both
/// the bind and the guard. This walks the full tree — `sub_ability`,
/// `else_ability`, and single-`Box<Effect>` wrappers — so that:
///   * with a hoisted `QuantityComparison` (`bound = Some`), each placeholder
///     binds to that `Difference` (Drizzt Do'Urden), and
///   * with none to bind (`bound = None`), the carrying counter effect is
///     downgraded to an explicit `Effect::Unimplemented` — an honest coverage
///     gap rather than a silently-zero `PutCounter` that reads as supported.
fn resolve_difference_anaphor_in_ability(
    def: &mut AbilityDefinition,
    bound: Option<&QuantityExpr>,
) {
    resolve_difference_anaphor_in_effect(&mut def.effect, bound);
    if let Some(sub) = def.sub_ability.as_deref_mut() {
        resolve_difference_anaphor_in_ability(sub, bound);
    }
    if let Some(els) = def.else_ability.as_deref_mut() {
        resolve_difference_anaphor_in_ability(els, bound);
    }
}

fn resolve_difference_anaphor_in_effect(effect: &mut Effect, bound: Option<&QuantityExpr>) {
    // Recurse into the single-`Box<Effect>` wrapper (the draw-replacement
    // substitute) so a placeholder nested inside it is reached. This is the only
    // `Effect` variant that wraps a heterogeneous sub-`Effect`; every other
    // nesting is via `AbilityDefinition` (`sub_ability`/`else_ability`), walked
    // by the caller.
    if let Effect::CreateDrawReplacement {
        replacement_effect: inner,
    } = effect
    {
        resolve_difference_anaphor_in_effect(inner, bound);
    }

    // Only counter effects ever carry the deferred placeholder (it is emitted
    // solely by the put-counter parser), so restrict both bind and downgrade to
    // them — this keeps the downgrade's `Effect::Unimplemented` name/description
    // honest without depending on `count_expr_mut` being counter-specific.
    if !matches!(
        effect,
        Effect::PutCounter { .. } | Effect::PutCounterAll { .. }
    ) {
        return;
    }
    let is_placeholder = effect
        .count_expr_mut()
        .is_some_and(|slot| crate::parser::oracle_effect::is_difference_anaphor_placeholder(slot));
    if !is_placeholder {
        return;
    }
    match bound {
        Some(count) => {
            if let Some(slot) = effect.count_expr_mut() {
                *slot = count.clone();
            }
        }
        None => {
            *effect = Effect::unimplemented("put", "counters equal to the difference");
        }
    }
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

    // CR 603.1 + CR 603.2: Disjunctive zone-change triggers ("whenever [A], or [B],
    // or [C]") must stay a single `TriggerDefinition` with `zone_change_clauses`.
    // `split_cross_subject_event_compound` mis-splits Syr Konrad's "another creature
    // dies, or a creature card is put into a graveyard..." at the first ", or "
    // because the second clause begins with "a creature card". That produces two
    // separate triggers that can both fire on the same zone-change event, doubling
    // the damage (issue #3299).
    if parse_disjunctive_zone_change_condition(&condition).is_some() {
        return vec![parse_trigger_line_with_index_ir(
            text,
            card_name,
            base_trigger_index,
            ctx,
        )];
    }

    // Pattern 2: disjunctive shared-subject event list — "whenever ~ A, B, or C"
    // (N-way serial) or "whenever ~ A or B" (2-way). CR 603.1: each listed event
    // is its own trigger condition, all sharing the one subject.
    if let Some(halves) = split_shared_subject_event_list(&cond_lower, &condition) {
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

/// Part D: If a `"for the first time ..."` qualifier appears as a
/// word-boundary phrase in `condition`, strip it and return the corresponding
/// trigger-event limit; otherwise return `(condition, None)` unchanged.
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
/// Returns stripped text and the detected "for the first time ..." trigger-event limit.
fn strip_first_time_each_turn_qualifier(condition: &str) -> (String, Option<FirstTimeLimit>) {
    const PHRASE: &str = "for the first time each turn";
    const PHRASE_PER_OPPONENT: &str = "for the first time during each of their turns";
    let lower = condition.to_lowercase();
    let Some((before_lower, matched_phrase, rest_lower)) = scan_preceded(&lower, |i| {
        alt((
            value(
                FirstTimeLimit::EachOpponentTurn,
                tag::<_, _, OracleError<'_>>(PHRASE_PER_OPPONENT),
            ),
            value(FirstTimeLimit::EachTurn, tag(PHRASE)),
        ))
        .parse(i)
    }) else {
        return (condition.to_string(), None);
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
    (stripped, Some(matched_phrase))
}

/// CR 608.2c + CR 506.2: "attack a player" — the attacked player is the
/// defending player, which resolves via `ControllerRef::DefendingPlayer` at
/// runtime. Distinct from damage-to-player triggers (which use
/// `ControllerRef::TargetPlayer` because the damaged player is not necessarily
/// the defending player in combat). This function specifically detects attack
/// patterns without matching damage-to-player patterns.
fn parse_trigger_actor(input: &str) -> OracleResult<'_, ()> {
    alt((
        value((), tag::<_, _, OracleError<'_>>("you ")),
        value((), tag("an opponent ")),
        value((), tag("a player ")),
        value((), tag("another player ")),
    ))
    .parse(input)
}

fn parse_attack_verb(input: &str) -> OracleResult<'_, ()> {
    alt((
        value((), tag::<_, _, OracleError<'_>>("attack ")),
        value((), tag("attacks ")),
    ))
    .parse(input)
}

fn parse_referenced_player_phrase(input: &str) -> OracleResult<'_, ()> {
    alt((
        value(
            (),
            tag::<_, _, OracleError<'_>>("one or more of your opponents"),
        ),
        value((), tag("one of your opponents")),
        value((), tag("another player")),
        value((), tag("an opponent")),
        value((), tag("a player")),
    ))
    .parse(input)
}

fn condition_introduces_defending_player(cond_lower: &str) -> bool {
    // Walk word boundaries — the actor/verb pair may be preceded by "whenever",
    // "when", or quantifiers like "one or more creatures you control".
    let mut remaining = cond_lower;
    while !remaining.is_empty() {
        if let Ok((after_actor, ())) = parse_trigger_actor(remaining) {
            if let Ok((after_verb, ())) = parse_attack_verb(after_actor) {
                if parse_referenced_player_phrase(after_verb).is_ok() {
                    return true;
                }
            }
        }
        // CR 506.2 + CR 508.1a: "[anything] attack[s] a player" — same subject
        // permissiveness. Covers cases where the actor is wrapped in a relative
        // clause that the explicit actor branch above cannot match, e.g. "one or
        // more Warriors you control attack a player" (Gornog, the Red Reaper) or
        // "a creature you control attacks a player". The verb phrase alone is
        // unambiguous in trigger-condition text — "attack" never appears as a
        // noun before "a player" here.
        if let Ok((after_verb, ())) = parse_attack_verb(remaining) {
            if parse_referenced_player_phrase(after_verb).is_ok() {
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

fn condition_introduces_target_player(cond_lower: &str) -> bool {
    /// CR 120.3: "deals [combat] damage to a player" — damage dealt to a player
    /// causes that player to lose life (CR 120.3a) and introduces the damaged
    /// player as the target-referring player, so "that player controls" in the
    /// effect refers to it
    /// (Dokuchi Silencer's "destroy target creature or planeswalker that player
    /// controls"). Attack-player triggers are intentionally handled by
    /// `condition_introduces_defending_player`.
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
        // CR 120.3: "[anything] deals [combat] damage to a player" — introduces
        // the damaged player as the target-referring player. The subject can be
        // SelfRef ("~"), equipped creature ("equipped creature"), or any typed
        // subject, so match on the verb phrase alone.
        if let Ok((after_damage, ())) = parse_damage_phrase(remaining) {
            if parse_referenced_player_phrase(after_damage).is_ok() {
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

fn condition_introduces_damage_source_controller_player(cond_lower: &str) -> bool {
    let input = cond_lower.trim_start();
    let input = alt((
        value((), tag::<_, _, OracleError<'_>>("whenever ")),
        value((), tag("when ")),
    ))
    .parse(input)
    .map(|(rest, _)| rest)
    .unwrap_or(input);
    let Ok((rest, source_filter)) = parse_damage_source_subject(input) else {
        return false;
    };
    let TargetFilter::Typed(TypedFilter {
        controller: Some(ControllerRef::Opponent),
        ..
    }) = source_filter
    else {
        return false;
    };
    let Ok((rest, _)) = tag::<_, _, OracleError<'_>>("deals ").parse(rest) else {
        return false;
    };
    let Ok((after_damage, _)) = parse_damage_predicate_tail(rest) else {
        return false;
    };

    matches!(
        parse_damage_to_qualifier(after_damage),
        Some(TargetFilter::Controller)
    )
}

/// Check if the trigger condition is a DamageDone trigger pattern
/// ("deals damage to a player" or "deals combat damage to a player").
fn is_damage_done_trigger_pattern(cond_lower: &str) -> bool {
    let input = cond_lower.trim_start();
    let input = alt((
        value((), tag::<_, _, OracleError<'_>>("whenever ")),
        value((), tag("when ")),
    ))
    .parse(input)
    .map(|(rest, _)| rest)
    .unwrap_or(input);

    // Check for "deals damage to a player" or "deals combat damage to a player"
    let Ok((rest, _)) = parse_damage_source_subject(input) else {
        return false;
    };
    let Ok((rest, _)) = tag::<_, _, OracleError<'_>>("deals ").parse(rest) else {
        return false;
    };
    let Ok((after_damage, _)) = parse_damage_predicate_tail(rest) else {
        return false;
    };

    // CR 120.3a + CR 603.2c: A damage-to-player trigger establishes the damaged
    // player as the relative `TriggeringPlayer` for "that player" anaphors.
    // Both the generic player recipient ("a player") and the opponent recipient
    // ("an opponent" → `Typed(controller: Opponent)`) are player recipients;
    // Fear of Burning Alive ("deals noncombat damage to an opponent ... target
    // creature that player controls") relies on the opponent form binding.
    matches!(
        parse_damage_to_qualifier(after_damage),
        Some(
            TargetFilter::Player
                | TargetFilter::Typed(TypedFilter {
                    controller: Some(ControllerRef::Opponent),
                    ..
                })
        )
    )
}

/// CR 109.4 + CR 115.1 + CR 506.2 + CR 603.7c: Derive the relative-player scope
/// that a trigger condition introduces for `"that player"`/`"they"`-style
/// anaphors in the trigger's effect body.
///
/// This is the single authority for mapping a trigger condition to the
/// `ControllerRef` that a later `"that player controls"` reference must resolve
/// against. Both the in-line trigger body parser (`parse_trigger`) and the
/// delayed-trigger split path (`parse_delayed_whenever_trigger` in
/// `oracle_effect`) call this so the same condition yields the same scope —
/// otherwise the delayed-trigger effect would lose the `TriggeringPlayer`
/// binding for "...deals combat damage to a player ... target creature that
/// player controls" (The Sea Devils III).
///
/// DamageDone (`TriggeringPlayer`) is checked BEFORE `condition_introduces_target_player`
/// because both match "deals [combat] damage to a player", but DamageDone needs
/// `TriggeringPlayer` while generic target-player triggers need `TargetPlayer`.
pub(crate) fn relative_player_scope_for_condition(cond_lower: &str) -> Option<ControllerRef> {
    if is_damage_done_trigger_pattern(cond_lower) {
        Some(ControllerRef::TriggeringPlayer)
    } else if condition_introduces_damage_source_controller_player(cond_lower) {
        Some(ControllerRef::ParentTargetController)
    } else if condition_introduces_defending_player(cond_lower) {
        // CR 608.2c: Attack triggers use DefendingPlayer (the attacked player
        // in combat), not TargetPlayer (which requires a player target to be
        // bound at runtime).
        Some(ControllerRef::DefendingPlayer)
    } else if condition_introduces_target_player(cond_lower) {
        Some(ControllerRef::TargetPlayer)
    } else if condition_introduces_chosen_player_phase(cond_lower) {
        Some(ControllerRef::SourceChosenPlayer)
    } else if condition_introduces_scoped_phase_player(cond_lower) {
        Some(ControllerRef::ScopedPlayer)
    } else if condition_matches_taps_for_mana_event(cond_lower) {
        // CR 603.2 + CR 605.1a: "that player" in tap-all-lands effects (War's
        // Toll) refers to the player who tapped for mana.
        Some(ControllerRef::TriggeringPlayer)
    } else {
        None
    }
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
    let (condition_text_stripped, first_time_limit) =
        strip_first_time_each_turn_qualifier(&condition_text_raw);
    let condition_text: &str = &condition_text_stripped;

    let effect_lower = effect_text.to_lowercase();
    // CR 701.42b: A meld instigator's effect text opens with the own/control
    // gate ("if you both own and control ~ and a [type] named [partner], exile
    // them, then meld them into [result]"). Recognize it as a unit: the gate
    // becomes the trigger's intervening-if condition, the partner name is staged
    // for the meld effect combinator, and the residual ("exile them, then meld
    // them into [result]") is parsed as the effect body. Falls through to the
    // generic `extract_if_condition` for every non-meld trigger.
    let (effect_without_if, if_condition, meld_partner) =
        match crate::parser::oracle_effect::meld::parse_meld_gate(&effect_text) {
            Some((gate, partner, residual)) => (residual, Some(gate), Some(partner)),
            None => {
                // Extract intervening-if condition from effect text first — a
                // leading "if X, " can hide the "you may " optional marker behind
                // the if-clause.
                let (without_if, cond) =
                    extract_if_condition_with_card_name(&effect_text, card_name);
                (without_if, cond, None)
            }
        };

    // CR 608.2c (resolution-order instructions): "You may" at the start of
    // the effect text makes the triggered effect optional at resolution.
    //
    // IMPORTANT — multi-sentence triggers must NOT hoist this flag to the
    // outer trigger. A pattern like
    //   "look at the top card of your library. If <cond>, you may reveal
    //    that card and put it into your hand."
    // contains a MANDATORY first action (`look at …`) followed by an
    // OPTIONAL second action gated on a condition. Hoisting `you may` to the
    // trigger-level `optional` flag would make the entire trigger
    // (including the mandatory look) skippable, which is wrong per
    // CR 608.2c (the controller follows instructions in printed order).
    // Per-chunk peel in `clause_shell::peel_clause` marks only the inner
    // optional sub_ability `optional = true`, which is the correct shape.
    // The detection below only fires when the `you may` is the FIRST token
    // (modulo an intervening-if), which excludes the multi-sentence case.
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
    // CR 107.4 + CR 202.1 + CR 603.4: Stage the cast-trigger's colored-mana-symbol
    // qualifier color (Namor) so a "create that many tokens" effect clause can
    // back-reference the cast spell's colored-pip count instead of the generic
    // EventContextAmount. Derived from the same condition/qualifier text whose
    // spell qualifier becomes the trigger's `valid_card`.
    let pending_mana_symbol_count_color =
        extract_colored_mana_symbol_spell_qualifier(condition_text);
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
        // CR 701.42a: stage the meld partner so the effect-clause combinator can
        // stamp `Effect::Meld { source, partner, .. }` (the context carries the
        // source name; the gate carried the partner name).
        pending_meld_partner: meld_partner,
        pending_mana_symbol_count_color,
        object_pronoun_ref: trigger_object_pronoun_ref_for_condition(condition_text),
        in_trigger: true,
        ..Default::default()
    };

    // CR 109.4 + CR 115.1 + CR 506.2 + CR 603.7c: Set relative-player scope for
    // `"that player"` resolution inside the trigger effect body. Delegated to the
    // single-authority `relative_player_scope_for_condition` so the delayed-trigger
    // split path derives the identical scope from the same condition.
    if let Some(scope) = relative_player_scope_for_condition(&cond_lower) {
        effect_ctx.relative_player_scope = Some(scope);
    }
    // Snapshot the condition-established scope before body parsing (which may
    // temporarily rebind it via `with_player_scope`) so lowering sees the scope
    // the condition introduced, not a transient nested-clause value.
    let relative_player_scope = effect_ctx.relative_player_scope.clone();

    // Parse the effect body
    let effect_for_parse_lower = effect_for_parse.to_lowercase();
    // CR 115.1d: Pre-lowered vote blocks do not flow through clause-level
    // multi-target extraction, so keep their legacy optional-targeting marker
    // local to that PreLowered path. Normal effect chains carry this metadata on
    // the specific parsed clause.
    let has_up_to = scan_contains(&effect_for_parse_lower, "up to one")
        || scan_contains(&effect_for_parse_lower, "any number of target");
    let body = if !effect_for_parse.is_empty() {
        if parse_monarch_turn_began_condition(effect_for_parse_lower.as_str()).is_some() {
            Some(TriggerBody::PreLowered(Box::new(AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::Unimplemented {
                    name: "Unsupported monarch turn-began condition".to_string(),
                    description: Some(effect_for_parse.clone()),
                },
            ))))
        // CR 701.38 + CR 207.2c: Vote blocks produce AbilityDefinition directly.
        } else if let Some(vote_def) =
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
            try_parse_exile_top_each_library_with_collection_counter(
                &effect_for_parse,
                AbilityKind::Spell,
            )
            .map(|ability| TriggerBody::PreLowered(Box::new(ability)))
            .or_else(|| {
                // CR 702.138a: triggered one-shot grant of escape to a target
                // graveyard card whose compound cost rides a continuation sentence
                // (Desdemona, Freedom's Edge). Fail-closed: declines unless the
                // whole two-sentence shape parses, so a card with an unparsed
                // target filter stays an honest Unimplemented rather than misparsing.
                try_parse_grant_graveyard_keyword_to_target(&effect_for_parse, AbilityKind::Spell)
                    .map(|ability| TriggerBody::PreLowered(Box::new(ability)))
            })
            .or_else(|| {
                // CR 101.4 + CR 707.2 + CR 122.1: whole-body "each player chooses …
                // creates a token copy of the first … (then scales by the second)"
                // (WHO phenomena). Fail-closed multi-sentence detector; sees the
                // full pre-split body. Placed before the terminal
                // `parse_effect_chain_ir` fallthrough so an unparsed shape stays an
                // honest Unimplemented rather than misparsing.
                try_parse_each_player_copy_chosen(&effect_for_parse, AbilityKind::Spell)
                    .map(|ability| TriggerBody::PreLowered(Box::new(ability)))
            })
            .or_else(|| {
                // CR 608.2c + CR 613.1f + CR 701.3a + CR 701.17a: whole-body
                // reanimator-Aura ETB effect (Animate Dead / Dance of the Dead) —
                // "it loses ... and gains ...", return/put the enchanted creature
                // card to the battlefield under your control, attach the Aura to
                // it, and register the leaves-battlefield sacrifice. Fail-closed:
                // declines unless the entire body matches, so a deviating card
                // stays an honest Unimplemented rather than misparsing.
                try_parse_reanimator_aura_etb_effect(&effect_for_parse, AbilityKind::Spell)
                    .map(|ability| TriggerBody::PreLowered(Box::new(ability)))
            })
            .or_else(|| {
                // CR 700.2 + CR 608.2d: Inline modal trigger body — "choose one —
                // mode1; or mode2" on a single line (no bullet-line modes). No
                // printed card currently reaches this branch: it needs an em-dash
                // *and* "; or " inside one trigger effect body. (Grenzo, Havoc
                // Raiser — long named here as the canonical case — uses bullet-line
                // modes, so `raw_modes.len() == 1` and it never arrives.) Route
                // through the modal parser so each mode body is independently
                // parsed with the trigger's established relative_player_scope (e.g.
                // TriggeringPlayer for DamageDone triggers) so "that player" in mode
                // bodies resolves to the damaged player (CR 603.7c).
                if let Some(modal_ability) = try_parse_inline_modal(
                    &effect_for_parse,
                    effect_ctx.relative_player_scope.clone(),
                ) {
                    return Some(TriggerBody::PreLowered(Box::new(modal_ability)));
                }
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
            first_time_limit,
            constraint,
            has_up_to,
            effect_lower: effect_lower.to_string(),
            relative_player_scope,
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

/// True when a resolution-time optional cast names a player-chosen target that
/// must be announced when the triggered ability is put on the stack (CR 603.3d).
fn trigger_effect_requires_stack_time_targets(ability: &AbilityDefinition) -> bool {
    matches!(ability.effect.as_ref(), Effect::CastFromZone { .. })
        && ability
            .effect
            .target_filter()
            .is_some_and(|filter| !filter.is_context_ref())
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
            crate::parser::oracle_effect::finalize_effect_chain(&mut ability);
            if effect_adds_mana_to_triggering_player(&modifiers.effect_lower)
                && matches!(
                    ability.effect.as_ref(),
                    crate::types::ability::Effect::Mana { .. }
                )
            {
                ability.player_scope = Some(PlayerFilter::TriggeringPlayer);
            }
            // CR 115.1d: Singleton "up to one target ..." effects that lower
            // without a `multi_target` spec still permit choosing zero targets.
            // Do not stamp this onto non-target head clauses in chains like
            // "draw a card. Attach any number of target Equipment ..."
            if modifiers.has_up_to
                && ability.multi_target.is_none()
                && ability
                    .effect
                    .target_filter()
                    .is_some_and(|filter| !filter.is_context_ref())
            {
                ability.optional_targeting = true;
            }
            // CR 603.5: A triggered ability whose effect is optional ("may") goes
            // on the stack regardless; the choice is made when it resolves. Carry
            // that optionality onto the execute ability, which is what resolves.
            if modifiers.optional {
                ability.optional = true;
            }
            Some(Box::new(ability))
        }
        Some(TriggerBody::PreLowered(ability)) => Some(ability.clone()),
        None => None,
    };

    // CR 603.7c + CR 120.3 + CR 506.2: For triggers that introduce an
    // event-bound player ("deals combat damage to a player, they lose half
    // their life"), rebind the body's `PlayerScope::Target` possessive
    // quantities to `PlayerScope::ScopedPlayer` so they resolve against the
    // damaged/attacked player rather than an absent chosen target.
    let mut execute = execute;
    if modifiers.relative_player_scope == Some(ControllerRef::TargetPlayer) {
        if let Some(ability) = execute.as_deref_mut() {
            crate::parser::oracle_effect::rewrite_event_player_quantity_refs_to_scoped(ability);
        }
    }
    if modifiers.relative_player_scope == Some(ControllerRef::SourceChosenPlayer) {
        if let Some(ability) = execute.as_deref_mut() {
            crate::parser::oracle_effect::rewrite_player_quantity_refs_to_source_chosen(ability);
        }
    }
    if let Some(ability) = execute.as_deref_mut() {
        rewrite_each_other_player_scope_for_any_caster_spell_triggers(
            &def,
            ability,
            &modifiers.effect_lower,
        );
    }

    def.execute = execute;
    def.optional = modifiers.optional;
    // CR 603.3d + CR 608.2c: "you may cast target … from [public zone]"
    // (Torrential Gearhulk / Toshiro / Emet-Selch class) — the leading "you
    // may" is resolution-time optionality on the cast (`execute.optional`), not
    // permission to skip putting the triggered ability on the stack. Target
    // selection happens when the ability is put on the stack (CR 603.3d).
    if def.optional
        && def
            .execute
            .as_ref()
            .is_some_and(|ability| trigger_effect_requires_stack_time_targets(ability))
    {
        def.optional = false;
    }
    def.unless_pay = modifiers.unless_pay.clone();

    // CR 603.4: Compose intervening-if with existing condition via And.
    def.condition = match modifiers.intervening_if.clone() {
        Some(if_cond) => Some(and_trigger_conditions(def.condition.take(), if_cond)),
        None => def.condition.take(),
    };

    // CR 601.2h + CR 400.7d: On a spell-cast-family trigger, an intervening-if
    // anaphor "mana spent to cast it"/"...this spell" denotes the *triggering
    // spell*, not the ability's source permanent. The bare anaphor parses to
    // `CastManaObjectScope::SelfObject` (correct for ETB / resolving-spell
    // contexts where the object the spell *is* equals the source), so when the
    // hoisted condition lands on a spell-cast trigger the `SelfObject` snapshot
    // would read the source's payment-time mana (normally 0) and wrongly block
    // the trigger. Remap to `TriggeringSpell` so the threshold evaluates the
    // cast spell that fired the trigger (The Emperor of Palamecia, #1490). The
    // comparison-form `extract_if_condition` path already emits `TriggeringSpell`
    // for "cast it"; this converges the threshold-form path with it.
    if is_spell_cast_trigger_mode(&def.mode) {
        if let Some(cond) = def.condition.as_mut() {
            remap_self_cast_scope_to_triggering_spell(cond);
        }
    }

    // CR 121.1 + CR 603.4: "draw cards equal to the difference" inside a
    // trigger body (Kozilek the Great Distortion, Damia Sage of Stone, Krang
    // Master Mind, The Ten Rings, Doctor Octopus). The "if you have fewer than
    // N cards in hand" gate is hoisted to the trigger-level condition above, so
    // the body's effect parser never sees the two operands and leaves the
    // anaphoric draw count as `Unimplemented`. Resolve it here against the
    // hoisted `QuantityComparison`, mirroring the standalone `difference_draw`
    // branch in `oracle_effect/mod.rs` (`QuantityExpr::Difference { lhs, rhs }`).
    let difference_count = def
        .condition
        .as_ref()
        .and_then(quantity_comparison_operands)
        .map(|(lhs, rhs)| QuantityExpr::Difference {
            left: Box::new(lhs.clone()),
            right: Box::new(rhs.clone()),
        });
    if let Some(count) = difference_count.as_ref() {
        if let Some(execute) = def.execute.as_deref_mut() {
            let is_difference_draw = matches!(
                execute.effect.as_ref(),
                Effect::Unimplemented { name, description: Some(desc) }
                    if name == "draw"
                        && desc
                            .trim()
                            .trim_end_matches('.')
                            .eq_ignore_ascii_case("draw cards equal to the difference")
            );
            if is_difference_draw {
                *execute.effect = Effect::Draw {
                    count: count.clone(),
                    target: TargetFilter::Controller,
                };
            }
            let is_difference_lose = matches!(
                execute.effect.as_ref(),
                Effect::Unimplemented { name, description: Some(desc) }
                    if name == "lose"
                        && {
                            let clean = desc.trim().trim_end_matches('.');
                            clean.eq_ignore_ascii_case("lose life equal to the difference")
                                || clean.eq_ignore_ascii_case(
                                    "they lose life equal to the difference",
                                )
                        }
            );
            if is_difference_lose {
                *execute.effect = Effect::LoseLife {
                    amount: count.clone(),
                    target: Some(TargetFilter::ParentTarget),
                };
            }
        }
    }

    // CR 122.1 + CR 603.4 + CR 603.10a coverage-honesty: bind — or, when there is
    // no hoisted comparison to bind, downgrade to an honest `Unimplemented` —
    // every deferred "the difference" counter anaphor placeholder in the FULL
    // effect tree. Recursing (not the top-level-only `count_expr_mut`) catches a
    // placeholder nested under a clause-level conditional `sub_ability`: Drizzt
    // Do'Urden's binds at the top level, while Conformer Shuriken's put-counter
    // sits under the tap's conditional continuation with no hoisted comparison,
    // so it must become an explicit unsupported residual rather than survive as a
    // silently-zero, false-green `PutCounter`.
    if let Some(execute) = def.execute.as_deref_mut() {
        resolve_difference_anaphor_in_ability(execute, difference_count.as_ref());
    }

    // CR 603.4: Intervening-if life-gain triggers check the gained-life
    // condition when they trigger and resolve, so "that many" distribution
    // references bind to the same turn-scoped life-gain quantity.
    if def.condition.as_ref().is_some_and(
        crate::parser::oracle_effect::trigger_condition_references_controller_life_gained,
    ) {
        if let Some(ability) = def.execute.as_deref_mut() {
            crate::parser::oracle_effect::rewrite_gained_life_that_many_distribution_refs(ability);
        }
    }

    // CR 109.4 + CR 603.7c: Surface TargetFilter::Player when execute
    // references ControllerRef::TargetPlayer, when the effect text names a
    // target opponent/player (Sméagol, Helpful Guide RingTemptsYou), or when a
    // RevealUntil names an opponent library without TargetPlayer binding.
    if def.valid_target.is_none() {
        let effect_lower = modifiers.effect_lower.as_str();
        if scan_contains(effect_lower, "target opponent")
            || scan_contains(effect_lower, "target player")
        {
            def.valid_target = Some(TargetFilter::Player);
        } else if let Some(execute) = def.execute.as_deref() {
            if execute_references_target_player(&execute.effect)
                || execute_references_opponent_player(&execute.effect)
            {
                def.valid_target = Some(TargetFilter::Player);
            }
        }
    }

    // Text-based constraints take precedence; fall back to condition-parser constraint.
    def.constraint = modifiers.constraint.clone().or(def.constraint.take());

    // CR 603.2: Apply trigger-event frequency limits as a fallback.
    if let (Some(limit), None) = (modifiers.first_time_limit, def.constraint.as_ref()) {
        def.constraint = Some(match limit {
            FirstTimeLimit::EachTurn => TriggerConstraint::OncePerTurn,
            FirstTimeLimit::EachOpponentTurn => TriggerConstraint::OncePerOpponentPerTurn,
        });
    }
    constrain_triggering_spell_with_nth_filter(&mut def);

    // Preserve original oracle text for coverage/UI annotation.
    def.description = Some(ir.source_text.clone());

    // CR 113.6k: Derive trigger source zones from typed trigger/effect
    // data. `trigger_condition_source_zones` collects across `And`/`Or`
    // composites, so an Eminence-style (ability word per CR 207.2c)
    // "in the command zone or on the battlefield" intervening-if yields
    // both zones — required for the runtime trigger scanner to locate
    // Edgar Markov (and any other Eminence card) when its source is in
    // the command zone (#817).
    let condition_zones = def
        .condition
        .as_ref()
        .map(trigger_condition_source_zones)
        .unwrap_or_default();
    if !condition_zones.is_empty() {
        def.trigger_zones = condition_zones;
    } else if matches!(def.valid_card, Some(TargetFilter::SelfRef))
        && def.destination == Some(Zone::Graveyard)
    {
        def.trigger_zones = vec![Zone::Graveyard];
    } else if let Some(zone) = def
        .execute
        .as_deref()
        .and_then(|execute| self_recursion_trigger_zone(execute, modifiers.effect_lower.as_str()))
    {
        def.trigger_zones = vec![zone];
    }

    stamp_self_return_origin_from_trigger_condition(&mut def);

    // CR 107.3a + CR 107.3i + CR 601.2f: Rewrite X in ETB-self triggers.
    if trigger_should_rewrite_cost_x(&def) {
        if let Some(execute) = def.execute.as_deref_mut() {
            rewrite_cost_x_in_ability(execute);
        }
    }

    // CR 603.4 + CR 111.1: Token intervening-ifs parsed via
    // `zone_change_object_token_condition` default to destination Battlefield
    // (correct for ETB). On dies/leave triggers the zone-change event's `to`
    // is Graveyard — rewrite so "if it's not a token" can match (Vaultborn
    // Tyrant, issue #3988).
    if def.destination == Some(Zone::Graveyard) {
        if let Some(TriggerCondition::ZoneChangeObjectMatchesFilter {
            destination,
            filter,
            ..
        }) = def.condition.as_mut()
        {
            if *destination == Zone::Battlefield {
                let is_token_predicate = matches!(
                    filter,
                    TargetFilter::Typed(typed)
                        if typed.properties.iter().any(|p| {
                            matches!(p, FilterProp::NonToken | FilterProp::Token)
                        })
                );
                if is_token_predicate {
                    *destination = Zone::Graveyard;
                }
            }
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
            && !valid_target_blocks_event_source_lift(&def.mode, def.valid_target.as_ref())
            && !execute.optional_targeting
        {
            lift_parent_target_to_triggering_source_in_ability(execute);
            lift_shared_quality_parent_target_to_triggering_source_in_ability(execute);
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
            | TriggerMode::Unattach
    )
}

fn valid_target_blocks_event_source_lift(
    mode: &TriggerMode,
    valid_target: Option<&TargetFilter>,
) -> bool {
    match mode {
        TriggerMode::Discarded | TriggerMode::DiscardedAll | TriggerMode::Unattach => false,
        _ => valid_target.is_some(),
    }
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
        Effect::Sacrifice { target, .. } => target,
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

fn lift_shared_quality_parent_target_to_triggering_source_in_ability(
    ability: &mut AbilityDefinition,
) {
    lift_shared_quality_parent_target_to_triggering_source(ability.effect.as_mut());
    if let Some(sub) = ability.sub_ability.as_deref_mut() {
        lift_shared_quality_parent_target_to_triggering_source_in_ability(sub);
    }
    if let Some(else_ability) = ability.else_ability.as_deref_mut() {
        lift_shared_quality_parent_target_to_triggering_source_in_ability(else_ability);
    }
}

fn lift_shared_quality_parent_target_to_triggering_source(effect: &mut Effect) {
    if let Effect::Mana { produced, .. } = effect {
        lift_mana_production_quantities_to_triggering_source(produced);
    }
}

fn lift_mana_production_quantities_to_triggering_source(produced: &mut ManaProduction) {
    match produced {
        ManaProduction::Colorless { count }
        | ManaProduction::AnyOneColor { count, .. }
        | ManaProduction::AnyCombination { count, .. }
        | ManaProduction::ChosenColor { count, .. }
        | ManaProduction::OpponentLandColors { count }
        | ManaProduction::AnyCombinationOfObjectColors { count, .. }
        | ManaProduction::AnyTypeProduceableBy { count, .. }
        | ManaProduction::AnyInCommandersColorIdentity { count, .. }
        | ManaProduction::AnyOneColorAmongPermanents { count, .. } => {
            lift_quantity_expr_shared_quality_parent_target_to_triggering_source(count);
        }
        ManaProduction::Fixed { .. }
        | ManaProduction::Mixed { .. }
        | ManaProduction::ChoiceAmongExiledColors { .. }
        | ManaProduction::ChoiceAmongCombinations { .. }
        | ManaProduction::DistinctColorsAmongPermanents { .. }
        | ManaProduction::TriggerEventManaType => {}
    }
}

fn lift_quantity_expr_shared_quality_parent_target_to_triggering_source(expr: &mut QuantityExpr) {
    match expr {
        QuantityExpr::Ref { qty } => {
            lift_quantity_ref_shared_quality_parent_target_to_triggering_source(qty);
        }
        QuantityExpr::DivideRounded { inner, .. }
        | QuantityExpr::Offset { inner, .. }
        | QuantityExpr::ClampMin { inner, .. }
        | QuantityExpr::Multiply { inner, .. }
        | QuantityExpr::UpTo { max: inner }
        | QuantityExpr::Power {
            exponent: inner, ..
        } => lift_quantity_expr_shared_quality_parent_target_to_triggering_source(inner),
        QuantityExpr::Difference { left, right } => {
            lift_quantity_expr_shared_quality_parent_target_to_triggering_source(left);
            lift_quantity_expr_shared_quality_parent_target_to_triggering_source(right);
        }
        QuantityExpr::Sum { exprs } | QuantityExpr::Max { exprs } => {
            for inner in exprs {
                lift_quantity_expr_shared_quality_parent_target_to_triggering_source(inner);
            }
        }
        QuantityExpr::Fixed { .. } => {}
    }
}

fn lift_quantity_ref_shared_quality_parent_target_to_triggering_source(qty: &mut QuantityRef) {
    match qty {
        QuantityRef::ObjectCount { filter }
        | QuantityRef::ObjectCountBySharedQuality { filter, .. } => {
            lift_filter_shared_quality_parent_target_to_triggering_source(filter);
        }
        _ => {}
    }
}

fn lift_filter_shared_quality_parent_target_to_triggering_source(filter: &mut TargetFilter) {
    match filter {
        TargetFilter::Typed(typed) => {
            for prop in &mut typed.properties {
                if let FilterProp::SharesQuality {
                    quality: SharedQuality::CreatureType,
                    reference: Some(reference),
                    ..
                } = prop
                {
                    if matches!(reference.as_ref(), TargetFilter::ParentTarget) {
                        **reference = TargetFilter::TriggeringSource;
                    }
                }
            }
        }
        TargetFilter::Or { filters } | TargetFilter::And { filters } => {
            for filter in filters {
                lift_filter_shared_quality_parent_target_to_triggering_source(filter);
            }
        }
        TargetFilter::Not { filter } => {
            lift_filter_shared_quality_parent_target_to_triggering_source(filter);
        }
        _ => {}
    }
}

/// CR 608.2c: Does this effect introduce a freshly *chosen* object target — one
/// that a subsequent "that <object>" (`ParentTarget`) anaphor binds to, rather
/// than the trigger event? True for player-selected object filters (`Typed`,
/// and `Or`/`And` combinations of them); false for anaphoric/contextual refs
/// (`ParentTarget`, `TriggeringSource`, `SelfRef`, …) and untargeted effects.
///
/// Effects that write `state.last_revealed_ids` also return `true`: sub-abilities
/// that follow them bind `ParentTarget` to those revealed objects — NOT to the
/// trigger event source. Allowing the lift to descend past a `RevealTop` would
/// rewrite the sub-ability's `ChangeZone.target` from `ParentTarget` to
/// `TriggeringSource`, causing the engine to put the *trigger source* (e.g.,
/// Coiling Oracle) onto the battlefield instead of the revealed land card,
/// which then re-emits a `ZoneChanged` event and loops the ETB trigger
/// (CR 603.2g: triggers fire only when their specific event occurs — the
/// trigger source must be the entering object).
fn introduces_chosen_object_target(effect: &Effect) -> bool {
    // CR 608.2c + CR 603.2g: Effects that populate state.last_revealed_ids
    // introduce revealed objects. Sub-ability ParentTarget binds to those
    // objects, not to the trigger event source. Stopping the lift here prevents
    // TriggeringSource from overriding the injected last_revealed_ids targets
    // in downstream ChangeZone sub-abilities.
    if matches!(
        effect,
        Effect::RevealTop { .. } | Effect::Dig { .. } | Effect::RevealUntil { .. } | Effect::Clash
    ) {
        return true;
    }
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
        // CR 603.2h: "Do this only once each turn" is functionally equivalent.
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
    if let Some((_, constraint, _)) = scan_preceded(lower, parse_nth_spell_this_turn_intervening_if)
    {
        return Some(constraint);
    }
    None
}

fn parse_nth_spell_this_turn_intervening_if(input: &str) -> OracleResult<'_, TriggerConstraint> {
    let (rest, _) = alt((
        tag::<_, _, OracleError<'_>>("if it's the "),
        tag("if it is the "),
    ))
    .parse(input)?;
    let (n, rest) = parse_ordinal(rest).ok_or_else(|| oracle_err(rest))?;
    let rest = rest.trim_start();

    if let Ok((rest, _)) = alt((
        tag::<_, _, OracleError<'_>>("spell you cast this turn"),
        tag("spell you've cast this turn"),
    ))
    .parse(rest)
    {
        return Ok((
            rest,
            TriggerConstraint::NthSpellThisTurn { n, filter: None },
        ));
    }

    let (rest, type_text) = take_until(" spell ").parse(rest)?;
    let (rest, _) = alt((
        tag::<_, _, OracleError<'_>>(" spell you cast this turn"),
        tag(" spell you've cast this turn"),
    ))
    .parse(rest)?;
    let filter = type_only_filter(type_text.trim()).ok_or_else(|| oracle_err(type_text))?;
    Ok((
        rest,
        TriggerConstraint::NthSpellThisTurn {
            n,
            filter: Some(filter),
        },
    ))
}

fn constrain_triggering_spell_with_nth_filter(def: &mut TriggerDefinition) {
    let filter = match &def.constraint {
        Some(TriggerConstraint::NthSpellThisTurn {
            filter: Some(filter),
            ..
        }) => filter.clone(),
        _ => return,
    };

    def.valid_card = Some(match def.valid_card.take() {
        None | Some(TargetFilter::Any) => filter,
        Some(existing) if existing == filter => existing,
        Some(existing) => TargetFilter::And {
            filters: vec![existing, filter],
        },
    });
}

/// CR 601.2a + CR 603.4: Recognize the disjunctive "first-of-type this turn"
/// intervening-if — "if it's the first instant spell, the first sorcery spell,
/// or the first Otter spell other than ~ you've cast this turn". Each disjunct
/// binds the triggering spell to its type AND to the ordinal-of-type count via a
/// composed `And(TriggeringSpellMatchesFilter(filter),
/// QuantityComparison(SpellsCastThisTurn{Controller, filter} == ordinal))`,
/// collected into `TriggerCondition::Or`. Requires >= 2 disjuncts so a
/// single-disjunct card (Vengevine, the NthSpellThisTurn constraint class) falls
/// through to the untouched fire-time `TriggerConstraint::NthSpellThisTurn` path.
fn parse_disjunctive_first_spell_intervening_if<'a>(
    input: &'a str,
    card_name: &str,
) -> OracleResult<'a, TriggerCondition> {
    let (mut rest, _) = alt((tag("if it's the "), tag("if it is the "))).parse(input)?;
    let mut disjuncts = Vec::new();
    loop {
        let (next, disjunct) = parse_first_spell_disjunct(rest, card_name)?;
        disjuncts.push(disjunct);
        rest = next;
        // Disjunct separator: ", the " / ", or the " continues to the next filter.
        match alt((tag::<_, _, OracleError<'_>>(", or the "), tag(", the "))).parse(rest) {
            Ok((next, _)) => rest = next,
            Err(_) => break,
        }
    }
    // Only a genuine >= 2-way OR belongs here; a single disjunct is the
    // constraint path's (NthSpellThisTurn) job.
    if disjuncts.len() < 2 {
        return Err(oracle_err(input));
    }
    let (rest, _) = alt((
        tag::<_, _, OracleError<'_>>(" you've cast this turn"),
        tag(" you cast this turn"),
    ))
    .parse(rest)?;
    let (rest, _) = opt(tag::<_, _, OracleError<'_>>(",")).parse(rest)?;
    Ok((
        rest,
        TriggerCondition::Or {
            conditions: disjuncts,
        },
    ))
}

/// One disjunct of the disjunctive first-of-type intervening-if:
/// "first [type] spell" optionally followed by " other than ~". The self-name
/// exclusion emits `Not(Named{card_name})` on the filter — the oracle normalizes
/// the card's own name to "~", and spell-history / spell-object name matching
/// keys on the full card name (CR 201.2). The same filter appears in both the
/// `TriggeringSpellMatchesFilter` anchor and the `SpellsCastThisTurn` count.
fn parse_first_spell_disjunct<'a>(
    input: &'a str,
    card_name: &str,
) -> OracleResult<'a, TriggerCondition> {
    let (n, rest) = parse_ordinal(input).ok_or_else(|| oracle_err(input))?;
    let (rest, type_text) = take_until(" spell").parse(rest)?;
    let (rest, _) = tag(" spell").parse(rest)?;
    let (rest, exclusion) = opt(tag::<_, _, OracleError<'_>>(" other than ~")).parse(rest)?;
    let base = type_only_filter(type_text.trim()).ok_or_else(|| oracle_err(type_text))?;
    let filter = match exclusion {
        Some(_) => TargetFilter::And {
            filters: vec![
                base,
                TargetFilter::Not {
                    filter: Box::new(TargetFilter::Named {
                        name: card_name.to_string(),
                    }),
                },
            ],
        },
        None => base,
    };
    let disjunct = TriggerCondition::And {
        conditions: vec![
            TriggerCondition::TriggeringSpellMatchesFilter {
                filter: filter.clone(),
            },
            TriggerCondition::QuantityComparison {
                lhs: QuantityExpr::Ref {
                    qty: QuantityRef::SpellsCastThisTurn {
                        scope: CountScope::Controller,
                        filter: Some(filter),
                    },
                },
                comparator: Comparator::EQ,
                rhs: QuantityExpr::Fixed { value: n as i32 },
            },
        ],
    };
    Ok((rest, disjunct))
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

/// CR 118.12a + CR 107.4: Parse mana / energy unless-payment tails after
/// "they pay " / "pays ". Disjunctive "{B} or {3}" lowers to `OneOf` of mana
/// branches (Lim-Dul's Hex); single-mana, dynamic-{X}, and for-each-scaling
/// forms are unchanged.
fn parse_unless_mana_payment(cost_str: &str) -> Option<AbilityCost> {
    let trimmed = cost_str.trim().trim_end_matches('.').trim();

    if let Some(costs) = super::oracle_cost::parse_or_separated_mana_costs(trimmed) {
        return Some(AbilityCost::OneOf {
            costs: costs
                .into_iter()
                .map(|cost| AbilityCost::Mana { cost })
                .collect(),
        });
    }

    if let Some((amount, rest)) = super::oracle_effect::parse_fixed_energy_unless_cost(trimmed) {
        if !rest.trim().is_empty() {
            return None;
        }
        return Some(AbilityCost::PayEnergy {
            amount: QuantityExpr::Fixed {
                value: amount as i32,
            },
        });
    }

    if let Ok((after_cost, _)) = tag::<_, _, OracleError<'_>>("{x}").parse(trimmed) {
        if tag::<_, _, OracleError<'_>>("{")
            .parse(after_cost.trim_start())
            .is_err()
        {
            if let Some(quantity) = super::oracle_effect::parse_where_x_is(after_cost) {
                return Some(AbilityCost::ManaDynamic { quantity });
            }
            let after_x = after_cost.trim().trim_start_matches(',').trim();
            let after_x_lower = after_x.to_lowercase();
            if tag::<_, _, OracleError<'_>>("where x is ")
                .parse(after_x_lower.as_str())
                .is_ok()
            {
                return None;
            }
            return Some(AbilityCost::ManaDynamic {
                quantity: QuantityExpr::Ref {
                    qty: QuantityRef::Variable {
                        name: "X".to_string(),
                    },
                },
            });
        }
    }

    let (mana_cost, after_cost) = super::oracle_effect::parse_unless_mana_cost_prefix(trimmed)?;
    if mana_cost == crate::types::mana::ManaCost::NoCost
        || mana_cost == crate::types::mana::ManaCost::zero()
    {
        return None;
    }
    if let Some(cost) = super::oracle_effect::parse_unless_for_each_payment(after_cost, &mana_cost)
    {
        return Some(cost);
    }
    Some(AbilityCost::Mana { cost: mana_cost })
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

    // CR 603.7a + CR 118.12a (issue #4369): when the "unless you pay" lives in a
    // DELAYED-trigger sentence ("At the beginning of [the/your] next ..., sacrifice
    // it unless you pay {cost}" — Ashling, the Limitless; Satya, Aetherflux
    // Genius), the alternative cost belongs to that delayed sacrifice, NOT the
    // parent trigger. Hoisting it here makes the engine demand payment when the
    // token is created (wrong time). Decline to hoist; the per-clause path
    // (`extract_resolution_unless_pay_modifier`, applied in `lower_effect_chain_ir`
    // before wrapping in `CreateDelayedTrigger`) attaches the cost to the delayed
    // trigger's inner def. Detected by reusing the canonical delayed-temporal-
    // prefix recognizer on the start of the sentence that contains the "unless".
    // Forward-scan past each sentence boundary preceding the "unless" (via the
    // `split_once_on` combinator) so `enclosing` is just the sentence that
    // directly contains it; the byte offset back into `text` preserves original
    // case for `strip_temporal_prefix`.
    let mut enclosing = &lower[..unless_pos];
    while let Ok((_, (_, after))) = nom_primitives::split_once_on(enclosing, ". ") {
        enclosing = after;
    }
    let unless_sentence_start = unless_pos - enclosing.len();
    if crate::parser::oracle_effect::lower::strip_temporal_prefix(
        text[unless_sentence_start..].trim_start(),
    )
    .1
    .is_some()
    {
        return (text.to_string(), None);
    }

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

    // CR 118.12a: "[trigger] ... unless they/that player/that opponent
    // {sacrifice|discard|pay life} [or ...]" — same disjunctive non-mana
    // unless-cost shape the resolution-time path handles. Delegate to the
    // single authority (`parse_unless_they_alt_cost_chain`) so trigger-side
    // and resolution-side parse identically. The chain requires an explicit
    // pronoun ("they"/"that player"/"that opponent"), so "you"/mana forms
    // fall through to the existing blocks unchanged.
    //
    // GUARD: the chain's per-branch `unless_branch_boundary` stops at the
    // first sentence terminator, so it would greedily strip the first
    // unless-clause of a multi-sentence modal effect ("... unless they
    // discard a card. If you're the monarch, instead ..." — Court of
    // Ambition). A later "if"-sentence means the whole effect is not a single
    // terminal unless-cost; defer to the gap path (mirrors the `unless`
    // dispatch guard below at the `Unsupported unless clause` site).
    if !has_later_sentence_if(&lower) {
        if let Some(cost) = parse_unless_they_alt_cost_chain(after_unless) {
            let payer = infer_pronoun_unless_payer(&lower[..unless_pos], condition_lower)
                .unwrap_or(TargetFilter::TriggeringPlayer);
            let cleaned = text[..unless_pos].trim().to_string();
            return (cleaned, Some(UnlessPayModifier { cost, payer }));
        }
    }

    // CR 115.1 + CR 118.12a: "target opponent/target player pays {cost}" — the
    // trigger's declared player target (chosen at stack placement, CR 603.3d)
    // restated as the unless-payer. Represented as a declared-target `Typed`
    // payer (resolved from ability.targets), distinct from the anaphoric "that
    // opponent" (-> TriggeringPlayer) handled above. The verb+cost remainder is
    // parsed by the shared `parse_unless_they_branch_by_verb` authority so the
    // full cost taxonomy (pays N life, sacrifices, discards) is covered.
    let declared_target_payer: Result<(&str, TargetFilter), _> = alt((
        value(
            TargetFilter::Typed(TypedFilter::default().controller(ControllerRef::Opponent)),
            tag::<_, _, OracleError<'_>>("target opponent "),
        ),
        value(
            TargetFilter::Typed(TypedFilter::default()),
            tag("target player "),
        ),
    ))
    .parse(after_unless);
    if let Ok((after_payer, payer)) = declared_target_payer {
        if let Some((cost, _)) = parse_unless_they_branch_by_verb(after_payer) {
            let cleaned = text[..unless_pos].trim().to_string();
            return (cleaned, Some(UnlessPayModifier { cost, payer }));
        }
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

    let Some(cost) = parse_unless_mana_payment(cost_str) else {
        return (text.to_string(), None);
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

/// CR 613.1 + CR 503.1a: "At the beginning of the chosen player's upkeep"
/// introduces the source's persisted opponent choice as the phase actor.
/// Effect pronouns ("that player", "their hand") and trigger matching must
/// read `SourceChosenPlayer`, not every active player at upkeep.
fn condition_introduces_chosen_player_phase(cond_lower: &str) -> bool {
    let phase_scope = preceded(
        tag::<_, _, OracleError<'_>>("at the beginning of "),
        alt((
            tag("the chosen player's "),
            tag("the chosen player\u{2019}s "),
            tag("the chosen players "),
            tag("the chosen players\u{2019} "),
            tag("chosen player's "),
            tag("chosen player\u{2019}s "),
            tag("chosen players "),
            tag("chosen players\u{2019} "),
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
    // CR 608.2c + CR 608.2f: in "each opponent [does X] unless they pay", the
    // lowered ability has `player_scope = Opponent`; the runtime fan-out binds
    // `ability.scoped_player` to each scoped opponent per iteration. The payer
    // must read that per-iteration binding via `ScopedPlayer` —
    // `resolve_effect_player_ref` maps `ScopedPlayer -> ability.scoped_player`
    // (targeting.rs). `Controller` would wrongly resolve to `state.active_player`
    // (effects/mod.rs), which is not the scoped opponent on a non-active turn.
    if scan_contains(effect_before_unless, "each opponent ") {
        return Some(TargetFilter::ScopedPlayer);
    }
    // CR 608.2b + CR 115.4: "... deals damage to target opponent/player
    // unless that player/they sacrifice ..." — the chosen player target pays
    // the unless cost (Demanding Dragon, Tergrid's Lantern-style punishers).
    if scan_contains(effect_before_unless, "target opponent")
        || scan_contains(effect_before_unless, "target player")
    {
        return Some(TargetFilter::Player);
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

    // CR 118.12 + CR 701.9: Try numeric count first ("two cards", "three cards"),
    // then fall back to article form ("a card", "an enchantment card").
    if let Some((n, after_num)) = parse_number(trailing) {
        let after_num = after_num.trim();
        if let Ok((rest, _)) = tag::<_, _, OracleError<'_>>("cards").parse(after_num) {
            let rest = rest.trim().trim_end_matches('.').trim();
            if rest.is_empty()
                || tag::<_, _, OracleError<'_>>("at random")
                    .parse(rest)
                    .is_ok()
            {
                return Some(AbilityCost::Discard {
                    count: QuantityExpr::Fixed { value: n as i32 },
                    filter: None,
                    selection: crate::types::ability::CardSelectionMode::Chosen,
                    self_scope: crate::types::ability::DiscardSelfScope::FromHand,
                });
            }
        }
    }

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
                    selection: crate::types::ability::CardSelectionMode::Chosen,
                    self_scope: crate::types::ability::DiscardSelfScope::FromHand,
                });
            }
        }
        if let Some(filter) = super::oracle_effect::imperative::parse_discard_card_filter(trailing)
        {
            return Some(AbilityCost::Discard {
                count: QuantityExpr::Fixed { value: 1 },
                filter: Some(filter),
                selection: crate::types::ability::CardSelectionMode::Chosen,
                self_scope: crate::types::ability::DiscardSelfScope::FromHand,
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
/// - `you discard [N] card(s)[ at random][.]` → `AbilityCost::Discard { count, .. }`
/// - `you sacrifice [N] [filter][.]`          → `AbilityCost::Sacrifice { count, filter }`
/// - `you pay N life[.]`                      → `AbilityCost::PayLife { amount }`
/// - `you mill [N] card(s)[.]`  → `AbilityCost::Mill { count }`
/// - `you remove [N] [type] counter(s) from [target][.]` → `AbilityCost::RemoveCounter`
///
/// Returns `None` for any other shape (mana costs and unknown forms fall
/// through to the existing mana-cost path in `extract_unless_pay_modifier`).
///
/// FIDELITY NOTE: `UnlessCost::DiscardCard` does not currently model "at random"
/// — the engine resolves via `WaitingFor::WardDiscardChoice` (player-chosen).
/// This is a known sub-fidelity gap (Balduvian Horde class). Post the
/// 2026-05-09 fold, the `random: bool` field on `AbilityCost::Discard` is
/// the natural home for this; wiring it into the runtime is future work.
pub(crate) fn parse_unless_alt_cost(after_unless: &str) -> Option<AbilityCost> {
    // CR 118.12 + CR 202.1: "you pay its mana cost" / "you pay ~'s mana cost" —
    // the unless cost is the ability source's OWN printed mana cost, which is
    // dynamic: it depends on the permanent the granting Aura is attached to
    // (Pendrell Flux, Disruption Aura, Pendrell Mists). Represent it with
    // `ManaCost::SelfManaCost`; the unless-pay resolver materializes it to the
    // source's `mana_cost` at resolution time. Checked before the "you pay N
    // life" arm below, with which it shares the "you pay " prefix.
    if let Ok((rest, _)) = tag::<_, _, OracleError<'_>>("you pay ").parse(after_unless) {
        let tail = rest.trim_end_matches('.').trim();
        let is_self_mana_cost = tag::<_, _, OracleError<'_>>("its mana cost")
            .parse(tail)
            .or_else(|_| tag::<_, _, OracleError<'_>>("~'s mana cost").parse(tail))
            .map(|(after, _)| after.trim().is_empty())
            .unwrap_or(false);
        if is_self_mana_cost {
            return Some(AbilityCost::Mana {
                cost: crate::types::mana::ManaCost::SelfManaCost,
            });
        }
    }
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

    // CR 107.14 + CR 202.3 (issue #4369): "you pay an amount of {E} equal to ..."
    // — a dynamic energy alternative (Satya, Aetherflux Genius's delayed
    // "sacrifice that token unless you pay an amount of {E} equal to its mana
    // value"). Must precede the brace-run mana arm below ("an amount of" is not a
    // mana run). Reuses the shared dynamic-energy building block.
    if let Ok((rest, _)) = tag::<_, _, OracleError<'_>>("you pay ").parse(after_unless) {
        if let Some(amount) = super::oracle_effect::parse_dynamic_energy_unless_cost(
            rest.trim_end_matches('.').trim(),
        ) {
            return Some(AbilityCost::PayEnergy { amount });
        }
    }

    // CR 118.12 + CR 202.1: "you pay {explicit mana}" — an explicit mana cost as
    // the alternative (issue #4369, Ashling, the Limitless's delayed "sacrifice
    // it unless you pay {W}{U}{B}{R}{G}"). The trigger-level caller reaches its
    // own brace-run mana arm after this function returns None, but the per-clause
    // resolution path (`extract_resolution_unless_pay_modifier`) relies solely on
    // this single authority, so a delayed sub-clause's mana unless is invisible
    // without it. Placed AFTER the "N life" arm so life keeps precedence; reuses
    // the shared `parse_unless_mana_payment` building block.
    if let Ok((rest, _)) = tag::<_, _, OracleError<'_>>("you pay ").parse(after_unless) {
        if let Some(cost) = parse_unless_mana_payment(rest.trim_end_matches('.').trim()) {
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

    // CR 118.12 + CR 701.20a: "you tap [count] untapped [filter] you control".
    // The tail parser extracts count and filter via shared target parsing.
    if let Ok((rest, _)) = tag::<_, _, OracleError<'_>>("you tap ").parse(after_unless) {
        if let Some(cost) = parse_unless_tap_untapped_cost(rest) {
            return Some(cost);
        }
    }

    // CR 118.12 + CR 701.7: "you exile a card from your graveyard" — exile one
    // card from your graveyard as an alternative cost. Card: Rotting Giant.
    if let Ok((rest, _)) = tag::<_, _, OracleError<'_>>("you exile ").parse(after_unless) {
        if let Some(cost) = parse_unless_exile_cost(rest) {
            return Some(cost);
        }
    }

    // CR 118.12 + CR 701.17: "you mill [N] cards" — mill as unless cost.
    // Cards: Deep Spawn.
    if let Ok((rest, _)) = tag::<_, _, OracleError<'_>>("you mill ").parse(after_unless) {
        if let Some(cost) = parse_unless_mill_cost(rest) {
            return Some(cost);
        }
    }

    // CR 118.12 + CR 122.1: "you remove [N] [type] counter(s) from [target]"
    // — remove counter(s) as unless cost. Cards: Chisei, Junk Golem, Magmatic
    // Sprinter.
    if let Ok((rest, _)) = tag::<_, _, OracleError<'_>>("you remove ").parse(after_unless) {
        if let Some(cost) = parse_unless_remove_counter_cost(rest) {
            return Some(cost);
        }
    }

    None
}

/// CR 118.12 + CR 701.20a: Parse the tail of "you tap ..." unless costs.
/// Supports articles and numeric counts before delegating the filter phrase to
/// the shared target parser.
fn parse_unless_tap_untapped_cost(rest: &str) -> Option<AbilityCost> {
    let (count, filter) = parse_unless_counted_target_filter(rest)?;
    Some(AbilityCost::TapCreatures {
        requirement: TapCreaturesRequirement::count(count),
        filter,
    })
}

/// CR 118.12 + CR 701.7: Parse the tail of "you exile ..." unless costs.
/// Supports articles and numeric counts before delegating the filter phrase to
/// the shared target parser.
fn parse_unless_exile_cost(rest: &str) -> Option<AbilityCost> {
    let (count, filter) = parse_unless_counted_target_filter(rest)?;
    Some(AbilityCost::Exile {
        count,
        zone: filter.extract_in_zone(),
        filter: Some(filter),
    })
}

/// CR 118.12 + CR 701.17: Parse the tail of "you mill ..." unless costs.
/// Expects "[N] card(s)" after the "you mill " prefix. Cards: Deep Spawn.
fn parse_unless_mill_cost(rest: &str) -> Option<AbilityCost> {
    let trimmed = rest.trim().trim_end_matches('.').trim();
    let (count, after) = parse_number(trimmed)?;
    let after = after.trim();
    if tag::<_, _, OracleError<'_>>("cards").parse(after).is_ok()
        || tag::<_, _, OracleError<'_>>("card").parse(after).is_ok()
    {
        return Some(AbilityCost::Mill { count });
    }
    None
}

/// CR 118.12 + CR 122.1: Parse the tail of "you remove ..." unless costs.
/// Expects "[N] [counter_type] counter(s) from [target]".
///
/// - "a counter from a permanent you control" → `CounterMatch::Any`
/// - "a +1/+1 counter from it" → `CounterMatch::OfType(Plus1Plus1)`, `SelfRef`
/// - "two oil counters from it" → count 2, `CounterMatch::OfType(Oil)`, `SelfRef`
///
/// Cards: Chisei Heart of Oceans, Junk Golem, Magmatic Sprinter.
fn parse_unless_remove_counter_cost(rest: &str) -> Option<AbilityCost> {
    use crate::types::ability::CounterCostSelection;
    use crate::types::counter::CounterMatch;

    let trimmed = rest.trim().trim_end_matches('.').trim();

    // Parse count: numeric word ("two") or article ("a"/"an" → 1).
    let (count, after_count) = if let Some((n, after)) = parse_number(trimmed) {
        (n, after.trim())
    } else {
        let after = alt((
            tag::<_, _, OracleError<'_>>("a "),
            tag::<_, _, OracleError<'_>>("an "),
        ))
        .parse(trimmed)
        .map(|(rest, _)| rest)
        .ok()?;
        (1u32, after.trim())
    };

    // Parse counter type. Try bare "counter(s) from" first (→ CounterMatch::Any),
    // because `parse_counter_type_typed` has a catch-all fallback that would
    // consume "counter" as `CounterType::Generic("counter")`. Only after the
    // bare-noun path fails do we try the typed combinator for "+1/+1 counter",
    // "oil counters", etc.
    let (counter_type, after_counter) = if let Ok((after, _)) = alt((
        tag::<_, _, OracleError<'_>>("counters"),
        tag::<_, _, OracleError<'_>>("counter"),
    ))
    .parse(after_count)
    {
        // Bare "counter"/"counters" without a type word → Any.
        (CounterMatch::Any, after.trim())
    } else if let Ok((after, ct)) = nom_primitives::parse_counter_type_typed(after_count) {
        // Typed counter: consume trailing " counter" / " counters" suffix.
        let after = after.trim();
        let after = alt((
            tag::<_, _, OracleError<'_>>("counters"),
            tag::<_, _, OracleError<'_>>("counter"),
        ))
        .parse(after)
        .map(|(rest, _)| rest)
        .unwrap_or(after);
        (CounterMatch::OfType(ct), after.trim())
    } else {
        return None;
    };

    // Expect "from " followed by a target reference.
    let (after_from, _) = tag::<_, _, OracleError<'_>>("from ")
        .parse(after_counter)
        .ok()?;
    let after_from = after_from.trim();

    // "from it" / "from ~" → SelfRef.
    let target = if tag::<_, _, OracleError<'_>>("it")
        .parse(after_from)
        .map(|(rest, _)| rest.trim().is_empty())
        .unwrap_or(false)
        || tag::<_, _, OracleError<'_>>("~")
            .parse(after_from)
            .map(|(rest, _)| rest.trim().is_empty())
            .unwrap_or(false)
    {
        None // target = None encodes "self" for RemoveCounter
    } else {
        // CR 122.6 + CR 118.12: a TARGETED remove-counter unless-cost
        // (`RemoveCounter { target: Some(_) }`, e.g. Chisei's "from a permanent
        // you control") has no runtime payment path — `handle_unless_payment`
        // leaves it in the unsupported fall-through, so emitting it would make
        // the card worse than unsupported (paying the cost still resolves the
        // punishment). Only the self-reference form ("from it"/"~", target None)
        // is payable. Leave the targeted form unsupported (coverage honesty)
        // until a target-choice payment flow exists.
        return None;
    };

    Some(AbilityCost::RemoveCounter {
        count,
        counter_type,
        target,
        selection: CounterCostSelection::default(),
    })
}

fn parse_unless_counted_target_filter(rest: &str) -> Option<(u32, TargetFilter)> {
    let trimmed = rest.trim();
    let (count, filter_text) = if let Some((n, after_num)) = parse_number(trimmed) {
        (n, after_num.trim().to_string())
    } else {
        let stripped = alt((tag::<_, _, OracleError<'_>>("a "), tag("an ")))
            .parse(trimmed)
            .map(|(rest, _)| rest)
            .unwrap_or(trimmed);
        (1u32, stripped.to_string())
    };

    if filter_text.is_empty() {
        return None;
    }

    let target_phrase = format!("target {filter_text}");
    let (filter, remainder) = super::oracle_target::parse_target(&target_phrase);
    if matches!(filter, TargetFilter::Any) {
        return None;
    }
    let (_, _) = all_consuming((
        opt(tag::<_, _, OracleError<'_>>(".")),
        eof::<_, OracleError<'_>>,
    ))
    .parse(remainder.trim())
    .ok()?;

    Some((count, filter))
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
    // CR 118.12a + CR 107.4: "pay(s) {mana}" / "{B} or {3}" disjunction
    if let Ok((rest, _)) = alt((tag::<_, _, OracleError<'_>>("pays "), tag("pay "))).parse(input) {
        let boundary = unless_branch_boundary(rest);
        let branch_text = rest[..boundary].trim();
        if let Some(cost) = parse_unless_mana_payment(branch_text) {
            return Some((cost, &rest[boundary..]));
        }
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
    // CR 109.5 + CR 118.12a: "they sacrifice" pins the sacrificed permanent
    // to the player paying the unless-cost. Cost payment evaluates filters with
    // the payer as the source controller (`FilterContext::from_source_with_controller`),
    // so `ControllerRef::You` is the payer-relative scope for target-player,
    // triggering-player, and scoped-player punishers alike.
    let filter = add_controller(filter, ControllerRef::You);
    Some((
        AbilityCost::Sacrifice(SacrificeCost::count(filter, 1)),
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
                    selection: crate::types::ability::CardSelectionMode::Chosen,
                    self_scope: crate::types::ability::DiscardSelfScope::FromHand,
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
                selection: crate::types::ability::CardSelectionMode::Chosen,
                self_scope: crate::types::ability::DiscardSelfScope::FromHand,
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
/// - `any number of creatures with total power 12 or greater`
/// - terminal sentence punctuation
fn parse_unless_sacrifice_filter(rest: &str) -> Option<AbilityCost> {
    // Trim trailing sentence punctuation so it doesn't leak into parse_target.
    let trimmed = rest.trim().trim_end_matches('.').trim();
    if trimmed.is_empty() {
        return None;
    }

    // CR 118.12: "any number of [filter] with total power N or greater"
    if let Ok((power_tail, filter_text)) = preceded(
        tag::<_, _, OracleError<'_>>("any number of "),
        terminated(take_until(" with total power "), tag(" with total power ")),
    )
    .parse(trimmed)
    {
        if let Ok((_, threshold)) = alt((
            terminated(
                nom_primitives::parse_number,
                tag::<_, _, OracleError<'_>>(" or greater"),
            ),
            terminated(
                nom_primitives::parse_number,
                tag::<_, _, OracleError<'_>>(" or more"),
            ),
        ))
        .parse(power_tail.trim())
        {
            if !filter_text.is_empty() {
                let target_phrase = format!("target {}", filter_text.trim());
                let (filter, remainder) = super::oracle_target::parse_target(&target_phrase);
                if !matches!(filter, TargetFilter::Any) && remainder.trim().is_empty() {
                    return Some(AbilityCost::Sacrifice(SacrificeCost::new(
                        filter,
                        SacrificeRequirement::Aggregate {
                            stat: SacrificeAggregateStat::TotalPower,
                            comparator: Comparator::GE,
                            value: threshold as i32,
                        },
                    )));
                }
            }
        }
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

    Some(AbilityCost::Sacrifice(SacrificeCost::count(filter, count)))
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
        QuantityExpr::Ref {
            qty:
                QuantityRef::ObjectCountBySharedQuality {
                    filter,
                    quality,
                    aggregate,
                },
        } => QuantityExpr::Ref {
            qty: QuantityRef::ObjectCountBySharedQuality {
                filter: substitute_another_in_filter(filter),
                quality: quality.clone(),
                aggregate: *aggregate,
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
        QuantityExpr::ClampMin { inner, minimum } => QuantityExpr::ClampMin {
            inner: Box::new(substitute_another_in_expr(inner)),
            minimum: *minimum,
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
        QuantityExpr::Max { exprs } => QuantityExpr::Max {
            exprs: exprs.iter().map(substitute_another_in_expr).collect(),
        },
        other => other.clone(),
    }
}

/// Bridge a `StaticCondition` (from the nom condition parser) to a `TriggerCondition`.
///
/// Parallel to `static_condition_to_ability_condition` in `oracle_effect/mod.rs`.
/// Returns `None` for variants that have no `TriggerCondition` equivalent —
/// the caller falls through to the next strategy.
///
/// Exhaustive on purpose — when you add a `StaticCondition` variant, decide
/// here whether it bridges (CLAUDE.md: bridges must be kept exhaustive).
/// CR 601.2i: Trigger modes whose triggering object is a *spell* (or a spell
/// copy / spell-or-ability cast). For these, a "mana spent to cast it" anaphor
/// in an intervening-if refers to that triggering spell rather than the ability
/// source. See `remap_self_cast_scope_to_triggering_spell`.
fn is_spell_cast_trigger_mode(mode: &TriggerMode) -> bool {
    matches!(
        mode,
        TriggerMode::SpellCast
            | TriggerMode::SpellCopy
            | TriggerMode::SpellCastOrCopy
            | TriggerMode::SpellAbilityCast
            | TriggerMode::SpellAbilityCopy
    )
}

/// CR 400.7d + CR 601.2h: Within a spell-cast trigger's intervening-if, remap
/// every `QuantityRef::ManaSpentToCast { scope: SelfObject }` to
/// `TriggeringSpell`. The condition parser cannot see the trigger mode, so the
/// bare "it"/"this spell" anaphor lands as `SelfObject`; only here, where the
/// owning trigger's mode is known, can it be resolved to the triggering spell.
/// Recurses through the compound (`And`/`Or`/`Not`) and arithmetic wrappers so
/// the remap reaches a `ManaSpentToCast` ref wherever it is nested.
fn remap_self_cast_scope_to_triggering_spell(cond: &mut TriggerCondition) {
    match cond {
        TriggerCondition::QuantityComparison { lhs, rhs, .. } => {
            remap_self_cast_scope_in_quantity(lhs);
            remap_self_cast_scope_in_quantity(rhs);
        }
        TriggerCondition::And { conditions } | TriggerCondition::Or { conditions } => {
            conditions
                .iter_mut()
                .for_each(remap_self_cast_scope_to_triggering_spell);
        }
        TriggerCondition::Not { condition } => remap_self_cast_scope_to_triggering_spell(condition),
        // All other variants are leaves that cannot carry a `ManaSpentToCast`
        // quantity ref — nothing to remap.
        _ => {}
    }
}

/// Recursive `QuantityExpr` companion of
/// `remap_self_cast_scope_to_triggering_spell`. Exhaustive over `QuantityExpr`
/// so a new arithmetic wrapper forces a compile error here rather than silently
/// skipping a nested `ManaSpentToCast` ref.
fn remap_self_cast_scope_in_quantity(expr: &mut QuantityExpr) {
    match expr {
        QuantityExpr::Ref {
            qty:
                QuantityRef::ManaSpentToCast {
                    scope: scope @ CastManaObjectScope::SelfObject,
                    ..
                },
        } => *scope = CastManaObjectScope::TriggeringSpell,
        QuantityExpr::Ref { .. } | QuantityExpr::Fixed { .. } => {}
        QuantityExpr::DivideRounded { inner, .. }
        | QuantityExpr::Offset { inner, .. }
        | QuantityExpr::ClampMin { inner, .. }
        | QuantityExpr::Multiply { inner, .. } => remap_self_cast_scope_in_quantity(inner),
        QuantityExpr::UpTo { max } => remap_self_cast_scope_in_quantity(max),
        QuantityExpr::Power { exponent, .. } => remap_self_cast_scope_in_quantity(exponent),
        QuantityExpr::Difference { left, right } => {
            remap_self_cast_scope_in_quantity(left);
            remap_self_cast_scope_in_quantity(right);
        }
        QuantityExpr::Sum { exprs } | QuantityExpr::Max { exprs } => {
            exprs.iter_mut().for_each(remap_self_cast_scope_in_quantity);
        }
    }
}

// pub(crate) so the runtime gate tests in game/triggers.rs can drive the
// production bridge directly (discriminating against this function rather than a
// hand-built filter). Sole non-test caller remains parse_trigger_line below.
pub(crate) fn static_condition_to_trigger_condition(
    sc: &StaticCondition,
) -> Option<TriggerCondition> {
    match sc {
        StaticCondition::DuringYourTurn => Some(TriggerCondition::DuringPlayersTurn {
            player: PlayerFilter::Controller,
        }),
        StaticCondition::DayNightIs { .. } => None,
        StaticCondition::SharesColorWithMostCommonColorAmongPermanents => None,

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

        // CR 701.64b + CR 702.186b: The harnessed designation has an
        // intervening-if equivalent — an ∞ (Infinity) triggered ability only
        // fires while the source is harnessed. Unlike `SourceIsMonstrous`
        // (static-only), this bridges so the ∞ ability-word gate reaches the
        // trigger's `condition`.
        StaticCondition::SourceIsHarnessed => Some(TriggerCondition::SourceIsHarnessed),

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

        // CR 601.2 + CR 611.3a: "as long as it was cast" — 1:1 bridge to the
        // trigger-side cast-origin check (same `cast_from_zone` field).
        StaticCondition::WasCast { zone } => Some(TriggerCondition::WasCast {
            zone: *zone,
            controller: None,
            owner: None,
        }),

        // CR 702.176a + CR 603.4: Impending's battlefield trigger checks the
        // persistent alternative-cost marker, not whether it was paid this turn.
        StaticCondition::CastVariantPaid { variant } => {
            Some(TriggerCondition::CastVariantPaidPersistent {
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

        // CR 603.4 + CR 608.2c: Source-bound intervening-if predicates bridge
        // directly; the trigger evaluator checks the ability source at
        // detection and resolution.
        StaticCondition::SourceMatchesFilter { filter } => {
            Some(TriggerCondition::SourceMatchesFilter {
                filter: filter.clone(),
            })
        }

        // CR 603.4 + CR 301.5a: "if [it/this permanent/this artifact] is equipped"
        // intervening-if. Source-object-wide, matching the layer evaluator
        // (game/layers.rs SourceIsEquipped) and FilterProp::HasAttachment
        // (game/filter.rs) — neither narrows by card type. The HasAttachment{Equipment}
        // subtype predicate already implies a legal creature host (CR 301.5a/301.5c),
        // so a creature() card-type gate would be redundant AND would diverge from the
        // layer path. TypedFilter::default() -> empty type_filters -> no card-type
        // constraint. SourceExclusion::Include: a permanent is never its own attachment.
        StaticCondition::SourceIsEquipped => Some(TriggerCondition::SourceMatchesFilter {
            filter: TargetFilter::Typed(TypedFilter::default().properties(vec![
                FilterProp::HasAttachment {
                    kind: AttachmentKind::Equipment,
                    controller: None,
                    exclude_source: crate::types::ability::SourceExclusion::Include,
                },
            ])),
        }),
        // CR 603.4 + CR 303.4: "if [it/this permanent/this land] is enchanted"
        // intervening-if. CR 303.4: an Aura enters the battlefield attached to an
        // object OR player -- the host is NOT restricted to creatures. Source-object-wide,
        // matching game/layers.rs SourceIsEnchanted and FilterProp::HasAttachment
        // (game/filter.rs), neither of which narrows by card type.
        // TypedFilter::default() -> empty type_filters -> no card-type constraint.
        StaticCondition::SourceIsEnchanted => Some(TriggerCondition::SourceMatchesFilter {
            filter: TargetFilter::Typed(TypedFilter::default().properties(vec![
                FilterProp::HasAttachment {
                    kind: AttachmentKind::Aura,
                    controller: None,
                    exclude_source: crate::types::ability::SourceExclusion::Include,
                },
            ])),
        }),

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
            // CR 725.1: "if you're not the monarch" / "if an opponent is the monarch".
            StaticCondition::IsMonarch => Some(TriggerCondition::Not {
                condition: Box::new(TriggerCondition::IsMonarch),
            }),
            // CR 725.1: "if there is a monarch" (negated no-monarch check).
            StaticCondition::NoMonarch => Some(TriggerCondition::Not {
                condition: Box::new(TriggerCondition::NoMonarch),
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
            StaticCondition::CastVariantPaid { variant } => Some(TriggerCondition::Not {
                condition: Box::new(TriggerCondition::CastVariantPaidPersistent {
                    variant: *variant,
                }),
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
        // CR 726.3: Initiative status bridges directly.
        StaticCondition::IsInitiative => Some(TriggerCondition::IsInitiative),
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

        // CR 614.12c + CR 607.2d: Anchor-word labels bridge directly between
        // static and trigger sides — both query the same persisted
        // `ChosenAttribute::Label` on the source permanent. Lets a single
        // `parse_inner_condition` invocation flow into either an
        // `ability.condition` (trigger-side intervening-if) or a
        // `static_def.condition` (continuous-ability gate).
        StaticCondition::ChosenLabelIs { label } => {
            Some(TriggerCondition::ChosenLabelIs {
                label: label.clone(),
            })
        }

        // CR 400.7 + CR 603.4: "if ~ entered this turn" intervening-if bridges to the
        // trigger-side source-entered check (e.g. Hixus, Prison Warden — "Whenever a
        // creature deals combat damage to you, if Hixus entered this turn, ..."). Both
        // sides read GameObject.entered_battlefield_turn (game/conditions.rs
        // eval_source_entered_this_turn) at trigger fire-time and at the resolution-time
        // recheck, so the intervening-if is honored rather than silently dropped.
        StaticCondition::SourceEnteredThisTurn => {
            Some(TriggerCondition::SourceEnteredThisTurn)
        }

        // Variants with no TriggerCondition equivalent (combat-only / source-state / cost).
        // CR 702.11b + CR 120.3: "has dealt damage since entering" is a static-only
        // Layer-6 gate with no intervening-if (`TriggerCondition`) equivalent.
        StaticCondition::SourceHasDealtDamage
        | StaticCondition::IsRingBearer
        | StaticCondition::RingLevelAtLeast { .. }
        | StaticCondition::DevotionGE { .. }
        | StaticCondition::ChosenColorIs { .. }
        | StaticCondition::SpeedGE { .. }
        | StaticCondition::RecipientHasCounters { .. }
        | StaticCondition::RecipientMatchesFilter { .. }
        // CR 509.1b: recipient-scoped block-evasion gate; no intervening-if
        // (`TriggerCondition`) equivalent — lowering returns `None`.
        | StaticCondition::RecipientAttackingOwnerTarget { .. }
        | StaticCondition::DefendingPlayerControls { .. }
        | StaticCondition::SourceAttackingAlone
        | StaticCondition::SourceIsAttacking
        | StaticCondition::SourceIsBlocking
        | StaticCondition::SourceIsBlocked
        | StaticCondition::SourceIsPaired
        | StaticCondition::SourceIsMonstrous
        // CR 110.5b + CR 611.2b: `IsTapped { scope }` is a duration-only
        // target-relative tap condition (Zygon Infiltrator's copy duration). It
        // is never produced as an intervening-if, so there is no
        // `TriggerCondition` equivalent — lowering returns `None`.
        | StaticCondition::IsTapped { .. }
        // CR 702.171b: the saddled designation has no intervening-if
        // (`TriggerCondition`) equivalent.
        | StaticCondition::SourceIsSaddled
        | StaticCondition::SourceAttachedToCreature
        | StaticCondition::OpponentPoisonAtLeast { .. }
        | StaticCondition::UnlessPay { .. }
        | StaticCondition::Unrecognized { .. }
        | StaticCondition::EnchantedIsFaceDown
        | StaticCondition::SourceControllerEquals { .. }
        // CR 702.166a: Bargain payment is a cost-determination predicate with no
        // intervening-if (`TriggerCondition`) equivalent.
        | StaticCondition::AdditionalCostPaid
        | StaticCondition::CastingAsVariant { .. }
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

/// CR 603.4 + CR 601.2 + CR 603.2c + CR 603.10a: Build the disjunctive
/// "entered-from-graveyard OR cast-from-graveyard" intervening-if.
///
/// The entering object either changed zones into the battlefield from a
/// graveyard (`ZoneChangeObjectMatchesFilter`) or was a spell cast from a
/// graveyard (`WasCast`). `owner` carries the graveyard's owner scope: "your
/// graveyard" (Prized Amalgam) restricts the entered-from filter to objects you
/// own; "a graveyard" (any graveyard) leaves it unrestricted.
///
/// The "your graveyard" form (Prized Amalgam) is templated "entered from your
/// graveyard or you cast it from your graveyard" — the cast arm carries an
/// explicit "you cast it" caster clause AND a "your graveyard" owner clause, so
/// the `WasCast` arm scopes BOTH the caster (`cast_controller`) and the
/// origin-zone owner (`owner`, CR 400.3 + CR 404.1). The compact "a graveyard"
/// form (Twilight Diviner) carries neither caster nor owner constraint.
fn graveyard_origin_or_condition(owner: Option<ControllerRef>) -> TriggerCondition {
    let filter = match owner {
        Some(ref controller) => with_owner_scope(TargetFilter::Any, controller.clone()),
        None => TargetFilter::Any,
    };
    TriggerCondition::Or {
        conditions: vec![
            TriggerCondition::ZoneChangeObjectMatchesFilter {
                origin: Some(Zone::Graveyard),
                destination: Zone::Battlefield,
                filter,
            },
            TriggerCondition::WasCast {
                zone: Some(Zone::Graveyard),
                controller: owner.clone(),
                owner,
            },
        ],
    }
}

/// CR 603.4 + CR 603.10a: Recognize the "entered/was-cast from graveyard"
/// disjunctive intervening-if as a single nom combinator covering the whole
/// class — both the compact "a graveyard" form (Twilight Diviner) and the
/// owner-scoped "your graveyard" form with an explicit "you cast it" arm
/// (Prized Amalgam). Returns the parsed condition; the caller excises the
/// matched clause from the effect text.
///
/// Grammar (subject anaphor × graveyard-origin disjunction):
///   "if " ( "it " | "they " )
///   ( <compact-form> | <split-your-form> )
/// where
///   <compact-form>     = "entered or " ( "was" | "were" ) " cast from a graveyard"
///   <split-your-form>  = "entered from your graveyard or you cast it from your graveyard"
fn parse_graveyard_origin_intervening_if(input: &str) -> OracleResult<'_, TriggerCondition> {
    let (rest, _) = tag("if ").parse(input)?;
    let (rest, _) = alt((tag("it "), tag("they "))).parse(rest)?;
    // Compact "a graveyard" form: "entered or (was|were) cast from a graveyard".
    let compact = map(
        (
            tag("entered or "),
            alt((tag("was"), tag("were"))),
            tag(" cast from a graveyard"),
        ),
        |_| graveyard_origin_or_condition(None),
    );
    // Owner-scoped "your graveyard" form with an explicit "you cast it" arm.
    let split_your = map(
        tag("entered from your graveyard or you cast it from your graveyard"),
        |_| graveyard_origin_or_condition(Some(ControllerRef::You)),
    );
    // CR 601.2 + CR 603.4: bare "(was|were) cast from [a|your] graveyard" with no
    // "entered" disjunction (Rocket-Powered Goblin Glider's Mayhem-gated ETB
    // attach: "if it was cast from your graveyard"). This is the cast-origin
    // check alone — `WasCast`, not the entered/cast `Or` the disjunctive forms
    // above build. CR 400.3 + CR 404.1: a graveyard is owner-specific, and this
    // wording carries NO "you cast it" caster clause, so "your graveyard" scopes
    // the origin-zone OWNER (`owner = You`), never the caster. "a graveyard" is
    // unscoped on both axes.
    let bare_cast = map(
        (
            alt((tag("was"), tag("were"))),
            tag(" cast from "),
            alt((
                value(Some(ControllerRef::You), tag("your graveyard")),
                value(None, tag("a graveyard")),
            )),
        ),
        |(_, _, owner)| TriggerCondition::WasCast {
            zone: Some(Zone::Graveyard),
            controller: None,
            owner,
        },
    );
    alt((compact, split_your, bare_cast)).parse(rest)
}

/// CR 701.26 + CR 603.4: "if it's the first time that creature/permanent has become
/// tapped this turn" — intervening-if gating a tap trigger on the triggering
/// object's first tap of the turn (Captain America, Living Legend). The "it's" is
/// the expletive subject; the real object is "that creature" / "that permanent" /
/// "it", which binds to the tapped object carried by the `PermanentTapped` event.
fn parse_first_time_tapped_intervening_if(input: &str) -> OracleResult<'_, TriggerCondition> {
    let (rest, _) = alt((
        tag("if it's the first time "),
        tag("if it is the first time "),
    ))
    .parse(input)?;
    let (rest, _) = alt((tag("that creature"), tag("that permanent"), tag("it"))).parse(rest)?;
    let (rest, _) = tag(" has become tapped this turn").parse(rest)?;
    Ok((rest, TriggerCondition::FirstTimeObjectTappedThisTurn))
}

/// CR 603.4 + CR 601.2: Parse "if you didn't cast it from your hand/exile" or
/// "if you didn't cast it from your graveyard" — negated zone-specific cast
/// provenance intervening-if (Chainer, Nightmare Adept; Phage the Untouchable;
/// Epochrasite). Nom combinator consumed by `scan_preceded` in
/// `extract_if_condition`.
fn parse_negated_cast_from_zone_intervening_if(input: &str) -> OracleResult<'_, TriggerCondition> {
    // Accept both the ASCII (`didn't`) and curly (`didn’t`, U+2019) apostrophe —
    // Scryfall/Oracle text uses the curly form for some printings.
    let (rest, _) = alt((
        tag("if you didn't cast it from "),
        tag("if you didn’t cast it from "),
    ))
    .parse(input)?;
    let (rest, zone) = alt((
        value(Zone::Hand, tag("your hand")),
        value(Zone::Graveyard, tag("your graveyard")),
        value(Zone::Exile, tag("exile")),
    ))
    .parse(rest)?;
    Ok((
        rest,
        TriggerCondition::Not {
            condition: Box::new(scoped_you_cast_from_zone(zone)),
        },
    ))
}

/// CR 601.2 + CR 400.3 + CR 404.1: Build the caster/owner-scoped `WasCast` for a
/// "you cast it from <zone>" intervening-if. The CASTER axis is always you
/// ("you cast it"); the ORIGIN-ZONE-OWNER axis is you only for owner-specific
/// zones ("your hand"/"your graveyard", CR 404.1) and stays unscoped for the
/// shared exile zone ("from exile" carries no possessive). Mirrors the scoped
/// cast arm of `graveyard_origin_or_condition` so both axes remain separately
/// resolvable — an opponent casting your card, or you casting from someone
/// else's owner-specific zone, must not satisfy the scoped condition.
fn scoped_you_cast_from_zone(zone: Zone) -> TriggerCondition {
    // CR 400.3 + CR 404.1: hand/graveyard/library are owner-specific; exile is shared.
    let owner = match zone {
        Zone::Hand | Zone::Graveyard | Zone::Library => Some(ControllerRef::You),
        _ => None,
    };
    TriggerCondition::WasCast {
        zone: Some(zone),
        controller: Some(ControllerRef::You),
        owner,
    }
}

/// CR 603.4 + CR 601.2: Parse "if you cast it from your hand/exile" or
/// "if you cast it from your graveyard" — positive zone-specific cast
/// provenance intervening-if (Myojin cycle enters-with-counter gate).
fn parse_cast_from_zone_intervening_if(input: &str) -> OracleResult<'_, TriggerCondition> {
    let (rest, _) = tag("if you cast it from ").parse(input)?;
    let (rest, zone) = alt((
        value(Zone::Hand, tag("your hand")),
        value(Zone::Graveyard, tag("your graveyard")),
        value(Zone::Exile, tag("exile")),
    ))
    .parse(rest)?;
    Ok((rest, scoped_you_cast_from_zone(zone)))
}

/// Extract an intervening-if condition from effect text.
/// Returns (cleaned effect text, optional condition).
///
/// Card-name-agnostic wrapper for the trigger-condition unit tests, which don't
/// carry the self-name. The self-name is only needed by the disjunctive
/// first-of-type recognizer's "other than ~" self-exclusion, which never fires
/// for these single-condition inputs; production trigger parsing routes through
/// [`extract_if_condition_with_card_name`] with the real card name.
#[cfg(test)]
fn extract_if_condition(text: &str) -> (String, Option<TriggerCondition>) {
    extract_if_condition_with_card_name(text, "")
}

/// Extract an intervening-if condition from effect text.
/// Returns (cleaned effect text, optional condition).
///
/// Architecture: delegates to `parse_inner_condition` (the shared nom combinator)
/// via the `static_condition_to_trigger_condition` bridge for ALL game-state
/// conditions. Only source-referential patterns that require the trigger source
/// as context ("if you cast it", "if it's attacking", ninjutsu costs, "if it was a
/// [type]", defending player) are handled directly here.
fn extract_if_condition_with_card_name(
    text: &str,
    card_name: &str,
) -> (String, Option<TriggerCondition>) {
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

    // CR 601.2a + CR 603.4: disjunctive "if it's the first [type] spell, the first
    // [type] spell, or the first [type] spell other than ~ you've cast this turn"
    // intervening-if (Alania, Divergent Storm). Each disjunct lowers to
    // And(TriggeringSpellMatchesFilter(type), SpellsCastThisTurn{You,type} == n),
    // collected into `TriggerCondition::Or`. The ≥2-disjunct guard inside the
    // recognizer keeps single-disjunct cards (Vengevine + the NthSpellThisTurn
    // constraint class) on the untouched fire-time `TriggerConstraint` path.
    if let Some((prefix, condition, rest)) = scan_preceded(&lower, |i| {
        parse_disjunctive_first_spell_intervening_if(i, card_name)
    }) {
        let clause_len = lower.len() - prefix.len() - rest.len();
        return (
            strip_condition_clause(text, prefix.len(), clause_len),
            Some(condition),
        );
    }

    // --- Source-referential patterns (cannot be StaticConditions) ---
    // These require trigger-source context that StaticCondition can't express.

    // CR 603.4 + CR 601.2: "if you didn't cast it from your hand/graveyard/exile"
    // — negated zone-specific cast check (Chainer, Nightmare Adept; Phage the
    // Untouchable). The entering object must NOT have been cast from the named
    // zone. MUST precede the zoneless "if you cast it" arm: the negated form
    // contains "cast it from" which the zoneless arm's guard would skip, but the
    // negated phrase would otherwise fall through to the effect parser and be
    // misinterpreted as `Not(EffectOutcome(OptionalEffectPerformed))`.
    if let Some((before, condition, rest)) =
        scan_preceded(&lower, parse_negated_cast_from_zone_intervening_if)
    {
        let pos = before.len();
        let clause_len = lower.len() - before.len() - rest.len();
        return (
            strip_condition_clause(text, pos, clause_len),
            Some(condition),
        );
    }

    // CR 603.4 + CR 601.2: "if you cast it from your hand/graveyard/exile" —
    // positive zone-specific cast check. The entering object must have been cast
    // from the named zone. MUST precede the zoneless "if you cast it" arm.
    if let Some((before, condition, rest)) =
        scan_preceded(&lower, parse_cast_from_zone_intervening_if)
    {
        let pos = before.len();
        let clause_len = lower.len() - before.len() - rest.len();
        return (
            strip_condition_clause(text, pos, clause_len),
            Some(condition),
        );
    }

    // CR 701.57a: "if you cast it" — zoneless cast check (Discover ETBs).
    // Guard: must not be followed by " from" (zone-specific variant).
    if let Some(pos) = tp.find("if you cast it") {
        let after = &lower[pos + "if you cast it".len()..];
        if !after.starts_with(" from") {
            return (
                strip_condition_clause(text, pos, "if you cast it".len()),
                Some(TriggerCondition::WasCast {
                    zone: None,
                    controller: None,
                    owner: None,
                }),
            );
        }
    }

    // CR 603.4 + CR 601.2 + CR 603.2c + CR 603.10a: disjunctive
    // "entered/was-cast from [a|your] graveyard" intervening-if. Scan at word
    // boundaries so the clause is recognized wherever it sits in the effect
    // text; `parse_graveyard_origin_intervening_if` covers both the compact
    // "a graveyard" form and the owner-scoped "your graveyard" form (Prized
    // Amalgam) as one combinator.
    if let Some((prefix, condition, rest)) =
        scan_preceded(&lower, parse_graveyard_origin_intervening_if)
    {
        let clause_len = lower.len() - prefix.len() - rest.len();
        return (
            strip_condition_clause(text, prefix.len(), clause_len),
            Some(condition),
        );
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
                        condition: Box::new(TriggerCondition::WasCast {
                            zone: None,
                            controller: None,
                            owner: None,
                        }),
                    },
                    TriggerCondition::ManaSpentCondition {
                        text: "no mana was spent to cast them".to_string(),
                    },
                ],
            }),
        );
    }

    // CR 603.4 + CR 601.2h: "if the amount of mana spent to cast it/that spell
    // was less than/greater than its mana value" — intervening-if for mana-spent
    // comparison triggers (Tokka & Rahzar, Liberator, Urza's Battlethopter).
    if let Some(result) = try_extract_mana_spent_comparison_condition(&lower, text) {
        return result;
    }

    // CR 603.4 + CR 601.2h: "if no mana was spent to cast it/that spell" —
    // intervening-if for free-spell counter triggers (Lavinia / Vexing Bauble).
    if let Some(result) = try_extract_no_mana_spent_condition(&lower, text) {
        return result;
    }

    // CR 702.188a + CR 603.4: "if <pronoun> was cast using web-slinging" —
    // intervening-if gating an ETB trigger on the alternative cast cost
    // (Spiders-Man, Heroic Horde). Mirrors the replacement-side
    // `ReplacementCondition::CastVariantPaid { WebSlinging }` used by Scarlet
    // Spider, reading the same recorded `GameObject.cast_variant_paid`.
    // MUST precede the zoneless "if it was cast" arm below: "if it was cast"
    // is a prefix of "if it was cast using web-slinging" and would shadow it.
    if let Some((before, condition, rest)) =
        scan_preceded(&lower, parse_cast_using_variant_intervening_if)
    {
        let pos = before.len();
        let clause_len = lower.len() - before.len() - rest.len();
        return (
            strip_condition_clause(text, pos, clause_len),
            Some(condition),
        );
    }

    // CR 701.26 + CR 603.4: "if it's the first time that creature has become tapped
    // this turn" — first-tap intervening-if (Captain America, Living Legend). Source/
    // triggering-object-referential, so it cannot lower through a StaticCondition.
    if let Some((before, condition, rest)) =
        scan_preceded(&lower, parse_first_time_tapped_intervening_if)
    {
        let pos = before.len();
        let clause_len = lower.len() - before.len() - rest.len();
        return (
            strip_condition_clause(text, pos, clause_len),
            Some(condition),
        );
    }

    // CR 603.4 + CR 601.2: "if it was cast" — the entering permanent must have
    // been cast (not put onto the battlefield by another effect). Wedding
    // Ring's ETB token-copy is gated this way so the created copy *token* —
    // which is not cast — does not re-trigger the ability infinitely. The
    // literal "if it was cast" is not a substring of "if it wasn't cast"
    // (the latter has "wasn't", not "was cast"), so the two arms are disjoint.
    // NOTE: a zone-specific "if it was cast from <zone>" arm must be ordered
    // BEFORE this one — "if it was cast" is a prefix of "if it was cast from
    // your hand" and would shadow it here.
    let was_cast_pos = tp.find("if it was cast"); // allow-noncombinator: anchor for strip_condition_clause — structural if-clause excision, not parse dispatch
    if let Some(pos) = was_cast_pos {
        return (
            strip_condition_clause(text, pos, "if it was cast".len()),
            Some(TriggerCondition::WasCast {
                zone: None,
                controller: None,
                owner: None,
            }),
        );
    }

    // CR 603.4 + CR 601.2: "if it wasn't cast" — negation of WasCast.
    if let Some(pos) = tp.find("if it wasn't cast") {
        return (
            strip_condition_clause(text, pos, "if it wasn't cast".len()),
            Some(TriggerCondition::Not {
                condition: Box::new(TriggerCondition::WasCast {
                    zone: None,
                    controller: None,
                    owner: None,
                }),
            }),
        );
    }

    // CR 603.4 + CR 701.9a + CR 702.35a: "if it has <alt-cost keyword>" — intervening-if
    // on discard triggers whose subject is the discarded card (Anje Falkenrath: "if it has
    // madness"). MUST precede the zone-change-object "if it " arms below, which would
    // otherwise mis-route this predicate.
    if let Some((before, condition, rest)) = scan_preceded(
        &lower,
        parse_event_object_has_alt_cost_keyword_intervening_if,
    ) {
        let pos = before.len();
        let clause_len = lower.len() - before.len() - rest.len();
        return (
            strip_condition_clause(text, pos, clause_len),
            Some(condition),
        );
    }

    // CR 603.4 + CR 603.6a: "if it wasn't put onto the battlefield with this
    // ability" — anti-recursion intervening-if (Kodama of the East Tree). The
    // entering permanent must NOT have been placed by this very ability, so a
    // permanent Kodama itself puts onto the battlefield does not re-trigger it.
    // Ordered before the positive arm; the two phrases are disjoint ("wasn't put"
    // does not contain "was put"), mirroring the WasCast/Not(WasCast) disjointness.
    if let Some((prefix, _)) = scan_split_at_phrase(&lower, |i| {
        tag::<_, _, OracleError<'_>>("if it wasn't put onto the battlefield with this ability")
            .parse(i)
    }) {
        let pat = "if it wasn't put onto the battlefield with this ability";
        return (
            strip_condition_clause(text, prefix.len(), pat.len()),
            Some(TriggerCondition::Not {
                condition: Box::new(TriggerCondition::PlacedByAbilitySource),
            }),
        );
    }

    // CR 603.4 + CR 603.6a: positive "if it was put onto the battlefield with
    // this ability" — the entering permanent was placed by this very ability.
    if let Some((prefix, _)) = scan_split_at_phrase(&lower, |i| {
        tag::<_, _, OracleError<'_>>("if it was put onto the battlefield with this ability")
            .parse(i)
    }) {
        let pat = "if it was put onto the battlefield with this ability";
        return (
            strip_condition_clause(text, prefix.len(), pat.len()),
            Some(TriggerCondition::PlacedByAbilitySource),
        );
    }

    // CR 603.4 + CR 702.105a: "if it's/is attacking the player with the most
    // life or tied for most life" — the same defending-player-life-total
    // predicate Dethrone's own synthesized trigger uses (`build_dethrone_trigger`,
    // database/synthesis.rs), reused here so a card phrasing the identical CR
    // 702.105a idiom as a plain intervening-if (Scourge of the Throne: "if
    // it's attacking the player with the most life or tied for most life,
    // untap all attacking creatures") gets the SAME typed condition instead of
    // either the wrong bare `SourceIsAttacking` (dropping the life-total
    // qualifier entirely — the phase-rs/phase#5449 review's second finding) or
    // an unconditional `None` (which would make the ability fire regardless of
    // who's being attacked — strictly worse than the original bug). Ordered
    // before the plain "if it's attacking" simple-table entry so the longer,
    // more specific phrase is never partially shadowed by the shorter one.
    if let Some((before, _, rest)) = scan_preceded(&lower, |i| {
        (
            tag::<_, _, OracleError<'_>>("if it"),
            alt((tag("'s"), tag(" is"))),
            tag(" attacking the player with the most life or tied for most life"),
        )
            .parse(i)
    }) {
        let pat_start = before.len();
        let clause_len = lower.len() - before.len() - rest.len();
        return (
            strip_condition_clause(text, pat_start, clause_len),
            Some(TriggerCondition::QuantityComparison {
                lhs: QuantityExpr::Ref {
                    qty: QuantityRef::LifeTotal {
                        player: PlayerScope::DefendingPlayer,
                    },
                },
                comparator: Comparator::GE,
                rhs: QuantityExpr::Ref {
                    qty: QuantityRef::LifeTotal {
                        player: PlayerScope::AllPlayers {
                            aggregate: AggregateFunction::Max,
                            exclude: None,
                        },
                    },
                },
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
            // CR 113.6b: source-scoped "if it's on the battlefield" — the ETB
            // trigger's own source (a reanimator Aura: Animate Dead / Dance of
            // the Dead) resolves only while it is still on the battlefield.
            (
                "if it's on the battlefield",
                TriggerCondition::SourceInZone {
                    zone: Zone::Battlefield,
                },
            ),
            (
                "if it is on the battlefield",
                TriggerCondition::SourceInZone {
                    zone: Zone::Battlefield,
                },
            ),
            // CR 508.1 + CR 603.4: source-scoped "if ~ attacked this turn" —
            // the trigger resolves only if the ability's own source creature
            // declared as an attacker this turn (Riders of the Mark, Taigam,
            // Ojutai Master). Composed from the existing, already-evaluated
            // `FilterProp::AttackedThisTurn` (checked against
            // `state.creatures_attacked_this_turn`) via `SourceMatchesFilter`,
            // so no new `TriggerCondition` variant is needed. Distinct from the
            // player-scoped `YouAttackedThisTurn` ("if you attacked this turn").
            (
                "if ~ attacked this turn",
                TriggerCondition::SourceMatchesFilter {
                    filter: TargetFilter::Typed(
                        TypedFilter::creature()
                            .properties(vec![FilterProp::AttackedThisTurn { defender: None }]),
                    ),
                },
            ),
            // CR 508.1 + CR 509.1 + CR 603.4: source-scoped "if ~ attacked or
            // blocked this turn" — the sibling of the attacked-only arm above,
            // gating on the source creature having attacked OR blocked this turn
            // (Inferno Hellion). Reuses the existing, already-evaluated
            // `FilterProp::AttackedOrBlockedThisTurn` (checked against
            // `state.creatures_attacked_this_turn` / `creatures_blocked_this_turn`)
            // via `SourceMatchesFilter`, so no new `TriggerCondition` variant is
            // needed. The turn-scoped "this turn" form only; the "this combat"
            // form (Clockwork cycle, Kjeldoran Home Guard) needs combat-scoped
            // tracking the engine does not yet keep and is intentionally excluded.
            (
                "if ~ attacked or blocked this turn",
                TriggerCondition::SourceMatchesFilter {
                    filter: TargetFilter::Typed(
                        TypedFilter::creature()
                            .properties(vec![FilterProp::AttackedOrBlockedThisTurn]),
                    ),
                },
            ),
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
            // CR 400.7 / CR 603.4: counter-state intervening-ifs are handled by
            // dedicated combinators, not verbatim entries here, so the type axis
            // (any / typed) composes rather than enumerating every card's list:
            // past-tense event-subject "if it had [no] [<type>] counter(s) on it"
            // by `try_extract_had_counter_condition`, and present-tense
            // source-scoped "if ~ has [a <type>] counter(s) on it" by
            // `try_extract_has_counter_condition`.
            // CR 702.112b: "if it's renowned" — the event-subject creature's designation.
            // CR 702.112a: "if ~ is renowned" — the source permanent's designation.
            (
                "if it's renowned",
                TriggerCondition::IsRenowned {
                    subject: RenownSubject::EventSubject,
                },
            ),
            (
                "if ~ is renowned",
                TriggerCondition::IsRenowned {
                    subject: RenownSubject::Source,
                },
            ),
            // CR 506.2 + CR 508.1b + CR 603.4: "if none of those creatures attacked you" —
            // intervening-if for "whenever another player attacks with N or more creatures"
            // triggers that reward defensive (non-aggressor) opponents.
            (
                "if none of those creatures attacked you",
                TriggerCondition::AttackersDeclaredCount {
                    subject: AttackersDeclaredCountSubject::AttackTarget {
                        controller: ControllerRef::You,
                        attacked: AttackTargetFilter::Player,
                        filter: None,
                    },
                    comparator: Comparator::EQ,
                    count: 0,
                },
            ),
            (
                "if it's the first combat phase of the turn",
                TriggerCondition::FirstCombatPhaseOfTurn,
            ),
            // CR 605.1a + CR 603.4: Bracers-class triggered abilities use
            // "if it isn't a mana ability" as a leading intervening-if after
            // the trigger event.
            (
                "if it isn't a mana ability",
                TriggerCondition::ActivatedAbilityIsNonMana,
            ),
            (
                "if it is not a mana ability",
                TriggerCondition::ActivatedAbilityIsNonMana,
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

    // CR 506.2 + CR 508.1b + CR 603.4: Mangara-class attack-batch
    // intervening-if, "if N or more of those creatures are attacking you
    // and/or planeswalkers you control."
    if let Some(result) = try_extract_attackers_to_controller_min(&lower, text) {
        return result;
    }

    // CR 309.7: "if you haven't completed [dungeon name]" — dynamic dungeon name parsing.
    if let Some(result) = try_extract_not_completed_dungeon(&tp, &lower, text) {
        return result;
    }

    // CR 400.7 + CR 603.10: "if it had [no] [a <type>] counter(s) on it" —
    // past-state counter check (positive, negated, typed, and untyped forms).
    if let Some(result) = try_extract_had_counter_condition(&tp, &lower, text) {
        return result;
    }

    // CR 603.4 + CR 122.1: "if <source> has [quantity] [type] counter(s) on it"
    // — present-tense source-scoped counter check. Delegates the grammar to the
    // shared `parse_source_has_counters` authority (any/typed/quantified forms).
    if let Some(result) = try_extract_has_counter_condition(&tp, &lower, text) {
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

    // CR 702.49 / CR 702.117a / CR 702.137a + CR 603.4: "if [possessive]
    // sneak/ninjutsu/surge/spectacle cost was paid [this turn]"
    // Guard: "instead" means conditional override, not intervening-if.
    if let Some(result) = try_extract_cast_variant_paid_condition(&tp, &lower, text) {
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

    // CR 603.4 + CR 120.1: "if any of that [combat] damage was dealt by
    // <source-chain>" — damage-source-type intervening-if (Mindblade Render).
    if let Some(result) = try_extract_event_damage_source_condition(&lower, text) {
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

    // CR 305.2a + CR 603.4: "if it wasn't the first land you played this turn" —
    // Fastbond's intervening-if. Evaluates to lands_played_this_turn >= 2
    // (the counter is incremented before the LandPlayed event fires, so at
    // detection/resolution time the 2nd land shows count == 2).
    fn first_land_played_condition(input: &str) -> OracleResult<'_, ()> {
        value(
            (),
            tag::<_, _, OracleError<'_>>("if it wasn't the first land you played this turn"),
        )
        .parse(input)
    }
    if let Some((before, _, _)) = scan_preceded(&lower, first_land_played_condition) {
        let pos = before.len();
        let pattern_len = "if it wasn't the first land you played this turn".len();
        return (
            strip_condition_clause(text, pos, pattern_len),
            Some(TriggerCondition::QuantityComparison {
                lhs: QuantityExpr::Ref {
                    qty: QuantityRef::LandsPlayedThisTurn {
                        player: PlayerScope::Controller,
                        from_zones: None,
                    },
                },
                comparator: Comparator::GE,
                rhs: QuantityExpr::Fixed { value: 2 },
            }),
        );
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
    if let Some(result) = try_extract_spell_targets_intervening_if(&tp, &lower, text) {
        return result;
    }
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

/// CR 603.4 + CR 120.1: "if any of that [combat] damage was dealt by
/// <source-chain>" — intervening-if whose subject is the set of sources that
/// dealt the triggering combat damage. An object that deals damage is the
/// source of that damage (CR 120.1); the predicate is true when ANY of those
/// sources matches the parsed `TargetFilter`. Used by Mindblade Render
/// ("Whenever your opponents are dealt combat damage, if any of that damage
/// was dealt by a Warrior, ..."). The source phrase reuses the same
/// `parse_target` + " or " + `merge_or_filters` chain as the
/// `OpponentDealtDamage { source }` PlayerFilter so a `~ or a Dragon`
/// style disjunction composes uniformly.
fn try_extract_event_damage_source_condition(
    lower: &str,
    text: &str,
) -> Option<(String, Option<TriggerCondition>)> {
    fn damage_source_prefix(i: &str) -> nom::IResult<&str, (), OracleError<'_>> {
        let (i, _) = tag("if any of that ").parse(i)?;
        // "combat " is optional: the canonical Oracle wording is "that damage",
        // but "that combat damage" appears on related cards.
        let (i, _) = opt(tag("combat ")).parse(i)?;
        let (i, _) = tag("damage was dealt by ").parse(i)?;
        Ok((i, ()))
    }
    let (before, _, after) = scan_preceded(lower, damage_source_prefix)?;
    let prefix_len = lower.len() - before.len() - after.len();
    let phrase_start = before.len() + prefix_len;
    // The source phrase ends at the clause boundary (comma) or sentence end.
    let phrase_end = after.find([',', '.']).unwrap_or(after.len());
    // Parse the original-case source phrase ("a Warrior", "~ or a Dragon") so
    // subtype canonicalization in `parse_target` sees the printed casing.
    let phrase = &text[phrase_start..phrase_start + phrase_end];
    let filter = parse_event_damage_source_chain(phrase);
    if matches!(filter, TargetFilter::None | TargetFilter::Any) {
        return None;
    }
    let clause_len = prefix_len + phrase_end;
    Some((
        strip_condition_clause(text, before.len(), clause_len),
        Some(TriggerCondition::EventDamageSourceMatchesFilter { filter }),
    ))
}

/// CR 120.1 + CR 608.2i: Fold a "X or Y or ..." damage-source phrase into a
/// single `TargetFilter` via `parse_target` + `merge_or_filters`. Mirrors
/// `parse_source_chain_phrase` in `oracle_quantity.rs` (the
/// `OpponentDealtDamage { source }` builder) so source-restriction
/// parsing stays consistent across the quantity and trigger-condition layers.
fn parse_event_damage_source_chain(phrase: &str) -> TargetFilter {
    let (first, rest) = super::oracle_target::parse_target(phrase.trim());
    let rest = rest.trim_start();
    if let Ok((after, _)) = tag::<_, _, OracleError<'_>>("or ").parse(rest) {
        let second = parse_event_damage_source_chain(after);
        return merge_or_filters(first, second);
    }
    first
}

/// CR 603.4 + CR 111.1: Token intervening-if with `'s not` contraction
/// ("if it's not a token"). The legacy `if it ` + `isn't`/`is not` path
/// already covers explicit negation; only the apostrophe contraction needs
/// a dedicated arm so attachment lookbacks (`if it was enchanted`) keep their
/// leading `was` for `parse_zone_change_object_filter_predicate`.
fn parse_zone_change_object_token_contraction_intervening_if(
    input: &str,
) -> OracleResult<'_, TriggerCondition> {
    let (rest, _) = tag("if it's not a ").parse(input)?;
    let (rest, _) = tag("token").parse(rest)?;
    Ok((rest, zone_change_object_token_condition(true)))
}

/// CR 603.4 + CR 701.9a + CR 702.35a: Build an event-object intervening-if
/// filter for a named alt-cost keyword on the triggering object.
fn event_object_has_alt_cost_keyword_condition(keyword: KeywordKind) -> TriggerCondition {
    TriggerCondition::EventObjectMatchesFilter {
        filter: TargetFilter::Typed(
            TypedFilter::card().properties(vec![FilterProp::HasKeywordKind { value: keyword }]),
        ),
    }
}

/// CR 603.4 + CR 701.9a + CR 702.35a: "if it has <alt-cost keyword>" on
/// event-object intervening-ifs (e.g. discard triggers: "if it has madness").
fn parse_event_object_has_alt_cost_keyword_intervening_if(
    input: &str,
) -> OracleResult<'_, TriggerCondition> {
    let (rest, _) = tag("if it has ").parse(input)?;
    let (rest, keyword) = nom_primitives::parse_alt_cost_keyword_name_to_kind.parse(rest)?;
    Ok((rest, event_object_has_alt_cost_keyword_condition(keyword)))
}

/// CR 603.4 + CR 603.6a + CR 208.1: Entering-object intervening-if comparing the
/// newcomer's power and/or toughness to the source permanent's same stat —
/// "if it has greater power [or toughness] than ~". Subject "it" is the
/// zone-change event object (the entering creature, CR 603.6a), evaluated live
/// in its destination (battlefield); "~" is the ability source (CR 113.7),
/// resolved via `ObjectScope::Source`. An enters-the-battlefield trigger is NOT
/// a CR 603.10a look-back trigger (that list is leaves-the-battlefield,
/// sacrifice, leaves-graveyard, and seen-by-all hand/library moves), so
/// `PtValueScope::Current` reads the post-layer P/T of both objects.
///
/// Each stat needs its OWN source-relative threshold (power→source power,
/// toughness→source toughness), so this cannot reuse
/// `nom_filter::parse_pt_comparison` (shared threshold, stat-before-comparator
/// word order). Disjunction composes via the existing `FilterProp::AnyOf`;
/// the single-stat form emits one `PtComparison`. Mirrors the per-stat
/// slice-map in `nom_filter::parse_pt_comparison` and `parse_combat_alone_props`.
fn parse_entering_pt_vs_source_condition(input: &str) -> OracleResult<'_, TriggerCondition> {
    let (rest, _) = tag("if it has greater ").parse(input)?;
    let (rest, stats): (_, &[PtStat]) = alt((
        value(
            &[PtStat::Power, PtStat::Toughness][..],
            tag("power or toughness"),
        ),
        value(&[PtStat::Power][..], tag("power")),
        value(&[PtStat::Toughness][..], tag("toughness")),
    ))
    .parse(rest)?;
    let (rest, _) = tag(" than ~").parse(rest)?;

    let props: Vec<FilterProp> = stats
        .iter()
        .map(|&stat| {
            // The selector above only ever emits Power/Toughness, so the
            // `TotalPowerToughness` arm is dead-by-construction. Mapping it to
            // `Power { Source }` (rather than `unreachable!()`) keeps the match
            // exhaustive without a panic-bearing arm.
            let qty = match stat {
                PtStat::Power => QuantityRef::Power {
                    scope: ObjectScope::Source,
                },
                PtStat::Toughness => QuantityRef::Toughness {
                    scope: ObjectScope::Source,
                },
                PtStat::TotalPowerToughness => QuantityRef::Power {
                    scope: ObjectScope::Source,
                },
            };
            FilterProp::PtComparison {
                stat,
                scope: PtValueScope::Current,
                comparator: Comparator::GT,
                value: QuantityExpr::Ref { qty },
            }
        })
        .collect();
    let prop = if props.len() == 1 {
        props.into_iter().next().unwrap()
    } else {
        FilterProp::AnyOf { props }
    };

    Ok((
        rest,
        TriggerCondition::ZoneChangeObjectMatchesFilter {
            origin: None,
            destination: Zone::Battlefield,
            filter: TargetFilter::Typed(TypedFilter::creature().properties(vec![prop])),
        },
    ))
}

fn parse_zone_change_object_filter_condition(input: &str) -> OracleResult<'_, TriggerCondition> {
    if let Ok((rest, condition)) = parse_entering_pt_vs_source_condition(input) {
        return Ok((rest, condition));
    }
    if let Ok((rest, condition)) = parse_zone_change_object_token_contraction_intervening_if(input)
    {
        return Ok((rest, condition));
    }
    preceded(tag("if it "), parse_zone_change_object_filter_predicate).parse(input)
}

fn parse_zone_change_object_filter_predicate(input: &str) -> OracleResult<'_, TriggerCondition> {
    if let Ok((rest, condition)) = parse_zone_change_object_token_predicate(input) {
        return Ok((rest, condition));
    }

    let (rest, negated) = alt((
        value(false, tag("was ")),
        value(true, tag("wasn't ")),
        value(true, tag("was not ")),
    ))
    .parse(input)?;

    // CR 506.5: combat-alone predicates are tried first, with the disjunctive
    // "attacking or blocking alone" form ordered ahead of the single-phrase
    // forms so longest-match wins (the same ordering discipline as the other
    // disjunctive look-back conditions in this module). They map to the
    // sole-attacker / sole-blocker `FilterProp`s, which evaluate via the
    // zone-change snapshot per CR 603.10a.
    let (rest, props) = alt((
        parse_combat_alone_props,
        map(
            alt((
                value(FilterProp::Blocking, tag("blocking")),
                map_attachment_kind_filter_prop,
            )),
            |prop| vec![prop],
        ),
    ))
    .parse(rest)?;
    let condition = TriggerCondition::ZoneChangeObjectMatchesFilter {
        origin: Some(Zone::Battlefield),
        destination: Zone::Graveyard,
        filter: TargetFilter::Typed(TypedFilter::creature().properties(props)),
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

/// CR 506.5: Parse the combat-alone predicate phrase of a zone-change
/// look-back intervening-if ("attacking or blocking alone", "attacking alone",
/// "blocking alone"). The disjunctive form is ordered first so it is not
/// shadowed by the single-phrase forms (longest-match precedence). Returns the
/// `FilterProp` list to drop into the creature filter — the disjunction is
/// expressed via the existing typed `FilterProp::AnyOf` rather than bespoke
/// parsing, so it composes with the surrounding negation axis for free.
fn parse_combat_alone_props(input: &str) -> OracleResult<'_, Vec<FilterProp>> {
    alt((
        value(
            vec![FilterProp::AnyOf {
                props: vec![FilterProp::AttackingAlone, FilterProp::BlockingAlone],
            }],
            tag("attacking or blocking alone"),
        ),
        value(vec![FilterProp::AttackingAlone], tag("attacking alone")),
        value(vec![FilterProp::BlockingAlone], tag("blocking alone")),
    ))
    .parse(input)
}

fn zone_change_object_token_condition(negated: bool) -> TriggerCondition {
    // CR 111.1: Tokens represent permanents that are not represented by cards.
    let prop = if negated {
        FilterProp::NonToken
    } else {
        FilterProp::Token
    };
    TriggerCondition::ZoneChangeObjectMatchesFilter {
        origin: None,
        destination: Zone::Battlefield,
        filter: TargetFilter::Typed(TypedFilter::permanent().properties(vec![prop])),
    }
}

fn parse_zone_change_object_token_predicate(input: &str) -> OracleResult<'_, TriggerCondition> {
    let (rest, contracted_negation) = alt((
        value(true, alt((tag("isn't"), tag("wasn't")))),
        value(false, alt((tag("is"), tag("was")))),
    ))
    .parse(input)?;
    let (rest, explicit_negation) = opt(preceded(space1, tag("not"))).parse(rest)?;
    let (rest, _) = space1.parse(rest)?;
    let (rest, _) = tag("a").parse(rest)?;
    let (rest, _) = space1.parse(rest)?;
    let (rest, _) = tag("token").parse(rest)?;

    let negated = contracted_negation || explicit_negation.is_some();
    Ok((rest, zone_change_object_token_condition(negated)))
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

/// CR 603.4 + CR 601.2h: Extract "if the amount of mana spent to cast it/that spell
/// was less than/greater than its mana value" — intervening-if for mana-spent
/// comparison triggers (Tokka & Rahzar, Liberator, Urza's Battlethopter).
fn try_extract_mana_spent_comparison_condition(
    lower: &str,
    text: &str,
) -> Option<(String, Option<TriggerCondition>)> {
    let (before, comparator, rest) = scan_preceded(lower, |i| {
        preceded(tag("if "), parse_mana_spent_comparison_clause).parse(i)
    })?;

    let rest_trimmed = rest.trim_start();
    if !(rest_trimmed.is_empty() || rest_trimmed.starts_with(',') || rest_trimmed.starts_with('.'))
    {
        return None;
    }
    let clause_start = before.len();
    let clause_len = lower.len() - before.len() - rest.len();

    // In a spell-cast trigger's intervening-if clause, "it"/"that spell"/
    // "this spell" all refer to the spell object carried by the trigger event.
    let lhs_qty = QuantityRef::ManaSpentToCast {
        scope: CastManaObjectScope::TriggeringSpell,
        metric: CastManaSpentMetric::Total,
    };
    let rhs_qty = QuantityRef::ObjectManaValue {
        scope: ObjectScope::EventSource,
    };

    let cleaned = strip_condition_clause(text, clause_start, clause_len);
    Some((
        cleaned,
        Some(TriggerCondition::QuantityComparison {
            lhs: QuantityExpr::Ref { qty: lhs_qty },
            comparator,
            rhs: QuantityExpr::Ref { qty: rhs_qty },
        }),
    ))
}

fn parse_mana_spent_comparison_clause(i: &str) -> OracleResult<'_, Comparator> {
    let (i, _) = (
        tag("the amount of mana spent to cast "),
        alt((tag("it"), tag("that spell"), tag("this spell"))),
        alt((tag(" was "), tag(" is "))),
    )
        .parse(i)?;
    let (i, comparator) = alt((
        value(Comparator::LT, tag("less than")),
        value(Comparator::GT, tag("greater than")),
    ))
    .parse(i)?;
    let (i, _) = tag(" its mana value").parse(i)?;
    Ok((i, comparator))
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
        alt((
            tag(" that player's turn"),
            // CR 603.4: Glademuse — "if it's not their turn" (pronoun = triggering player).
            tag(" their turn"),
        )),
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

fn try_extract_attackers_to_controller_min(
    lower: &str,
    text: &str,
) -> Option<(String, Option<TriggerCondition>)> {
    let (before, condition, rest) =
        scan_preceded(lower, parse_attackers_to_controller_min_condition)?;
    let rest_trimmed = rest.trim_start();
    if !(rest_trimmed.is_empty() || rest_trimmed.starts_with(',') || rest_trimmed.starts_with('.'))
    {
        return None;
    }
    let consumed = lower.len() - before.len() - rest.len();
    Some((
        strip_condition_clause(text, before.len(), consumed),
        Some(condition),
    ))
}

fn parse_attackers_to_controller_min_condition(input: &str) -> OracleResult<'_, TriggerCondition> {
    let (rest, _) = tag("if ").parse(input)?;
    let (rest, minimum) = nom_primitives::parse_number.parse(rest)?;
    let (rest, _) = tag(" or more of those creatures are attacking ").parse(rest)?;
    let (rest, attacked) = alt((
        value(
            AttackTargetFilter::PlayerOrPlaneswalker,
            tag("you and/or planeswalkers you control"),
        ),
        value(
            AttackTargetFilter::PlayerOrPlaneswalker,
            tag("you or planeswalkers you control"),
        ),
        value(
            AttackTargetFilter::Planeswalker,
            tag("planeswalkers you control"),
        ),
        value(AttackTargetFilter::Player, tag("you")),
    ))
    .parse(rest)?;
    Ok((
        rest,
        TriggerCondition::AttackersDeclaredCount {
            subject: AttackersDeclaredCountSubject::AttackTarget {
                controller: ControllerRef::You,
                attacked,
                filter: None,
            },
            comparator: Comparator::GE,
            count: minimum,
        },
    ))
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
fn try_extract_spell_targets_intervening_if(
    _tp: &TextPair<'_>,
    lower: &str,
    text: &str,
) -> Option<(String, Option<TriggerCondition>)> {
    let lower_trim = lower.trim();
    let text_trim = text.trim();
    let leading_skip = text.len() - text_trim.len();

    let (rest_lower, filter) = parse_leading_spell_targets_if_clause(lower_trim)?;
    let consumed = lower_trim.len() - rest_lower.len();
    let remaining = text[leading_skip + consumed..].trim_start();

    Some((
        remaining.to_string(),
        Some(TriggerCondition::TriggeringSpellTargetsFilter { filter }),
    ))
}

/// CR 603.4: Leading `if <spell-targets-filter>,` intervening-if on a trigger.
fn parse_leading_spell_targets_if_clause(input: &str) -> Option<(&str, TargetFilter)> {
    let (after_if, _) = tag::<_, _, OracleError<'_>>("if ").parse(input).ok()?;
    let (rest, cond_part) = terminated(take_until::<_, _, OracleError<'_>>(","), tag(","))
        .parse(after_if)
        .ok()?;
    let cond_core = cond_part.trim().trim_end_matches('.').trim();
    let ParsedCondition::SpellTargetsFilter { filter } =
        crate::parser::oracle_condition::parse_spell_targets_filter(cond_core)?
    else {
        return None;
    };
    Some((rest, filter))
}

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
            def.mode = TriggerMode::KeywordAbilityActivated(AbilityTag::Boast);
            return Some((TriggerMode::KeywordAbilityActivated(AbilityTag::Boast), def));
        }
        // CR 602.1 + CR 603.1b: Match "a power-up ability" — Marvel Boy triggers
        // when its controller activates a power-up ability (one of two trigger
        // conditions split from the dual-condition text by `split_and_when_compound`).
        if tag::<_, _, OracleError<'_>>("a power-up ability")
            .parse(rest)
            .is_ok()
        {
            let mut def = make_base();
            def.mode = TriggerMode::KeywordAbilityActivated(AbilityTag::PowerUp);
            return Some((
                TriggerMode::KeywordAbilityActivated(AbilityTag::PowerUp),
                def,
            ));
        }
        #[derive(Clone, Copy)]
        enum KeywordActivationSubject {
            SelfRef,
            Generic,
            Controller,
            Opponent,
        }

        fn parse_outlast_activation_subject(
            input: &str,
        ) -> OracleResult<'_, KeywordActivationSubject> {
            alt((
                value(KeywordActivationSubject::SelfRef, tag("~'s")),
                value(KeywordActivationSubject::Opponent, tag("an opponent's")),
                value(KeywordActivationSubject::Controller, tag("your")),
                value(KeywordActivationSubject::Generic, tag("an")),
            ))
            .parse(input)
        }

        fn parse_outlast_activation_reference(
            input: &str,
        ) -> OracleResult<'_, KeywordActivationSubject> {
            all_consuming(terminated(
                parse_outlast_activation_subject,
                preceded(space1, tag("outlast ability")),
            ))
            .parse(input)
        }

        // CR 702.107a: Match possessive/generic outlast activation subjects.
        // "this creature" normalizes to ~ before trigger parsing, so the self-ref form
        // arrives as "~'s outlast ability"; generic and controller-scoped variants
        // share the same keyword event and differ only by `valid_card`.
        if let Ok((_, subject)) = parse_outlast_activation_reference(rest) {
            let mut def = make_base();
            def.mode = TriggerMode::KeywordAbilityActivated(AbilityTag::Outlast);
            match subject {
                KeywordActivationSubject::SelfRef => {
                    def.valid_card = Some(TargetFilter::SelfRef);
                }
                KeywordActivationSubject::Controller => {
                    def.valid_card = Some(TargetFilter::Typed(
                        TypedFilter::default().controller(ControllerRef::You),
                    ));
                }
                KeywordActivationSubject::Opponent => {
                    def.valid_card = Some(TargetFilter::Typed(
                        TypedFilter::default().controller(ControllerRef::Opponent),
                    ));
                }
                KeywordActivationSubject::Generic => {}
            }
            return Some((
                TriggerMode::KeywordAbilityActivated(AbilityTag::Outlast),
                def,
            ));
        }
        if all_consuming(tag::<_, _, OracleError<'_>>("an exhaust ability"))
            .parse(rest)
            .is_ok()
        {
            let mut def = make_base();
            def.mode = TriggerMode::KeywordAbilityActivated(AbilityTag::Exhaust);
            return Some((
                TriggerMode::KeywordAbilityActivated(AbilityTag::Exhaust),
                def,
            ));
        }
        if all_consuming(tag::<_, _, OracleError<'_>>(
            "an exhaust ability that isn't a mana ability",
        ))
        .parse(rest)
        .is_ok()
        {
            let mut def = make_base();
            def.mode = TriggerMode::KeywordAbilityActivated(AbilityTag::Exhaust);
            def.condition = Some(TriggerCondition::ActivatedAbilityIsNonMana);
            return Some((
                TriggerMode::KeywordAbilityActivated(AbilityTag::Exhaust),
                def,
            ));
        }
    }
    None
}

/// CR 602.1 + CR 603.2 + CR 605.1a: Parse "Whenever <player_scope> activates
/// an ability [that isn't a mana ability]" triggers — the generic activated-
/// ability trigger class covering Burning-Tree Shaman ("a player"),
/// Flamescroll Celebrant ("an opponent"), and future cards using the same
/// shape ("you"). Player scope is composed via three independent axes that
/// each map to a typed value:
///
/// - **prefix**: "whenever" / "when" — handled by the outer `alt` (CR 603.1)
/// - **subject**: `a player` / `an opponent` / `you` → `TargetFilter` for
///   `valid_target` (CR 602.2a: "Its controller is the player who activated
///   the ability"). "a player" leaves `valid_target` unset so
///   `valid_player_matches` accepts every player (Burning-Tree Shaman).
/// - **non-mana qualifier**: optional " that isn't a mana ability" (CR
///   605.1a). Sets `TriggerCondition::ActivatedAbilityIsNonMana` so the
///   qualifier is preserved in the AST even though `GameEvent::AbilityActivated`
///   already excludes mana abilities (CR 605.3b).
///
/// Nesting by prefix dispatch avoids enumerating the 6-way prefix × subject
/// permutation as separate `tag` arms.
fn try_parse_ability_activation_trigger(lower: &str) -> Option<(TriggerMode, TriggerDefinition)> {
    // Pair subject with its verb conjugation: third-person-singular subjects
    // ("a player", "an opponent") take "activates"; second-person ("you")
    // takes "activate". Each arm carries the typed `valid_target` filter so
    // the activating player is matched correctly via `valid_player_matches`.
    fn parse_subject_and_verb(input: &str) -> OracleResult<'_, Option<TargetFilter>> {
        alt((
            // CR 602.2a: "a player" — leave `valid_target` unset so every
            // player's activation matches (Burning-Tree Shaman).
            value(None, tag("a player activates ")),
            value(
                Some(TargetFilter::Typed(
                    TypedFilter::default().controller(ControllerRef::Opponent),
                )),
                tag("an opponent activates "),
            ),
            value(Some(TargetFilter::Controller), tag("you activate ")),
        ))
        .parse(input)
    }

    // Object noun phrase: the "ability" being activated. Decomposed into
    // article + optional modifier + noun so the grammar accepts both "an
    // ability" and "an activated ability". An optional "of <source>" suffix
    // narrows the trigger to abilities whose source matches a type filter.
    // The matcher already consults `def.valid_card` via `valid_card_matches`,
    // so the source-object filter is propagated directly.
    // Cards: Crackdown Construct, Ashnod the Uncaring, Wizened Mentor,
    // Runic Armasaur, Ceaseless Searblades.
    fn parse_ability_object(input: &str) -> OracleResult<'_, Option<TargetFilter>> {
        let (rest, _) = (tag("an "), opt(tag("activated ")), tag("ability")).parse(input)?;
        // CR 602.1a + CR 113.7: Optional source-object filter narrows the
        // trigger to abilities whose source matches a type filter ("of an
        // artifact or creature", "of a creature or land", "of a permanent").
        opt(preceded(
            tag(" of "),
            preceded(alt((tag("a "), tag("an "))), parse_source_type_disjunction),
        ))
        .map(|filter| filter.map(source_object_filter))
        .parse(rest)
    }

    fn source_object_filter(type_filters: Vec<TypeFilter>) -> TargetFilter {
        // CR 109.2: A card type/subtype description without "card", "spell",
        // "source", or a zone means a permanent of that type on the battlefield.
        TargetFilter::Typed(TypedFilter {
            type_filters,
            properties: vec![FilterProp::InZone {
                zone: Zone::Battlefield,
            }],
            ..TypedFilter::default()
        })
    }

    /// CR 602.1a: Parse a disjunction of card types for the source-object
    /// filter: "artifact or creature", "creature or land", "permanent", etc.
    /// Uses `separated_list1` per R1 (nom combinators on the first pass).
    fn parse_source_type_disjunction(input: &str) -> OracleResult<'_, Vec<TypeFilter>> {
        separated_list1(tag(" or "), parse_single_source_type)
            .map(|types| {
                if types.len() == 1 {
                    types
                } else {
                    vec![TypeFilter::AnyOf(types)]
                }
            })
            .parse(input)
    }

    /// CR 205: Match a single card type keyword for source-object filters.
    fn parse_single_source_type(input: &str) -> OracleResult<'_, TypeFilter> {
        alt((
            value(TypeFilter::Artifact, tag("artifact")),
            value(TypeFilter::Creature, tag("creature")),
            value(TypeFilter::Land, tag("land")),
            value(TypeFilter::Enchantment, tag("enchantment")),
            value(TypeFilter::Planeswalker, tag("planeswalker")),
            value(TypeFilter::Permanent, tag("permanent")),
            parse_source_subtype,
        ))
        .parse(input)
    }

    fn parse_source_subtype(input: &str) -> OracleResult<'_, TypeFilter> {
        parse_subtype(input)
            .map(|(subtype, consumed)| (&input[consumed..], TypeFilter::Subtype(subtype)))
            .ok_or_else(|| oracle_err(input))
    }

    fn parse_qualifier(input: &str) -> OracleResult<'_, Option<TriggerCondition>> {
        // CR 605.1a: "that isn't a mana ability" qualifier is optional in
        // principle (no such printed card exists today without it, but the
        // grammar admits a bare "activates an ability").
        alt((
            value(
                Some(TriggerCondition::ActivatedAbilityIsNonMana),
                tag(" that isn't a mana ability"),
            ),
            value(None, eof),
        ))
        .parse(input)
    }

    let parse_line = preceded(
        alt((tag("whenever "), tag("when "))),
        (
            parse_subject_and_verb,
            parse_ability_object,
            parse_qualifier,
        ),
    );

    if let Ok((_, (subject, source_filter, qualifier))) = all_consuming(parse_line).parse(lower) {
        let mut def = make_base();
        def.mode = TriggerMode::AbilityActivated;
        def.valid_target = subject;
        def.valid_card = source_filter;
        def.condition = qualifier;
        return Some((TriggerMode::AbilityActivated, def));
    }

    // Passive form: "whenever an ability of [source] is activated"
    // The source object filter goes into `valid_card` so `match_ability_activated`
    // can check `valid_card_matches` against the activated ability's source.
    // CR 605.1a: the following "if it isn't a mana ability" intervening-if is
    // stripped by `extract_if_condition` and represented as `ActivatedAbilityIsNonMana`.
    // Cards: Battlemage's Bracers, Illusionist's Bracers.
    fn parse_attached_to_subject(input: &str) -> OracleResult<'_, TargetFilter> {
        value(
            TargetFilter::AttachedTo,
            (
                alt((tag("equipped"), tag("enchanted"))),
                space1,
                // CR 303.4m: "enchanted planeswalker" is a valid Aura host noun
                // (Elspeth's Talent, Rowan's Talent). Purely additive to the
                // existing creature/land/permanent set.
                alt((
                    tag("creature"),
                    tag("land"),
                    tag("planeswalker"),
                    tag("permanent"),
                )),
            ),
        )
        .parse(input)
    }

    fn parse_passive_source(input: &str) -> OracleResult<'_, TargetFilter> {
        preceded(tag("an ability of "), parse_attached_to_subject).parse(input)
    }

    fn parse_passive_line(input: &str) -> OracleResult<'_, TargetFilter> {
        preceded(
            alt((tag("whenever "), tag("when "))),
            terminated(parse_passive_source, tag(" is activated")),
        )
        .parse(input)
    }

    if let Ok((_, source_filter)) = all_consuming(parse_passive_line).parse(lower) {
        let mut def = make_base();
        def.mode = TriggerMode::AbilityActivated;
        def.valid_card = Some(source_filter);
        return Some((TriggerMode::AbilityActivated, def));
    }

    // CR 606.2 + CR 606.1: "Whenever you activate a loyalty ability of <pw>"
    // (Chandra's Regulator, Keral Keep Disciples → "a Chandra planeswalker";
    // Elspeth's Talent, Rowan's Talent → "enchanted planeswalker"). The
    // planeswalker scope rides on `valid_card`:
    //   * "enchanted planeswalker" → `TargetFilter::AttachedTo` (Aura host).
    //   * "a <subtype> planeswalker" → typed Planeswalker + Subtype filter
    //     (CR 205.3j: planeswalker subtypes are the planeswalker's name).
    fn parse_loyalty_planeswalker(input: &str) -> OracleResult<'_, TargetFilter> {
        // "a <subtype> planeswalker" — `parse_subtype` is the single subtype
        // authority and canonicalizes casing ("chandra" → "Chandra").
        fn parse_subtyped_planeswalker(input: &str) -> OracleResult<'_, TargetFilter> {
            let (rest, _) = tag("a ").parse(input)?;
            let (subtype, consumed) = parse_subtype(rest).ok_or_else(|| oracle_err(rest))?;
            let after_subtype = &rest[consumed..];
            let (rest, _) = (space1, tag("planeswalker")).parse(after_subtype)?;
            Ok((
                rest,
                TargetFilter::Typed(TypedFilter::new(TypeFilter::Planeswalker).subtype(subtype)),
            ))
        }
        // "enchanted planeswalker" reuses the shared Aura-host noun combinator.
        alt((parse_subtyped_planeswalker, parse_attached_to_subject)).parse(input)
    }

    fn parse_loyalty_line(input: &str) -> OracleResult<'_, TargetFilter> {
        preceded(
            alt((tag("whenever "), tag("when "))),
            preceded(
                tag("you activate a loyalty ability of "),
                parse_loyalty_planeswalker,
            ),
        )
        .parse(input)
    }

    if let Ok((_, pw_filter)) = all_consuming(parse_loyalty_line).parse(lower) {
        let mut def = make_base();
        def.mode = TriggerMode::LoyaltyAbilityActivated;
        def.valid_card = Some(pw_filter);
        return Some((TriggerMode::LoyaltyAbilityActivated, def));
    }

    None
}

/// CR 702.49 / CR 702.117a / CR 702.137a: Extract alternative-cost "cost was
/// paid" intervening-if conditions (ninjutsu/sneak/surge/spectacle).
/// Guard: "instead" after the condition means conditional override, not intervening-if.
/// CR 702.188a + CR 603.4: Parse an intervening-if of the form
/// "if <pronoun> was cast using <variant>" and emit the matching
/// `TriggerCondition::CastVariantPaidPersistent`.
///
/// Built for the class along two orthogonal axes (CLAUDE.md "compose nom
/// combinators, don't enumerate permutations"):
///   * pronoun axis — the entering subject is referenced by any singular or
///     plural pronoun (Spiders-Man's reflexive token batch uses "they were";
///     single-permanent cards use "it was"). A single `alt` over the
///     pronoun + linking verb consumes this.
///   * variant axis — the named alternative cast cost. Web-slinging is the
///     only cast cost spelled "cast using <name>" in current Oracle text
///     (mirrors the replacement-side coverage in `oracle_replacement.rs`);
///     adding a future variant is a one-line `alt` arm here.
///
/// Emits `CastVariantPaidPersistent` (turn-agnostic) so the trigger agrees
/// with the replacement path, which reads `GameObject.cast_variant_paid`
/// without a turn bound.
fn parse_cast_using_variant_intervening_if(input: &str) -> OracleResult<'_, TriggerCondition> {
    let (input, _) = tag("if ").parse(input)?;
    // Subject pronoun + linking verb. "they were" / "it was" / "this was" /
    // "he was" / "she was" all reference the entering object's cast provenance.
    let (input, _) = alt((
        tag("they were cast using "),
        tag("it was cast using "),
        tag("this was cast using "),
        tag("he was cast using "),
        tag("she was cast using "),
    ))
    .parse(input)?;
    // CR 702.188a: web-slinging is the sole "cast using" alternative cost.
    let (input, variant) = value(CastVariantPaid::WebSlinging, tag("web-slinging")).parse(input)?;
    Ok((
        input,
        TriggerCondition::CastVariantPaidPersistent { variant },
    ))
}

/// Single source of truth for the "<variant> cost was paid" intervening-if /
/// instead phrases. Consumed by both the trigger-condition extractor below and
/// the instead-clause recognizer (`parse_cast_variant_cost_paid_condition` in
/// `oracle_effect/conditions.rs`); each call site filters to the membership it
/// needs (the instead route accepts only `Emerge`). Sharing the pairs keeps the
/// recognized strings from drifting between the two consumers. Per-variant CR
/// cites: Surge CR 702.117a, Spectacle CR 702.137a, Prowl CR 702.76a, Emerge
/// CR 702.119a.
pub(crate) const CAST_VARIANT_COST_PAID_PHRASES: &[(&str, CastVariantPaid)] = &[
    ("sneak cost was paid", CastVariantPaid::Sneak),
    ("ninjutsu cost was paid", CastVariantPaid::Ninjutsu),
    ("surge cost was paid", CastVariantPaid::Surge),
    ("spectacle cost was paid", CastVariantPaid::Spectacle),
    ("prowl cost was paid", CastVariantPaid::Prowl),
    ("emerge cost was paid", CastVariantPaid::Emerge),
];

fn try_extract_cast_variant_paid_condition(
    tp: &TextPair<'_>,
    lower: &str,
    text: &str,
) -> Option<(String, Option<TriggerCondition>)> {
    for (keyword, variant) in CAST_VARIANT_COST_PAID_PHRASES {
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
///
/// CR 603.4 + CR 608.2c: a pattern here must match the WHOLE condition clause,
/// not just a prefix of it — a conjunctive condition ("if it's on the
/// battlefield and you control 9 or fewer creatures, ...", "Name Sticker"
/// Goblin) has its own composite meaning that this table cannot express.
/// Matching only the leading half and treating the residual "and ..." text as
/// part of the effect body silently corrupts the trigger (phase-rs/phase#5449
/// review). Require the match to be immediately followed by a clause boundary
/// (`,`/`.`/end of string) so a conjunctive form falls through to the general
/// condition parser instead.
fn try_extract_simple_condition(
    tp: &TextPair<'_>,
    text: &str,
    patterns: &[(&str, TriggerCondition)],
) -> Option<(String, Option<TriggerCondition>)> {
    let first_if = tp.find("if "); // allow-noncombinator: structural first-if anchor for trigger-level intervening-if extraction
    for (pattern, condition) in patterns {
        if let Some(pos) = tp.find(pattern) {
            if Some(pos) != first_if {
                continue;
            }
            let after = &tp.lower[pos + pattern.len()..];
            let is_whole_clause = after.is_empty() || after.starts_with([',', '.']);
            if !is_whole_clause {
                continue;
            }
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

/// CR 400.7 + CR 603.10: Extract "if it had [no] [a <type>] counter(s) on it"
/// past-state counter conditions from the source's last-known information.
///
/// Composed along two orthogonal axes rather than enumerated as verbatim
/// phrases (CLAUDE.md "compose nom combinators, don't enumerate permutations"):
///   * negation axis — an optional leading `"no "` (after `"if it had "`) wraps
///     the predicate in `TriggerCondition::Not`. Covers the Unstoppable Slasher
///     class ("if it had no counters on it", which gates the recursion-return so
///     a creature that returned with stun counters does not return a second
///     time).
///   * type axis — an optional `"a <type> "` discriminator selects a single
///     counter type (`HadCounters { counter_type: Some(_) }`); its absence
///     means any counter (`counter_type: None`).
///
/// Recognized forms: "if it had counters on it", "if it had no counters on it",
/// "if it had a +1/+1 counter on it", "if it had no +1/+1 counters on it", etc.
/// The trailing `" on it"` is optional grammatical filler.
fn try_extract_had_counter_condition(
    tp: &TextPair<'_>,
    lower: &str,
    text: &str,
) -> Option<(String, Option<TriggerCondition>)> {
    let prefix = "if it had ";
    let pos = tp.find(prefix)?;
    let after = &lower[pos + prefix.len()..];

    let (rest, (negated, counter_type)) = parse_had_counters_body(after).ok()?;
    let clause_len = prefix.len() + (after.len() - rest.len());

    let mut condition = TriggerCondition::HadCounters { counter_type };
    if negated {
        condition = TriggerCondition::Not {
            condition: Box::new(condition),
        };
    }
    Some((
        strip_condition_clause(text, pos, clause_len),
        Some(condition),
    ))
}

/// Parse the body that follows `"if it had "`: an optional `"no "` negation, an
/// optional `"a <type> "` type discriminator, then `"counter(s)[ on it]"`.
/// Returns `(negated, counter_type)` where `counter_type` is `Some` only for the
/// typed form. The type discriminator is whatever non-empty token precedes
/// `" counter"`, with an optional leading article (`"a "` / `"an "`) consumed —
/// so "a +1/+1 counter on it" and the negated plural "no +1/+1 counters on it"
/// both classify the type, while the bare "counters on it" form yields `None`.
fn parse_had_counters_body(input: &str) -> OracleResult<'_, (bool, Option<CounterType>)> {
    let (input, negated) = opt(tag("no ")).parse(input)?;
    let negated = negated.is_some();

    // Typed form: "[a |an ]<type> counter(s) [on it]". `take_until(" counter")`
    // is anchored on the literal " counter" so the type token cannot bleed past
    // it. The article is optional grammatical filler (present in the singular
    // "a +1/+1 counter", absent in the plural "no +1/+1 counters").
    let (after_article, _) = opt(alt((tag("a "), tag("an ")))).parse(input)?;
    if let Ok((rest, type_text)) =
        take_until::<_, _, OracleError<'_>>(" counter").parse(after_article)
    {
        if let Some(counter_type) = crate::types::counter::try_parse_counter_type(type_text) {
            // `take_until` stops before the leading space of " counter"; consume
            // it so `parse_counter_word_tail` starts on the bare word.
            let (rest, _) = tag(" ").parse(rest)?;
            let (rest, _) = parse_counter_word_tail(rest)?;
            return Ok((rest, (negated, Some(counter_type))));
        }
    }

    // Any-counter form: "counter(s) [on it]".
    let (rest, _) = parse_counter_word_tail(input)?;
    Ok((rest, (negated, None)))
}

/// CR 603.4 + CR 122.1: Extract source-scoped present-tense counter conditions —
/// the intervening-if "if <source> has [quantity] [type] counter(s) on it" (The
/// Ozolith, Denry Klin, and the quantified cycle: Bloodchief Ascension, Ventifact
/// Bottle, Simic Ascendancy). The subject × quantity × counter-type × "on it"
/// grammar is delegated to the single authority `parse_source_has_counters`
/// (shared with static gates via `parse_inner_condition`), and the resulting
/// `StaticCondition::HasCounters` is bridged to a `TriggerCondition` by
/// `static_condition_to_trigger_condition` — the same lowering the state-trigger
/// form (`try_parse_source_counter_state_trigger`) uses. The source permanent
/// must currently hold a matching counter for the trigger to resolve (evaluated
/// against `source_id` in `game/triggers.rs`).
///
/// Runs ahead of the generic `try_extract_intervening` "if" path because a
/// non-leading source-referential "~ has …" clause is classified re-homeable
/// there and deferred; extracting it directly always hoists it to the
/// trigger-level condition. Distinct from the past-tense event-subject "if it
/// had counters on it" (`HadCounters`) handled by
/// `try_extract_had_counter_condition`.
fn try_extract_has_counter_condition(
    tp: &TextPair<'_>,
    lower: &str,
    text: &str,
) -> Option<(String, Option<TriggerCondition>)> {
    let prefix = "if ";
    let pos = tp.find(prefix)?;
    let after = &lower[pos + prefix.len()..];

    // Delegate the counter grammar to the shared authority; it fails fast for
    // any non-counter "if …" clause, so the broad "if " anchor is safe.
    let (rest, static_cond) = parse_source_has_counters(after).ok()?;
    let condition = static_condition_to_trigger_condition(&static_cond)?;
    let clause_len = prefix.len() + (after.len() - rest.len());

    Some((
        strip_condition_clause(text, pos, clause_len),
        Some(condition),
    ))
}

/// Consume `" counter"` (or, when already at the word, `"counter"`), an optional
/// plural `"s"`, and an optional trailing `" on it"`. Shared tail for both the
/// typed and any-counter branches of [`parse_had_counters_body`].
fn parse_counter_word_tail(input: &str) -> OracleResult<'_, ()> {
    let (input, _) = tag("counter").parse(input)?;
    let (input, _) = opt(tag("s")).parse(input)?;
    let (input, _) = opt(tag(" on it")).parse(input)?;
    Ok((input, ()))
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
    // CR 603.2: "When ~ enters and at the beginning of your upkeep" (Gathering
    // Stone) — two independent trigger events sharing one effect.
    if let Ok((_, (before, _))) = split_once_on(cond_lower, " and at the beginning of ") {
        let pos = before.len();
        let first = condition[..pos].trim().to_string();
        let second_start = pos + " and ".len();
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

/// CR 702.55c: "~ enters or the creature it haunts dies" is a dedicated compound
/// trigger mode, not a cross-subject or shared-subject split.
fn is_enters_or_haunted_creature_dies_compound(cond_lower: &str) -> bool {
    scan_contains(cond_lower, "enters or the creature it haunts dies")
        || scan_contains(
            cond_lower,
            "enters the battlefield or the creature it haunts dies",
        )
}

/// Split a disjunctive shared-subject event trigger into one reconstructed
/// trigger line per event, with the subject shared across all of them. This is
/// the single entry point for the whole class: the N-way serial form
/// ("Whenever ~ A, B, or C") and the 2-way "or" form ("Whenever ~ A or B") are
/// its two branches. CR 603.1: each listed event is an independent trigger
/// condition. Dedicated 2-way compound `TriggerMode` variants (AttacksOrBlocks,
/// EntersOrAttacks, EntersOrHauntedCreatureDies) are intentionally left unsplit
/// by `split_or_event_compound`.
///
/// Serial is tried first so a comma list ("A, B, or C") is not mis-split by the
/// 2-way scanner; this preserves the prior dispatch order exactly.
fn split_shared_subject_event_list(cond_lower: &str, condition: &str) -> Option<Vec<String>> {
    split_serial_event_compound(cond_lower, condition)
        .or_else(|| split_cross_subject_event_compound(cond_lower, condition))
        .or_else(|| split_or_event_compound(cond_lower, condition))
}

/// Split compound events with different subjects — the cross-subject branch.
///
/// Handles patterns like "Whenever a player casts a spell or a creature attacks"
/// (Norin the Wary) where the two halves have different subjects ("a player" vs
/// "a creature"). Each half is a complete trigger line with its own subject.
///
/// CR 603.1: Each event is an independent trigger condition.
fn split_cross_subject_event_compound(cond_lower: &str, condition: &str) -> Option<Vec<String>> {
    if is_enters_or_haunted_creature_dies_compound(cond_lower) {
        return None;
    }
    let (after_lower, _) = parse_cross_subject_or_split(cond_lower).ok()?;
    let (after_original, before_original) = parse_cross_subject_or_split(condition).ok()?;

    // Check if what follows " or " starts with a valid subject phrase
    // (a/an/the + type word, or "a player", "an opponent", etc.)
    let after_trimmed = after_lower.trim_start();
    if parse_cross_subject_phrase_start(after_trimmed).is_err() {
        return None;
    }

    // CR 508.3a: The second half must contain an event verb to be a genuine
    // cross-subject compound trigger. Without this guard, attack-target scope
    // extensions ("attacks you or a planeswalker you control") are mis-split
    // because "a planeswalker you control" starts with an article but has no
    // event verb — it extends the attack target, not the trigger event.
    scan_preceded(after_trimmed, |i| parse_event_verb_start(i))?;

    let (_, keyword) = parse_trigger_keyword_prefix(cond_lower).ok()?;

    let first = before_original.trim().to_string();
    let second = format!("{keyword}{}", after_original.trim());

    Some(vec![first, second])
}

fn parse_cross_subject_or_split(input: &str) -> OracleResult<'_, &str> {
    terminated(take_until(" or "), tag(" or ")).parse(input)
}

fn parse_cross_subject_phrase_start(input: &str) -> OracleResult<'_, ()> {
    value(
        (),
        alt((
            tag("a "),
            tag("an "),
            tag("the "),
            tag("player "),
            tag("opponent "),
            tag("you "),
        )),
    )
    .parse(input)
}

fn parse_trigger_keyword_prefix(input: &str) -> OracleResult<'_, &'static str> {
    alt((
        value("Whenever ", tag("whenever ")),
        value("When ", tag("when ")),
    ))
    .parse(input)
}

/// Split serial compound events sharing one subject — the N-way branch of
/// [`split_shared_subject_event_list`].
///
/// Example: "Whenever ~ attacks, blocks, or becomes the target of a spell"
/// becomes three trigger conditions, each reusing the same subject.
fn split_serial_event_compound(cond_lower: &str, condition: &str) -> Option<Vec<String>> {
    use super::oracle_nom::primitives::split_once_on;

    // Split the original and lowercase forms in lockstep on the same ASCII
    // delimiters rather than slicing `condition` with byte offsets taken from
    // `cond_lower`. `str::to_lowercase()` is not byte-position-preserving for
    // non-ASCII input (e.g. `İ` grows, `ẞ` shrinks), so a lowercase-derived
    // offset could land mid-`char` in the original and panic. The delimiters
    // (`", or "`, `", "`) are pure punctuation and identical in both views, so
    // parallel splits keep the two aligned without cross-case byte arithmetic.
    let Ok((_, (before_or_lower, after_or_lower))) = split_once_on(cond_lower, ", or ") else {
        return None;
    };
    let Ok((_, (before_or, after_or))) = split_once_on(condition, ", or ") else {
        return None;
    };
    if parse_event_verb_start(after_or_lower.trim_start()).is_err() {
        return None;
    }

    let Ok((_, (first_lower, rest_events_lower))) = split_once_on(before_or_lower, ", ") else {
        return None;
    };
    let Ok((_, (first_original, rest_events_original))) = split_once_on(before_or, ", ") else {
        return None;
    };
    let keyword_and_subject = extract_keyword_and_subject(first_lower.trim());
    let mut results = vec![first_original.trim().to_string()];

    let mut remaining_lower = rest_events_lower;
    let mut remaining_original = rest_events_original;
    loop {
        if let Ok((_, (event_lower, tail_lower))) = split_once_on(remaining_lower, ", ") {
            let Ok((_, (event_original, tail_original))) = split_once_on(remaining_original, ", ")
            else {
                return None;
            };
            if parse_event_verb_start(event_lower.trim()).is_err() {
                return None;
            }
            results.push(format!("{keyword_and_subject} {}", event_original.trim()));
            remaining_lower = tail_lower;
            remaining_original = tail_original;
        } else {
            if parse_event_verb_start(remaining_lower.trim()).is_err() {
                return None;
            }
            results.push(format!(
                "{keyword_and_subject} {}",
                remaining_original.trim()
            ));
            break;
        }
    }

    results.push(format!("{keyword_and_subject} {}", after_or.trim()));

    Some(results)
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
        parse_event_verb_start(text).is_ok()
    }

    // Patterns already handled as dedicated compound TriggerMode variants
    // (EntersOrAttacks, AttacksOrBlocks, EntersOrHauntedCreatureDies) — do not split these.
    fn is_existing_compound_mode(cond_lower: &str) -> bool {
        is_enters_or_haunted_creature_dies_compound(cond_lower)
            || scan_contains(cond_lower, "enters or attacks")
            || scan_contains(cond_lower, "enters the battlefield or attacks")
            || scan_contains(cond_lower, "attacks or blocks")
            // CR 702.29d: "cycle or discard" is a dedicated compound mode
            // (CycledOrDiscarded) — do not split.
            || scan_contains(cond_lower, "cycle or discard")
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

            // Extract the trigger keyword ("When"/"Whenever") and subject from the first condition.
            // The subject is everything between the keyword and the first event verb.
            let keyword_and_subject = extract_keyword_and_subject(&cond_lower[..pos]);
            let first_lower = cond_lower[..pos].trim();
            let second_event = condition[pos + 4..].trim();
            let second = format!("{keyword_and_subject} {second_event}");
            let first = append_shared_object_if_bare_event(
                condition[..pos].trim(),
                first_lower,
                after,
                second_event,
            );

            return Some(vec![first, second]);
        }
        search_start = pos + 4;
    }
    None
}

fn parse_event_boundary(input: &str) -> OracleResult<'_, ()> {
    value(
        (),
        peek(alt((
            value((), eof),
            value((), space1),
            value((), tag(",")),
            value((), tag(".")),
        ))),
    )
    .parse(input)
}

fn parse_event_word<'a>(
    word: &'static str,
) -> impl Parser<&'a str, Output = (), Error = OracleError<'a>> {
    value((), terminated(tag(word), parse_event_boundary))
}

fn parse_event_phrase<'a>(
    phrase: &'static str,
) -> impl Parser<&'a str, Output = (), Error = OracleError<'a>> {
    value((), tag(phrase))
}

fn parse_event_verb_start(input: &str) -> OracleResult<'_, ()> {
    let combat_or_zone = alt((
        parse_event_word("dies"),
        parse_event_phrase("die "),
        parse_event_phrase("deals "),
        parse_event_phrase("deal "),
        parse_event_word("enters"),
        parse_event_phrase("enter "),
        parse_event_word("attacks"),
        parse_event_phrase("attack "),
        parse_event_word("blocks"),
        parse_event_phrase("block "),
        parse_event_word("leaves"),
        parse_event_phrase("is put into"),
    ));
    let passive_player_actions = alt((
        parse_event_word("is sacrificed"),
        parse_event_word("are sacrificed"),
        parse_event_word("is exiled"),
        parse_event_word("are exiled"),
    ));
    let sacrifice_discard_actions = alt((
        parse_event_phrase("sacrifices "),
        parse_event_word("sacrifices"),
        parse_event_phrase("sacrifice "),
        parse_event_word("sacrifice"),
        parse_event_phrase("discards "),
        parse_event_word("discards"),
        parse_event_phrase("discard "),
        parse_event_word("discard"),
    ));
    let play_cast_create_actions = alt((
        // CR 121.2: Player draw events in disjunctive trigger lists (Trouble in
        // Pairs: "draws their second card each turn, or casts...").
        parse_event_phrase("draws "),
        parse_event_word("draws"),
        parse_event_phrase("draw "),
        parse_event_word("draw"),
        // CR 305.1 + CR 601.2: Player-action verbs for Rocco-class
        // "a player plays a land from exile or casts a spell from exile".
        parse_event_phrase("plays "),
        parse_event_word("plays"),
        parse_event_phrase("play "),
        parse_event_word("play"),
        parse_event_phrase("casts "),
        parse_event_word("casts"),
        parse_event_phrase("cast "),
        parse_event_word("cast"),
        // CR 701.7: Token creation as compound event verb (Mirkwood Bats:
        // "whenever you create or sacrifice a token").
        parse_event_phrase("creates "),
        parse_event_word("creates"),
        parse_event_phrase("create "),
        parse_event_word("create"),
        // CR 702.29c: Cycling as compound event verb (Warped Tusker:
        // "when you cast or cycle ~").
        parse_event_phrase("cycle "),
        parse_event_word("cycle"),
    ));
    let simple_event_verbs = alt((
        // CR 702.100b + CR 701.44b: SimpleEvent verbs that may appear in
        // compound triggers (e.g. "~ evolves or dies").
        parse_event_word("evolves"),
        parse_event_phrase("evolve "),
        parse_event_word("explores"),
        parse_event_phrase("explore "),
        parse_event_word("exploits"),
        parse_event_word("mutates"),
        parse_event_word("transforms"),
        parse_event_phrase("becomes the target of a spell or ability"),
        parse_event_phrase("become the target of a spell or ability"),
        parse_event_phrase("becomes the target of an aura spell"),
        parse_event_phrase("becomes the target of an instant or sorcery spell"),
        parse_event_phrase("become the target of an instant or sorcery spell"),
        parse_event_phrase("becomes the target of a spell"),
        // CR 702.26c: "phases in" / "phase in" — phasing trigger verb.
        parse_event_phrase("phases in"),
        parse_event_phrase("phase in"),
        // CR 702.26b: "phases out" / "phase out" — phasing-out trigger verb.
        parse_event_phrase("phases out"),
        parse_event_phrase("phase out"),
    ));
    let player_actions = alt((
        passive_player_actions,
        sacrifice_discard_actions,
        play_cast_create_actions,
    ));
    alt((combat_or_zone, player_actions, simple_event_verbs)).parse(input)
}

fn parse_bare_shared_event_verb(input: &str) -> OracleResult<'_, ()> {
    alt((
        parse_event_word("creates"),
        parse_event_word("create"),
        parse_event_word("sacrifices"),
        parse_event_word("sacrifice"),
        parse_event_word("discards"),
        parse_event_word("discard"),
        parse_event_word("plays"),
        parse_event_word("play"),
        parse_event_word("casts"),
        parse_event_word("cast"),
        // CR 121.2: Player draw events in disjunctive trigger lists.
        parse_event_phrase("draws "),
        parse_event_word("draws"),
        parse_event_phrase("draw "),
        parse_event_word("draw"),
        // CR 702.29c: "cycle" as bare event verb for shared-object propagation.
        parse_event_word("cycle"),
    ))
    .parse(input)
}

fn parse_shared_object_verb_head(input: &str) -> OracleResult<'_, ()> {
    alt((
        parse_event_phrase("creates "),
        parse_event_phrase("create "),
        parse_event_phrase("sacrifices "),
        parse_event_phrase("sacrifice "),
        parse_event_phrase("discards "),
        parse_event_phrase("discard "),
        parse_event_phrase("plays "),
        parse_event_phrase("play "),
        parse_event_phrase("casts "),
        parse_event_phrase("cast "),
        // CR 121.2: Player draw events in disjunctive trigger lists.
        parse_event_phrase("draws "),
        parse_event_phrase("draw "),
        // CR 702.29c: "cycle" as shared-object verb head.
        parse_event_phrase("cycle "),
    ))
    .parse(input)
}

fn ends_with_bare_event_verb(text: &str) -> bool {
    scan_preceded(text, |i| {
        all_consuming(parse_bare_shared_event_verb).parse(i)
    })
    .is_some()
}

/// Extract the shared object from a verb+object phrase.
/// E.g., `"sacrifice a token"` → `Some("a token")` (using original case).
fn extract_shared_object<'a>(lower: &str, original: &'a str) -> Option<&'a str> {
    let (rest_lower, ()) = parse_shared_object_verb_head(lower).ok()?;
    let object_start = original.len() - rest_lower.len();
    let obj = original[object_start..].trim();
    if obj.is_empty() {
        None
    } else {
        Some(obj)
    }
}

fn append_shared_object_if_bare_event(
    first: &str,
    first_lower: &str,
    after_lower: &str,
    second_event: &str,
) -> String {
    // CR 701.7 + CR 701.21: Shared-object compound verbs — "you create
    // or sacrifice a token" shares the object between both verbs. If the
    // first half ends with a bare verb (no object), propagate the object
    // from the second verb.
    if ends_with_bare_event_verb(first_lower) {
        if let Some(obj) = extract_shared_object(after_lower, second_event) {
            return format!("{first} {obj}");
        }
    }
    first.to_string()
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
    if let Some((prefix, _)) = scan_split_at_phrase(text, parse_event_verb_start) {
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

fn spell_quality_equal_to_chosen_number_tail<'a>(
    input: &'a str,
) -> nom::IResult<&'a str, (), OracleError<'a>> {
    let (rest, _) = alt((
        tag::<_, _, OracleError<'_>>("equal to the chosen number"),
        tag("equal to that number"),
    ))
    .parse(input)?;
    Ok((rest, ()))
}

/// CR 202.3 + CR 208.1: Commas inside a spell-quality disjunction
/// ("mana value, power, or toughness equal to …") are not the condition/effect
/// boundary. Talion, the Kindly Lord is the motivating card.
fn continues_spell_quality_disjunction(after_comma: &str) -> bool {
    let trimmed = after_comma.trim_start();

    alt((
        preceded(
            tag::<_, _, OracleError<'_>>("power, or toughness "),
            spell_quality_equal_to_chosen_number_tail,
        ),
        preceded(
            tag::<_, _, OracleError<'_>>("or toughness "),
            spell_quality_equal_to_chosen_number_tail,
        ),
    ))
    .parse(trimmed)
    .map(|(rest, _)| rest.is_empty() || tag::<_, _, OracleError<'_>>(", ").parse(rest).is_ok())
    .unwrap_or(false)
}

/// CR 105.2 + CR 601.2a: Commas inside a spell-color disjunction
/// ("a spell that's white, blue, black, or red") are color-list separators, not
/// the condition/effect boundary. Questing Druid is the motivating card. The
/// text after such a comma is the next color leg — `[or ]<color>` immediately
/// followed by a list separator (`,`) or end of clause — never a sentence/effect
/// start. Restricting the trailing boundary to `,`/end (not a space) keeps a
/// genuine effect that merely begins with a color word ("..., Red ... draws")
/// from being mistaken for a list continuation.
fn continues_spell_color_disjunction(after_comma: &str) -> bool {
    let trimmed = after_comma.trim_start();
    let after_or = value((), tag::<_, _, OracleError<'_>>("or "))
        .parse(trimmed)
        .map(|(rest, _)| rest)
        .unwrap_or(trimmed);
    let Ok((rest, _)) = nom_primitives::parse_color(after_or) else {
        return false;
    };
    // The color leg must terminate at the clause end or a list separator comma,
    // not run into a wider word — `tag(",")` is the combinator counterpart of the
    // `tag(", ")` boundary check in `continues_spell_quality_disjunction`.
    rest.is_empty() || tag::<_, _, OracleError<'_>>(",").parse(rest).is_ok()
}

fn find_effect_boundary(lower: &str) -> Option<usize> {
    use super::oracle_nom::primitives::split_once_on;
    let mut search_start = 0;
    while let Ok((_, (before, after))) = split_once_on(&lower[search_start..], ", ") {
        let comma_pos = search_start + before.len();
        if !continues_player_action_list(after)
            && !continues_disjunctive_zone_change_condition(after)
            && !continues_serial_event_condition(after)
            && !continues_spell_quality_disjunction(after)
            && !continues_spell_color_disjunction(after)
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

fn continues_serial_event_condition(after_comma: &str) -> bool {
    use super::oracle_nom::primitives::split_once_on;

    let trimmed = after_comma.trim_start();
    if let Ok((after_or, ())) = value((), tag::<_, _, OracleError<'_>>("or ")).parse(trimmed) {
        return parse_event_verb_start(after_or.trim_start()).is_ok();
    }

    let Ok((_, (first_event, after_or))) = split_once_on(trimmed, ", or ") else {
        return false;
    };
    parse_event_verb_start(first_event.trim()).is_ok()
        && parse_event_verb_start(after_or.trim_start()).is_ok()
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
    // Avatar crossover: a comma-separated bending-verb disjunction
    // ("whenever you waterbend, earthbend, firebend, or airbend") is a single
    // batched trigger event, so the comma after each verb is a list separator,
    // not the condition/effect boundary.
    if all_consuming(parse_bend_verb).parse(candidate).is_ok() {
        return true;
    }

    if type_phrase_continues_to_combat_damage_player_event(trimmed) {
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
        value((), tag::<_, _, OracleError<'_>>("and/or ")),
        value((), tag::<_, _, OracleError<'_>>("or ")),
        value((), tag("and ")),
    ))
    .parse(trimmed)
    .map(|(rest, _)| rest)
    .unwrap_or(trimmed);
    if type_phrase_continues_to_combat_damage_player_event(after_conjunction) {
        return true;
    }
    if !starts_with_type_word(after_conjunction) {
        return false;
    }
    // Type word found — distinguish continuation from new sentence.
    // A continuation has no predicate verb before the trigger event verb;
    // a new sentence has a subject + predicate verb ("creatures you control get").
    !is_new_sentence_not_type_continuation(after_conjunction)
}

fn type_phrase_continues_to_combat_damage_player_event(text: &str) -> bool {
    let (filter, rest) = parse_type_phrase(text);
    if matches!(filter, TargetFilter::Any) || rest.len() >= text.len() {
        return false;
    }
    let rest = rest.trim_start();
    parse_combat_damage_to_player(rest).is_ok()
}

fn parse_combat_damage_to_player(input: &str) -> OracleResult<'_, ()> {
    value(
        (),
        (
            alt((
                tag::<_, _, OracleError<'_>>("deal"),
                tag::<_, _, OracleError<'_>>("deals"),
            )),
            tag(" combat damage"),
            tag(" to a player"),
        ),
    )
    .parse(input)
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
        if PREDICATE_VERBS.contains(&normalized.as_str()) {
            return true;
        }
        // CR 608.2c: Negated modal verbs ("can't", "don't", "doesn't", "won't")
        // indicate a restriction predicate — recognize the base verb before the
        // negation contraction so "creatures you control can't be the targets ..."
        // is correctly classified as a new subject-predicate sentence rather than
        // a type-list continuation.
        if is_negated_auxiliary_predicate_token(w) {
            return true;
        }
        false
    })
}

fn is_negated_auxiliary_predicate_token(token: &str) -> bool {
    let token = token.trim_matches(|c: char| !c.is_alphabetic() && c != '\'');
    // allow-noncombinator: verb-morphology suffix check on pre-tokenized word
    let Some(base) = token.strip_suffix("n't") else {
        return false;
    };
    matches!(
        base,
        "ca" | "do"
            | "does"
            | "did"
            | "is"
            | "are"
            | "was"
            | "were"
            | "wo"
            | "sha"
            | "have"
            | "has"
            | "had"
    )
}

fn make_base() -> TriggerDefinition {
    TriggerDefinition::new(TriggerMode::Unknown("unknown".to_string()))
        .trigger_zones(vec![Zone::Battlefield])
}

/// CR 202.3 + CR 208.1: Spell-cast quality suffix comparing mana value and/or
/// power/toughness against the source's chosen number (Talion class).
fn parse_spell_chosen_number_quality(spell_clause: &str) -> Option<TypedFilter> {
    const SUFFIXES: &[(&str, bool)] = &[
        (
            "with mana value, power, or toughness equal to the chosen number",
            true,
        ),
        (
            "with mana value, power, or toughness equal to that number",
            true,
        ),
        ("with mana value equal to the chosen number", false),
        ("with mana value equal to that number", false),
    ];
    let (rest, include_pt) = SUFFIXES.iter().find_map(|(suffix, include_pt)| {
        spell_clause
            .strip_suffix(suffix) // allow-noncombinator: structural suffix on tokenized spell-quality chunk
            .map(|rest| (rest.trim(), *include_pt))
    })?;
    let base_tf = if rest.is_empty() || rest == "spell" {
        TypedFilter::default()
    } else {
        let (filter, _) = parse_type_phrase(rest);
        match filter {
            TargetFilter::Typed(tf) => tf,
            _ => TypedFilter::default(),
        }
    };
    let chosen = QuantityExpr::Ref {
        qty: QuantityRef::ChosenNumber,
    };
    let props = if include_pt {
        vec![FilterProp::AnyOf {
            props: vec![
                FilterProp::Cmc {
                    comparator: Comparator::EQ,
                    value: chosen.clone(),
                },
                FilterProp::PtComparison {
                    stat: PtStat::Power,
                    scope: PtValueScope::Current,
                    comparator: Comparator::EQ,
                    value: chosen.clone(),
                },
                FilterProp::PtComparison {
                    stat: PtStat::Toughness,
                    scope: PtValueScope::Current,
                    comparator: Comparator::EQ,
                    value: chosen,
                },
            ],
        }]
    } else {
        vec![FilterProp::Cmc {
            comparator: Comparator::EQ,
            value: chosen,
        }]
    };
    Some(base_tf.properties(props))
}

/// CR 603.4: AND-compose a newly extracted trigger condition onto any existing
/// one. When the trigger already carries a condition (e.g. a parsed
/// intervening-`if`), both must hold, so they are combined under
/// `TriggerCondition::And`; otherwise the new condition stands alone. Shared by
/// the intervening-`if` composition and the `"while ~ is attacking"` state-gate
/// composition so both sites compose conditions identically.
fn and_trigger_conditions(
    existing: Option<TriggerCondition>,
    new: TriggerCondition,
) -> TriggerCondition {
    match existing {
        Some(existing) => TriggerCondition::And {
            conditions: vec![existing, new],
        },
        None => new,
    }
}

/// CR 603.4 + CR 508.1: Strip a trailing `"while [self-ref] is [combat
/// state]"` gate from a trigger-event clause and convert it to a
/// `TriggerCondition`.
///
/// A `"while ..."` clause appended to the trigger event ("Whenever you cast a
/// spell **while ~ is attacking**, ...") is a state gate that mirrors the
/// intervening-`if` rule: the trigger only fires while the source is in that
/// combat state, and the state is rechecked on resolution. Fire Lord Azula's
/// copy trigger is the motivating card; the clause generalizes to any
/// `"[event] while [self-ref] is attacking/blocking/blocked"` trigger, so it is
/// stripped here — before mode dispatch — and the remaining event clause is
/// parsed unchanged.
///
/// Delegates condition recognition to `parse_inner_condition` (the shared
/// combinator authority). The combat-state special case handles attacking
/// directly (`SourceIsAttacking` is the sole combat-state variant of
/// `TriggerCondition` — CR 508.1); other representable states are bridged
/// through `static_condition_to_trigger_condition`. Recognized but
/// unrepresentable states ("while ~ is blocking") return `None`, leaving the
/// clause intact rather than dropping the gate silently. Returns the event
/// clause with the `"while ..."` suffix removed plus the extracted condition, or
/// `None` when no representable `"while ..."` gate is present.
fn strip_while_state_clause(condition: &str) -> Option<(String, TriggerCondition)> {
    let lower = condition.to_lowercase();
    // The gate is introduced by " while " and runs to the end of the event
    // clause (the effect was already split off at the first ", " boundary).
    // `take_until` consumes the pre-`while` prefix and `tag(" while ")` discards
    // the delimiter, leaving the gate fragment as the remainder. The prefix
    // length is the byte boundary used to slice the original-case `condition`
    // for the returned event clause.
    let (fragment, before) = terminated(
        take_until(" while "),
        tag::<_, _, OracleError<'_>>(" while "),
    )
    .parse(lower.as_str())
    .ok()?;
    let pos = before.len();
    let (rest, sc) = parse_inner_condition(fragment.trim()).ok()?;
    // CR 603.4: only accept when the whole "while ..." tail is the condition —
    // a dangling remainder means this isn't a clean state gate.
    if !rest.trim().is_empty() {
        return None;
    }
    // CR 508.1 / CR 603.4: combat-state gates are not bridged by
    // `static_condition_to_trigger_condition`, so handle attacking directly.
    // All other representable static conditions (e.g. HasCounters for "while
    // this enchantment has two or more quest counters on it") bridge through
    // the shared mapper.
    let cond = if matches!(sc, StaticCondition::SourceIsAttacking) {
        TriggerCondition::SourceIsAttacking
    } else {
        static_condition_to_trigger_condition(&sc)?
    };
    Some((condition[..pos].trim_end().to_string(), cond))
}

pub(crate) fn parse_trigger_condition(
    condition: &str,
    ctx: &mut ParseContext,
) -> (TriggerMode, TriggerDefinition) {
    // CR 603.4 + CR 508.1: A trailing "while [state]" gate on the trigger event
    // ("Whenever you cast a spell while ~ is attacking") restricts the trigger to
    // that state. Strip it before mode dispatch and AND it onto the parsed
    // trigger's condition so the rest of the event clause parses exactly as it
    // would unqualified.
    if let Some((stripped, while_cond)) = strip_while_state_clause(condition) {
        let (mode, mut def) = parse_trigger_condition(&stripped, ctx);
        def.condition = Some(and_trigger_conditions(def.condition.take(), while_cond));
        return (mode, def);
    }

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
            // CR 115.1: Bare `Player` target means the effect explicitly
            // targets a player (e.g. "target player mills ...").
            TargetFilter::Player => true,
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

/// CR 115.1 + CR 701.20a: Returns `true` when a `RevealUntil` (or nested
/// sub-ability) names an opponent/player library without `TargetPlayer`
/// binding — e.g. Sméagol's "target opponent reveals..." RingTemptsYou trigger.
fn execute_references_opponent_player(effect: &crate::types::ability::Effect) -> bool {
    fn filter_references_opponent(filter: &TargetFilter) -> bool {
        match filter {
            TargetFilter::Typed(TypedFilter { controller, .. }) => {
                matches!(controller, Some(ControllerRef::Opponent))
            }
            TargetFilter::And { filters } | TargetFilter::Or { filters } => {
                filters.iter().any(filter_references_opponent)
            }
            TargetFilter::Not { filter } => filter_references_opponent(filter),
            _ => false,
        }
    }
    match effect {
        Effect::RevealUntil { player, .. } => filter_references_opponent(player),
        _ => false,
    }
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

fn trigger_object_pronoun_ref_for_condition(condition_text: &str) -> Option<TargetFilter> {
    let lower = condition_text.to_lowercase();
    let after_keyword = alt((
        value((), tag::<_, _, OracleError<'_>>("whenever ")),
        value((), tag("when ")),
    ))
    .parse(lower.as_str())
    .map(|(rest, _)| rest)
    .unwrap_or(&lower);

    // CR 608.2k + CR 601.2a: spell-cast trigger conditions introduce the cast
    // spell as the effect body's untargeted object antecedent. "it" in
    // "Whenever you cast a spell, put it ..." names the spell on the stack.
    let is_spell_cast_trigger = alt((
        tag::<_, _, OracleError<'_>>("you cast "),
        tag("a player casts "),
        tag("an opponent casts "),
        tag("each opponent casts "),
        tag("one or more players cast "),
        tag("enchanted player casts "),
    ))
    .parse(after_keyword)
    .is_ok();
    is_spell_cast_trigger.then_some(TargetFilter::TriggeringSource)
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

    // CR 702.138c + CR 603.11: "it enters [this way]" — the linked triggered
    // ability of an "[this permanent] escapes with [counters]" replacement
    // effect refers to the source permanent by the pronoun "it". Resolve "it"
    // to `SelfRef` only when immediately followed by the "enters" event verb so
    // the bare pronoun is not over-broadened to other event types (the
    // remaining "enters this way" qualifier is consumed by the ETB rider).
    if let Ok((rest, ())) = value((), tag::<_, _, OracleError<'_>>("it ")).parse(text) {
        if tag::<_, _, OracleError<'_>>("enters").parse(rest).is_ok() {
            return (TargetFilter::SelfRef, rest);
        }
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

        if let Ok((rest, filter)) = alt((
            value(
                TargetFilter::Typed(TypedFilter::default().controller(ControllerRef::Opponent)),
                alt((
                    tag::<_, _, OracleError<'_>>("your opponents"),
                    tag("opponents"),
                )),
            ),
            value(TargetFilter::Player, tag("players")),
        ))
        .parse(after_quantifier)
        {
            // CR 603.2c: "one or more opponents/players each <verb>" — the
            // distributive "each" belongs to the subject phrase.  Strip it here
            // at the subject seam so every event-verb parser (draws, loses life,
            // etc.) sees a bare verb without a stray "each " prefix.
            let trimmed = rest.trim_start();
            let (trimmed, _) = opt(tag::<_, _, OracleError<'_>>("each "))
                .parse(trimmed)
                .unwrap_or((trimmed, None));
            return (filter, trimmed);
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
            alt((
                tag::<_, _, OracleError<'_>>("your opponents"),
                tag("opponents"),
                tag("an opponent"),
                tag("opponent"),
            )),
        ),
        value(
            TargetFilter::Typed(TypedFilter::default().controller(ControllerRef::Opponent)),
            tag("each opponent"),
        ),
        value(
            TargetFilter::Controller,
            terminated(tag("you"), peek(alt((space1, eof)))),
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

    if let Some((filter, rest)) = parse_commander_subject_filter_prefix(text) {
        return (filter, rest.trim_start());
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
        TargetFilter::And { filters } => TargetFilter::And {
            filters: filters.into_iter().map(add_another_prop).collect(),
        },
        other => TargetFilter::And {
            filters: vec![
                other,
                TargetFilter::Typed(TypedFilter::default().properties(vec![FilterProp::Another])),
            ],
        },
    }
}

/// CR 700.2: Attach `FilterProp::Modal` to a spell filter parsed from a "modal
/// [type] spell" qualifier, mirroring `add_another_prop`. Distributes into
/// `Or`/`And` branches; wraps a non-`Typed` filter in an `And` so the modality
/// constraint is preserved.
fn add_modal_prop(filter: TargetFilter) -> TargetFilter {
    match filter {
        TargetFilter::Typed(TypedFilter {
            type_filters,
            controller,
            mut properties,
        }) => {
            properties.push(FilterProp::Modal);
            TargetFilter::Typed(TypedFilter {
                type_filters,
                controller,
                properties,
            })
        }
        TargetFilter::Or { filters } => TargetFilter::Or {
            filters: filters.into_iter().map(add_modal_prop).collect(),
        },
        TargetFilter::And { filters } => TargetFilter::And {
            filters: filters.into_iter().map(add_modal_prop).collect(),
        },
        other => TargetFilter::And {
            filters: vec![
                other,
                TargetFilter::Typed(TypedFilter::default().properties(vec![FilterProp::Modal])),
            ],
        },
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
        TargetFilter::And { filters } => TargetFilter::And {
            filters: filters
                .into_iter()
                .map(|filter| add_controller(filter, controller.clone()))
                .collect(),
        },
        TargetFilter::Not { filter } => TargetFilter::Not {
            filter: Box::new(add_controller(*filter, controller)),
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
///
/// Returns a `TargetFilter` for the recognized recipient phrases:
/// - "to a player"                  → `Player`
/// - "to an opponent"               → opponent-controlled TypedFilter
/// - "to another player"            → opponent-controlled TypedFilter
/// - "to one of your opponents"     → opponent-controlled TypedFilter
/// - "to you"                       → `Controller`
/// - "to a player or planeswalker"  → `Or { Player, Planeswalker }`
fn parse_damage_to_qualifier(after_verb: &str) -> Option<TargetFilter> {
    parse_damage_to_qualifier_with_rest(after_verb)
        .ok()
        .map(|(_, filter)| filter)
}

/// CR 120.1 + CR 603.2: Parse the `"to <recipient>"` clause that follows a
/// damage predicate, returning the recipient `TargetFilter` AND the remainder so
/// the caller can consume a trailing qualifier. `parse_damage_to_qualifier` is
/// the discard-remainder convenience wrapper for call sites that don't read the
/// tail.
///
/// Recognizes the *player* recipient axis only ("a player", "an opponent",
/// "you", "a player or planeswalker"). The object recipient axis ("a creature",
/// "a permanent", typed object) is handled separately by the guarded
/// [`parse_object_recipient_filter`], which the call sites try BEFORE this
/// player-axis parser. Keeping the object arm out of this parser is what lets a
/// mixed "to a creature or player" / "to a creature or opponent" recipient
/// (Crovax, Flesh Reaver) decline both parsers and leave `valid_target = None`
/// (any recipient fires) rather than being mis-scoped to `Typed([Creature])`
/// with its player leg dropped.
///
/// The Taii-Wakeen equal-to-P/T shape is the sole province of
/// [`parse_object_recipient_pt_gate`] (which additionally yields the
/// recipient-relative `QuantityRef`); a bare object recipient ("to a creature"
/// with no P/T tail, Strax's class) is the sole province of
/// [`parse_object_recipient_filter`].
fn parse_damage_to_qualifier_with_rest(after_verb: &str) -> OracleResult<'_, TargetFilter> {
    let (rest, ()) =
        value((), tag::<_, _, OracleError<'_>>("to ")).parse(after_verb.trim_start())?;

    fn opponent_player_filter() -> TargetFilter {
        TargetFilter::Typed(TypedFilter::default().controller(ControllerRef::Opponent))
    }

    fn parse_opponent_player_recipient(input: &str) -> OracleResult<'_, TargetFilter> {
        value(
            opponent_player_filter(),
            alt((
                preceded(tag("an "), tag("opponent")),
                preceded(tag("one of your "), tag("opponents")),
                preceded(tag("one or more of your "), tag("opponents")),
                preceded(tag("another "), tag("player")),
            )),
        )
        .parse(input)
    }

    // CR 120.1 + CR 120.1a: "or battle" disjunction — extends player/opponent
    // damage recipients to include battles (March of the Machine onward).
    fn parse_opponent_or_battle_recipient(input: &str) -> OracleResult<'_, TargetFilter> {
        value(
            TargetFilter::Or {
                filters: vec![
                    opponent_player_filter(),
                    TargetFilter::Typed(TypedFilter::new(TypeFilter::Battle)),
                ],
            },
            preceded(
                tag("an "),
                alt((tag("opponent or battle"), tag("opponent or a battle"))),
            ),
        )
        .parse(input)
    }

    alt((
        // CR 120.1 + CR 120.1a: Three-way disjunction — longest match first.
        value(
            TargetFilter::Or {
                filters: vec![
                    TargetFilter::Player,
                    TargetFilter::Typed(TypedFilter::new(TypeFilter::Planeswalker)),
                    TargetFilter::Typed(TypedFilter::new(TypeFilter::Battle)),
                ],
            },
            alt((
                tag("a player, planeswalker, or battle"),
                tag("a player, a planeswalker, or a battle"),
            )),
        ),
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
        // CR 120.1 + CR 120.1a: Two-way player-or-battle disjunction.
        value(
            TargetFilter::Or {
                filters: vec![
                    TargetFilter::Player,
                    TargetFilter::Typed(TypedFilter::new(TypeFilter::Battle)),
                ],
            },
            alt((tag("a player or battle"), tag("a player or a battle"))),
        ),
        value(TargetFilter::Player, tag("a player")),
        parse_opponent_or_battle_recipient,
        parse_opponent_player_recipient,
        value(TargetFilter::Controller, tag("you")),
    ))
    .parse(rest)
}

/// CR 120.1 + CR 208.1 + CR 603.4: Parse the full Taii Wakeen recipient shape —
/// `"to <object> equal to that <object>'s toughness|power"` — atomically. Only
/// succeeds when an object recipient is immediately followed by the equal-to-P/T
/// qualifier, so it never perturbs the `valid_target` of an ordinary DamageDone
/// trigger that merely names an object recipient (those have no such tail and
/// fall through to the player-only [`parse_damage_to_qualifier_with_rest`]).
///
/// Returns the recipient object `TargetFilter` (so the trigger's `valid_target`
/// scopes to the damaged object's type) and the recipient-relative `QuantityRef`
/// for the intervening-`if` `QuantityComparison`.
fn parse_object_recipient_pt_gate(
    after_verb: &str,
) -> OracleResult<'_, (TargetFilter, QuantityRef)> {
    let (rest, ()) =
        value((), tag::<_, _, OracleError<'_>>("to ")).parse(after_verb.trim_start())?;
    // CR 120.1: object recipient ("a creature" / "a permanent" / typed). The
    // leading article is consumed before delegating to `parse_type_phrase`.
    let (rest, _) = alt((tag("a "), tag("an "))).parse(rest)?;
    let (rest, filter) = parse_type_phrase_nom(rest)?;
    // The equal-to-P/T qualifier is mandatory: without it this is an ordinary
    // damage recipient that the player-only qualifier already declined, and the
    // caller must not set an EventTarget gate.
    let (rest, recipient_pt) = parse_damage_equal_to_recipient_pt(rest.trim_start())?;
    Ok((rest, (filter, recipient_pt)))
}

/// CR 120.1 + CR 120.3: Parse a bare object recipient ("to a creature",
/// "to a permanent", "to a planeswalker") on a damage trigger, with NO trailing
/// equal-to-P/T qualifier. Returns the recipient object `TargetFilter` so the
/// trigger's `valid_target` scopes the matcher's recipient check:
/// `match_damage_done`'s `TargetRef::Object` arm routes a non-player-scope
/// filter through `target_filter_matches_object`, and both its `TargetRef::Player`
/// arm and the aggregate `matching_combat_damage_to_player_sources` reject it
/// (the recipient is an object, not a player).
///
/// Tried AFTER [`parse_object_recipient_pt_gate`] (Taii Wakeen wins on the P/T
/// tail) and BEFORE the player-axis [`parse_damage_to_qualifier`]. A trailing
/// phrase-terminator guard is REQUIRED: `parse_type_list` parses only the leading
/// core type, so "to a creature or player" / "to a creature or opponent" (Crovax,
/// Flesh Reaver) leave " or player"/" or opponent" as remainder. Without the
/// guard those would be mis-scoped to `Typed([Creature])` and drop their
/// player-recipient leg. The guard rejects any alphanumeric continuation and the
/// " or " disjunction, so such mixed recipients decline here and reach the player
/// axis (which also declines them, leaving `valid_target = None` — the
/// "any recipient" behavior these cards require). Mirrors the word-boundary idiom
/// in [`parse_enters_tapped_state_rider`]. Strax, Sontaran Nurse + the
/// "deals [combat] damage to a [type]" class.
fn parse_object_recipient_filter(after_verb: &str) -> OracleResult<'_, TargetFilter> {
    let (rest, ()) =
        value((), tag::<_, _, OracleError<'_>>("to ")).parse(after_verb.trim_start())?;
    let (rest, _) = alt((tag("a "), tag("an "))).parse(rest)?;
    let (rest, filter) = parse_type_phrase_nom(rest)?;
    // Phrase-terminator guard: accept only end-of-string or a non-alphanumeric
    // terminator (comma, period, space-before-clause), and explicitly reject the
    // " or <player-word>" disjunction. Declines "creature or player",
    // "creature or opponent", and any longer type word the bare-prefix scan would
    // otherwise truncate. The disjunction is detected with a `peek(tag("or "))`
    // combinator (no string dispatch); the alphanumeric word-boundary check
    // mirrors the file's established idiom in `parse_enters_tapped_state_rider`.
    // Uses the file's `OracleError::new(rest, ErrorKind::Eof)` decline idiom.
    let is_disjunction = peek(tag::<_, _, OracleError<'_>>("or "))
        .parse(rest.trim_start())
        .is_ok();
    if is_disjunction
        || rest
            .chars()
            .next()
            .is_some_and(|c| c.is_alphanumeric() || c == '_')
    {
        return Err(nom::Err::Error(OracleError::new(
            rest,
            nom::error::ErrorKind::Eof,
        )));
    }
    Ok((rest, filter))
}

/// CR 120.1 + CR 108.3: Recognize the relational damage recipient "to its owner"
/// — the damaged player is the OWNER of the damaging object. On match, the caller
/// emits [`TriggerCondition::DamagedPlayerIsEventSourceOwner`] and leaves
/// `valid_target = None` (the recipient is a player gated by a relation, not a
/// static type filter). A trailing word-boundary guard rejects continuations like
/// "its owners" / "its owner's hand". The Beast, Deathless Prince.
fn parse_damage_to_its_owner(after_verb: &str) -> OracleResult<'_, ()> {
    let (rest, ()) = value(
        (),
        preceded(tag::<_, _, OracleError<'_>>("to "), tag("its owner")),
    )
    .parse(after_verb.trim_start())?;
    // Reject "its owners" / "its owner's hand": only an exact phrase terminator
    // (end / space / punctuation) is accepted. Mirrors the file's existing
    // `OracleError::new(rest, ErrorKind::Eof)` decline idiom so this combinator
    // cleanly declines and the caller falls through to the player axis.
    if rest
        .chars()
        .next()
        .is_some_and(|c| c.is_alphanumeric() || c == '_' || c == '\'')
    {
        return Err(nom::Err::Error(OracleError::new(
            rest,
            nom::error::ErrorKind::Eof,
        )));
    }
    Ok((rest, ()))
}

/// CR 115.10a + CR 120.1 + CR 120.3: Recognize a mixed non-target damage
/// recipient ("to a permanent or player") without lowering it to a
/// `valid_target` filter. The exact recipient is carried by the `DamageDealt`
/// event and later used by DealDamage-local `EventTarget` resolution.
fn parse_damage_to_permanent_or_player(after_verb: &str) -> OracleResult<'_, ()> {
    all_consuming(value(
        (),
        preceded(
            tag::<_, _, OracleError<'_>>("to "),
            alt((tag("a permanent or player"), tag("a permanent or a player"))),
        ),
    ))
    .parse(after_verb.trim_start())
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

/// CR 702.138c + CR 603.11: After consuming the `"enter"` prefix in a SelfRef
/// ETB trigger clause, recognize the linked-ability rider `"enters this way"` —
/// the triggered ability linked to an `"[this permanent] escapes with [counters]"`
/// replacement effect. Per CR 702.138c such a trigger "triggers when that
/// permanent enters the battlefield after its replacement effect was applied,"
/// i.e. only when the permanent escaped. Emit `CastVariantPaid { Escape }` so
/// the intervening-if (CR 603.4, checked at fire AND resolution) gates the ETB
/// effect on the escape cast (Pharika's Spawn).
///
/// The `input` is the remainder after `tag("enter")`, so the rider begins with
/// `"s "` (the rest of "enters") followed by `"this way"`. A word-boundary check
/// rejects accidental prefix matches.
fn parse_enters_this_way_rider(input: &str) -> Option<TriggerCondition> {
    let (after, ()) = value(
        (),
        preceded(
            tag::<_, _, OracleError<'_>>("s "),
            tag::<_, _, OracleError<'_>>("this way"),
        ),
    )
    .parse(input)
    .ok()?;

    // Word-boundary: reject false prefix matches (e.g. "this ways").
    if !after.is_empty()
        && after
            .chars()
            .next()
            .is_some_and(|c| c.is_alphanumeric() || c == '_')
    {
        return None;
    }

    Some(TriggerCondition::CastVariantPaid {
        variant: CastVariantPaid::Escape,
    })
}

fn parse_enters_control_rider(input: &str) -> Option<ControllerRef> {
    scan_preceded(input, |input| {
        preceded(
            tag::<_, _, OracleError<'_>>("under "),
            alt((
                value(ControllerRef::You, tag("your control")),
                value(ControllerRef::Opponent, tag("an opponent's control")),
                value(ControllerRef::Opponent, tag("opponent's control")),
            )),
        )
        .parse(input)
    })
    .map(|(_, controller, _)| controller)
}

fn append_trigger_condition(
    existing: Option<TriggerCondition>,
    condition: TriggerCondition,
) -> TriggerCondition {
    match existing {
        Some(existing) => TriggerCondition::And {
            conditions: vec![existing, condition],
        },
        None => condition,
    }
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

fn subject_is_player(subject: &TargetFilter) -> bool {
    matches!(
        subject,
        TargetFilter::Player | TargetFilter::Controller | TargetFilter::AllPlayers
    ) || matches!(
        subject,
        TargetFilter::Typed(TypedFilter {
            type_filters,
            controller: Some(_),
            properties,
        }) if type_filters.is_empty() && properties.is_empty()
    )
}

/// Collapse a list of subject leaves back into a single filter: one element stays
/// bare, multiple elements re-wrap as `Or`.
fn collapse_or(mut filters: Vec<TargetFilter>) -> TargetFilter {
    if filters.len() == 1 {
        filters.pop().expect("len checked")
    } else {
        TargetFilter::Or { filters }
    }
}

fn set_trigger_subject(def: &mut TriggerDefinition, subject: &TargetFilter) {
    if subject_is_player(subject) {
        def.valid_target = Some(subject.clone());
    } else if let TargetFilter::Or { filters } = subject {
        // CR 115.1: A mixed "a player or <permanent>" subject spans both target
        // axes (objects and/or players). Route player leaves -> valid_subject_player
        // and object leaves -> valid_card so the matcher can fire on either kind
        // independently. The player leaf must NOT land in valid_target: that field
        // is the EFFECT-target slot (populated by "target opponent/player" effects,
        // e.g. Venerated Rotpriest), so conflating the two would over-fire the
        // becomes-target Player arm. Gated on a player leaf being present: a
        // pure-object `Or` (e.g. "an artifact or creature you control") stays
        // byte-identical to the pre-existing behavior (whole `Or` into valid_card).
        let (players, objects): (Vec<_>, Vec<_>) =
            filters.iter().cloned().partition(subject_is_player);
        if players.is_empty() {
            def.valid_card = Some(subject.clone());
        } else {
            def.valid_subject_player = Some(collapse_or(players));
            if !objects.is_empty() {
                def.valid_card = Some(collapse_or(objects));
            }
        }
    } else {
        def.valid_card = Some(subject.clone());
    }
}

/// CR 110.1: A permanent is a card or token on the battlefield. A targeted card in
/// a graveyard or exile is also a `TargetRef::Object`, so a "permanent" subject for
/// a becomes-target trigger must be battlefield-scoped to exclude non-permanents.
/// Applied ONLY in the becomes-target-ability arm (never to dies/leaves triggers,
/// whose object legitimately lives in the graveyard at match time).
fn battlefield_scope_permanent(subject: &TargetFilter) -> TargetFilter {
    fn gate(f: &TargetFilter) -> TargetFilter {
        match f {
            TargetFilter::Typed(t)
                if t.type_filters
                    .iter()
                    .any(|tf| matches!(tf, TypeFilter::Permanent)) =>
            {
                let mut props = t.properties.clone();
                if !props.iter().any(|p| matches!(p, FilterProp::InZone { .. })) {
                    props.push(FilterProp::InZone {
                        zone: Zone::Battlefield,
                    });
                }
                TargetFilter::Typed(TypedFilter {
                    properties: props,
                    ..t.clone()
                })
            }
            TargetFilter::Or { filters } => TargetFilter::Or {
                filters: filters.iter().map(gate).collect(),
            },
            other => other.clone(),
        }
    }
    gate(subject)
}

fn parse_attachment_self_host(input: &str) -> OracleResult<'_, ()> {
    value(
        (),
        alt((
            tag::<_, _, OracleError<'_>>("~"),
            tag("this creature"),
            tag("this permanent"),
        )),
    )
    .parse(input)
}

/// CR 603.2 + CR 106.1: Parse a single mana letter from inside `{…}`.
fn parse_taps_for_mana_color_code(i: &str) -> OracleResult<'_, ManaType> {
    alt((
        value(ManaType::Colorless, alt((tag("C"), tag("c")))),
        value(ManaType::White, alt((tag("W"), tag("w")))),
        value(ManaType::Blue, alt((tag("U"), tag("u")))),
        value(ManaType::Black, alt((tag("B"), tag("b")))),
        value(ManaType::Red, alt((tag("R"), tag("r")))),
        value(ManaType::Green, alt((tag("G"), tag("g")))),
    ))
    .parse(i)
}

/// CR 603.2 + CR 106.1: Parse one braced mana symbol, expanding hybrids
/// (`{W/U}` → two types).
fn parse_taps_for_mana_braced_symbol(i: &str) -> OracleResult<'_, Vec<ManaType>> {
    let (rest, types) = delimited(
        tag("{"),
        separated_list1(tag("/"), parse_taps_for_mana_color_code),
        tag("}"),
    )
    .parse(i)?;
    Ok((rest, types))
}

/// CR 603.2 + CR 106.1: Parse the trailing "for mana" / "for {C}" /
/// "for {G} or {U}" clause of a TapsForMana trigger subject.
fn parse_taps_for_mana_for_clause_body(i: &str) -> OracleResult<'_, Option<Vec<ManaType>>> {
    alt((
        value(None, tag("mana")),
        map(
            separated_list1(
                preceded(space1, tag("or ")),
                parse_taps_for_mana_braced_symbol,
            ),
            |chunks| {
                let mut produced = Vec::new();
                for types in chunks {
                    produced.extend(types);
                }
                Some(produced)
            },
        ),
    ))
    .parse(i)
}

/// CR 603.2 + CR 106.1: Split a TapsForMana subject line into the permanent
/// filter text and an optional produced-mana constraint from the trailing
/// "for mana" / "for {C}" / "for {G}" clause.
fn split_taps_for_mana_for_clause(text: &str) -> Option<(String, Option<Vec<ManaType>>)> {
    let (_, (subject, for_clause)) = nom_primitives::split_once_on(text, " for ").ok()?;
    let (_, produced) = all_consuming(parse_taps_for_mana_for_clause_body)
        .parse(for_clause.trim())
        .ok()?;
    Some((subject.to_string(), produced))
}

/// CR 603.2 + CR 605.1a: Shared nom dispatch for "Whenever [an opponent / a player]
/// taps … for mana" trigger conditions. Used by trigger-line dispatch and by
/// `condition_matches_taps_for_mana_event` for `"that player"` scope binding.
///
/// The leading "whenever "/"when " keyword is OPTIONAL. Printed-permanent
/// dispatch (`try_parse_player_trigger`) passes the full keyworded condition,
/// but the instant/sorcery delayed-trigger split path
/// (`try_parse_whenever_this_turn` in `oracle_effect`) strips the "whenever "
/// keyword before handing the condition to `parse_trigger_condition`, so the
/// keyword is already gone by the time this recognizer runs on High Tide /
/// Bubbling Muck. `opt()` accepts both forms; because both callers require the
/// recognizer to consume the ENTIRE candidate line (empty remainder), the
/// looser match cannot false-positive on mid-sentence text.
fn parse_taps_for_mana_actor_line(
    i: &str,
) -> OracleResult<'_, (Option<ControllerRef>, String, Option<Vec<ManaType>>)> {
    let (rest, actor_controller) = preceded(
        opt(alt((tag("whenever "), tag("when ")))),
        alt((
            value(Some(ControllerRef::Opponent), tag("an opponent taps ")),
            value(None, tag("a player taps ")),
        )),
    )
    .parse(i)?;
    let (subject_text, produced_filter) =
        split_taps_for_mana_for_clause(rest).ok_or_else(|| oracle_err(rest))?;
    Ok(("", (actor_controller, subject_text, produced_filter)))
}

/// CR 603.2 + CR 605.1a: Returns true when `cond_lower` is a taps-for-mana trigger
/// condition ("Whenever [you / an opponent / a player] taps … for mana").
fn condition_matches_taps_for_mana_event(cond_lower: &str) -> bool {
    if let Ok((rem, (_, subject_text, _))) = parse_taps_for_mana_actor_line(cond_lower) {
        if rem.trim().is_empty() {
            let (_, sub_rem) = parse_trigger_subject(&subject_text, &mut ParseContext::default());
            if sub_rem.trim().is_empty() {
                return true;
            }
        }
    }
    for prefix in ["whenever you tap ", "when you tap "] {
        let Ok((rest, ())) = value((), tag::<_, _, OracleError<'_>>(prefix)).parse(cond_lower)
        else {
            continue;
        };
        if split_taps_for_mana_for_clause(rest).is_some() {
            return true;
        }
    }
    false
}

/// CR 509.1h: Strip a trailing "and isn't/aren't blocked" qualifier from attack
/// trigger event text. Covers singular self-referential phrasing ("attacks and
/// isn't blocked", Frenzy) and plural batched phrasing ("attack you and aren't
/// blocked", Coveted Jewel).
fn strip_attack_unblocked_qualifier(after: &str) -> (bool, &str) {
    fn parse_suffix(input: &str) -> OracleResult<'_, &str> {
        alt((
            terminated(take_until(" and isn't blocked"), tag(" and isn't blocked")),
            terminated(
                take_until(" and isn\u{2019}t blocked"),
                tag(" and isn\u{2019}t blocked"),
            ),
            terminated(
                take_until(" and aren't blocked"),
                tag(" and aren't blocked"),
            ),
            terminated(
                take_until(" and aren\u{2019}t blocked"),
                tag(" and aren\u{2019}t blocked"),
            ),
            terminated(
                take_until(" and are not blocked"),
                tag(" and are not blocked"),
            ),
        ))
        .parse(input)
    }

    if let Ok((_, rest)) = all_consuming(parse_suffix).parse(after) {
        return (true, rest);
    }
    (false, after)
}

/// CR 506.5: Strip a trailing " alone" qualifier from attack trigger event
/// text ("attacks alone", Exalted / Sharon Carter class). Encoded as
/// `Not(MinCoAttackers { minimum: 1 })` — zero same-controller co-attackers.
fn strip_attack_alone_qualifier(after: &str) -> (bool, &str) {
    if let Ok((rest, _)) = tag::<_, _, OracleError<'_>>(" alone").parse(after) {
        (true, rest)
    } else {
        (false, after)
    }
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

    // CR 702.55c: "~ enters or the creature it haunts dies" — one compound trigger;
    // the haunted-dies half is cloned into exile by `database::haunt` synthesis.
    if is_enters_or_haunted_creature_dies_compound(rest) {
        let mut def = make_base();
        def.mode = TriggerMode::EntersOrHauntedCreatureDies;
        def.destination = Some(Zone::Battlefield);
        def.valid_card = Some(subject.clone());
        return Some((TriggerMode::EntersOrHauntedCreatureDies, def));
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
        // CR 603.6a + CR 603.6c: at each word boundary, first try the rich
        // `parse_origin_constraint_tail` combinator (which recognizes
        // `from anywhere`, `from anywhere other than <zone>`, and the list-form
        // `from anywhere other than <zone> or <zone>`). When it produces
        // anything richer than `Equals(z)` / `Any`, route the constraint
        // through `zone_change_clauses` so the disjunctive-clause matcher path
        // (`zone_change_clause_matches`) can enforce `NotEquals` / `OneOf`
        // — the scalar `def.origin` path only models a single positive zone.
        // Otherwise fall through to the legacy scalar scan so existing card
        // data (one positive `from <single-zone>` clause) stays byte-identical.
        // This is the SelfRef-ETB analog of the cast-origin / put-into-graveyard
        // routing already wired elsewhere in this file ("Name Sticker" Goblin's
        // "enters from anywhere other than a graveyard or exile" is the
        // first card to exercise it from the ETB side).
        let mut scan = after_enter.trim_start();
        let mut matched_clause = false;
        while !scan.is_empty() {
            if let Ok((_, constraint)) = parse_origin_constraint_tail(scan, parse_cast_origin_zone)
            {
                match constraint {
                    // Bare "from anywhere" / no "from" clause / single-zone
                    // positive: drop back to the scalar path so the emitted
                    // JSON shape is unchanged for the existing card corpus.
                    OriginConstraint::Any | OriginConstraint::Equals(_) => {}
                    rich @ (OriginConstraint::NotEquals(_) | OriginConstraint::OneOf(_)) => {
                        def.zone_change_clauses.push(ZoneChangeClause {
                            origin: rich,
                            destination: Some(Zone::Battlefield),
                            destination_constraint: DestinationConstraint::Any,
                            valid_card: Some(subject.clone()),
                        });
                        matched_clause = true;
                        break;
                    }
                }
            }
            if let Ok((_, origin)) = parse_enters_origin_zone(scan) {
                def.origin = Some(origin);
                break;
            }
            scan = scan.find(' ').map_or("", |i| scan[i + 1..].trim_start());
        }
        // When a `zone_change_clauses` entry was emitted, the disjunctive
        // matcher path supersedes the scalar `valid_card` / `destination` /
        // `origin` fields (see `match_changes_zone` in `game/trigger_matchers.rs`).
        // Clearing the scalar fields keeps the JSON minimal and prevents the
        // matcher from double-counting the destination via two routes.
        if matched_clause {
            def.valid_card = None;
            def.destination = None;
        }

        if let Some(controller) = parse_enters_control_rider(after_enter) {
            if let Some(valid_card) = def.valid_card.take() {
                def.valid_card = Some(add_controller(valid_card, controller.clone()));
            }
            for clause in &mut def.zone_change_clauses {
                if let Some(valid_card) = clause.valid_card.take() {
                    clause.valid_card = Some(add_controller(valid_card, controller.clone()));
                }
            }
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
            def.condition = Some(append_trigger_condition(def.condition.take(), cond));
        }

        // CR 702.138c + CR 603.11: "enters this way" — the linked triggered
        // ability of an "[this permanent] escapes with [counters]" replacement
        // effect. Gate the ETB trigger on `CastVariantPaid { Escape }` so it
        // fires only when the permanent escaped (Pharika's Spawn).
        if let Some(cond) = parse_enters_this_way_rider(after_enter) {
            def.condition = Some(append_trigger_condition(def.condition.take(), cond));
        }

        // CR 305.1 + CR 603.4: "without being played" distinguishes lands put
        // onto the battlefield by effects from normal land plays. Track it as a
        // negated land-play provenance condition on the entering object.
        if scan_contains(after_enter, "without being played") {
            def.condition = Some(append_trigger_condition(
                def.condition.take(),
                TriggerCondition::Not {
                    condition: Box::new(TriggerCondition::WasPlayed),
                },
            ));
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

    // CR 120.1 + CR 120.3 + CR 603.2: Subject-led damage trigger —
    //   "deal[s] [combat|noncombat] [N or more] damage [to <recipient>]".
    // Composed from three independent axes:
    //   * damage-kind adjective   → `DamageKindFilter::{CombatOnly, NoncombatOnly}`
    //   * "N or more" quantifier  → `damage_amount = Some((GE, N))`
    //   * recipient "to <…>"      → `valid_target` via `parse_damage_to_qualifier`
    // Singular ("deals") and plural ("deal", for &-names) collapse into one
    // verb alternative. Unlocks Deus of Calamity ("~ deals 6 or more damage to
    // an opponent") in addition to the established "deals damage" / "deals
    // combat damage" classes — same handler, no new arm.
    if let Ok((after_verb, ())) = alt((
        value((), tag::<_, _, OracleError<'_>>("deals ")),
        value((), tag("deal ")),
    ))
    .parse(rest)
    {
        if let Ok((after_damage, (kind, amount))) = parse_damage_predicate_tail(after_verb) {
            let mut def = make_base();
            def.mode = TriggerMode::DamageDone;
            def.damage_kind = kind;
            def.damage_amount = amount;
            def.valid_source = Some(subject.clone());
            // CR 120.1 + CR 208.1 + CR 603.4: Taii Wakeen's damage==recipient-P/T
            // shape ("to <object> equal to that <object>'s toughness/power") is
            // tried first and atomically — it succeeds only when the object
            // recipient is immediately followed by the equal-to-P/T qualifier, so
            // it never fires for "deals damage equal to its power to X" (amount =
            // source's power) or for ordinary object recipients without the tail.
            if parse_damage_to_its_owner(after_damage).is_ok() {
                // CR 120.1 + CR 108.3: relational "to its owner" recipient — the
                // damaged player must be the damaging object's owner (The Beast,
                // Deathless Prince). Gated by a typed condition, no valid_target.
                def.condition = Some(append_trigger_condition(
                    def.condition.take(),
                    TriggerCondition::DamagedPlayerIsEventSourceOwner,
                ));
            } else if let Ok((_, (filter, recipient_pt))) =
                parse_object_recipient_pt_gate(after_damage)
            {
                def.valid_target = Some(filter);
                def.condition = Some(TriggerCondition::QuantityComparison {
                    lhs: QuantityExpr::Ref {
                        qty: QuantityRef::EventContextAmount,
                    },
                    comparator: Comparator::EQ,
                    rhs: QuantityExpr::Ref { qty: recipient_pt },
                });
            } else if parse_damage_to_permanent_or_player(after_damage).is_ok() {
                // CR 115.10a + CR 120.1 + CR 120.3: mixed object/player
                // recipient ("to a permanent or player") is not a target
                // filter. The concrete recipient is the DamageDealt event
                // target.
            } else if let Ok((_, filter)) = parse_object_recipient_filter(after_damage) {
                // CR 120.3: bare object recipient ("to a creature") — gate the
                // matcher's recipient check on the damaged object's type (Strax +
                // the "deals [combat] damage to a [type]" class). The terminator
                // guard inside the combinator declines "creature or player" /
                // "creature or opponent", which fall through to the player axis.
                def.valid_target = Some(filter);
            } else if let Some(filter) = parse_damage_to_qualifier(after_damage) {
                // Ordinary player recipient ("to a player" / "to an opponent" /
                // …) — unchanged from the pre-Taii behavior.
                def.valid_target = Some(filter);
            }
            return Some((TriggerMode::DamageDone, def));
        }
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
                // CR 508.1a: Battalion/Pack Tactics counts any N *other* creatures
                // (untyped head noun) → no condition-level type axis.
                def.condition = Some(TriggerCondition::MinCoAttackers {
                    minimum: n,
                    filter: None,
                });
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
        let (attacks_and_unblocked, after) = strip_attack_unblocked_qualifier(after);
        let (attacks_alone, after) = strip_attack_alone_qualifier(after);
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
                value(AttackTargetFilter::Player, tag(" one of your opponents")),
                value(AttackTargetFilter::Player, tag(" a player")),
                value(AttackTargetFilter::Player, tag(" you")),
                value(AttackTargetFilter::Battle, tag(" a battle")),
            ))
            .parse(input)
        }
        let attack_target_filter = parse_attack_target.parse(after).ok().map(|(_, f)| f);
        let attacks_one_of_your_opponents = tag::<_, _, OracleError<'_>>(" one of your opponents")
            .parse(after)
            .is_ok();
        let mut def = make_base();
        // CR 508.3d: "Whenever [a player] attacks" triggers fire once per attack declaration,
        // not once per attacker. This applies to "opponent attacks you" patterns (e.g., Lulu,
        // Cunning Rhetoric) where the subject is an opponent and the target is "you".
        let is_opponent_attacks_you =
            matches!(
                subject,
                TargetFilter::Typed(TypedFilter {
                    controller: Some(ControllerRef::Opponent),
                    ..
                })
            ) && matches!(
                attack_target_filter,
                Some(AttackTargetFilter::PlayerOrPlaneswalker) | Some(AttackTargetFilter::Player)
            ) && tag::<_, _, OracleError<'_>>(" you").parse(after).is_ok();
        def.mode = if attacks_and_unblocked && matches!(subject, TargetFilter::SelfRef) {
            TriggerMode::AttackerUnblocked
        } else if attacks_and_unblocked {
            TriggerMode::YouAttackUnblocked
        } else if is_opponent_attacks_you {
            TriggerMode::AttackersDeclared
        } else {
            TriggerMode::Attacks
        };
        // CR 508.1a + CR 508.5: player-subject attack triggers scope the attacking
        // player via `valid_source`, not `valid_card` — `TargetFilter::Player` never
        // matches an object (Breena, the Demagogue).
        if subject_is_player(subject) {
            def.valid_source = Some(subject.clone());
        } else {
            def.valid_card = Some(subject.clone());
        }
        def.attack_target_filter = attack_target_filter;
        if attacks_one_of_your_opponents {
            def.valid_target = Some(TargetFilter::Typed(
                TypedFilter::default().controller(ControllerRef::Opponent),
            ));
        } else if matches!(
            def.attack_target_filter,
            Some(AttackTargetFilter::PlayerOrPlaneswalker) | Some(AttackTargetFilter::Player)
        ) && tag::<_, _, OracleError<'_>>(" you").parse(after).is_ok()
        {
            def.valid_target = Some(TargetFilter::Controller);
        }
        if attacks_alone {
            // CR 506.5: attacks alone — zero same-controller co-attackers.
            def.condition = Some(TriggerCondition::Not {
                condition: Box::new(TriggerCondition::MinCoAttackers {
                    minimum: 1,
                    filter: None,
                }),
            });
        }
        let mode = def.mode.clone();
        return Some((mode, def));
    }

    // "blocks or becomes blocked [by a <filter>]" — Karn (bare), Goblin Cadets
    // (bare), Venom/Mammoth Harness (filtered). CR 509.1h + CR 509.3d: the
    // compound form must be dispatched before the bare "blocks" arm because
    // `tag("blocks")` would otherwise match its prefix and silently drop the
    // "or becomes blocked [by a <filter>]" remainder.
    if let Ok((after_compound, _)) =
        tag::<_, _, OracleError<'_>>("blocks or becomes blocked").parse(rest)
    {
        let mut def = make_base();
        def.mode = TriggerMode::BlocksOrBecomesBlocked;
        def.valid_card = Some(subject.clone());
        def.valid_target = parse_becomes_blocked_by_filter(after_compound);
        return Some((TriggerMode::BlocksOrBecomesBlocked, def));
    }

    // "blocks [a <filter>]" — fires for the blocking creature. Wall of Frost
    // (bare), High-Rise Sawjack (filtered). CR 509.3b: capture the
    // "a <filter> creature" attacker-side qualifier so a filtered "blocks"
    // trigger fires only against a matching attacker.
    if let Ok((after_blocks, _)) = tag::<_, _, OracleError<'_>>("blocks").parse(rest) {
        let mut def = make_base();
        def.mode = TriggerMode::Blocks;
        def.valid_card = Some(subject.clone());
        def.valid_target = parse_blocks_a_filter(after_blocks);
        return Some((TriggerMode::Blocks, def));
    }

    // "leaves the battlefield" / "leaves" / "leave the battlefield" / "leave"
    // CR 603.2c: Plural "leave" form is used with batched "one or more" subjects.
    let leaves_tail = alt((
        value((), tag::<_, _, OracleError<'_>>("leaves the battlefield")),
        value((), tag::<_, _, OracleError<'_>>("leave the battlefield")),
        value((), tag::<_, _, OracleError<'_>>("leaves")),
        value((), tag("leave")),
    ))
    .parse(rest)
    .ok()
    .map(|(tail, _)| tail);
    if let Some(tail) = leaves_tail {
        let tail = tail.trim_start();
        // CR 603.6c + CR 603.4: Strip trailing "during your turn" condition
        // before checking for "without dying" or bare tail. CR 603.6c governs
        // leave-the-battlefield triggers; CR 603.4 governs the intervening-if
        // turn condition. This supports Oni-Cult Anvil ("one or more artifacts
        // you control leave the battlefield during your turn") and Suki,
        // Courageous Rescuer ("another permanent you control leaves the
        // battlefield during your turn").
        let (tail, turn_constraint) = peel_trailing_turn_constraint(tail);
        let turn_condition = match turn_constraint {
            Some(TriggerConstraint::OnlyDuringYourTurn) => {
                Some(TriggerCondition::DuringPlayersTurn {
                    player: PlayerFilter::Controller,
                })
            }
            Some(TriggerConstraint::OnlyDuringOpponentsTurn) => {
                Some(TriggerCondition::DuringPlayersTurn {
                    player: PlayerFilter::Opponent,
                })
            }
            _ => None,
        };
        let without_dying = all_consuming(tag::<_, _, OracleError<'_>>("without dying"))
            .parse(tail)
            .is_ok();
        if tail.is_empty() || without_dying {
            let mut def = make_base();
            def.mode = TriggerMode::LeavesBattlefield;
            def.valid_card = Some(subject.clone());
            if without_dying {
                def.destination_constraint = DestinationConstraint::NotEquals(Zone::Graveyard);
            }
            if let Some(condition) = turn_condition {
                def.condition = Some(condition);
            }
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

        // CR 603.10a + CR 603.6: "[subject] leaves <zone>" for a non-battlefield
        // zone (e.g. Murktide Regent's "whenever an instant or sorcery card
        // leaves your graveyard"). The battlefield form is handled above via the
        // dedicated LeavesBattlefield mode; other zones route through the general
        // ChangesZone matcher with a zone-change clause whose destination is
        // unconstrained (CR 603.10a -- the card may move to any zone). Reuses
        // `parse_zone_change_clause`, the same building block the
        // disjunctive-condition path uses for Syr Konrad's "leaves graveyard"
        // clause, so the runtime `zone_change_clause_matches` path is shared.
        // CR 603.4: peel the trailing "during your turn" intervening-if condition
        // off the full verb phrase before the zone-change clause parser (which
        // requires an empty tail) — the singular "card leaves your graveyard
        // during your turn" form (Kishla Skimmer) otherwise collapses to Unknown.
        // Mirrors the LeavesBattlefield branch above and the plural batched-leave
        // path; `turn_condition` was already derived from the same trailing peel.
        let (rest_peeled, _) = peel_trailing_turn_constraint(rest);
        if let Some(clause) = parse_zone_change_clause(subject, rest_peeled) {
            let mut def = make_base();
            def.mode = TriggerMode::ChangesZone;
            // CR 113.6k + CR 603.10: a self-referential leaves trigger resolves
            // after its source has left, so the ability must stay live in the
            // zones the source can reach. Non-self triggers (e.g. Murktide) live
            // on a battlefield permanent and keep the battlefield default.
            if filter_references_self(subject) {
                def.trigger_zones = vec![Zone::Battlefield, Zone::Graveyard, Zone::Exile];
            }
            def.zone_change_clauses = vec![clause];
            if let Some(condition) = turn_condition {
                def.condition = Some(condition);
            }
            return Some((TriggerMode::ChangesZone, def));
        }
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

    // CR 603.6c + CR 603.10a: "[subject] is/are returned to [possessive] hand" —
    // bounce trigger. Maps to ChangesZone from Battlefield to Hand.
    if let Some(result) = try_parse_returned_to_hand(subject, rest) {
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

    // CR 701.24: "shuffles their library" / "shuffles" — shuffle trigger
    if let Ok((tail, _)) = pair(
        alt((tag::<_, _, OracleError<'_>>("shuffles"), tag("shuffle"))),
        opt(preceded(
            space1,
            alt((
                tag("their library"),
                tag("his or her library"),
                tag("your library"),
                tag("a library"),
            )),
        )),
    )
    .parse(rest)
    {
        let mut def = make_base();
        def.mode = TriggerMode::Shuffled;
        def.valid_target = Some(subject.clone());
        attach_event_timing_tail(&mut def, tail);
        return Some((TriggerMode::Shuffled, def));
    }

    // Simple event verbs using nom alt() — each maps to a single TriggerMode
    // These are all "is_some()" pattern strip_prefix calls
    #[derive(Clone)]
    enum SimpleEvent {
        BecomesBlocked,
        BecomesSaddled,
        BecomesCrewed,
        BecomesTargetSpellOrAbility,
        BecomesTargetSpell {
            qualifier: Option<TargetFilter>,
        },
        /// CR 702.165a: the targeting source is a Backup keyword ability on the
        /// stack (e.g. Huge Truck "becomes the target of a backup ability").
        BecomesTargetBackupAbility,
        /// CR 115.1a + CR 602.2b: the targeting source is an ability (not a spell)
        /// on the stack — "becomes the target of an ability [you control]". Loki,
        /// God of Mischief.
        BecomesTargetAbility,
        DealtCombatDamage,
        DealtDamage,
        /// CR 120.10 + CR 120.2b: Excess noncombat damage received by the subject.
        DealtExcessNoncombatDamage,
        /// CR 120.10: Excess damage (combat or noncombat) received by the subject.
        DealtExcessDamage,
        BecomesTapped,
        TappedForMana,
        BecomesUntapped,
        TurnFaceUp,
        BecomesMonstrous,
        BecomesRenowned,
        Mutates,
        ExploitsCreature,
        Exploits,
        /// CR 701.44b: A permanent "explores" after the explore process completes.
        Explores,
        /// CR 701.50f: A permanent "connives" after the connive process completes.
        Connives,
        /// CR 702.100b: A creature "evolves" when +1/+1 counters are put on it
        /// as a result of its evolve ability resolving.
        Evolves,
        Transforms,
        Stations,
        SaddlesOrCrews,
        Crews,
        Saddles,
        /// Digital-only Specialize — permanent specializes into a color.
        Specializes,
        /// CR 702.26c: Permanent phases in from phased-out state.
        PhasesIn,
        /// CR 702.26b: Permanent phases out.
        PhasesOut,
        /// CR 701.3d: Equipment/Aura becomes unattached from a permanent.
        BecomesUnattached(Option<TargetFilter>),
        // CR 701.3a: Equipment/Aura becomes attached to a permanent.
        BecomesAttached,
    }
    fn parse_becomes_unattached(input: &str) -> OracleResult<'_, SimpleEvent> {
        let (remaining, _) = tag("becomes unattached").parse(input)?;
        if remaining.is_empty() {
            return Ok((remaining, SimpleEvent::BecomesUnattached(None)));
        }
        let (host, _) = tag(" from ").parse(remaining)?;
        let (filter, rest) = parse_type_phrase(host);
        if !rest.trim().is_empty() {
            return Err(nom::Err::Error(OracleError::new(
                rest,
                nom::error::ErrorKind::Eof,
            )));
        }
        Ok((rest, SimpleEvent::BecomesUnattached(Some(filter))))
    }
    fn parse_simple_event(input: &str) -> OracleResult<'_, SimpleEvent> {
        alt((
            value(SimpleEvent::BecomesBlocked, tag("becomes blocked")),
            // CR 509.1h: Plural form for batched "one or more creatures … become
            // blocked" triggers (Hezrou, etc.).
            value(SimpleEvent::BecomesBlocked, tag("become blocked")),
            // CR 702.171b: Mount becomes saddled (saddled designation acquired).
            value(SimpleEvent::BecomesSaddled, tag("becomes saddled")),
            // CR 702.122e: "Whenever [this Vehicle] becomes crewed" — trigger fires
            // when a crew ability of this Vehicle resolves. Needed for Mighty Servant
            // of Leuk-O, Mindlink Mech, etc.
            value(SimpleEvent::BecomesCrewed, tag("becomes crewed")),
            value(
                SimpleEvent::BecomesTargetSpellOrAbility,
                tag("becomes the target of a spell or ability"),
            ),
            // CR 115.1: Plural form for batched "become the target" triggers.
            value(
                SimpleEvent::BecomesTargetSpellOrAbility,
                tag("become the target of a spell or ability"),
            ),
            value(
                SimpleEvent::BecomesTargetSpell {
                    qualifier: Some(TargetFilter::Typed(
                        TypedFilter::default().subtype("Aura".to_string()),
                    )),
                },
                tag("becomes the target of an aura spell"),
            ),
            // CR 115.1a: "instant or sorcery spell" source restriction.
            value(
                SimpleEvent::BecomesTargetSpell {
                    qualifier: Some(TargetFilter::Or {
                        filters: vec![
                            TargetFilter::Typed(TypedFilter::new(TypeFilter::Instant)),
                            TargetFilter::Typed(TypedFilter::new(TypeFilter::Sorcery)),
                        ],
                    }),
                },
                alt((
                    tag("becomes the target of an instant or sorcery spell"),
                    tag("become the target of an instant or sorcery spell"),
                )),
            ),
            value(
                SimpleEvent::BecomesTargetSpell { qualifier: None },
                tag("becomes the target of a spell"),
            ),
            // CR 120.10 + CR 120.2b: Excess noncombat damage — precede generic damage arms.
            value(
                SimpleEvent::DealtExcessNoncombatDamage,
                tag("is dealt excess noncombat damage"),
            ),
            value(
                SimpleEvent::DealtExcessNoncombatDamage,
                tag("are dealt excess noncombat damage"),
            ),
            // CR 120.10: Excess damage without combat/noncombat qualifier.
            value(
                SimpleEvent::DealtExcessDamage,
                tag("is dealt excess damage"),
            ),
            value(
                SimpleEvent::DealtExcessDamage,
                tag("are dealt excess damage"),
            ),
            value(
                SimpleEvent::DealtCombatDamage,
                tag("is dealt combat damage"),
            ),
            // CR 120.2: Plural form for batched "are dealt combat damage" triggers.
            value(
                SimpleEvent::DealtCombatDamage,
                tag("are dealt combat damage"),
            ),
            value(SimpleEvent::DealtDamage, tag("is dealt damage")),
            // CR 120.2: Plural form for batched "are dealt damage" triggers.
            value(SimpleEvent::DealtDamage, tag("are dealt damage")),
            value(SimpleEvent::BecomesTapped, tag("becomes tapped")),
            // CR 701.26: Plural form for batched "one or more ... become tapped" triggers.
            value(SimpleEvent::BecomesTapped, tag("become tapped")),
            value(SimpleEvent::TappedForMana, tag("is tapped for mana")),
        ))
        .or(alt((
            // CR 702.165a: "becomes the target of a backup ability" — the source
            // is specifically a Backup keyword ability on the stack. Lives in this
            // second `alt` block (the first hit nom's tuple-arity limit); collision-
            // free with every arm because no other phrase shares its "of a backup
            // ability" suffix (neither generic spell-or-ability arm matches it).
            value(
                SimpleEvent::BecomesTargetBackupAbility,
                tag("becomes the target of a backup ability"),
            ),
            // CR 115.1: Plural form for batched "become the target" triggers.
            value(
                SimpleEvent::BecomesTargetBackupAbility,
                tag("become the target of a backup ability"),
            ),
            value(SimpleEvent::BecomesUntapped, tag("becomes untapped")),
            // CR 701.26: Plural form for batched "one or more ... become untapped" triggers.
            value(SimpleEvent::BecomesUntapped, tag("become untapped")),
            value(SimpleEvent::BecomesUntapped, tag("untaps")),
            value(SimpleEvent::TurnFaceUp, tag("is turned face up")),
            // CR 701.37b: "When ~ becomes monstrous" trigger event.
            value(
                SimpleEvent::BecomesMonstrous,
                (tag("becomes"), space1, tag("monstrous")),
            ),
            // CR 702.112b: "When [subject] becomes renowned" trigger event.
            value(
                SimpleEvent::BecomesRenowned,
                (tag("becomes"), space1, tag("renowned")),
            ),
            value(SimpleEvent::Mutates, tag("mutates")),
            // CR 702.110b: "exploits a creature" — exploit trigger
            value(SimpleEvent::ExploitsCreature, tag("exploits a creature")),
            value(SimpleEvent::Exploits, tag("exploits")),
            // CR 701.44b: "explores" / "explore" — explore trigger
            value(SimpleEvent::Explores, tag("explores")),
            value(SimpleEvent::Explores, tag("explore")),
            // CR 701.50f: "connives" — connive trigger (fires after the connive
            // process completes).
            value(SimpleEvent::Connives, tag("connives")),
            // CR 702.100b: "evolves" / "evolve" — evolve trigger
            value(SimpleEvent::Evolves, tag("evolves")),
            value(SimpleEvent::Evolves, tag("evolve")),
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
        .or(alt((
            // CR 115.1a + CR 602.2b: "becomes the target of an ability [you control]".
            // Ability-only source (excludes spells) — distinct from the spell-or-
            // ability arm. Placed in this THIRD `.or(alt(..))` block because the
            // second block is at nom 8.0's 21/21 `alt` tuple-arity ceiling. Loki,
            // God of Mischief. The trailing controller/source clause is validated by
            // the dispatch arm's remaining-empty guard (rejects source-restricted
            // siblings like Skophos Maze-Warden / Agrus Kos).
            value(
                SimpleEvent::BecomesTargetAbility,
                tag("becomes the target of an ability"),
            ),
            // CR 702.26c: "phases in" / "phase in" — phasing trigger.
            value(SimpleEvent::PhasesIn, tag("phases in")),
            value(SimpleEvent::PhasesIn, tag("phase in")),
            // CR 702.26b: "phases out" / "phase out" — phasing-out trigger.
            value(SimpleEvent::PhasesOut, tag("phases out")),
            value(SimpleEvent::PhasesOut, tag("phase out")),
            // Digital-only: "specializes" — Specialize trigger (not in CR).
            value(SimpleEvent::Specializes, tag("specializes")),
            // CR 701.3d: Equipment/Aura becomes unattached from a permanent.
            parse_becomes_unattached,
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
    fn parse_becomes_blocked_by_filter(input: &str) -> Option<TargetFilter> {
        let (type_phrase, _) = alt((tag::<_, _, OracleError<'_>>(" by a "), tag(" by an ")))
            .parse(input)
            .ok()?;
        let (filter, rest) = parse_type_phrase(type_phrase);
        rest.trim().is_empty().then_some(filter)
    }
    /// CR 509.3b: "blocks a <filter>" carries a target-side (attacker) qualifier —
    /// mirrors `parse_becomes_blocked_by_filter`'s blocker-side qualifier exactly.
    fn parse_blocks_a_filter(input: &str) -> Option<TargetFilter> {
        let (type_phrase, _) = alt((tag::<_, _, OracleError<'_>>(" a "), tag(" an ")))
            .parse(input)
            .ok()?;
        let (filter, rest) = parse_type_phrase(type_phrase);
        rest.trim().is_empty().then_some(filter)
    }
    if let Ok((remaining, event)) = parse_simple_event.parse(rest) {
        let mut def = make_base();
        match event {
            SimpleEvent::BecomesBlocked => {
                def.mode = TriggerMode::BecomesBlocked;
                def.valid_card = Some(subject.clone());
                // CR 509.3c vs 509.3d: a bare "becomes blocked" triggers once per
                // combat, while "becomes blocked by a <type>" triggers once for
                // each matching blocker. Capture the "by a/an <type phrase>"
                // qualifier as the blocker filter so the runtime matcher can tell
                // the two apart (a present `valid_target` => per-blocker). Without
                // this, plain "becomes blocked by a creature" cards (which carry no
                // further qualifier) were indistinguishable from the bare form.
                // Only the singular-article form qualifies — a threshold like
                // "by two or more creatures" stays the bare once-per-combat form.
                def.valid_target = parse_becomes_blocked_by_filter(remaining);
            }
            SimpleEvent::BecomesTargetSpellOrAbility => {
                def.mode = TriggerMode::BecomesTarget;
                set_trigger_subject(&mut def, subject);
                // CR 115.1: "a spell or ability you control" / "an opponent
                // controls" restricts the targeting source's controller. Without
                // it the trigger fires for any source — including the opponent's
                // (Valiant bug #1378).
                if let Some(controller) = parse_target_source_controller(remaining) {
                    def.valid_source = Some(becomes_target_source_filter(controller));
                }
            }
            // CR 115.1a + CR 115.1b: "target" spell text defines targeted spells,
            // and Aura spells are always targeted via enchant.
            SimpleEvent::BecomesTargetSpell { qualifier } => {
                def.mode = TriggerMode::BecomesTarget;
                set_trigger_subject(&mut def, subject);
                def.valid_source = Some(if let Some(extra_filter) = qualifier {
                    TargetFilter::And {
                        filters: vec![TargetFilter::StackSpell, extra_filter],
                    }
                } else {
                    TargetFilter::StackSpell
                });
            }
            // CR 702.165a + CR 115.1: the targeting source must be a Backup
            // keyword ability on the stack — filtered by `AbilityTag::Backup`.
            SimpleEvent::BecomesTargetBackupAbility => {
                def.mode = TriggerMode::BecomesTarget;
                set_trigger_subject(&mut def, subject);
                def.valid_source = Some(TargetFilter::StackAbility {
                    controller: None,
                    tag: Some(AbilityTag::Backup),
                    kind: None,
                });
            }
            // CR 115.1a + CR 602.2b: ability-only targeting source (no spell branch).
            // "you control" / "an opponent controls" restricts the source controller.
            // F1 guard: after consuming the OPTIONAL controller clause, the remainder
            // MUST be empty (modulo whitespace) or we fall through to Unknown. This
            // rejects source-restricted siblings whose tail this arm cannot model —
            // Skophos Maze-Warden ("...of an ability of a land you control named...")
            // and Agrus Kos ("...of an ability that targets only it...") — instead of
            // silently dropping the restriction and over-firing. Scoped to THIS arm
            // only; the shared spell-or-ability arms are untouched.
            SimpleEvent::BecomesTargetAbility => {
                let (controller, tail) = parse_target_source_controller_tail(remaining);
                if !tail.trim().is_empty() {
                    return None;
                }
                def.mode = TriggerMode::BecomesTarget;
                // CR 110.1: scope the permanent leaf to the battlefield so a targeted
                // graveyard/exile card (also a TargetRef::Object) does not fire.
                set_trigger_subject(&mut def, &battlefield_scope_permanent(subject));
                def.valid_source = Some(TargetFilter::StackAbility {
                    controller,
                    tag: None,
                    kind: None,
                });
            }
            SimpleEvent::DealtCombatDamage => {
                def.mode = TriggerMode::DamageReceived;
                def.damage_kind = DamageKindFilter::CombatOnly;
                set_trigger_subject(&mut def, subject);
            }
            // CR 120.10: Any source deals excess damage to permanents matching `subject`.
            SimpleEvent::DealtExcessDamage => {
                def.mode = TriggerMode::ExcessDamageAll;
                set_trigger_subject(&mut def, subject);
            }
            // CR 120.10 + CR 120.2b: Noncombat excess damage to `subject`.
            SimpleEvent::DealtExcessNoncombatDamage => {
                def.mode = TriggerMode::ExcessDamageAll;
                def.damage_kind = DamageKindFilter::NoncombatOnly;
                set_trigger_subject(&mut def, subject);
            }
            SimpleEvent::DealtDamage => {
                def.mode = TriggerMode::DamageReceived;
                set_trigger_subject(&mut def, subject);
            }
            SimpleEvent::BecomesTapped => {
                def.mode = TriggerMode::Taps;
                def.valid_card = Some(subject.clone());
                // CR 603.2e: a "becomes tapped" trigger event may carry a "during
                // your turn" turn restriction (Captain America, Living Legend —
                // "Whenever a creature you control becomes tapped during your turn,
                // if it's the first time …"). The intervening-if extractor (CR
                // 603.4) strips the trailing "if …" clause but leaves the "during
                // your turn" phrase
                // on the event tail (`remaining`); peel it into the trigger's turn
                // constraint so the ability only fires on the controller's turn,
                // instead of being silently dropped. `remaining` still carries the
                // trailing ", if …/untap it" clause, so match the turn phrase as a
                // LEADING prefix (boundary-checked), not an all-consuming peel.
                // Builds for the class — any "<subject> becomes tapped during
                // your/opponent's turn" trigger.
                if let Some(constraint) = parse_leading_turn_constraint(remaining) {
                    def.constraint = Some(constraint);
                }
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
            SimpleEvent::BecomesMonstrous => {
                def.mode = TriggerMode::BecomeMonstrous;
                def.valid_card = Some(subject.clone());
            }
            SimpleEvent::BecomesRenowned => {
                def.mode = TriggerMode::BecomeRenowned;
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
            SimpleEvent::Explores => {
                if !remaining.trim().is_empty() {
                    return None;
                }
                // CR 701.44b: "explores" fires after the explore process completes.
                def.mode = TriggerMode::Explored;
                def.valid_card = Some(subject.clone());
            }
            SimpleEvent::Connives => {
                if !remaining.trim().is_empty() {
                    return None;
                }
                // CR 701.50f: "connives" fires after the connive process completes.
                // `subject` ("a creature you control" / "~") scopes the conniver.
                def.mode = TriggerMode::Connives;
                def.valid_card = Some(subject.clone());
            }
            SimpleEvent::Evolves => {
                if !remaining.trim().is_empty() {
                    return None;
                }
                // CR 702.100b: "evolves" fires when +1/+1 counters are put on
                // the creature as a result of its evolve ability resolving.
                def.mode = TriggerMode::Evolved;
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
                // CR 702.122e: "Whenever [this Vehicle] becomes crewed" — fires when
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
            SimpleEvent::PhasesIn => {
                // CR 702.26c: Permanent phases in from phased-out state.
                def.mode = TriggerMode::PhaseIn;
                def.valid_card = Some(subject.clone());
            }
            SimpleEvent::PhasesOut => {
                // CR 702.26b: Permanent phases out.
                def.mode = TriggerMode::PhaseOut;
                def.valid_card = Some(subject.clone());
            }
            SimpleEvent::Specializes => {
                // Digital-only Specialize trigger (not in CR).
                def.mode = TriggerMode::Specializes;
                def.valid_card = Some(subject.clone());
            }
            SimpleEvent::BecomesUnattached(host_filter) => {
                // CR 701.3d: Equipment/Aura becomes unattached from a permanent.
                // `valid_card` records the Equipment/Aura filter (the subject).
                def.mode = TriggerMode::Unattach;
                def.valid_card = Some(subject.clone());
                def.valid_target = host_filter;
                if filter_references_self(subject) {
                    def.trigger_zones = vec![Zone::Battlefield, Zone::Graveyard, Zone::Exile];
                }
            }
            SimpleEvent::BecomesAttached => {
                // CR 701.3a: Attachment triggers share one data shape:
                // valid_card = object that became attached, valid_target = host.
                def.mode = TriggerMode::Attached;
                def.valid_card = Some(subject.clone());
                let remaining = remaining.trim();
                if matches!(subject, TargetFilter::SelfRef) {
                    // Pattern 1: "Whenever ~ becomes attached to [host]"
                    if !remaining.is_empty() {
                        let (filter, rest) = parse_type_phrase(remaining);
                        if !rest.trim().is_empty() {
                            return None;
                        }
                        def.valid_target = Some(filter);
                    }
                } else {
                    // Pattern 2: "Whenever [an Aura] becomes attached to ~"
                    if all_consuming(parse_attachment_self_host)
                        .parse(remaining)
                        .is_err()
                    {
                        return None;
                    }
                    def.valid_target = Some(TargetFilter::SelfRef);
                }
            }
        }
        return Some((def.mode.clone(), def));
    }

    // CR 119.3 + CR 603.2: "Whenever [subject] gains/loses/gains or loses life"
    // — player-scoped life-change triggers. Subject filter (`a player`,
    // `an opponent`, etc.) becomes `valid_target` so the matcher's
    // `valid_player_matches` honors the scoping.
    // Covers Exquisite Blood ("Whenever an opponent loses life, ..."),
    // Vito, Thorn of the Dusk Rose ("Whenever you gain life, each opponent loses..."),
    // Bloodchief Ascension-adjacent cards, and Moonstone Harbinger-style combined
    // life-change triggers.
    fn parse_life_verb(input: &str) -> OracleResult<'_, (TriggerMode, Option<(Comparator, u32)>)> {
        // Combined "gain or lose" form carries no amount qualifier.
        if let Ok((rest, mode)) = alt((
            value(
                TriggerMode::LifeChanged,
                tag::<_, _, OracleError<'_>>("gains or loses life"),
            ),
            value(TriggerMode::LifeChanged, tag("gain or lose life")),
        ))
        .parse(input)
        {
            return Ok((rest, (mode, None)));
        }

        // CR 119.3: singular life verb + optional magnitude qualifier + "life"
        // ("loses exactly 1 life", "lose 3 or more life"). "loses"/"gains" must
        // precede "lose"/"gain" so the longer conjugation wins.
        let (rest, mode) = alt((
            value(
                TriggerMode::LifeLost,
                alt((tag::<_, _, OracleError<'_>>("loses"), tag("lose"))),
            ),
            value(TriggerMode::LifeGained, alt((tag("gains"), tag("gain")))),
        ))
        .parse(input)?;
        let (rest, _) = tag(" ").parse(rest)?;
        let (rest, amount) = opt(parse_event_amount_quantifier).parse(rest)?;
        let (rest, _) = tag("life").parse(rest)?;
        Ok((rest, (mode, amount)))
    }
    if let Ok((tail, (mode, amount))) = parse_life_verb.parse(rest) {
        let mut def = make_base();
        def.mode = mode.clone();
        def.valid_target = Some(subject.clone());
        // CR 119.3: per-event magnitude constraint ("loses exactly N life").
        def.life_amount = amount;
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
    // CR 702.5a + CR 303.4 + CR 121.1: "enchanted player/opponent draws a card"
    // (Curse of Fool's Wisdom, Psychic Possession). Recognized ONLY in the draws
    // path — NOT in the shared parse_attached_to_prefix/_exact combinators, which
    // feed every verb. Containing it here keeps "enchanted player is dealt damage"
    // (Grievous Wound) honestly Unknown rather than a parses-but-dead
    // DamageReceived/valid_card=AttachedTo trigger. The Enchant keyword already
    // restricts attachment to the right player, so bare AttachedTo suffices.
    let enchanted_player_subject: OracleResult<'_, ()> = alt((
        value((), tag("enchanted player ")),
        value((), tag("enchanted opponent ")),
    ))
    .parse(rest);
    let (draws_subject, draws_rest) = match enchanted_player_subject {
        Ok((after_prefix, ())) => (TargetFilter::AttachedTo, after_prefix),
        Err(_) => (subject.clone(), rest),
    };
    if let Ok((tail, ())) = parse_draws_card.parse(draws_rest) {
        let mut def = make_base();
        def.mode = TriggerMode::Drawn;
        def.valid_target = Some(draws_subject.clone());
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

    // CR 702.55c: Haunt payoff — "When the creature {this card|it} haunts dies".
    // The standalone form appears on instant/sorcery haunt cards (Cry of
    // Contrition, Seize the Soul); the creature-form disjunct ("~ enters or the
    // creature it haunts dies") is split off at synthesis. The trigger functions
    // in the exile zone, where the haunting card lives (CR 702.55b).
    if (
        alt((tag::<_, _, OracleError<'_>>("whenever "), tag("when "))),
        tag("the creature "),
        alt((tag("this card "), tag("it "))),
        tag("haunts dies"),
    )
        .parse(lower)
        .is_ok()
    {
        def.mode = TriggerMode::HauntedCreatureDies;
        def.valid_card = Some(TargetFilter::SelfRef);
        def.trigger_zones = vec![Zone::Exile];
        return Some((TriggerMode::HauntedCreatureDies, def));
    }

    // CR 311.7 / CR 901.9b: "Whenever/When chaos ensues" — the active plane's
    // chaos-triggered ability. Self-referential (fires for its own plane).
    if (
        alt((tag::<_, _, OracleError<'_>>("whenever "), tag("when "))),
        tag("chaos ensues"),
    )
        .parse(lower)
        .is_ok()
    {
        def.mode = TriggerMode::ChaosEnsues;
        def.valid_card = Some(TargetFilter::SelfRef);
        return Some((TriggerMode::ChaosEnsues, def));
    }

    // CR 701.31d: "Whenever you planeswalk away from ~" — fires when this
    // plane/phenomenon is the card planeswalked away from.
    if (
        alt((tag::<_, _, OracleError<'_>>("whenever "), tag("when "))),
        tag("you planeswalk away from ~"),
    )
        .parse(lower)
        .is_ok()
    {
        def.mode = TriggerMode::PlaneswalkedFrom;
        def.valid_card = Some(TargetFilter::SelfRef);
        def.valid_target = Some(TargetFilter::Controller);
        return Some((TriggerMode::PlaneswalkedFrom, def));
    }

    // CR 312.5 / CR 701.31d: encounter == the planeswalked-to face-up endpoint.
    // A plane/phenomenon's arrival trigger fires when it becomes the face-up
    // card. Oracle text names that arrival several ways — all one phrase axis,
    // composed with a single `alt` rather than enumerated as whole sentences:
    //   * "planeswalk here"            (e.g. Ghirapur Grand Prix)
    //   * "planeswalk to ~"            (the card naming itself, normalized to ~)
    //   * "planeswalk to this plane"   (literal self-reference)
    //   * "encounter ~"                (the phenomenon naming itself, normalized)
    //   * "encounter this phenomenon"  (literal self-reference)
    // "this plane"/"this phenomenon" are absent from `SELF_REF_TYPE_PHRASES`, so
    // they survive normalization as literals — hence the explicit literal arms.
    if (
        alt((tag::<_, _, OracleError<'_>>("whenever "), tag("when "))),
        tag("you "),
        alt((
            tag("planeswalk here"),
            tag("planeswalk to ~"),
            tag("planeswalk to this plane"),
            tag("encounter ~"),
            tag("encounter this phenomenon"),
        )),
    )
        .parse(lower)
        .is_ok()
    {
        def.mode = TriggerMode::PlaneswalkedTo;
        def.valid_card = Some(TargetFilter::SelfRef);
        def.valid_target = Some(TargetFilter::Controller);
        return Some((TriggerMode::PlaneswalkedTo, def));
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
    // CR 309.7: "Whenever you complete a dungeon" — fires as that dungeon card
    // is removed from the game.
    if (
        alt((tag::<_, _, OracleError<'_>>("whenever "), tag("when "))),
        tag("you "),
        tag("complete "),
        tag("a dungeon"),
    )
        .parse(lower)
        .is_ok()
    {
        def.mode = TriggerMode::DungeonCompleted;
        def.valid_target = Some(TargetFilter::Controller);
        return Some((TriggerMode::DungeonCompleted, def));
    }

    // CR 705.2: "Whenever you/a player win(s)/lose(s) a coin flip" — FlippedCoin
    // with result filter.  Decomposed into three independent axes:
    //   1. keyword ("whenever" | "when")
    //   2. player  ("you" → Controller | "a player" → Player)
    //   3. result  ("win"/"wins" → Won | "lose"/"loses" → Lost)
    if let Some((target, result)) = parse_coin_flip_result_trigger(lower) {
        def.mode = TriggerMode::FlippedCoin;
        def.coin_flip_result = Some(result);
        def.valid_target = target;
        return Some((TriggerMode::FlippedCoin, def));
    }

    if let Some(result) = try_parse_die_roll_trigger(lower) {
        return Some(result);
    }

    // CR 104.3a: "Whenever [player] loses the game" — player-loss trigger.
    if let Some(valid_target) = parse_loses_game_trigger(lower) {
        let mut def = make_base();
        def.mode = TriggerMode::LosesGame;
        def.valid_target = valid_target;
        return Some((TriggerMode::LosesGame, def));
    }

    // CR 701.54d: "Whenever the Ring tempts you" / "When the Ring tempts you" —
    // the Ring temptation event fires once per temptation resolution.
    if all_consuming(pair(
        alt((tag::<_, _, OracleError<'_>>("whenever "), tag("when "))),
        tag("the ring tempts you"),
    ))
    .parse(lower)
    .is_ok()
    {
        def.mode = TriggerMode::RingTemptsYou;
        return Some((TriggerMode::RingTemptsYou, def));
    }
    None
}

fn parse_coin_flip_result_trigger(lower: &str) -> Option<(Option<TargetFilter>, CoinFlipResult)> {
    let (_, (_, target, result, _)) = all_consuming((
        alt((tag::<_, _, OracleError<'_>>("whenever "), tag("when "))),
        alt((
            value(Some(TargetFilter::Controller), tag("you ")),
            value(
                Some(TargetFilter::Typed(
                    TypedFilter::default().controller(ControllerRef::Opponent),
                )),
                tag("an opponent "),
            ),
            value(Some(TargetFilter::Player), tag("a player ")),
        )),
        alt((
            value(CoinFlipResult::Won, alt((tag("wins"), tag("win")))),
            value(CoinFlipResult::Lost, alt((tag("loses"), tag("lose")))),
        )),
        tag(" a coin flip"),
    ))
    .parse(lower)
    .ok()?;
    Some((target, result))
}

fn parse_loses_game_trigger(lower: &str) -> Option<Option<TargetFilter>> {
    let (_, target) = all_consuming(preceded(
        alt((tag::<_, _, OracleError<'_>>("whenever "), tag("when "))),
        parse_loses_game_actor,
    ))
    .parse(lower)
    .ok()?;
    Some(target)
}

fn parse_loses_game_actor(input: &str) -> OracleResult<'_, Option<TargetFilter>> {
    let (rest, (target, third_person)) = alt((
        value((Some(TargetFilter::Controller), false), tag("you ")),
        value(
            (
                Some(TargetFilter::Typed(
                    TypedFilter::default().controller(ControllerRef::Opponent),
                )),
                true,
            ),
            tag("an opponent "),
        ),
        value((Some(TargetFilter::Player), true), tag("a player ")),
    ))
    .parse(input)?;
    let verb = if third_person { "loses" } else { "lose" };
    let (rest, _) = pair(tag(verb), tag(" the game")).parse(rest)?;
    Ok((rest, target))
}

fn try_parse_die_roll_trigger(lower: &str) -> Option<(TriggerMode, TriggerDefinition)> {
    // CR 706.2: die-roll triggers compose the triggering player axis
    // (you/opponent/player) with the die object axis (a die/a d20/one or more dice).
    let (rest, _) = alt((
        value((), tag::<_, _, OracleError<'_>>("whenever ")),
        value((), tag("when ")),
    ))
    .parse(lower)
    .ok()?;

    let (rest, valid_target) = parse_die_roll_actor(rest).ok()?;
    let (rest, (mode, batched, die_sides, die_result)) = parse_die_roll_object(rest).ok()?;
    if !rest.is_empty() {
        return None;
    }

    let mut def = make_base();
    def.mode = mode.clone();
    def.valid_target = Some(valid_target);
    def.batched = batched;
    def.die_sides = die_sides;
    def.die_result = die_result;
    Some((mode, def))
}

fn parse_die_roll_actor(input: &str) -> OracleResult<'_, TargetFilter> {
    alt((
        value(TargetFilter::Controller, pair(tag("you "), tag("roll "))),
        value(
            TargetFilter::Typed(TypedFilter::default().controller(ControllerRef::Opponent)),
            pair(tag("an opponent "), tag("rolls ")),
        ),
        value(TargetFilter::Player, pair(tag("a player "), tag("rolls "))),
    ))
    .parse(input)
}

fn parse_die_roll_object(
    input: &str,
) -> OracleResult<'_, (TriggerMode, bool, Option<u8>, Option<DieResultFilter>)> {
    alt((
        value(
            (TriggerMode::RolledDie, true, None, None),
            tag("one or more dice"),
        ),
        value(
            (TriggerMode::RolledDieOnce, false, Some(20), None),
            tag("a d20"),
        ),
        value(
            (TriggerMode::RolledDieOnce, false, None, None),
            tag("a die"),
        ),
        // CR 706.2: "a [result]" — single face, disjunction, or GE threshold.
        // Placed after the literal "a d20"/"a die" arms; `parse_number` declines
        // on the leading "d" of "d20"/"die", so those arms always win their text.
        map(parse_die_roll_result, |filter| {
            (TriggerMode::RolledDieOnce, false, None, Some(filter))
        }),
    ))
    .parse(input)
}

/// CR 706.2: "Whenever you roll a [result]" — the rolled-face filter. Tries the
/// GE threshold ("N or higher"/"N or more") before the disjunction ("N or M")
/// so the shared "or" keyword is never mis-claimed by the disjunction arm, then
/// falls back to a single exact face. `u8::try_from` guards the d100 ceiling and
/// declines (via `oracle_err`) on any face that overflows a single byte.
fn parse_die_roll_result(input: &str) -> OracleResult<'_, DieResultFilter> {
    let (rest, _) = tag::<_, _, OracleError<'_>>("a ").parse(input)?;
    let (rest, first_raw) = nom_primitives::parse_number.parse(rest)?;
    let first = u8::try_from(first_raw).map_err(|_| oracle_err(input))?;

    // GE threshold special case: "N or higher" / "N or more" → AtLeast(N).
    if let Ok((rest, _)) =
        alt((tag::<_, _, OracleError<'_>>(" or higher"), tag(" or more"))).parse(rest)
    {
        return Ok((rest, DieResultFilter::AtLeast(first)));
    }

    // Disjunction: "N or M" → Exact([N, M]).
    if let Ok((rest, second_raw)) = preceded(
        tag::<_, _, OracleError<'_>>(" or "),
        nom_primitives::parse_number,
    )
    .parse(rest)
    {
        let second = u8::try_from(second_raw).map_err(|_| oracle_err(input))?;
        return Ok((rest, DieResultFilter::Exact(vec![first, second])));
    }

    // Single exact face: "N" → Exact([N]).
    Ok((rest, DieResultFilter::Exact(vec![first])))
}

/// CR 120.1 + CR 120.3 + CR 603.2: "Whenever a source [you control] deals
/// [combat/noncombat] [N or more] damage [to <recipient>], …" — the source-led
/// damage-event trigger class. Composes four independent axes (source filter ×
/// damage kind × amount threshold × recipient filter) so adding a new
/// recipient or a new source qualifier is a one-line change to the relevant
/// sub-combinator, not a new arm here.
///
/// Returns `None` when the line doesn't match the source-led damage shape so
/// the dispatcher can fall through to the next pattern.
fn try_parse_source_deals_damage_trigger(lower: &str) -> Option<(TriggerMode, TriggerDefinition)> {
    // CR 603.1: "When"/"Whenever" lead-in for triggered ability syntax.
    let (rest, _) = alt((
        value((), tag::<_, _, OracleError<'_>>("whenever ")),
        value((), tag("when ")),
    ))
    .parse(lower)
    .ok()?;

    // Source subject — must match before "deals".
    let (rest, source_filter) = parse_damage_source_subject(rest).ok()?;

    let (rest, _) = tag::<_, _, OracleError<'_>>("deals ").parse(rest).ok()?;

    // Shared predicate tail: optional kind, optional "N or more", "damage".
    let (after_damage, (damage_kind, threshold)) = parse_damage_predicate_tail(rest).ok()?;

    let mut def = make_base();
    def.mode = TriggerMode::DamageDone;
    def.damage_kind = damage_kind;
    def.valid_source = Some(source_filter);
    // CR 120.1 + CR 208.1 + CR 603.4: Taii Wakeen's damage==recipient-P/T shape
    // ("to <object> equal to that <object>'s toughness/power"). Tried first and
    // atomically — succeeds only when an object recipient is immediately
    // followed by the equal-to-P/T qualifier, so it never fires for "deals
    // damage equal to its power to X" (amount = source's power) nor changes the
    // recipient handling of any ordinary damage trigger.
    if let Ok((_, (filter, recipient_pt))) = parse_object_recipient_pt_gate(after_damage) {
        def.valid_target = Some(filter);
        def.condition = Some(TriggerCondition::QuantityComparison {
            lhs: QuantityExpr::Ref {
                qty: QuantityRef::EventContextAmount,
            },
            comparator: Comparator::EQ,
            rhs: QuantityExpr::Ref { qty: recipient_pt },
        });
        def.damage_amount = threshold;
        return Some((TriggerMode::DamageDone, def));
    }

    // CR 120.1 + CR 108.3: relational "to its owner" recipient (no valid_target;
    // gated by DamagedPlayerIsEventSourceOwner). Before the player-axis recipient
    // and before the bail-out guard.
    if parse_damage_to_its_owner(after_damage).is_ok() {
        def.condition = Some(append_trigger_condition(
            def.condition.take(),
            TriggerCondition::DamagedPlayerIsEventSourceOwner,
        ));
        def.damage_amount = threshold;
        return Some((TriggerMode::DamageDone, def));
    }
    // CR 115.10a + CR 120.1 + CR 120.3: "to a permanent or player" scopes the
    // trigger to damage events with any permanent/player recipient, but does not
    // create a static `valid_target` filter. The event carries the exact
    // recipient for the resolving effect.
    if parse_damage_to_permanent_or_player(after_damage).is_ok() {
        def.damage_amount = threshold;
        return Some((TriggerMode::DamageDone, def));
    }
    // CR 120.3: bare object recipient ("to a creature") — scope valid_target. The
    // terminator guard inside `parse_object_recipient_filter` declines "creature
    // or player"/"creature or opponent", which then reach the player-axis
    // qualifier below.
    if let Ok((_, filter)) = parse_object_recipient_filter(after_damage) {
        def.valid_target = Some(filter);
        def.damage_amount = threshold;
        return Some((TriggerMode::DamageDone, def));
    }

    // Optional recipient: "to <recipient>" narrows the damage target; absence
    // means any damage target may satisfy the event. If a "to ..." tail exists
    // but is not one of this parser's recipient qualifiers, leave the line for
    // narrower parsers such as "a source deals damage to this creature".
    let valid_target = parse_damage_to_qualifier(after_damage);
    let has_recipient_tail = preceded(opt(space1), tag::<_, _, OracleError<'_>>("to "))
        .parse(after_damage)
        .is_ok();
    if has_recipient_tail && valid_target.is_none() {
        return None;
    }
    def.valid_target = valid_target;
    def.damage_amount = threshold;
    Some((TriggerMode::DamageDone, def))
}

/// CR 109.4 + CR 120.1: Parse the source subject of a damage trigger up to (but
/// not including) the trailing `"deals "` verb. Returns the matching
/// `TargetFilter` and the remainder beginning at `"deals "`.
///
/// Composable axes:
///   * article         — "a " | "an "
///   * other-prefix    — optional "another "  → `FilterProp::Another`
///   * color qualifier — optional ManaColor   → `FilterProp::HasColor`
///   * head noun       — "source" | supported object type head nouns
///   * controller      — optional " you control" → `ControllerRef::You`
fn parse_damage_source_subject(input: &str) -> OracleResult<'_, TargetFilter> {
    // CR 109.4: printed damage-source phrases either use an article
    // ("a source") or the article-less determiner "another source" (Ghyrson).
    // Parse "another" on the same axis as the article so it can feed the
    // existing `FilterProp::Another` runtime evaluator.
    let (rest, another) = alt((
        value(
            Some(FilterProp::Another),
            tag::<_, _, OracleError<'_>>("another "),
        ),
        value(None, tag::<_, _, OracleError<'_>>("a ")),
        value(None, tag::<_, _, OracleError<'_>>("an ")),
    ))
    .parse(input)?;

    // Optional post-article "another " → FilterProp::Another. CR 109.4 governs object
    // identity in references; "another" reads "an object distinct from the
    // ability source" and is enforced by `FilterProp::Another` at the
    // `game/filter.rs` runtime evaluator.
    let (rest, post_article_another) = opt(value(
        FilterProp::Another,
        tag::<_, _, OracleError<'_>>("another "),
    ))
    .parse(rest)?;

    // CR 702.112b: "renowned creature you control deals..." uses the renowned
    // designation as an adjective on the damage source.
    let (rest, renowned) = opt(value(
        FilterProp::Renowned,
        tag::<_, _, OracleError<'_>>("renowned "),
    ))
    .parse(rest)?;

    // Optional color qualifier ("red source you control"). `parse_color` is the
    // shared color-word combinator and has no internal word boundary; the
    // mandatory `tag(" ")` after it is structural — without it `parse_color`
    // would match the "red" prefix of "redirect" / "redacted" / etc. and
    // misclassify the subject.
    let (rest, color) = opt(terminated(nom_primitives::parse_color, tag(" "))).parse(rest)?;

    // CR 109.4: Head noun — "source" (any), a card type, or a negated type
    // prefix ("noncreature source"). The negated variant uses
    // `TypeFilter::Non(Box::new(…))` so the runtime filter excludes that type.
    let core_head = alt((
        value(
            Some(TypeFilter::Non(Box::new(TypeFilter::Creature))),
            (
                tag::<_, _, OracleError<'_>>("noncreature source"),
                opt(tag("s")),
            ),
        ),
        value(
            None,
            (tag::<_, _, OracleError<'_>>("source"), opt(tag("s"))),
        ),
        value(Some(TypeFilter::Creature), tag("creature")),
        value(Some(TypeFilter::Artifact), tag("artifact")),
        value(Some(TypeFilter::Enchantment), tag("enchantment")),
        value(Some(TypeFilter::Planeswalker), tag("planeswalker")),
        value(Some(TypeFilter::Battle), tag("battle")),
        value(Some(TypeFilter::Land), tag("land")),
    ))
    .parse(rest);
    // CR 205.3 + CR 603.2: When the damage source is named by a creature subtype
    // ("a Salamander deals combat damage to a player" — The Sea Devils III)
    // rather than a core type or "source", fall back to the shared subtype
    // recognizer so the DamageDone trigger pattern is detected and "that player"
    // binds to TriggeringPlayer. `parse_subtype` is the single subtype authority
    // (oracle_util.rs); it returns the canonical name and consumed byte length.
    let (rest, head_type) = match core_head {
        Ok((rest, head_type)) => (rest, head_type),
        Err(_) => match crate::parser::oracle_util::parse_subtype(rest) {
            Some((subtype, consumed)) => (&rest[consumed..], Some(TypeFilter::Subtype(subtype))),
            None => {
                return Err(nom::Err::Error(OracleError::new(
                    rest,
                    nom::error::ErrorKind::Alt,
                )))
            }
        },
    };

    // Optional controller scope. Absence → no controller restriction
    // (matches any source — Phyrexian Obliterator class, deferred).
    // CR 109.4: "a source you control" / "a source an opponent controls".
    let (rest, controller) = opt(alt((
        value(
            ControllerRef::You,
            tag::<_, _, OracleError<'_>>(" you control"),
        ),
        value(
            ControllerRef::Opponent,
            tag::<_, _, OracleError<'_>>(" an opponent controls"),
        ),
    )))
    .parse(rest)?;

    // Optional "with <property>" clause on the damage source — delegates to the
    // shared `parse_with_property` inner-clause combinator so every "with X" the
    // filter grammar supports (P/T constraints, keywords, etc.) attaches to the
    // source. CR 208.1 + CR 613.4b: Ms. Marvel, Elastic Ally — "a creature you
    // control with power greater than its base power deals combat damage…".
    let (rest, with_prop) = opt(preceded(space1, parse_with_property)).parse(rest)?;

    // Require trailing space before the "deals" verb so we don't match
    // "sourceless" / "sourced".
    let (rest, _) = tag::<_, _, OracleError<'_>>(" ").parse(rest)?;

    let mut typed = head_type.map_or_else(TypedFilter::default, TypedFilter::new);
    if let Some(c) = controller {
        typed = typed.controller(c);
    }
    let mut props = Vec::new();
    if let Some(p) = another.or(post_article_another) {
        props.push(p);
    }
    if let Some(p) = renowned {
        props.push(p);
    }
    if let Some(col) = color {
        props.push(FilterProp::HasColor { color: col });
    }
    if let Some(p) = with_prop {
        props.push(p);
    }
    if !props.is_empty() {
        typed.properties = props;
    }
    Ok((rest, TargetFilter::Typed(typed)))
}

/// CR 120.3: Parse the damage-kind adjective (`"combat "` / `"noncombat "`).
/// Each branch consumes its trailing space so the caller can chain directly
/// into `tag("damage")`.
fn parse_damage_kind_adjective(input: &str) -> OracleResult<'_, DamageKindFilter> {
    alt((
        value(DamageKindFilter::CombatOnly, tag("combat ")),
        value(DamageKindFilter::NoncombatOnly, tag("noncombat ")),
    ))
    .parse(input)
}

/// CR 120.1 + CR 120.3 + CR 603.2: Compose the predicate tail that follows the
/// damage verb (`"deal"` / `"deals"`) — optional kind adjective, optional
/// amount quantifier, and the mandatory `"damage"` head noun.
/// Returns the parsed kind/amount pair and the remainder after `"damage"` so
/// the caller can hand it to `parse_damage_to_qualifier`.
///
/// Used by both the source-led grammar (`try_parse_source_deals_damage_trigger`)
/// and the subject-led grammar in `try_parse_event`. Keeping the tail in one
/// combinator means a new kind (e.g. "noncreature damage") or a new comparator
/// (e.g. "less than N") is added in exactly one place.
fn parse_damage_predicate_tail(
    input: &str,
) -> OracleResult<'_, (DamageKindFilter, Option<(Comparator, u32)>)> {
    let (rest, kind) = opt(parse_damage_kind_adjective).parse(input)?;
    let (rest, amount) = opt(parse_event_amount_quantifier).parse(rest)?;
    let (rest, _) = tag("damage").parse(rest)?;
    Ok((rest, (kind.unwrap_or(DamageKindFilter::Any), amount)))
}

/// CR 120.1 + CR 208.1 + CR 603.2: Parse the `"equal to <recipient>'s
/// toughness|power"` tail that gates a damage trigger on the dealt amount
/// matching the damaged object's characteristic (Taii Wakeen — "deals noncombat
/// damage to a creature equal to that creature's toughness").
///
/// The possessive antecedent is a demonstrative referring back to the damage
/// recipient ("that creature's" / "that permanent's" / "that planeswalker's") or
/// the pronoun "its"; all resolve to `ObjectScope::EventTarget` (the object that
/// received the triggering damage). Returns the recipient-relative `QuantityRef`
/// so the caller can lift it into the intervening-`if` `QuantityComparison`.
fn parse_damage_equal_to_recipient_pt(input: &str) -> OracleResult<'_, QuantityRef> {
    let (rest, _) = tag("equal to ").parse(input.trim_start())?;
    let (rest, _) = alt((
        tag("that creature's "),
        tag("that permanent's "),
        tag("that planeswalker's "),
        tag("its "),
    ))
    .parse(rest)?;
    alt((
        value(
            QuantityRef::Toughness {
                scope: ObjectScope::EventTarget,
            },
            tag("toughness"),
        ),
        value(
            QuantityRef::Power {
                scope: ObjectScope::EventTarget,
            },
            tag("power"),
        ),
    ))
    .parse(rest)
}

/// CR 603.2: Parse an event-magnitude quantifier ("`5 or more `" / "`exactly 5 `")
/// that precedes a head noun, returning the resulting `(Comparator, threshold)`
/// pair. The trailing space is consumed so the caller can chain directly into
/// the head noun (`tag("damage")` for CR 120.1 damage triggers,
/// `tag("life")` for CR 119.3 life triggers). Event-agnostic by design — the
/// caller decides which constraint field the pair lands on.
///
/// `"less than N"` slots in here via the same axis when needed.
fn parse_event_amount_quantifier(input: &str) -> OracleResult<'_, (Comparator, u32)> {
    fn parse_or_more(input: &str) -> OracleResult<'_, (Comparator, u32)> {
        let (rest, n) = nom_primitives::parse_number(input)?;
        let (rest, _) = tag(" or more ").parse(rest)?;
        Ok((rest, (Comparator::GE, n)))
    }

    fn parse_exactly(input: &str) -> OracleResult<'_, (Comparator, u32)> {
        let (rest, _) = tag("exactly ").parse(input)?;
        let (rest, n) = nom_primitives::parse_number(rest)?;
        let (rest, _) = tag(" ").parse(rest)?;
        Ok((rest, (Comparator::EQ, n)))
    }

    alt((parse_exactly, parse_or_more)).parse(input)
}

/// CR 601.2a + CR 707.10: "When[ever] {actor} cast[s] or cop[ies] a[n] [type] spell"
/// — unified parser for all SpellCastOrCopy trigger variants. `match_spell_cast`
/// in trigger_matchers.rs validates the caster against `valid_target`, so:
/// - "you cast or copy"       → `TargetFilter::Controller`
/// - "an opponent casts or copies" → `TypedFilter` with `ControllerRef::Opponent`
///   (evaluates as `source_controller != player_id` in the current engine model)
/// - "a player casts or copies" → `TargetFilter::Player` (any player, CR 102.1)
///
/// Covers Storm-Kiln Artist (you), Mage Hunter (opponent), and any future card
/// that triggers on any player casting or copying a spell.
fn try_parse_casts_or_copies_trigger(lower: &str) -> Option<(TriggerMode, TriggerDefinition)> {
    // CR 603.1: "When"/"Whenever" trigger keyword.
    let (rest, _) = alt((
        value((), tag::<_, _, OracleError<'_>>("whenever ")),
        value((), tag("when ")),
    ))
    .parse(lower)
    .ok()?;

    // Actor + verb phrase — encodes caster identity in valid_target.
    // `terminated` binds each actor tag to its conjugated verb so the two
    // dimensions (who + action) are parsed as a single atomic combinator arm.
    let (rest, caster_filter) = alt((
        // "you cast or copy " — controller of the triggered permanent.
        terminated(
            value(
                TargetFilter::Controller,
                tag::<_, _, OracleError<'_>>("you"),
            ),
            tag(" cast or copy "),
        ),
        // "An opponent" uses the engine's existing opponent controller filter.
        terminated(
            value(
                TargetFilter::Typed(TypedFilter::default().controller(ControllerRef::Opponent)),
                tag::<_, _, OracleError<'_>>("an opponent"),
            ),
            tag(" casts or copies "),
        ),
        // "a player" in Oracle means any player including the controller (CR 102.1).
        terminated(
            value(TargetFilter::Player, tag("a player")),
            tag(" casts or copies "),
        ),
    ))
    .parse(rest)
    .ok()?;

    // CR 601.2a + CR 707.10: optional spell-type restriction.
    // `terminated(..., tag(" spell"))` avoids repeating the trailing word across arms.
    let (_, spell_filter) = terminated(
        alt((
            value(
                Some(TargetFilter::Or {
                    filters: vec![
                        TargetFilter::Typed(TypedFilter::new(TypeFilter::Instant)),
                        TargetFilter::Typed(TypedFilter::new(TypeFilter::Sorcery)),
                    ],
                }),
                tag::<_, _, OracleError<'_>>("an instant or sorcery"),
            ),
            value(
                Some(TargetFilter::Typed(TypedFilter::new(TypeFilter::Creature))),
                tag("a creature"),
            ),
            value(
                Some(TargetFilter::Typed(TypedFilter::new(TypeFilter::Instant))),
                tag("an instant"),
            ),
            value(
                Some(TargetFilter::Typed(TypedFilter::new(TypeFilter::Sorcery))),
                tag("a sorcery"),
            ),
            // Generic — any spell, no type restriction.
            value(None, tag("a")),
        )),
        tag(" spell"),
    )
    .parse(rest)
    .ok()?;

    let mut def = make_base();
    def.mode = TriggerMode::SpellCastOrCopy;
    def.valid_card = spell_filter;
    def.valid_target = Some(caster_filter);
    Some((TriggerMode::SpellCastOrCopy, def))
}

fn try_parse_special_trigger_pattern(lower: &str) -> Option<(TriggerMode, TriggerDefinition)> {
    if let Some(result) = try_parse_self_or_another_controlled_subtype_enters(lower) {
        return Some(result);
    }

    if let Some(result) = try_parse_another_controlled_subtype_enters(lower) {
        return Some(result);
    }

    // "of the chosen type" variant: "whenever a/an [type] you control of the
    // chosen type enters". Must precede the bare subtype variant because the
    // subtype variant's `take_until(" you control enters")` would fail on this
    // form (the sentinel is interrupted by "of the chosen type").
    if let Some(result) = try_parse_controlled_chosen_type_enters(lower) {
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

    if let Some(result) = try_parse_you_attack_with_commander(lower) {
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

    // CR 120.1 + CR 120.3: "a source [you control] deals [combat/noncombat]
    // [N or more] damage to <recipient>". One combinator-driven parser that
    // composes four independent axes:
    //   * source filter   — `parse_source_subject` ("a source", "a source you control",
    //                       "another source you control", "a <color> source you control")
    //   * damage kind     — `DamageKindFilter` axis (any / combat-only / noncombat-only)
    //   * amount threshold — optional "N or more " quantifier → `damage_amount`
    //   * recipient       — `parse_damage_to_qualifier` (player / opponent / planeswalker / you / …)
    // Covers Dragonborn Champion ("…deals 5 or more damage to a player, draw a card")
    // and the earlier-shipped "source you control deals noncombat damage to an
    // opponent" pattern (Virtue of Courage) without a second arm.
    if let Some(result) = try_parse_source_deals_damage_trigger(lower) {
        return Some(result);
    }

    fn parse_source_deals_damage_to_self(input: &str) -> OracleResult<'_, ()> {
        all_consuming(preceded(
            alt((tag("whenever "), tag("when "))),
            value((), (tag("a source "), tag("deals damage to "), tag("~"))),
        ))
        .parse(input)
    }

    // CR 120.3: Damage to this card can cause an ability to trigger.
    // "this creature" / card name is normalized to ~ before trigger parsing.
    if parse_source_deals_damage_to_self(lower).is_ok() {
        let mut def = make_base();
        def.mode = TriggerMode::DamageReceived;
        def.valid_card = Some(TargetFilter::SelfRef);
        return Some((TriggerMode::DamageReceived, def));
    }

    if let Some(result) = try_parse_commit_crime(lower) {
        return Some(result);
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
            // CR 508.3b: only fires when the player themselves is attacked,
            // not when a planeswalker they control or battle they protect is.
            def.attack_target_filter = Some(AttackTargetFilter::Player);
            return Some((TriggerMode::Attacks, def));
        }
    }

    // CR 601.2a + CR 707.10: all "cast or copy a spell" trigger variants —
    // covers "you", "an opponent", and "a player" actor phrases.
    if let Some(result) = try_parse_casts_or_copies_trigger(lower) {
        return Some(result);
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

    // CR 700.4 + CR 120.1 + CR 608.2i: "another creature dealt damage this turn
    // by [source filter] dies" (Shelob, Child of Ungoliant).
    let mut damaged_this_turn_prefix = alt((
        tag::<_, _, OracleError<'_>>("whenever another creature dealt damage this turn by "),
        tag("when another creature dealt damage this turn by "),
    ));
    if let Ok((rest, _)) = damaged_this_turn_prefix.parse(lower) {
        if let Some((after_source, source)) =
            super::oracle_replacement::parse_damage_history_source(rest)
        {
            if tag::<_, _, OracleError<'_>>(" dies")
                .parse(after_source)
                .is_ok()
            {
                let mut def = make_base();
                def.mode = TriggerMode::ChangesZone;
                def.origin = Some(Zone::Battlefield);
                def.destination = Some(Zone::Graveyard);
                def.valid_card = Some(TargetFilter::Typed(
                    TypedFilter::creature().properties(vec![FilterProp::Another]),
                ));
                def.condition = Some(TriggerCondition::DealtDamageThisTurnBySource { source });
                return Some((TriggerMode::ChangesZone, def));
            }
        }
    }

    // CR 603.8: "when ~ has no [type] counters on it" / "when ~ has N or more
    // [type] counters on it" — state trigger on the source permanent's counter
    // state. Two accepted forms:
    //   - Depletion (minimum: 0, maximum: Some(0)): Dark Depths, Afiya Grove —
    //     self-limiting because the effect removes the source or its counters.
    //   - Threshold (minimum > 0, maximum: None): Darksteel Reactor — self-limiting
    //     because the effect ends the game (WinTheGame) before re-firing.
    if let Some(result) = try_parse_source_counter_state_trigger(lower) {
        return Some(result);
    }

    None
}

/// CR 603.8: Parse "when ~ has [no / N or more] [type] counters on it" as a
/// state trigger.
///
/// Delegates subject × quantity × counter-type × "on it" grammar to
/// `parse_source_has_counters` and bridges the resulting
/// `StaticCondition::HasCounters` to a `TriggerCondition` via
/// `static_condition_to_trigger_condition` — the same path intervening-if
/// counter conditions use.
///
/// Two accepted forms:
/// - **Depletion** (`minimum: 0, maximum: Some(0)`): Dark Depths, Afiya Grove,
///   vanishing-style "has no … counters". Self-limiting: the effect removes the
///   source from the battlefield or depletes its counters.
/// - **Threshold** (`minimum > 0, maximum: None`): Darksteel Reactor ("has
///   twenty or more charge counters"). For all currently printed threshold state
///   triggers, the effect is self-limiting (the game ends, the source leaves
///   the battlefield, or counters fall below the threshold before re-checking);
///   the parser does not enforce this constraint structurally.
fn try_parse_source_counter_state_trigger(lower: &str) -> Option<(TriggerMode, TriggerDefinition)> {
    let (rest, _) = alt((tag::<_, _, OracleError<'_>>("whenever "), tag("when ")))
        .parse(lower)
        .ok()?;
    let (_, static_cond) = parse_source_has_counters(rest).ok()?;
    // CR 603.8: accept depletion form (minimum: 0, maximum: Some(0)) and
    // threshold form (minimum > 0, maximum: None). Reject mixed/range forms.
    if !matches!(
        static_cond,
        StaticCondition::HasCounters {
            minimum: 0,
            maximum: Some(0),
            ..
        } | StaticCondition::HasCounters {
            minimum: 1..,
            maximum: None,
            ..
        }
    ) {
        return None;
    }
    let condition = static_condition_to_trigger_condition(&static_cond)?;
    let mut def = make_base();
    def.mode = TriggerMode::StateCondition;
    def.condition = Some(condition);
    def.valid_card = Some(TargetFilter::SelfRef);
    Some((TriggerMode::StateCondition, def))
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
                exclude_source: crate::types::ability::SourceExclusion::Include,
            },
        ),
        (
            " that is enchanted by an aura you control",
            FilterProp::HasAttachment {
                kind: AttachmentKind::Aura,
                controller: Some(ControllerRef::You),
                exclude_source: crate::types::ability::SourceExclusion::Include,
            },
        ),
        (
            " that are equipped by an equipment you control",
            FilterProp::HasAttachment {
                kind: AttachmentKind::Equipment,
                controller: Some(ControllerRef::You),
                exclude_source: crate::types::ability::SourceExclusion::Include,
            },
        ),
        (
            " that is equipped by an equipment you control",
            FilterProp::HasAttachment {
                kind: AttachmentKind::Equipment,
                controller: Some(ControllerRef::You),
                exclude_source: crate::types::ability::SourceExclusion::Include,
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

/// CR 508.1a: True when `filter` narrows the attacker class beyond a bare
/// "creature[s]" head noun — i.e. it carries a subtype/negated-type/property
/// constraint or a non-creature type. Used to decide whether a typed
/// attacker-COUNT trigger ("two or more Dinosaurs attack") needs the
/// condition-level type axis on `AttackersDeclaredCount`'s `Controller`
/// subject, or whether the untyped "two or more creatures attack" path can
/// keep `filter: None`.
///
/// Controller scope ("creatures you control") does NOT narrow the class for
/// counting purposes — it is already enforced by `matching_you_attack_pairs`'
/// attacking-player gate, and every attacker in a CR 506.2 batch shares one
/// controller — so a bare `Creature`/`Permanent` filter with only a controller
/// set still returns `false` here.
fn filter_narrows_beyond_creature(filter: &TargetFilter) -> bool {
    match filter {
        TargetFilter::Typed(tf) => {
            !tf.properties.is_empty()
                || tf
                    .type_filters
                    .iter()
                    .any(|t| !matches!(t, TypeFilter::Creature | TypeFilter::Permanent))
        }
        // Disjunctions, SelfRef, Another, etc. always carry a meaningful
        // narrowing — count them.
        _ => true,
    }
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

/// CR 903.3 + CR 508.1: "whenever you attack with [your] commander" — fires when
/// the scoped player declares a commander as an attacker (Jocasta, Automaton Avenger).
fn try_parse_you_attack_with_commander(lower: &str) -> Option<(TriggerMode, TriggerDefinition)> {
    let (after_prefix, ()) = alt((
        value((), tag::<_, _, OracleError<'_>>("whenever ")),
        value((), tag("when ")),
    ))
    .parse(lower)
    .ok()?;

    let (after_actor, ()) = value((), tag::<_, _, OracleError<'_>>("you attack with "))
        .parse(after_prefix)
        .ok()?;

    let (filter, rest) = parse_commander_subject_filter_prefix(after_actor)?;
    if !rest.trim().is_empty() {
        return None;
    }

    let mut def = make_base();
    def.mode = TriggerMode::YouAttack;
    def.valid_target = Some(TargetFilter::Typed(
        TypedFilter::default().controller(ControllerRef::You),
    ));
    def.valid_card = Some(filter);
    def.batched = true;
    Some((TriggerMode::YouAttack, def))
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

        let has_attachment_clause = attachment_prop.is_some();
        let filter = apply_attachment_prop(filter, attachment_prop);

        let mut def = make_base();
        def.mode = TriggerMode::YouAttack;
        // CR 508.3a: a lexical "attack a player" restriction narrows the attacked
        // target through the purpose-built attack_target_filter channel, not the
        // valid_target overload.
        if attacks_player {
            def.attack_target_filter = Some(AttackTargetFilter::Player);
        }
        // CR 303.4e + CR 506.2: an attachment-relation subject ("enchanted by an
        // Aura / equipped by an Equipment you control") binds "you control" to the
        // attachment, not the attacker — the enchanted/equipped creature may be
        // controlled by an opponent. Set the attacking-player gate to pass-through
        // (any attacking player) WITHOUT an attack-target restriction, so the trigger
        // fires regardless of whether the attack targets a player, planeswalker, or
        // battle (Killian, Decisive Mentor; #3314).
        if has_attachment_clause {
            def.valid_target = Some(TargetFilter::Player);
        }
        if min_count > 1 {
            // CR 508.1a + CR 603.2c: the count condition must count only attackers
            // of the SAME filtered class (e.g. Dinosaurs), not every co-attacker —
            // otherwise "two or more Dinosaurs attack" over-fires on 1 Dinosaur +
            // 1 unrelated attacker. This head-noun form is not source-relative,
            // so use the AttackersDeclared batch count rather than the
            // source-excluding MinCoAttackers condition.
            let count_filter = filter_narrows_beyond_creature(&filter).then_some(filter.clone());
            def.condition = Some(TriggerCondition::AttackersDeclaredCount {
                subject: AttackersDeclaredCountSubject::Controller {
                    scope: ControllerRef::You,
                    filter: count_filter,
                },
                comparator: Comparator::GE,
                count: min_count,
            });
        }
        def.valid_card = Some(filter);
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
///   - `condition = AttackersDeclaredCount { subject: Controller { scope, filter },
///     comparator: GE, count }` so only batches with at least N attackers from the
///     scoped player — matching the typed `filter` when present — fire the trigger.
///     The typed count>1 case ("attack with two or more Dinosaurs") parses the
///     head-noun type phrase into both the matcher's `valid_card` gate and the
///     subject's `filter` (CR 508.1), so it counts only Dinosaurs and cannot
///     over-fire.
fn try_parse_attack_with_n_creatures(lower: &str) -> Option<(TriggerMode, TriggerDefinition)> {
    use nom::combinator::opt;

    let (after_prefix, ()) = alt((
        value((), tag::<_, _, OracleError<'_>>("whenever ")),
        value((), tag("when ")),
    ))
    .parse(lower)
    .ok()?;

    // Actor dispatch. Handle scoped actors first (you / another player / an
    // opponent). Also handle the any-player head-noun form "a player attacks"
    // (e.g. Aurelia, the Law Above) by mapping it to `ControllerRef::TriggeringPlayer`
    // and emitting an `Attacks` trigger so the triggering player becomes the
    // event's subject during matching/resolution.
    let actor_parse = alt((
        value(
            ControllerRef::You,
            tag::<_, _, OracleError<'_>>("you attack"),
        ),
        value(ControllerRef::Opponent, tag("another player attacks")),
        value(ControllerRef::Opponent, tag("an opponent attacks")),
        value(
            ControllerRef::TriggeringPlayer,
            tag::<_, _, OracleError<'_>>("a player attacks"),
        ),
    ))
    .parse(after_prefix)
    .ok()?;
    let (after_actor, actor): (&str, ControllerRef) = actor_parse;

    // CR 508.3e: "attacks you with N or more creatures" — player-level attack
    // trigger where the defending player is the controller (Trouble in Pairs).
    let (after_target, attacks_you) = if let Ok((rest, ())) =
        value((), tag::<_, _, OracleError<'_>>(" you")).parse(after_actor)
    {
        (rest, true)
    } else {
        (after_actor, false)
    };

    // Required " with " separator.
    let (after_with, ()) = value((), tag::<_, _, OracleError<'_>>(" with "))
        .parse(after_target)
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

    // Capture the head-noun type phrase once for both count==1 and count>1.
    // Count==1 needs only the matcher's valid_card gate; count>1 additionally
    // uses AttackersDeclaredCount when the type phrase narrows beyond bare
    // "creatures".
    let (filter, remainder) = parse_type_phrase(after_or_more);
    // Accept optional trailing " each turn" / " this turn" qualifier (unused here,
    // but keeps the matcher permissive for CR 603.4 timing qualifiers). Must end
    // at the condition boundary — the caller already split the effect text off,
    // so the remainder should be empty or punctuation-only.
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
    def.batched = true;

    // If the actor is the triggering player (the any-player head-noun form),
    // emit an `Attacks` trigger scoped to a generic player source; otherwise
    // emit the legacy `YouAttack` batched trigger and attach a controller
    // filter to `valid_target` so the matcher resolves the attacking player.
    let mode = if matches!(actor, ControllerRef::TriggeringPlayer) {
        def.valid_source = Some(TargetFilter::Player);
        TriggerMode::Attacks
    } else {
        def.valid_target = Some(TargetFilter::Typed(
            TypedFilter::default().controller(actor.clone()),
        ));
        TriggerMode::YouAttack
    };
    def.mode = mode.clone();
    if attacks_you {
        def.attack_target_filter = Some(AttackTargetFilter::Player);
    }
    if n == 1 {
        // CR 508.1 + CR 603.2c: the matcher's "at least one attacker matching
        // valid_card" gate is the whole "one or more" condition.
        def.valid_card = Some(filter);
        return Some((mode, def));
    }

    // CR 508.1: for count > 1, only typed head nouns need a condition-level
    // filter. Bare "creatures" keeps the pre-existing untyped batch count.
    let narrows = filter_narrows_beyond_creature(&filter);
    if narrows {
        def.valid_card = Some(filter.clone());
    }
    let count_filter = narrows.then_some(filter);
    def.condition = Some(TriggerCondition::AttackersDeclaredCount {
        subject: if attacks_you {
            AttackersDeclaredCountSubject::AttackTarget {
                controller: ControllerRef::You,
                attacked: AttackTargetFilter::Player,
                filter: count_filter,
            }
        } else {
            AttackersDeclaredCountSubject::Controller {
                scope: actor,
                filter: count_filter,
            }
        },
        comparator: Comparator::GE,
        count: n,
    });

    Some((mode, def))
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
    // CR 701.7 + CR 603.2c: "one or more" form fires once per batch.
    let batched_match = alt((
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
    .ok();

    // CR 701.7: Simple "you create a token" fires per token (not batched).
    let simple_match = alt((
        value((), tag::<_, _, OracleError<'_>>("whenever you create a ")),
        value((), tag::<_, _, OracleError<'_>>("when you create a ")),
    ))
    .parse(lower)
    .map(|(r, _)| ((), r))
    .ok();

    let (rest, batched) = if let Some((_, rest)) = batched_match {
        (rest, true)
    } else if let Some((_, rest)) = simple_match {
        (rest, false)
    } else {
        return None;
    };

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
    def.batched = batched;
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
            TargetFilter::Typed(TypedFilter::card())
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
                    TargetFilter::Or { filters }
                } else {
                    continue;
                }
            } else {
                let (filter, remainder) = parse_type_phrase(type_text);
                if !remainder.trim().is_empty() {
                    continue;
                }
                filter
            }
        } else {
            continue;
        };

        let mut def = make_base();
        def.mode = TriggerMode::ChangesZoneAll;
        def.origin = Some(Zone::Graveyard);
        let scoped = with_owner_scope(filter, ControllerRef::You);
        def.batched = true;
        // CR 113.6 / CR 113.6b: a permanent's triggered ability functions only on the
        // battlefield unless it states otherwise, so this batched "leave your graveyard"
        // trigger keeps make_base()'s battlefield-only default. CR 113.6k + CR 603.10a:
        // when the source card is itself the object leaving its own graveyard, the trigger
        // condition cannot trigger from the battlefield and needs graveyard/exile zones.
        if filter_references_self(&scoped) {
            def.trigger_zones = vec![Zone::Battlefield, Zone::Graveyard, Zone::Exile];
        }
        def.valid_card = Some(scoped);
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
        // CR 113.6 / CR 113.6b: this batched "cards are put into exile from
        // library/graveyard" ability is a permanent's triggered ability whose source
        // (e.g. Laelia the Blade Reforged, Rakshasa Vizier) is on the battlefield, and
        // it doesn't state that it functions from any other zone — so it keeps
        // make_base()'s battlefield-only default. There is no self-referential subject
        // here (valid_card is None), so no graveyard/exile look-back zones are needed.
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
        // CR 120.1a: Try battle-inclusive suffixes first (longer match wins).
        // Covers "deal(s) combat damage to a player or (a )battle".
        let (subject_text, recipient_filter) = if let Ok(("", t)) = terminated(
            take_until::<_, _, OracleError<'_>>(" deal"),
            (
                tag(" deal"),
                opt(tag("s")),
                tag(" combat damage to a player or "),
                opt(tag("a ")),
                tag("battle"),
            ),
        )
        .parse(rest)
        {
            (
                t,
                TargetFilter::Or {
                    filters: vec![
                        TargetFilter::Player,
                        TargetFilter::Typed(TypedFilter::new(TypeFilter::Battle)),
                    ],
                },
            )
        } else if let Some(t) = rest
            .strip_suffix(" deal combat damage to a player")
            .or_else(|| rest.strip_suffix(" deals combat damage to a player"))
        {
            (t, TargetFilter::Player)
        } else {
            continue;
        };

        let (filter, remainder) = parse_type_phrase(subject_text);
        let filter = if remainder.trim().is_empty() {
            filter
        } else if let Some(or_filter) = try_split_or_compound_type_phrase(subject_text) {
            // CR 205.3m: Handle "ninja or rogue creatures you control" compound subtypes
            or_filter
        } else {
            // Try to parse "with [property]" qualifiers appended after the type phrase.
            // Covers cards like Primo, the Unbounded: "creatures you control with base power 0".
            let mut rem = remainder.trim();
            let mut extra_props = Vec::new();
            while let Ok((next_rem, prop)) = parse_with_property(rem) {
                extra_props.push(prop);
                rem = next_rem.trim();
            }
            if !extra_props.is_empty() && rem.is_empty() {
                match filter {
                    TargetFilter::Typed(mut tf) => {
                        tf.properties.extend(extra_props);
                        TargetFilter::Typed(tf)
                    }
                    other => other,
                }
            } else {
                continue;
            }
        };

        let mut def = make_base();
        def.mode = TriggerMode::DamageDoneOnceByController;
        def.damage_kind = DamageKindFilter::CombatOnly;
        def.valid_source = Some(filter);
        def.valid_target = Some(recipient_filter);
        def.batched = true;
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

/// Parse "whenever a/an/another [type] you control of the chosen type enters
/// [the battlefield]". Covers Dawn-Blessed Pennant ("permanent you control of
/// the chosen type"), Molten Echoes ("nontoken creature you control of the
/// chosen type"), and similar — the `IsChosenCreatureType` filter prop restricts
/// the trigger to permanents matching the source's chosen creature type.
///
/// Composed from nom combinators: prefix `alt` for article/another variants,
/// `take_until` for the type word extraction, `tag` for the sentinel, and `opt`
/// for the optional " the battlefield" suffix.
fn try_parse_controlled_chosen_type_enters(
    lower: &str,
) -> Option<(TriggerMode, TriggerDefinition)> {
    // Detect "another" prefix to set the Another property.
    let (after_prefix, another) = alt((
        value(true, tag::<_, _, OracleError<'_>>("whenever another ")),
        value(true, tag("when another ")),
        value(false, tag("whenever a ")),
        value(false, tag("whenever an ")),
        value(false, tag("when a ")),
        value(false, tag("when an ")),
    ))
    .parse(lower)
    .ok()?;

    // Extract the type word(s) before " you control of the chosen type enters".
    let sentinel = " you control of the chosen type enters";
    let (after_type, type_text) = take_until::<_, _, OracleError<'_>>(sentinel)
        .parse(after_prefix)
        .ok()?;

    // Consume the sentinel.
    let (after_sentinel, ()) = value((), tag::<_, _, OracleError<'_>>(sentinel))
        .parse(after_type)
        .ok()?;

    // Accept either bare "enters" (sentinel already consumed it) or
    // "enters the battlefield".
    let (tail, ()) = alt((
        value((), tag::<_, _, OracleError<'_>>(" the battlefield")),
        value((), tag("")),
    ))
    .parse(after_sentinel)
    .ok()?;

    if !tail.is_empty() {
        return None;
    }

    // Parse the type word (e.g. "permanent", "creature", "nontoken creature").
    // Use parse_type_phrase which handles "nontoken" prefixes and type words.
    let (filter, remainder) = parse_type_phrase(type_text);
    if !remainder.is_empty() {
        return None;
    }

    // Build the valid_card filter with IsChosenCreatureType + controller + optional Another.
    let mut props = vec![FilterProp::IsChosenCreatureType];
    if another {
        props.push(FilterProp::Another);
    }

    // Extract the TypedFilter from the parsed filter and augment it.
    let valid_card = match filter {
        TargetFilter::Typed(mut typed) => {
            typed.controller = Some(ControllerRef::You);
            typed.properties.extend(props);
            TargetFilter::Typed(typed)
        }
        _ => {
            // parse_type_phrase should always return Typed for a type word;
            // if not, bail.
            return None;
        }
    };

    let mut def = make_base();
    def.mode = TriggerMode::ChangesZone;
    def.destination = Some(Zone::Battlefield);
    def.valid_card = Some(valid_card);
    Some((TriggerMode::ChangesZone, def))
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

/// CR 303.4e: nom combinator for the "enchanted [creature|permanent]'s controller"
/// possessive that scopes an Aura phase trigger to the controller of the permanent
/// the source is attached to. Composed along its grammar axes — the fixed
/// `enchanted ` lead, the object-kind alternation (`creature`/`permanent`), the
/// possessive apostrophe (ASCII `'s` or curly `’s`), and the `controller` head —
/// rather than enumerating the cartesian product of literals, so a new object-kind
/// sibling is a single `alt()` arm. Matched at any word boundary by the caller
/// because the phrase trails the phase noun ("the upkeep of …").
fn parse_enchanted_controller_phrase(input: &str) -> OracleResult<'_, ()> {
    let (input, _) = tag("enchanted ").parse(input)?;
    let (input, _) = alt((tag("creature"), tag("permanent"))).parse(input)?;
    let (input, _) = alt((tag("'s"), tag("\u{2019}s"))).parse(input)?;
    let (input, _) = tag(" controller").parse(input)?;
    Ok((input, ()))
}

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
    let phase = scan_for_phase(phase_text);
    let is_generic_main_phase = phase.is_none() && scan_for_generic_main_phase(phase_text);
    def.phase = phase;

    // CR 503.1a / CR 507.1: Parse possessive qualifier and trailing suffix for turn constraint.
    // Uses nom prefix dispatch: opponent possessives checked before bare "your" to avoid
    // "your opponent's" matching as "your".
    let turn_constraint = parse_turn_constraint(phase_text);
    def.constraint = if is_generic_main_phase {
        match turn_constraint {
            // CR 505.1 + CR 603.2b: "each of your main phases" is not one
            // concrete phase; it triggers at the start of either main phase
            // on the source controller's turn.
            Some(TriggerConstraint::OnlyDuringYourTurn) => {
                Some(TriggerConstraint::OnlyDuringYourMainPhase)
            }
            other => other,
        }
    } else {
        turn_constraint
    };
    if scan_contains(phase_text, "enchanted player's") {
        def.valid_target = Some(TargetFilter::AttachedTo);
    }
    // CR 303.4e + CR 109.4 + CR 503.1: "the upkeep of enchanted creature's
    // controller" (Super Intelligence) / "enchanted permanent's controller" —
    // the turn-owner scope is the controller of the permanent this Aura is
    // attached to, which per CR 303.4e may differ from the Aura's controller.
    // `ParentTargetController` resolves to the attached permanent's controller
    // for Aura phase triggers. Without this scope the phase trigger has no
    // player constraint and fires on EVERY player's phase — a Howling-Mine
    // over-fire (issue #5275). The phrase is composed along its axes
    // (`enchanted` + object kind + possessive) and matched at any word boundary
    // (it trails the phase noun, e.g. "the upkeep of …").
    else if nom_primitives::scan_at_word_boundaries(phase_text, parse_enchanted_controller_phrase)
        .is_some()
    {
        def.valid_target = Some(TargetFilter::ParentTargetController);
    }
    if scan_contains(phase_text, "chosen player's")
        || scan_contains(phase_text, "chosen player\u{2019}s ")
        || scan_contains(phase_text, "chosen players ")
        || scan_contains(phase_text, "the chosen player's")
        || scan_contains(phase_text, "the chosen player\u{2019}s ")
        || scan_contains(phase_text, "the chosen players ")
    {
        def.valid_target = Some(TargetFilter::SourceChosenPlayer);
    }
    // CR 603.2b + CR 103.8: "at the beginning of the first upkeep of the game"
    // names one unique step — the starting player's first upkeep — so the trigger
    // fires exactly once. Modeled as OncePerGame: the source (a plane/scheme or
    // other permanent present at game start) fires the first time its upkeep
    // trigger is checked and never again. Checked before the "each turn" arm
    // because "of the game" and "each turn" are mutually exclusive phrasings.
    if scan_contains(phase_text, "first upkeep of the game") {
        def.constraint = Some(TriggerConstraint::OncePerGame);
    } else if scan_contains(phase_text, "first upkeep") && scan_contains(phase_text, "each turn") {
        def.constraint = Some(TriggerConstraint::MaxTimesPerTurn { max: 1 });
    }
    // "each player's upkeep" / "each upkeep" / "the end step" → no constraint (fires every turn)

    Some((TriggerMode::Phase, def))
}

/// Avatar crossover: recognize a single bending verb and map it to its
/// specific `TriggerMode`. The full four-verb disjunction is collapsed by
/// `try_parse_bend_trigger` to `TriggerMode::ElementalBend` (which matches any of
/// the four bending `GameEvent`s for the source's controller); a partial
/// disjunction fails closed (see `try_parse_bend_trigger`). Single source of
/// truth for both the trigger-mode dispatch and the `continues_player_action_list`
/// condition/effect boundary check.
fn parse_bend_verb(input: &str) -> OracleResult<'_, TriggerMode> {
    alt((
        value(TriggerMode::Waterbend, tag("waterbend")),
        value(TriggerMode::Earthbend, tag("earthbend")),
        value(TriggerMode::Firebend, tag("firebend")),
        value(TriggerMode::Airbend, tag("airbend")),
    ))
    .parse(input)
}

/// Avatar crossover (CR 603.2): "whenever you {waterbend|earthbend|firebend|
/// airbend}[, {verb}]*[, or {verb}]" — a single bending verb fires its specific
/// bend trigger; the full four-verb batch (Avatar Aang) fires on ANY of the four
/// bend events via `TriggerMode::ElementalBend`, whose matcher
/// `match_elemental_bend` already scopes to the source's controller.
///
/// A PARTIAL disjunction (a strict subset of two or three distinct verbs, e.g.
/// "whenever you waterbend or earthbend") has no faithful runtime representation:
/// the only any-bend matcher is `match_elemental_bend`, which fires on all four,
/// and there is no parameterized bend-set matcher yet. Collapsing a partial set to
/// `ElementalBend` would over-fire on the unlisted bend events. So this parser
/// returns `None` for any partial set, leaving such cards to fail closed
/// (strict-failure `Unknown`) rather than ship a trigger broader than its
/// semantics. When a partial-bend card actually appears, add a parameterized
/// bend-set matcher and route the parsed set through to it. `valid_target =
/// Controller` is redundant with the matcher's controller scoping but kept for
/// consistency with the other player-action bend-adjacent triggers.
fn try_parse_bend_trigger(lower: &str) -> Option<(TriggerMode, TriggerDefinition)> {
    let rest = alt((
        value((), tag::<_, _, OracleError<'_>>("whenever you ")),
        value((), tag("when you ")),
    ))
    .parse(lower)
    .map(|(rest, ())| rest)
    .ok()?;

    let mut modes: Vec<TriggerMode> = Vec::new();
    let mut remaining = rest.trim();
    loop {
        let (after_verb, mode) = parse_bend_verb(remaining).ok()?;
        modes.push(mode);
        // Consume an optional list separator: ", or ", ", ", " or " — or stop at
        // the end of the condition clause.
        let next = alt((
            value((), tag::<_, _, OracleError<'_>>(", or ")),
            value((), tag(", ")),
            value((), tag(" or ")),
        ))
        .parse(after_verb);
        match next {
            Ok((tail, ())) => remaining = tail.trim_start(),
            Err(_) => {
                if !after_verb.trim().is_empty() {
                    return None;
                }
                break;
            }
        }
    }

    let distinct: std::collections::HashSet<&TriggerMode> = modes.iter().collect();
    let mode = match modes.as_slice() {
        [] => return None,
        [single] => single.clone(),
        // CR 603.2: only the complete four-verb batch maps to the any-bend matcher.
        // Anything narrower (partial subset, or repeated verbs) lacks a faithful
        // runtime matcher and must fail closed rather than over-fire.
        _ if distinct.len() == 4 => TriggerMode::ElementalBend,
        _ => return None,
    };

    let mut def = make_base();
    def.mode = mode.clone();
    def.valid_target = Some(TargetFilter::Controller);
    Some((mode, def))
}

/// Parse player-centric triggers: "you gain life", "you cast a/an ...", "you draw a card"
fn try_parse_player_trigger(lower: &str) -> Option<(TriggerMode, TriggerDefinition)> {
    // Avatar crossover: bending-verb triggers ("whenever you waterbend, …") must
    // run before the generic player-action dispatch, which does not recognize the
    // bend verbs and would fall through to `TriggerMode::Unknown`.
    if let Some(result) = try_parse_bend_trigger(lower) {
        return Some(result);
    }

    if let Some(result) = try_parse_player_action_trigger(lower) {
        return Some(result);
    }

    // CR 702.49a: "whenever you activate a ninjutsu ability" — ninjutsu-family activation trigger.
    // Covers all ninjutsu variants (ninjutsu, commander ninjutsu, sneak).
    if let Some(result) = try_parse_keyword_activation_trigger(lower) {
        return Some(result);
    }

    // CR 602.1 + CR 605.1a: "Whenever <player> activates an ability that
    // isn't a mana ability" — generic activated-ability trigger class.
    // Covers Burning-Tree Shaman, Flamescroll Celebrant. Ordering is
    // incidental here: "activates" is not in `parse_player_action_phrase`'s
    // lookup table, and `try_parse_keyword_activation_trigger` rejects any
    // shape lacking a keyword name, so the earlier dispatchers correctly
    // fall through to this one regardless of position.
    if let Some(result) = try_parse_ability_activation_trigger(lower) {
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
    // CR 725.1: "Whenever you become the monarch" / "Whenever an opponent becomes
    // the monarch" — monarch-designation trigger. Decompose into prefix (whenever/when)
    // + subject (you/opponent/player) + verb (become/becomes) + "the monarch".
    if let Some((valid_target, after)) = parse_become_monarch_trigger(lower) {
        if !after.trim().is_empty() {
            return None;
        }
        let mut def = make_base();
        def.mode = TriggerMode::BecomeMonarch;
        def.valid_target = valid_target;
        return Some((TriggerMode::BecomeMonarch, def));
    }

    // CR 726.2: "Whenever you take the initiative" / "Whenever a player takes the
    // initiative" — initiative-designation trigger.
    if let Some((valid_target, after)) = parse_takes_initiative_trigger(lower) {
        if !after.trim().is_empty() {
            return None;
        }
        let mut def = make_base();
        def.mode = TriggerMode::TakesInitiative;
        def.valid_target = valid_target;
        return Some((TriggerMode::TakesInitiative, def));
    }

    // CR 309.4c + CR 701.49: "Whenever you venture into the dungeon" — fires each
    // time the controller's venture marker enters a dungeon room.
    if let Some(after) = parse_venture_into_dungeon_trigger(lower) {
        if !after.trim().is_empty() {
            return None;
        }
        let mut def = make_base();
        def.mode = TriggerMode::RoomEntered;
        def.valid_target = Some(TargetFilter::Controller);
        return Some((TriggerMode::RoomEntered, def));
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

    // CR 603.8: "when you control a [filter]" — the positive-existence sibling of
    // the "control no [filter]" arm above. Fires whenever the controller controls
    // a permanent matching the filter (Endangered Armodon: "When you control a
    // creature with toughness 2 or less, sacrifice this creature."). Filter
    // recognition is delegated to `parse_inner_condition` (the shared game-state
    // condition authority) and bridged to `TriggerCondition::ControlsType` via
    // `static_condition_to_trigger_condition`, so every presence filter the
    // condition parser already handles (subtype, type, P/T comparator, keyword,
    // …) is covered without re-implementing filter parsing here. Gated on a
    // `ControlsType` result so only genuine single-permanent presence conditions
    // become state triggers; the effect ("sacrifice this creature") is parsed
    // separately by the caller, exactly as for the `ControlsNone` arm.
    for prefix in ["whenever ", "when "] {
        if let Ok((rest, ())) = value((), tag::<_, _, OracleError<'_>>(prefix)).parse(lower) {
            if let Ok((cond_rest, sc)) = parse_inner_condition(rest) {
                if cond_rest.trim().is_empty() {
                    if let Some(cond @ TriggerCondition::ControlsType { .. }) =
                        static_condition_to_trigger_condition(&sc)
                    {
                        let mut def = make_base();
                        def.mode = TriggerMode::StateCondition;
                        def.condition = Some(cond);
                        def.valid_card = Some(TargetFilter::SelfRef);
                        return Some((TriggerMode::StateCondition, def));
                    }
                }
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

    // CR 601.1a + CR 701.18b: "whenever you play a card" — playing a card means
    // playing it as a land OR casting it as a spell, so this fires on both
    // events. A single `PlayCard` trigger mode (Recycle, Null Profusion) covers
    // both via `match_play_card`. Matched before the land-play arm; the
    // "a card" object cannot match the land-play helper's "a land"/"another land"
    // object, so neither shadows the other.
    if let Some(origin) = parse_play_card_trigger_subject(lower) {
        let mut def = make_base();
        def.mode = TriggerMode::PlayCard;
        def.valid_target = Some(TargetFilter::Controller);
        // CR 601.1a + CR 400.1: the cast half honors the play origin via
        // `spell_cast_origin`; `match_play_card` gates the land half on the same
        // constraint. `Any` (no "from <zone>" tail) preserves plain play-card
        // triggers unchanged.
        def.spell_cast_origin = origin;
        return Some((TriggerMode::PlayCard, def));
    }

    // CR 305.1 + CR 603.2 + CR 701.18a: "whenever [X] plays/play a land
    // [from <zone>]" fires on the CR 305 special action. Handles both the
    // third-person "plays a land" form (a player, an opponent) and the
    // second-person "play a land" form (you — e.g. Fastbond). The optional
    // from-zone tail rides through `parse_type_phrase`, matching the existing
    // cast-spell trigger shape used by Rocco, Street Chef.
    if let Some((valid_target, land_filter)) = parse_land_play_trigger_subject(lower) {
        let mut def = make_base();
        def.mode = TriggerMode::LandPlayed;
        def.valid_target = valid_target;
        def.valid_card = land_filter;
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

    // CR 701.26: "you tap an untapped creature an opponent controls"
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
        let Some((subject_text, produced_filter)) = split_taps_for_mana_for_clause(rest) else {
            continue;
        };
        let (filter, remainder) =
            parse_trigger_subject(&subject_text, &mut ParseContext::default());
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
        def.taps_for_mana_produced = produced_filter;
        return Some((TriggerMode::TapsForMana, def));
    }

    // CR 603.2 + CR 605.1a: "Whenever <actor> taps <subject> for mana".
    // Shared frame for both:
    //   - "a player taps …"  → no controller constraint on the source
    //   - "an opponent taps …" → source must be opponent-controlled (Vorinclex class)
    if let Ok((rem, (actor_controller, subject_text, produced_filter))) =
        parse_taps_for_mana_actor_line(lower)
    {
        if rem.trim().is_empty() {
            let (mut filter, sub_rem) =
                parse_trigger_subject(&subject_text, &mut ParseContext::default());
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
                def.taps_for_mana_produced = produced_filter;
                return Some((TriggerMode::TapsForMana, def));
            }
        }
    }

    // CR 603.2 + CR 613.3: "When you lose control of ~" — fires after a
    // controller change on the source object. Maps to ChangesController so
    // the trigger fires on the GainControl/GiveControl effect-resolved event.
    // valid_card = SelfRef gates the trigger to changes on this specific object.
    // The trigger controller is the previous controller (still Khârn's holder
    // at trigger-scan time, because layer re-evaluation runs after trigger
    // collection in the post-action pipeline).
    fn parse_you_lose_control_of_self(i: &str) -> OracleResult<'_, ()> {
        all_consuming(preceded(
            alt((tag("when "), tag("whenever "))),
            value((), tag("you lose control of ~")),
        ))
        .parse(i)
    }
    if parse_you_lose_control_of_self(lower).is_ok() {
        let mut def = make_base();
        def.mode = TriggerMode::ChangesController;
        def.valid_card = Some(TargetFilter::SelfRef);
        return Some((TriggerMode::ChangesController, def));
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

    // CR 107.14: "Whenever you get one or more {E}" — batched energy-counter trigger.
    if scan_contains(lower, "you get one or more {e}") {
        let mut def = make_base();
        def.mode = TriggerMode::CounterPlayerAddedAll;
        def.valid_target = Some(TargetFilter::Controller);
        def.batched = true;
        return Some((TriggerMode::CounterPlayerAddedAll, def));
    }

    fn parse_countering_spell_or_ability_line(i: &str) -> OracleResult<'_, ControllerRef> {
        preceded(
            alt((tag("whenever "), tag("when "))),
            delimited(
                tag("a spell or ability "),
                alt((
                    value(ControllerRef::You, tag("you control")),
                    value(ControllerRef::Opponent, tag("an opponent controls")),
                )),
                tag(" counters a spell"),
            ),
        )
        .parse(i)
    }
    if let Ok((rem, controller)) = parse_countering_spell_or_ability_line(lower) {
        if rem.trim().is_empty() {
            // CR 109.5 + CR 701.6 + CR 603.2: "Whenever a spell or ability you
            // control counters a spell" fires on `SpellCountered` events where the
            // countering spell/ability controller matches this controller filter.
            let mut def = make_base();
            def.mode = TriggerMode::Countered;
            def.valid_source = Some(TargetFilter::Typed(
                TypedFilter::default().controller(controller),
            ));
            return Some((TriggerMode::Countered, def));
        }
    }

    // CR 701.6a + CR 603.2 + CR 108.4: "Whenever a spell you've cast is
    // countered" is the *passive* dual of the countering-side arm above -- it
    // fires when a spell whose controller is you leaves the stack via a
    // counter. `TriggerMode::Countered` matches on `SpellCountered` events;
    // `valid_card` gates the *countered* spell (the event's `object_id`), so a
    // `You` controller filter restricts the trigger to your own countered
    // spells, exactly as `valid_card_matches` evaluates it inside
    // `match_countered`. This differs from the arm above, which gates the
    // *countering* source via `valid_source`. A spell's controller is
    // "you've cast"/"you control" both (CR 108.4), so both possessive forms
    // route to the single `ControllerRef::You` filter.
    fn parse_own_spell_countered_line(i: &str) -> OracleResult<'_, ()> {
        value(
            (),
            all_consuming(preceded(
                alt((tag("whenever "), tag("when "))),
                preceded(
                    tag("a spell "),
                    terminated(
                        alt((tag("you've cast"), tag("you control"))),
                        tag(" is countered"),
                    ),
                ),
            )),
        )
        .parse(i)
    }
    if parse_own_spell_countered_line(lower).is_ok() {
        let mut def = make_base();
        def.mode = TriggerMode::Countered;
        def.valid_card = Some(TargetFilter::Typed(
            TypedFilter::default().controller(ControllerRef::You),
        ));
        return Some((TriggerMode::Countered, def));
    }

    // CR 601.2: "Whenever you cast a/an [type] spell [post-spell modifier]" — extract
    // the spell filter. Handles pre-spell type qualifier, post-spell modifier
    // (e.g. "with {X} in its mana cost", CR 107.3 + CR 202.1), or both.
    // CR 603.4: "another" prefix adds FilterProp::Another to exclude the source.
    if let Some((_, is_another, after)) = scan_preceded(lower, |i| {
        alt((
            value(true, tag::<_, _, OracleError<'_>>("you cast another ")),
            value(false, tag("you cast an ")),
            value(false, tag("you cast a ")),
        ))
        .parse(i)
    }) {
        let mut def = make_base();
        def.mode = TriggerMode::SpellCast;
        // "you" = trigger's controller
        def.valid_target = Some(TargetFilter::Controller);

        // CR 105.2 + CR 601.2a: "spell that's <colors>" disjunctions span commas
        // (Questing Druid: "a spell that's white, blue, black, or red"). The
        // condition/effect splitter keeps the full list in `after`; recognize it
        // on the untruncated remainder before the comma-truncation below, which
        // would otherwise cut the color list to its first leg.
        if let Some(filter) = parse_spell_that_clause_filter(after.trim()) {
            let filter = if is_another {
                add_another_prop(filter)
            } else {
                filter
            };
            def.valid_card = Some(filter);
            return Some((TriggerMode::SpellCast, def));
        }

        // Truncate at ", " so any effect clause doesn't leak into the type
        // parser — but skip commas that continue an Oxford-comma type list
        // (CR 205.3a: "an Aura, Equipment, or Vehicle spell" is set-union
        // sugar whose internal commas belong to the payload). Naive
        // first-comma truncation collapsed such filters to their first leg,
        // so the trigger fired only on the first listed type (issue #5324,
        // Sram, Senior Edificer).
        let payload = {
            let mut scan = after;
            let mut cut = after;
            while let Ok((_, (_, rest))) = nom_primitives::split_once_on(scan, ", ") {
                if starts_with_type_list_continuation(rest) {
                    scan = rest;
                } else {
                    cut = &after[..after.len() - rest.len() - ", ".len()];
                    break;
                }
            }
            cut
        }
        .trim();
        let (payload, spell_not_owned_by_you) = strip_spell_not_owned_qualifier(payload);
        let (payload, turn_constraint) = peel_trailing_turn_constraint(payload);
        if let Some(constraint) = turn_constraint {
            def.constraint = Some(constraint);
        }

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

        // CR 700.2: Peel an optional leading "modal " qualifier off the payload
        // BEFORE the type-phrase / spell-qualifier parsers run. "modal" is not a
        // card type, so `parse_type_phrase("modal spell")` yields an empty filter
        // and `parse_spell_qualifier_payload` treats "modal" as an unknown pre-
        // spell word — either way the modality is silently dropped and
        // `valid_card` is left `None`, over-triggering on every spell (issue
        // #750, Riku, of Many Paths). Peeling here reduces the payload to the
        // bare type phrase (e.g. "modal spell" → "spell", "modal instant spell"
        // → "instant spell"); the modality is re-attached as `FilterProp::Modal`
        // after the filter is built. Scoped to the type-phrase / spell-qualifier
        // paths below — the color-disjunction `parse_spell_that_clause_filter`
        // branch above (Questing Druid: "a spell that's white…") returns before
        // reaching here and is not exercised by any "modal <color>" card.
        let (payload, is_modal) = opt(value((), tag::<_, _, OracleError<'_>>("modal ")))
            .parse(payload)
            .map(|(rest, matched)| (rest, matched.is_some()))
            .unwrap_or((payload, false));

        // First, try the post-spell-modifier-aware decomposition for shapes
        // that include "with {X} in its mana cost" etc.
        if let Some(filter) = parse_spell_qualifier_payload(payload) {
            let filter = if is_another {
                add_another_prop(filter)
            } else {
                filter
            };
            let filter = if is_modal {
                add_modal_prop(filter)
            } else {
                filter
            };
            let filter = if spell_not_owned_by_you {
                with_owner_scope(filter, ControllerRef::Opponent)
            } else {
                filter
            };
            def.valid_card = Some(filter);
            return Some((TriggerMode::SpellCast, def));
        }

        // Fall back to the classic type-phrase parser for bare type filters.
        let (filter, _rest) = parse_type_phrase(payload);
        let filter = if is_another {
            add_another_prop(filter)
        } else {
            filter
        };
        let filter = if is_modal {
            add_modal_prop(filter)
        } else {
            filter
        };
        let filter = if spell_not_owned_by_you {
            with_owner_scope(filter, ControllerRef::Opponent)
        } else {
            filter
        };
        let is_meaningful = match &filter {
            TargetFilter::Typed(tf) => tf.has_meaningful_type_constraint(),
            // Or-filters are always meaningful (e.g. "instant or sorcery spell")
            TargetFilter::Or { .. } => true,
            _ => false,
        };
        if is_meaningful || spell_not_owned_by_you {
            def.valid_card = Some(filter);
        }
        return Some((TriggerMode::SpellCast, def));
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
            } else if scan_contains(who, "enchanted player") {
                // CR 303.4m + CR 702.5a: "enchanted player" refers to whatever
                // player this Aura is attached to (CR 303.4m); Enchant player
                // (CR 702.5a) guarantees the source is attached to a player. So
                // "Whenever enchanted player casts a [quality] spell" (Maddening
                // Hex) scopes the caster to the enchanted player. `AttachedTo` —
                // which `player_matches_filter` resolves to the source's attached
                // player — fires the trigger only for that player's casts, not
                // every player's. Without this the caster filter stays unset (any
                // player).
                def.valid_target = Some(TargetFilter::AttachedTo);
            }

            // Parse the spell quality generically (e.g., "creature spell", "multicolored spell")
            // using the same parse_type_phrase building block as the "you cast" branch above.
            // The condition/effect boundary was already split by `split_trigger`; do not
            // truncate on ", " here — spell qualities may contain commas (Talion:
            // "mana value, power, or toughness equal to the chosen number").
            let after_casts = &lower[who.len() + " casts a".len()..].trim_start();
            let after_article = value((), tag::<_, _, OracleError<'_>>("n ")) // "an" → strip the trailing "n "
                .parse(after_casts)
                .map(|(rest, _)| rest)
                .unwrap_or(after_casts)
                .trim_start();
            let (spell_clause, spell_not_owned_by_caster) =
                strip_spell_they_dont_own_qualifier(after_article);
            let (spell_clause, turn_constraint) = peel_trailing_turn_constraint(spell_clause);
            if let Some(constraint) = turn_constraint {
                def.constraint = Some(constraint);
            }
            if spell_not_owned_by_caster {
                def.valid_target = Some(TargetFilter::TriggeringPlayer);
            }
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
            // Handle "with mana value [, power, or toughness] equal to the chosen number"
            // (Talion, the Kindly Lord). CR 202.3 + CR 208.1: disjunctive match on
            // mana value, power, or toughness against the source's chosen number.
            if let Some(base_tf) = parse_spell_chosen_number_quality(spell_clause) {
                let filter = if spell_not_owned_by_caster {
                    with_owner_scope(TargetFilter::Typed(base_tf), ControllerRef::Opponent)
                } else {
                    TargetFilter::Typed(base_tf)
                };
                def.valid_card = Some(filter);
                return Some((TriggerMode::SpellCast, def));
            }
            // Handle "multicolored" as a spell property (not a type phrase)
            if scan_contains(spell_clause, "multicolored") {
                let filter = if spell_not_owned_by_caster {
                    with_owner_scope(
                        TargetFilter::Typed(TypedFilter::default().properties(vec![
                            FilterProp::ColorCount {
                                comparator: Comparator::GE,
                                count: 2,
                            },
                        ])),
                        ControllerRef::Opponent,
                    )
                } else {
                    TargetFilter::Typed(TypedFilter::default().properties(vec![
                        FilterProp::ColorCount {
                            comparator: Comparator::GE,
                            count: 2,
                        },
                    ]))
                };
                def.valid_card = Some(filter);
            } else {
                let (filter, _rest) = parse_type_phrase(spell_clause);
                let is_meaningful = match &filter {
                    TargetFilter::Typed(tf) => tf.has_meaningful_type_constraint(),
                    TargetFilter::Or { .. } => true,
                    _ => false,
                };
                let filter = if spell_not_owned_by_caster {
                    with_owner_scope(filter, ControllerRef::Opponent)
                } else {
                    filter
                };
                if is_meaningful || spell_not_owned_by_caster {
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

    // CR 120.2a (combat damage) + CR 120.1 (damage): the player is dealt combat
    // damage. Both active ("you're dealt combat damage") and passive ("combat
    // damage is dealt to you") voice describe the same event; this arm must
    // precede the generic "dealt damage" arm below.
    if matches!(
        lower,
        "whenever you're dealt combat damage"
            | "when you're dealt combat damage"
            | "whenever combat damage is dealt to you"
            | "when combat damage is dealt to you"
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

    // CR 701.38: "whenever players finish voting" fires once when all votes
    // for a vote instruction have been cast and tallied.
    // Cards: Model of Unity, Erestor of the Council, Grudge Keeper.
    if all_consuming(preceded(
        alt((tag::<_, _, OracleError<'_>>("whenever "), tag("when "))),
        value(
            (),
            (tag("players"), space1, tag("finish"), space1, tag("voting")),
        ),
    ))
    .parse(lower)
    .is_ok()
    {
        let mut def = make_base();
        def.mode = TriggerMode::Vote;
        return Some((TriggerMode::Vote, def));
    }

    // CR 701.30b-c: "whenever you clash" fires when the controller of the
    // trigger source is either player participating in a clash.
    // Cards: Entangling Trap, Rebellion of the Flamekin.
    //
    // CR 701.30d + CR 603.4: an optional "...and win" tail (Sylvan Echoes)
    // narrows the trigger so it fires ONLY when the controller won the clash.
    // The win requirement rides on `clash_result` into trigger MATCHING (checked
    // when the clash event occurs), so a lost or tied clash never creates a
    // pending trigger — rather than gating the effect at resolution.
    if let Ok((tail, ())) = preceded(
        alt((tag::<_, _, OracleError<'_>>("whenever "), tag("when "))),
        value((), (tag("you"), space1, tag("clash"))),
    )
    .parse(lower)
    {
        // Empty residual = plain "you clash" (any outcome). A " and win" residual
        // = the win-required shape (Sylvan Echoes). Any other residual is a
        // different clause we decline so a later dispatcher can try it.
        let clash_result = if tail.is_empty() {
            Some(None)
        } else if all_consuming(value(
            (),
            (
                space1,
                tag::<_, _, OracleError<'_>>("and"),
                space1,
                tag("win"),
            ),
        ))
        .parse(tail)
        .is_ok()
        {
            Some(Some(ClashResult::Won))
        } else {
            None
        };
        if let Some(clash_result) = clash_result {
            let mut def = make_base();
            def.mode = TriggerMode::Clashed;
            def.valid_target = Some(TargetFilter::Controller);
            def.clash_result = clash_result;
            return Some((TriggerMode::Clashed, def));
        }
    }

    // CR 701.30: "whenever a player clashes" — fires for any clashing player.
    if all_consuming(preceded(
        alt((tag::<_, _, OracleError<'_>>("whenever "), tag("when "))),
        value(
            (),
            (tag("a"), space1, tag("player"), space1, tag("clashes")),
        ),
    ))
    .parse(lower)
    .is_ok()
    {
        let mut def = make_base();
        def.mode = TriggerMode::Clashed;
        return Some((TriggerMode::Clashed, def));
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
        (
            "whenever each opponent ",
            Some(TargetFilter::Typed(
                TypedFilter::default().controller(ControllerRef::Opponent),
            )),
        ),
        ("whenever a player ", Some(TargetFilter::Player)),
        ("when you ", Some(TargetFilter::Controller)),
        (
            "when an opponent ",
            Some(TargetFilter::Typed(
                TypedFilter::default().controller(ControllerRef::Opponent),
            )),
        ),
        (
            "when each opponent ",
            Some(TargetFilter::Typed(
                TypedFilter::default().controller(ControllerRef::Opponent),
            )),
        ),
        ("when a player ", Some(TargetFilter::Player)),
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
            // CR 701.59a: Collect evidence — exile cards from your graveyard with total mana value N or more.
            [PlayerActionKind::CollectEvidence] => {
                def.mode = TriggerMode::CollectEvidence;
                return Some((TriggerMode::CollectEvidence, def));
            }
            // CR 701.16a: Investigate — create a Clue artifact token.
            [PlayerActionKind::Investigate] => {
                def.mode = TriggerMode::Investigated;
                return Some((TriggerMode::Investigated, def));
            }
            // CR 701.24a: Shuffle — player-action trigger, scoped by
            // valid_target so "you", "an opponent", and "a player" forms all
            // use the same matcher path.
            [PlayerActionKind::ShuffledLibrary] => {
                def.mode = TriggerMode::Shuffled;
                return Some((TriggerMode::Shuffled, def));
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
    if let Ok(("", action)) = parse_proliferate_player_action(text) {
        return Some(action);
    }
    match text {
        "search your library" | "searches their library" => Some(PlayerActionKind::SearchedLibrary),
        "scry" | "scries" => Some(PlayerActionKind::Scry),
        "surveil" | "surveils" => Some(PlayerActionKind::Surveil),
        // CR 701.59a: Collect evidence — exile cards from your graveyard with total mana value N or more.
        "collect evidence" | "collects evidence" => Some(PlayerActionKind::CollectEvidence),
        // CR 701.16a: Investigate — create a Clue artifact token.
        "investigate" | "investigates" => Some(PlayerActionKind::Investigate),
        "shuffle your library"
        | "shuffles their library"
        | "shuffle their library"
        | "shuffles his or her library"
        | "shuffle his or her library"
        | "shuffles a library"
        | "shuffle a library" => Some(PlayerActionKind::ShuffledLibrary),
        _ => None,
    }
}

fn parse_proliferate_player_action(input: &str) -> OracleResult<'_, PlayerActionKind> {
    // CR 701.34a: Proliferate — choose permanents/players with counters.
    all_consuming(alt((
        value(PlayerActionKind::Proliferate, tag("proliferate")),
        value(PlayerActionKind::Proliferate, tag("proliferates")),
    )))
    .parse(input)
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
/// timing), or "during <player's> turn" (restricted to the active player
/// matching the parsed `PlayerFilter`; e.g. The Council of Four, Rashmi and
/// Ragavan).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum NthEventTimingKind {
    /// "each turn" / "in a turn" — no turn-ownership restriction.
    Unrestricted,
    /// Restricted to a specific player's turn (CR 603.4 + CR 102.1).
    /// The inner `PlayerFilter` identifies whose turn must be active.
    Restricted(PlayerFilter),
}

/// Map a timing kind to the intervening-if condition it implies, if any.
/// `Unrestricted` implies no condition. The restricted kinds gate on
/// the active player via `TriggerCondition::DuringPlayersTurn`.
fn timing_condition(timing: NthEventTimingKind) -> Option<TriggerCondition> {
    match timing {
        NthEventTimingKind::Unrestricted => None,
        NthEventTimingKind::Restricted(player) => {
            Some(TriggerCondition::DuringPlayersTurn { player })
        }
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

/// Nom combinator for a complete timing-tail clause: matches unrestricted
/// per-turn tails ("each turn", "in a turn") and player-scoped turn tails
/// ("during each opponent's turn", "during their turn", "during each of their
/// turns", "during each of your turns", "during your turn"). Wrapped in
/// `all_consuming` so it succeeds only when the clause consumes the entire
/// (already-trimmed) input. Shared by the nth-spell, nth-draw, and related
/// event timing classifiers.
fn parse_timing_tail(i: &str) -> OracleResult<'_, NthEventTimingKind> {
    all_consuming(alt((
        value(NthEventTimingKind::Unrestricted, tag("each turn")),
        value(NthEventTimingKind::Unrestricted, tag("in a turn")),
        value(
            NthEventTimingKind::Restricted(PlayerFilter::Opponent),
            alt((
                tag("during each opponent's turn"),
                tag("during each opponent\u{2019}s turn"),
            )),
        ),
        value(
            NthEventTimingKind::Restricted(PlayerFilter::TriggeringPlayer),
            alt((tag("during their turn"), tag("during each of their turns"))),
        ),
        value(
            NthEventTimingKind::Restricted(PlayerFilter::Controller),
            alt((tag("during each of your turns"), tag("during your turn"))),
        ),
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
        .filter(|kind| *kind != NthEventTimingKind::Restricted(PlayerFilter::Opponent))
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
    if timing == NthEventTimingKind::Restricted(PlayerFilter::Opponent) {
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
    if timing == NthEventTimingKind::Restricted(PlayerFilter::Opponent) {
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

/// CR 105.2 + CR 601.2a: Parse a SpellCast `valid_card` filter from a
/// `"spell that's <relative clause>"` payload, e.g. Questing Druid's
/// "spell that's white, blue, black, or red" → an `Or` of `HasColor` legs.
///
/// Delegates the relative clause to the shared `parse_that_clause_suffix`
/// building block (the same combinator that powers target/search "that's …"
/// clauses), so every relative-clause property it understands — color
/// disjunctions, color-count, supertypes — is supported here for free. The
/// payload is consumed on the FULL pre-truncation `"you cast a …"` remainder
/// because a color disjunction spans commas the effect-boundary splitter would
/// otherwise cut.
///
/// Returns `None` unless the payload is exactly `"spell that's …"` with the
/// relative clause consuming the entire remainder, so type-only qualifiers and
/// post-spell modifiers continue to flow through `parse_spell_qualifier_payload`.
fn parse_spell_that_clause_filter(payload: &str) -> Option<TargetFilter> {
    // Strip the "spell" head noun with a nom tag; the remainder keeps its
    // leading space, which is exactly what `parse_that_clause_suffix` expects
    // before "that's".
    let (rest, _) = tag::<_, _, OracleError<'_>>("spell").parse(payload).ok()?;
    let (props, consumed) = super::oracle_target::parse_that_clause_suffix(rest, None)?;
    if consumed != rest.len() || props.is_empty() {
        return None;
    }
    Some(TargetFilter::Typed(
        TypedFilter::default().properties(props),
    ))
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
    // CR 105.2b: bare color-quality qualifiers ("multicolored", "monocolored",
    // "colorless") are color-count properties, not type phrases.
    if let Ok((remainder, prop)) = parse_color_property(qualifier) {
        if remainder.trim().is_empty() {
            return Some(TargetFilter::Typed(
                TypedFilter::new(TypeFilter::Card).properties(vec![prop]),
            ));
        }
    }
    if all_consuming(tag::<_, _, OracleError<'_>>("kicked"))
        .parse(qualifier)
        .is_ok()
    {
        // CR 702.33d: A spell whose controller declared any kicker payment has
        // been kicked; use the existing cast snapshot filter for kicked spells.
        return Some(TargetFilter::Typed(
            TypedFilter::card().properties(vec![FilterProp::WasKicked]),
        ));
    }
    let (filter, remainder) = parse_type_phrase(qualifier);
    if remainder.trim().is_empty() && !matches!(filter, TargetFilter::Any) {
        Some(filter)
    } else {
        None
    }
}

/// CR 102.1 + CR 603.2c: On "whenever a player casts..." triggers, "each other
/// player" excludes the caster (the triggering player), not the source's
/// controller. The effect-chain stripper maps bare "each other player" to
/// `Opponent` (controller-relative); rewrite here once the trigger subject is
/// known to be any player.
fn rewrite_each_other_player_scope_for_any_caster_spell_triggers(
    trigger_def: &TriggerDefinition,
    ability: &mut AbilityDefinition,
    effect_lower: &str,
) {
    if trigger_def.mode != TriggerMode::SpellCast || trigger_def.valid_target.is_some() {
        return;
    }
    if !scan_contains(effect_lower, "each other player") {
        return;
    }
    if ability.player_scope != Some(PlayerFilter::Opponent) {
        return;
    }
    ability.player_scope = Some(PlayerFilter::AllExcept {
        exclude: Box::new(PlayerFilter::TriggeringPlayer),
    });
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
/// CR 107.4 + CR 202.1: Pure-nom recognizer for the spell-qualifier phrase
/// "with one or more `<color>` mana symbol(s) in its mana cost"
/// (Namor the Sub-Mariner — "Whenever you cast a noncreature spell with one or
/// more blue mana symbols in its mana cost"). Returns the named `ManaColor`;
/// the implied comparison is `Comparator::GE` against `1` ("one or more").
///
/// Reuses the shared atomic combinator `parse_color` rather than re-implementing
/// color recognition or pip counting. This is a different grammatical position
/// from `parse_colored_mana_symbol_count_target_condition` (a target eligibility
/// condition, "it has N or more colored mana symbols in its mana cost"), so it
/// is a separate recognizer that shares the atomics, not the call shape.
fn parse_colored_mana_symbol_spell_qualifier(input: &str) -> OracleResult<'_, ManaColor> {
    delimited(
        tag("with one or more "),
        nom_primitives::parse_color,
        (tag(" mana symbol"), opt(tag("s")), tag(" in its mana cost")),
    )
    .parse(input)
}

/// CR 107.4 + CR 202.1: Pre-extraction helper for trigger plumbing. Locates the
/// colored-mana-symbol spell qualifier inside finalized condition/qualifier text
/// (e.g. the "noncreature spell with one or more blue mana symbols in its mana
/// cost" valid-card phrase) and returns the named color so the effect-context can
/// thread it to the token-count override (Namor "create that many" → EventSource
/// pip count). Scans word boundaries so the qualifier need not begin the string.
pub(crate) fn extract_colored_mana_symbol_spell_qualifier(text: &str) -> Option<ManaColor> {
    let lower = text.to_lowercase();
    nom_primitives::scan_preceded(&lower, parse_colored_mana_symbol_spell_qualifier)
        .map(|(_, color, _)| color)
}

pub(crate) fn parse_post_spell_modifier(modifier: &str) -> Option<TargetFilter> {
    use crate::types::ability::{FilterProp, TypedFilter};

    // CR 608.2b: "that has the same name as a card in your graveyard"
    // (Pyromancer's Ascension). Reuse the search-filter name-reference suffix
    // combinator so graveyard SharesQuality semantics stay aligned.
    if let Ok((rest, prop)) = super::oracle_effect::parse_search_name_reference_suffix(modifier) {
        if rest.trim().is_empty() {
            return Some(TargetFilter::Typed(
                TypedFilter::default().properties(vec![prop]),
            ));
        }
    }

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

    // CR 601.2a: cast-origin qualifier. A spell is cast from a zone; "from
    // anywhere other than [hand]" matches the cast-capable zones except the hand
    // (The Twelfth Doctor), and "from exile" / "from your graveyard" match that
    // single origin zone (Wild-Magic Sorcerer class). Emits an InAnyZone /
    // InZone origin predicate consumed by `spell_object_matches_filter_from_state`
    // against the cast-from zone.
    if let Ok((rest, ())) = value(
        (),
        tag::<_, _, OracleError<'_>>("from anywhere other than your hand"),
    )
    .parse(modifier)
    {
        if rest.trim().is_empty() {
            return Some(TargetFilter::Typed(TypedFilter::default().properties(
                vec![FilterProp::InAnyZone {
                    zones: super::oracle_target::cast_capable_zones_except(Zone::Hand),
                }],
            )));
        }
    }
    if let Ok((rest, zone)) = alt((
        value(Zone::Exile, tag::<_, _, OracleError<'_>>("from exile")),
        value(Zone::Graveyard, tag("from your graveyard")),
    ))
    .parse(modifier)
    {
        if rest.trim().is_empty() {
            return Some(TargetFilter::Typed(
                TypedFilter::default().properties(vec![FilterProp::InZone { zone }]),
            ));
        }
    }

    // CR 107.4 + CR 202.1 + CR 603.2: "with one or more <color> mana symbol(s) in
    // its mana cost" (Namor the Sub-Mariner). The implied comparison is
    // Comparator::GE against 1 ("one or more"). Emits the colored-pip filter prop
    // so the trigger's valid_card only matches spells with at least one such
    // symbol, fixing the over-fire on every noncreature spell.
    if let Ok((rest, color)) = parse_colored_mana_symbol_spell_qualifier(modifier) {
        if rest.trim().is_empty() {
            return Some(TargetFilter::Typed(TypedFilter::default().properties(
                vec![FilterProp::ManaSymbolCount {
                    color: Some(color),
                    comparator: Comparator::GE,
                    value: 1,
                }],
            )));
        }
    }

    // CR 205.3m: "that doesn't share a creature type with a creature you control
    // or a creature card in your graveyard" (Volo, Guide to Monsters). Reuse the
    // shared-quality clause combinator so the disjunctive reference ("creature
    // you control or a creature card in your graveyard") is not mis-split as a
    // type-phrase `Or` inside `parse_type_phrase`.
    if let Ok((rest, prop)) =
        super::oracle_target::parse_shared_quality_clause(modifier, &ParseContext::default())
    {
        if rest.trim().is_empty() {
            return Some(TargetFilter::Typed(
                TypedFilter::default().properties(vec![prop]),
            ));
        }
    }

    None
}

/// Parse an activated-ability qualifier phrase for "when you next … or activate
/// an ability <qualifier>" delayed triggers.
///
/// Currently supports:
/// - "with {x} in its activation cost" — CR 107.3 + CR 601.2f. Produces a
///   `TargetFilter` containing `FilterProp::HasXInActivationCost`.
///
/// Shared with `oracle_effect::try_parse_when_next_event` — exposed as
/// `pub(crate)` to keep the combinator definition in a single place.
pub(crate) fn parse_post_activation_modifier(modifier: &str) -> Option<TargetFilter> {
    use crate::types::ability::{FilterProp, TypedFilter};

    if let Ok((rest, ())) = alt((
        value(
            (),
            tag::<_, _, OracleError<'_>>("with {x} in its activation cost"),
        ),
        value((), tag("with an {x} in its activation cost")),
    ))
    .parse(modifier)
    {
        if rest.trim().is_empty() {
            return Some(TargetFilter::Typed(
                TypedFilter::default().properties(vec![FilterProp::HasXInActivationCost]),
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

fn origin_zones_except(excluded: &[Zone]) -> Vec<Zone> {
    [
        Zone::Library,
        Zone::Hand,
        Zone::Battlefield,
        Zone::Graveyard,
        Zone::Stack,
        Zone::Exile,
        Zone::Command,
    ]
    .into_iter()
    .filter(|zone| !excluded.contains(zone))
    .collect()
}

/// Parses an Oxford-comma tolerant enumeration of source zones using the
/// caller's zone combinator. The separator grammar accepts `", "`, `" or "`,
/// and `", or "` as a single `alt()` consumed by `separated_list1`. This is
/// templated Oracle text grammar, not a CR-specified construct; the CR anchors
/// live on the callers that interpret the list. Zone tokens come from
/// `zone_combinator`; a bare `None` ("anywhere" leaf) inside the list is
/// treated as malformed.
fn parse_zone_list<'a, F>(input: &'a str, zone_combinator: &mut F) -> OracleResult<'a, Vec<Zone>>
where
    F: FnMut(&'a str) -> OracleResult<'a, Option<Zone>>,
{
    let mut zone = |inner| {
        let (rest, zone) = zone_combinator(inner)?;
        let Some(zone) = zone else {
            return Err(oracle_err(inner));
        };
        Ok((rest, zone))
    };
    let separator = alt((
        value((), tag::<_, _, OracleError<'_>>(", or ")),
        value((), tag::<_, _, OracleError<'_>>(" or ")),
        value((), tag::<_, _, OracleError<'_>>(", ")),
    ));
    {
        let mut parser = separated_list1(separator, &mut zone);
        parser.parse(input)
    }
}

/// CR 601.2a + CR 603.6: Parse a "from <zone>" source-zone tail into an
/// `OriginConstraint`. Handles `from anywhere other than <zone>` (NotEquals),
/// `from anywhere other than <zone> or <zone>` (OneOf all non-excluded zones), bare `from
/// anywhere` (Any), single-zone tails (Equals), and the absent-clause case
/// (Any).
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
    // CR 603.6c + CR 601.2a: "from anywhere other than <zone-list>" — the
    // negative-discriminator form. Single-zone form: Ghostly Pilferer ("from
    // anywhere other than their hand"), Syr Konrad clause 2 ("from anywhere
    // other than the battlefield"). List form: "Name Sticker" Goblin ("from
    // anywhere other than a graveyard or exile"). The zone tokens come from
    // the caller's combinator; the list grammar (Oxford-comma tolerant) is
    // shared across all callers. A single-element list collapses back to
    // `NotEquals(Zone)` so existing card-data snapshots remain byte-identical.
    // Multi-zone negation reuses `OneOf` with every concrete zone except the
    // excluded zones to avoid adding a polarity/cardinality sibling variant.
    if let Ok((after_other, ())) =
        value((), tag::<_, _, OracleError<'_>>("anywhere other than ")).parse(after_from)
    {
        if let Ok((rest, zones)) = parse_zone_list(after_other, &mut zone_combinator) {
            let constraint = match zones.len() {
                1 => OriginConstraint::NotEquals(zones[0]),
                _ => OriginConstraint::OneOf(origin_zones_except(&zones)),
            };
            return Ok((rest, constraint));
        }
        // If the list combinator failed (e.g., bare "anywhere" as the inner
        // phrase — "from anywhere other than anywhere" is malformed), fall
        // through to the generic path which will treat the residual as Any.
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
            destination_constraint: DestinationConstraint::Any,
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
            destination_constraint: DestinationConstraint::Any,
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
            destination_constraint: DestinationConstraint::Any,
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

/// CR 400.3: Shared parser for possessive hand forms in zone-change triggers.
/// Recognises "your hand", "an opponent's hand", "its owner's hand",
/// "their owner's hand", "their owners' hands", "a player's hand", "a hand",
/// and bare "hand". Returns `Some(controller)` when the possessive constrains
/// the destination owner, `None` when any player's hand matches.
fn parse_hand_possessive(input: &str) -> OracleResult<'_, Option<ControllerRef>> {
    alt((
        value(Some(ControllerRef::You), tag("your hand")),
        value(Some(ControllerRef::Opponent), tag("an opponent's hand")),
        value(None, tag("its owner's hand")),
        value(None, tag("their owner's hand")),
        value(None, tag("their owners' hands")),
        value(None, tag("a player's hand")),
        value(None, tag("a hand")),
        value(None, tag("hand")),
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

    let (after_hand, hand_owner) = parse_hand_possessive.parse(after_verb).ok()?;
    let valid_target =
        hand_owner.map(|ctrl| TargetFilter::Typed(TypedFilter::default().controller(ctrl)));

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

/// CR 603.6c + CR 603.10a: "[subject] is/are returned to [possessive] hand" —
/// zone-change trigger for bounce effects. The verb "returned" implies the
/// object moved from the battlefield to a player's hand. Examples:
/// - Warped Devotion: "Whenever a permanent is returned to a player's hand"
/// - Azorius Aethermage: "Whenever a permanent is returned to your hand"
/// - Stormfront Riders: "Whenever … is returned to your hand"
///
/// Maps to `ChangesZone` with `origin: Battlefield`, `destination: Hand`.
/// The hand-owner qualifier ("your"/"opponent's") is merged into `valid_card`
/// via `add_controller` so that `match_changes_zone` correctly filters by
/// the bounced permanent's controller.
fn try_parse_returned_to_hand(
    subject: &TargetFilter,
    rest: &str,
) -> Option<(TriggerMode, TriggerDefinition)> {
    let (after_verb, ()) = alt((
        value((), tag::<_, _, OracleError<'_>>("is returned to ")),
        value((), tag("are returned to ")),
    ))
    .parse(rest)
    .ok()?;

    let (_after_hand, hand_owner) = parse_hand_possessive.parse(after_verb).ok()?;

    // CR 400.3: Objects always go to their owner's zone, so "your hand"
    // means the bounced permanent was controlled by you. Merge the
    // constraint into `valid_card` because `match_changes_zone` only
    // inspects `valid_card`, not `valid_target`.
    let valid_card = match hand_owner {
        Some(ctrl) => add_controller(subject.clone(), ctrl),
        None => subject.clone(),
    };

    let mut def = make_base();
    def.mode = TriggerMode::ChangesZone;
    def.origin = Some(Zone::Battlefield);
    def.destination = Some(Zone::Hand);
    def.valid_card = Some(valid_card);
    // Self-referential bounce triggers (e.g. "when ~ is returned to your hand")
    // must fire from hand because the source has already moved.
    if filter_references_self(subject) {
        def.trigger_zones = vec![Zone::Battlefield, Zone::Hand];
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

    fn discard_actor_filter(controller_ref: Option<ControllerRef>) -> Option<TargetFilter> {
        match controller_ref {
            Some(ControllerRef::You) => Some(TargetFilter::Controller),
            Some(ControllerRef::Opponent) => Some(TargetFilter::Typed(
                TypedFilter::default().controller(ControllerRef::Opponent),
            )),
            _ => None,
        }
    }

    fn discard_card_filter(after_verb: &str) -> Option<TargetFilter> {
        fn discard_filter_tail_is_complete(rest: &str) -> bool {
            let rest = rest.trim().trim_end_matches('.').trim();
            if rest.is_empty() {
                return true;
            }
            all_consuming(alt((tag::<_, _, OracleError<'_>>("cards"), tag("card"))))
                .parse(rest)
                .is_ok()
        }

        let (filter, rest) = parse_type_phrase(after_verb);
        if !discard_filter_tail_is_complete(rest)
            && tag::<_, _, OracleError<'_>>("or ")
                .parse(rest.trim_start())
                .is_err()
        {
            return None;
        }
        match filter {
            TargetFilter::Typed(tf)
                if tf
                    .type_filters
                    .iter()
                    .any(|t| !matches!(t, TypeFilter::Card | TypeFilter::Any))
                    || !tf.properties.is_empty() =>
            {
                Some(TargetFilter::Typed(tf))
            }
            _ => Some(TargetFilter::Typed(TypedFilter::new(TypeFilter::Card))),
        }
    }

    fn is_plain_card_filter(filter: &TargetFilter) -> bool {
        match filter {
            TargetFilter::Typed(tf) => {
                tf.controller.is_none()
                    && tf.type_filters.as_slice() == [TypeFilter::Card]
                    && tf.properties.is_empty()
            }
            _ => false,
        }
    }

    fn is_discarded_self_reference(after_verb: &str) -> bool {
        all_consuming(alt((tag::<_, _, OracleError<'_>>("this card"), tag("~"))))
            .parse(after_verb.trim().trim_end_matches('.').trim())
            .is_ok()
    }

    // CR 603.2c: Batched discard triggers — "one or more" fire once per batch.
    if let Ok((after_verb, _)) =
        tag::<_, _, OracleError<'_>>("you discard one or more ").parse(event)
    {
        let mut def = make_base();
        def.mode = TriggerMode::DiscardedAll;
        def.valid_target = Some(TargetFilter::Controller);
        let card_filter = discard_card_filter(after_verb)?;
        if !is_plain_card_filter(&card_filter) {
            def.valid_card = Some(card_filter);
        }
        def.batched = true;
        return Some((TriggerMode::DiscardedAll, def));
    }
    if let Ok((after_verb, _)) =
        tag::<_, _, OracleError<'_>>("one or more players discard one or more ").parse(event)
    {
        let mut def = make_base();
        def.mode = TriggerMode::DiscardedAll;
        let card_filter = discard_card_filter(after_verb)?;
        if !is_plain_card_filter(&card_filter) {
            def.valid_card = Some(card_filter);
        }
        def.batched = true;
        return Some((TriggerMode::DiscardedAll, def));
    }

    // CR 109.5 + CR 603.2: "a spell or ability an opponent controls causes you to
    // discard this card" — the self-discard caused by an opponent's spell/ability
    // (Guerrilla Tactics, Sand Golem, Quagnoth, Mangara's Blessing). The
    // `EventSourceControlledBy { Opponent }` constraint gates on the discard
    // event's cause; mirrors the replacement form in `oracle_replacement.rs`.
    if tag::<_, _, OracleError<'_>>(
        "a spell or ability an opponent controls causes you to discard this card",
    )
    .parse(event)
    .is_ok()
    {
        let mut def = make_base();
        def.mode = TriggerMode::Discarded;
        def.valid_card = Some(TargetFilter::SelfRef);
        def.valid_target = Some(TargetFilter::Controller);
        def.constraint = Some(
            crate::types::ability::TriggerConstraint::EventSourceControlledBy {
                controller: ControllerRef::Opponent,
            },
        );
        // CR 113.6 + CR 113.6k: the source is the discarded card itself. By the time
        // process_triggers scans the Discarded event, complete_discard_to_graveyard has
        // already moved the card hand->graveyard (CR 701.9a), so the trigger must
        // function from the graveyard it lands in (the same off-zone seam Necropotence-
        // style self-discard triggers use). Exile keeps it live under a madness/RIP-class
        // redirect (a redirected discard is still a discard).
        def.trigger_zones = vec![Zone::Graveyard, Zone::Exile];
        return Some((TriggerMode::Discarded, def));
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

    def.valid_target = discard_actor_filter(controller_ref);
    if is_discarded_self_reference(after_verb) {
        def.valid_card = Some(TargetFilter::SelfRef);
        def.trigger_zones = vec![Zone::Graveyard, Zone::Exile];
    } else {
        def.valid_card = Some(discard_card_filter(after_verb)?);
    }

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

/// CR 505.1: "main phase" collectively names the precombat and postcombat
/// main phases without selecting one concrete `Phase` value.
fn parse_generic_main_phase_keyword(input: &str) -> nom::IResult<&str, (), OracleError<'_>> {
    value((), pair(tag("main phase"), opt(tag("s")))).parse(input)
}

/// Scan phase_text for a phase keyword at each word boundary using nom combinators.
fn scan_for_phase(text: &str) -> Option<Phase> {
    super::oracle_nom::primitives::scan_at_word_boundaries(text, parse_phase_keyword)
}

fn scan_for_generic_main_phase(text: &str) -> bool {
    super::oracle_nom::primitives::scan_at_word_boundaries(text, parse_generic_main_phase_keyword)
        .is_some()
}

/// CR 503.1a / CR 507.1: Parse turn constraint from phase text using nom prefix dispatch.
///
/// Tries opponent possessives first (more specific) before bare "your" to avoid
/// the substring ambiguity where "your opponent's" would match "your".
/// Also checks for trailing "on your turn" suffix.
fn parse_turn_constraint(phase_text: &str) -> Option<TriggerConstraint> {
    // Prefix-based: try at the start of the text
    if alt((
        tag::<_, _, OracleError<'_>>("an opponent's "),
        tag::<_, _, OracleError<'_>>("each opponent's "),
        tag("each opponents\u{2019} "),
        tag("each opponents' "),
        tag("your opponent's "),
        tag("your opponents\u{2019} "),
        tag("your opponents' "),
        tag("each of your opponents\u{2019} "),
        tag("each of your opponents' "),
    ))
    .parse(phase_text)
    .is_ok()
    {
        return Some(TriggerConstraint::OnlyDuringOpponentsTurn);
    }
    if alt((tag::<_, _, OracleError<'_>>("each of your "), tag("your ")))
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

/// CR 603.1: match the turn specifier that follows "during " in a trigger's
/// turn-restriction clause → the corresponding trigger constraint. Single
/// authority shared by the trailing peel ([`peel_trailing_turn_constraint`]) and
/// the leading matcher ([`parse_leading_turn_constraint`]).
fn parse_during_turn_spec(input: &str) -> OracleResult<'_, TriggerConstraint> {
    alt((
        value(
            TriggerConstraint::OnlyDuringOpponentsTurn,
            alt((
                tag("an opponent's turn"),
                tag("each opponent's turn"),
                tag("each opponents\u{2019} turn"),
                tag("each opponents' turn"),
                tag("your opponent's turn"),
                tag("your opponents\u{2019} turn"),
                tag("your opponents' turn"),
                tag("each of your opponents\u{2019} turn"),
                tag("each of your opponents' turn"),
            )),
        ),
        value(
            TriggerConstraint::OnlyDuringYourTurn,
            alt((tag("your turn"), tag("each of your turns"))),
        ),
    ))
    .parse(input)
}

/// CR 603.1: match a "during `<your/opponent's>` turn" restriction at the START of
/// a post-event tail (Captain America, Living Legend: "becomes tapped during your
/// turn, if it's the first time …"). Unlike [`peel_trailing_turn_constraint`] the
/// phrase need not consume the whole tail — the intervening-if / effect clause
/// follows it. The trailing `terminated(..)` boundary combinator requires the turn
/// spec to be followed by end-of-input or a `,` (the start of the ", if …/effect"
/// clause), so it doesn't misfire on a longer phrase that merely starts with
/// "during your …". Boundary acceptance stays inside the nom combinator (no
/// string dispatch).
fn parse_leading_turn_constraint(input: &str) -> Option<TriggerConstraint> {
    terminated(
        preceded(
            tag::<_, _, OracleError<'_>>("during "),
            parse_during_turn_spec,
        ),
        alt((
            value((), eof),
            value((), peek(tag::<_, _, OracleError<'_>>(","))),
        )),
    )
    .parse(input.trim_start())
    .map(|(_, constraint)| constraint)
    .ok()
}

fn peel_trailing_turn_constraint(input: &str) -> (&str, Option<TriggerConstraint>) {
    let mut remaining = input.trim();
    loop {
        if let Ok((_, constraint)) = preceded(
            tag::<_, _, OracleError<'_>>("during "),
            all_consuming(parse_during_turn_spec),
        )
        .parse(remaining)
        {
            let split_at = input.len() - remaining.len();
            return (input[..split_at].trim(), Some(constraint));
        }

        // allow-noncombinator: word-boundary scan to find a trailing "during <turn>" suffix;
        // the candidate suffix itself is parsed by nom above.
        let Some(idx) = remaining.find(' ') else {
            return (input, None);
        };
        remaining = remaining[idx + 1..].trim_start();
    }
}

/// CR 305.1 + CR 603.2: Parse the subject and land-play verb from
/// "whenever/when [subject] plays/play a land".
fn parse_land_play_trigger_subject(
    lower: &str,
) -> Option<(Option<TargetFilter>, Option<TargetFilter>)> {
    let (after_prefix, _) = alt((
        tag::<_, _, OracleError<'_>>("whenever "),
        tag::<_, _, OracleError<'_>>("when "),
    ))
    .parse(lower)
    .ok()?;
    let (after_subject, valid_target) = alt((
        value(
            Some(TargetFilter::Controller),
            tag::<_, _, OracleError<'_>>("you "),
        ),
        value(
            Some(TargetFilter::Typed(
                TypedFilter::default().controller(ControllerRef::Opponent),
            )),
            tag::<_, _, OracleError<'_>>("an opponent "),
        ),
        value(None, tag::<_, _, OracleError<'_>>("a player ")),
        value(None, tag::<_, _, OracleError<'_>>("each player ")),
    ))
    .parse(after_prefix)
    .ok()?;
    // CR 305.1: Decompose verb ("play"/"plays") from the land object phrase.
    // The object phrase itself goes through `parse_type_phrase`, the shared
    // typed-filter grammar for supertypes, subtypes, "another", and zone
    // suffixes. That keeps "a legendary land", "another land", "an Island",
    // and "a land from exile" on one parser seam.
    let (after_verb, _) = alt((tag::<_, _, OracleError<'_>>("plays "), tag("play ")))
        .parse(after_subject)
        .ok()?;
    let (filter, rest) = parse_type_phrase(after_verb);
    if rest.len() == after_verb.len() {
        return None;
    }
    let land_filter = normalize_land_play_filter(filter)?;
    Some((valid_target, land_filter))
}

fn normalize_land_play_filter(filter: TargetFilter) -> Option<Option<TargetFilter>> {
    match &filter {
        TargetFilter::Typed(tf) if is_plain_land_filter(tf) => Some(None),
        TargetFilter::Typed(tf) if is_land_play_filter(tf) => Some(Some(filter)),
        _ => None,
    }
}

fn is_plain_land_filter(tf: &TypedFilter) -> bool {
    tf.controller.is_none()
        && tf.properties.is_empty()
        && tf.type_filters.as_slice() == [TypeFilter::Land]
}

fn is_land_play_filter(tf: &TypedFilter) -> bool {
    tf.type_filters.iter().any(|type_filter| match type_filter {
        TypeFilter::Land => true,
        TypeFilter::Subtype(subtype) => is_land_subtype(subtype),
        _ => false,
    })
}

/// CR 601.1a + CR 701.18b: Parse "whenever/when you play a card" — the
/// play-a-card trigger subject. A player "plays a card" by playing a land or
/// casting a spell, so a single `PlayCard` trigger mode covers both events
/// (Recycle, Null Profusion, Jinxed Ring).
///
/// Decomposed into axes (prefix × verb × object) via nom combinators, mirroring
/// `parse_land_play_trigger_subject`. Returns `Some(OriginConstraint)` when the
/// full "you play a card" subject is present, capturing an optional `from <zone>`
/// tail: "you play a card from exile" yields an exile-origin constraint, while
/// bare "you play a card" yields the unrestricted (`Any`) origin. The subject
/// must be followed only by end-of-input, the effect comma, or that zone tail.
/// Restricted to the exact second-person bare-card form. Qualified variants
/// such as "a player plays a card exiled with ~" need additional linked-card
/// filtering before they can safely share this parser arm.
fn parse_play_card_trigger_subject(lower: &str) -> Option<OriginConstraint> {
    let (after_prefix, _) = alt((
        tag::<_, _, OracleError<'_>>("whenever "),
        tag::<_, _, OracleError<'_>>("when "),
    ))
    .parse(lower)
    .ok()?;
    let (after_verb, _) = pair(
        tag::<_, _, OracleError<'_>>("you "),
        tag::<_, _, OracleError<'_>>("play "),
    )
    .parse(after_prefix)
    .ok()?;
    let (after_card, _) = tag::<_, _, OracleError<'_>>("a card")
        .parse(after_verb)
        .ok()?;
    // CR 601.1a + CR 400.1: An optional "from <zone>" tail restricts the play
    // origin ("whenever you play a card from exile"). The shared cast-origin
    // combinator consumes the "from " prefix and the zone phrase; absent a
    // "from" clause it returns `Any` (plain "whenever you play a card"). This
    // mirrors the Rocco, Street Chef cast-from-zone shape.
    let (after_origin, origin) =
        parse_origin_constraint_tail(after_card.trim_start(), parse_cast_origin_zone).ok()?;
    // The subject must be the whole condition: either end-of-input, or the
    // effect comma ("..., draw a card.") follows directly. `eof` / `tag(",")`
    // reject any further qualifier text.
    alt((value((), eof), value((), tag::<_, _, OracleError<'_>>(","))))
        .parse(after_origin.trim_start())
        .ok()?;
    Some(origin)
}

/// CR 725.1: Parse "whenever/when [subject] become(s) the monarch" trigger.
///
/// Decomposes the phrase into three axes via nom combinators:
/// 1. Prefix: "whenever " / "when "
/// 2. Subject + verb: "you become" / "an opponent becomes" / "a player becomes"
/// 3. Object: " the monarch"
///
/// Returns `(valid_target, remaining_text)` on success.
fn parse_become_monarch_trigger(lower: &str) -> Option<(Option<TargetFilter>, &str)> {
    let (after_prefix, _) = alt((
        tag::<_, _, OracleError<'_>>("whenever "),
        tag::<_, _, OracleError<'_>>("when "),
    ))
    .parse(lower)
    .ok()?;
    let (after_verb, valid_target) = alt((
        value(
            Some(TargetFilter::Controller),
            tag::<_, _, OracleError<'_>>("you become"),
        ),
        value(
            Some(TargetFilter::Typed(
                TypedFilter::default().controller(ControllerRef::Opponent),
            )),
            tag::<_, _, OracleError<'_>>("an opponent becomes"),
        ),
        value(
            Some(TargetFilter::Player),
            pair(tag::<_, _, OracleError<'_>>("a player"), tag(" becomes")),
        ),
    ))
    .parse(after_prefix)
    .ok()?;
    let (after_monarch, _) = tag::<_, _, OracleError<'_>>(" the monarch")
        .parse(after_verb)
        .ok()?;
    Some((valid_target, after_monarch))
}

/// CR 726.2: Parse "when/whenever [you|a player] take(s) the initiative".
fn parse_takes_initiative_trigger(lower: &str) -> Option<(Option<TargetFilter>, &str)> {
    let (after_prefix, _) = alt((
        tag::<_, _, OracleError<'_>>("whenever "),
        tag::<_, _, OracleError<'_>>("when "),
    ))
    .parse(lower)
    .ok()?;
    let (after_initiative, valid_target) = alt((
        value(
            Some(TargetFilter::Controller),
            tag::<_, _, OracleError<'_>>("you take the initiative"),
        ),
        value(
            Some(TargetFilter::Player),
            tag::<_, _, OracleError<'_>>("a player takes the initiative"),
        ),
    ))
    .parse(after_prefix)
    .ok()?;
    Some((valid_target, after_initiative))
}

/// CR 309.4c + CR 701.49: Parse "when/whenever you venture into the dungeon"
/// (or Undercity — a specific dungeon venture still enters a room).
fn parse_venture_into_dungeon_trigger(lower: &str) -> Option<&str> {
    let (after_prefix, _) = alt((
        tag::<_, _, OracleError<'_>>("whenever "),
        tag::<_, _, OracleError<'_>>("when "),
    ))
    .parse(lower)
    .ok()?;
    let (after_venture, _) = (
        tag::<_, _, OracleError<'_>>("you "),
        alt((
            tag::<_, _, OracleError<'_>>("venture into the dungeon"),
            tag::<_, _, OracleError<'_>>("venture into the undercity"),
        )),
    )
        .parse(after_prefix)
        .ok()?;
    Some(after_venture)
}

fn parse_monarch_turn_began_condition(lower: &str) -> Option<&str> {
    let (after_condition, _) =
        tag::<_, _, OracleError<'_>>("if you were the monarch as the turn began,")
            .parse(lower)
            .ok()?;
    Some(after_condition)
}

/// CR 700.13: "Whenever [subject] commits a crime" — scoped crime trigger parser.
///
/// Handles three subject forms (trailing space bundled into each tag for precision):
/// - "you commit a crime" → `valid_target = Controller`
/// - "an opponent commits a crime" → `valid_target = Typed(Opponent)`
/// - "a player commits a crime" → `valid_target = Player` (any player)
///
/// Also recognizes the optional " during your turn" suffix, which adds
/// `TriggerConstraint::OnlyDuringYourTurn` (Overzealous Muscle and similar).
fn try_parse_commit_crime(lower: &str) -> Option<(TriggerMode, TriggerDefinition)> {
    // Strip "whenever " / "when " prefix.
    let (after_prefix, ()) = alt((
        value((), tag::<_, _, OracleError<'_>>("whenever ")),
        value((), tag("when ")),
    ))
    .parse(lower)
    .ok()?;

    // Subject axis — trailing space bundled in tag to avoid prefix-loose ambiguity.
    fn parse_crime_actor(input: &str) -> OracleResult<'_, Option<TargetFilter>> {
        alt((
            value(Some(TargetFilter::Controller), tag("you ")),
            value(
                Some(TargetFilter::Typed(
                    TypedFilter::default().controller(ControllerRef::Opponent),
                )),
                tag("an opponent "),
            ),
            value(Some(TargetFilter::Player), tag("a player ")),
        ))
        .parse(input)
    }
    let (after_subject, valid_target) = parse_crime_actor.parse(after_prefix).ok()?;

    // Verb axis — "commits" before "commit" so the longer string wins (nom is greedy).
    let (after_verb, ()) = alt((
        value((), tag::<_, _, OracleError<'_>>("commits")),
        value((), tag("commit")),
    ))
    .parse(after_subject)
    .ok()?;

    // " a crime"
    let (after_crime, ()) = value((), tag::<_, _, OracleError<'_>>(" a crime"))
        .parse(after_verb)
        .ok()?;

    // Optional " during your turn" constraint suffix.
    let (remainder, during_your_turn) = opt(tag::<_, _, OracleError<'_>>(" during your turn"))
        .parse(after_crime)
        .ok()?;

    // Nothing else allowed — this is a complete trigger condition clause.
    if !remainder.trim().is_empty() {
        return None;
    }

    let mut def = make_base();
    def.mode = TriggerMode::CommitCrime;
    def.valid_target = valid_target;
    if during_your_turn.is_some() {
        def.constraint = Some(TriggerConstraint::OnlyDuringYourTurn);
    }
    Some((TriggerMode::CommitCrime, def))
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

#[cfg(test)]
#[path = "oracle_trigger_tests.rs"]
mod tests;

/// Snapshot tests locking current trigger parser output before the IR split.
/// These verify behavioral parity: identical snapshots before and after the
/// `parse_trigger_line_with_index_ir` / `lower_trigger_ir` refactor.
#[cfg(test)]
#[path = "oracle_trigger_snapshot_tests.rs"]
mod snapshot_tests;

#[cfg(test)]
#[path = "oracle_trigger_slicer_control_handoff_tests.rs"]
mod slicer_control_handoff_tests;

/// Issue #2346 (bullet-line modal form): Grenzo, Havoc Raiser's printed Oracle
/// text lists its modes as bullet lines (`\n\u{2022} ...`), which route through
/// the `TriggeredModal` block path rather than the inline `"; or"` path. "that
/// player" in each mode body must scope to the damaged player (`TriggeringPlayer`),
/// not Grenzo's controller (`You`).
#[cfg(test)]
mod grenzo_bullet_modal_tests {
    use crate::parser::oracle::parse_oracle_text;
    use crate::types::ability::{AbilityDefinition, ControllerRef, Effect, TargetFilter};
    use crate::types::TriggerMode;

    /// Walk a chained `AbilityDefinition` collecting one effect per node (parent
    /// then each nested `sub_ability`).
    fn flatten_effects(def: &AbilityDefinition) -> Vec<&Effect> {
        let mut out = Vec::new();
        let mut node = Some(def);
        while let Some(d) = node {
            out.push(&*d.effect);
            node = d.sub_ability.as_deref();
        }
        out
    }

    /// For a "deals combat damage to a player" trigger, the damaged player is
    /// the triggering player; "that player controls" / "that player's library"
    /// in the modes must resolve to them.
    #[test]
    fn damage_done_bullet_modal_uses_triggering_player_for_that_player() {
        let parsed = parse_oracle_text(
            "Whenever a creature you control deals combat damage to a player, choose one \u{2014}\n\u{2022} Goad target creature that player controls.\n\u{2022} Exile the top card of that player's library.",
            "Grenzo, Havoc Raiser",
            &[],
            &["Creature".into()],
            &["Goblin".into()],
        );
        let trigger = parsed
            .triggers
            .iter()
            .find(|t| matches!(t.mode, TriggerMode::DamageDone))
            .expect("DamageDone trigger must parse");
        let execute = trigger.execute.as_ref().expect("execute must be Some");
        let mode_abilities = &execute.mode_abilities;
        assert_eq!(mode_abilities.len(), 2, "must have two modes");

        let mode0_effects = flatten_effects(&mode_abilities[0]);
        let goad_controller = mode0_effects
            .iter()
            .find_map(|e| match e {
                Effect::Goad {
                    target: TargetFilter::Typed(tf),
                } => Some(tf.controller.clone()),
                _ => None,
            })
            .unwrap_or_else(|| panic!("mode 0 must contain Goad, got {mode0_effects:?}"));
        assert_eq!(
            goad_controller,
            Some(ControllerRef::TriggeringPlayer),
            "BULLET Goad controller must be TriggeringPlayer (damaged player), got {goad_controller:?}",
        );

        let mode1_effects = flatten_effects(&mode_abilities[1]);
        let exile_player = mode1_effects
            .iter()
            .find_map(|e| match e {
                Effect::ExileTop { player, .. } => Some(player.clone()),
                _ => None,
            })
            .unwrap_or_else(|| panic!("mode 1 must contain ExileTop, got {mode1_effects:?}"));
        assert_eq!(
            exile_player,
            TargetFilter::TriggeringPlayer,
            "BULLET ExileTop player must be TriggeringPlayer (damaged player), got {exile_player:?}",
        );
    }
}

#[cfg(test)]
#[path = "oracle_trigger_controlled_chosen_type_enters_tests.rs"]
mod controlled_chosen_type_enters_tests;

#[cfg(test)]
#[path = "oracle_trigger_enchanted_player_controls_tests.rs"]
mod enchanted_player_controls_tests;

/// Issue #750: "Whenever you cast a modal spell" (Riku, of Many Paths) must
/// parse a `valid_card` carrying `FilterProp::Modal` (CR 700.2), not `None`.
/// A `None` `valid_card` over-triggers on every spell the controller casts.
#[cfg(test)]
mod modal_spell_cast_trigger_tests {
    use crate::parser::oracle::parse_oracle_text;
    use crate::types::ability::{FilterProp, TargetFilter, TypeFilter};
    use crate::types::TriggerMode;

    /// Collect the top-level `FilterProp`s of a `Typed` `valid_card`, or an empty
    /// vec for any other shape (so assertions read `contains`).
    fn valid_card_props(vc: Option<&TargetFilter>) -> Vec<FilterProp> {
        match vc {
            Some(TargetFilter::Typed(tf)) => tf.properties.clone(),
            _ => Vec::new(),
        }
    }

    /// PARSER (revert-failing): Riku's SpellCast trigger's `valid_card` must be
    /// `Some(Typed{ properties contains FilterProp::Modal })`. Reverting the
    /// peel/attach leaves `valid_card == None` (the over-trigger bug).
    #[test]
    fn riku_modal_spell_cast_trigger_attaches_modal_prop() {
        let parsed = parse_oracle_text(
            "Whenever you cast a modal spell, choose up to X, where X is the number of times you chose a mode for that spell —\n\u{2022} Exile the top card of your library. Until the end of your next turn, you may play it.\n\u{2022} Put a +1/+1 counter on Riku. It gains trample until end of turn.\n\u{2022} Create a 1/1 blue Bird creature token with flying.",
            "Riku, of Many Paths",
            &[],
            &["Legendary".into(), "Creature".into()],
            &["Human".into(), "Wizard".into()],
        );
        let trigger = parsed
            .triggers
            .iter()
            .find(|t| matches!(t.mode, TriggerMode::SpellCast))
            .expect("Riku must parse a SpellCast trigger");
        // CR 700.2: the "modal" qualifier must survive as FilterProp::Modal.
        assert!(
            trigger.valid_card.is_some(),
            "valid_card must NOT be None — a None filter over-triggers on every spell"
        );
        assert!(
            valid_card_props(trigger.valid_card.as_ref()).contains(&FilterProp::Modal),
            "valid_card must carry FilterProp::Modal, got {:?}",
            trigger.valid_card
        );
    }

    /// Class generality: "modal instant spell" keeps BOTH the Modal prop and the
    /// Instant type constraint (the peel only removes the "modal " qualifier).
    #[test]
    fn modal_instant_spell_keeps_type_and_modal() {
        let parsed = parse_oracle_text(
            "Whenever you cast a modal instant spell, draw a card.",
            "Test Modal Instant Watcher",
            &[],
            &["Creature".into()],
            &[],
        );
        let trigger = parsed
            .triggers
            .iter()
            .find(|t| matches!(t.mode, TriggerMode::SpellCast))
            .expect("must parse a SpellCast trigger");
        let props = valid_card_props(trigger.valid_card.as_ref());
        assert!(
            props.contains(&FilterProp::Modal),
            "must carry FilterProp::Modal, got {props:?}"
        );
        // The Instant type constraint must remain on the type_filters.
        match trigger.valid_card.as_ref() {
            Some(TargetFilter::Typed(tf)) => assert!(
                tf.type_filters.contains(&TypeFilter::Instant),
                "modal instant must keep the Instant type filter, got {:?}",
                tf.type_filters
            ),
            other => panic!("expected Typed valid_card, got {other:?}"),
        }
    }

    /// Negative (no over-attach): a plain "you cast a spell" trigger must NOT
    /// gain a Modal prop — the optional peel matches nothing.
    #[test]
    fn plain_spell_cast_trigger_has_no_modal_prop() {
        let parsed = parse_oracle_text(
            "Whenever you cast a spell, draw a card.",
            "Test Plain Spell Watcher",
            &[],
            &["Creature".into()],
            &[],
        );
        let trigger = parsed
            .triggers
            .iter()
            .find(|t| matches!(t.mode, TriggerMode::SpellCast))
            .expect("must parse a SpellCast trigger");
        assert!(
            !valid_card_props(trigger.valid_card.as_ref()).contains(&FilterProp::Modal),
            "a plain 'you cast a spell' trigger must not gain FilterProp::Modal"
        );
    }
}

#[cfg(test)]
mod ood_sphere_tests {
    use crate::parser::oracle::parse_oracle_text;
    use crate::types::ability::{
        ContinuousModification, Duration, Effect, PlayerScope, StaticDefinition, TargetFilter,
    };
    use crate::types::keywords::Keyword;
    use crate::types::statics::{StaticMode, StaticModeKind};
    use crate::types::TriggerMode;

    const OOD_SPHERE_ORACLE: &str = "Song of the Ood \u{2014} Noncreature spells have convoke. (A player's creatures can help cast those spells. Each creature they tap while casting a noncreature spell pays for {1} or one mana of that creature's color.)\nRed-Eye \u{2014} Whenever chaos ensues, for each opponent, goad up to one target creature that opponent controls. Until your next turn, those creatures can't become tapped unless they're being declared as attackers.";

    fn parse_ood_sphere() -> crate::parser::oracle::ParsedAbilities {
        parse_oracle_text(
            OOD_SPHERE_ORACLE,
            "Ood Sphere",
            &[],
            &["Plane".into()],
            &["Horsehead Nebula".into()],
        )
    }

    fn effect_is_unimplemented(effect: &Effect) -> bool {
        matches!(effect, Effect::Unimplemented { .. })
    }

    fn flatten_chain(def: &crate::types::ability::AbilityDefinition) -> Vec<&Effect> {
        let mut out = Vec::new();
        let mut node = Some(def);
        while let Some(d) = node {
            out.push(&*d.effect);
            node = d.sub_ability.as_deref();
        }
        out
    }

    /// Defect 1: the "Noncreature spells have convoke." static must be a functional
    /// casting-keyword grant, not the runtime-inert anthem AddKeyword.
    #[test]
    fn song_of_the_ood_static_grants_cast_with_keyword_convoke() {
        let parsed = parse_ood_sphere();
        let statics = &parsed.statics;
        assert_eq!(statics.len(), 1, "exactly one static: {statics:?}");
        assert_eq!(
            statics[0].mode,
            StaticMode::CastWithKeyword {
                keyword: Keyword::Convoke
            },
            "must be CastWithKeyword, not anthem AddKeyword: {:?}",
            statics[0]
        );
    }

    /// Defect 2: the Red-Eye trigger's second sentence must lower to a real
    /// "can't become tapped" continuous grant, NOT `Effect::Unimplemented`.
    #[test]
    fn red_eye_cant_become_tapped_sub_ability_grants_cant_tap_static() {
        let parsed = parse_ood_sphere();
        let trigger = parsed
            .triggers
            .iter()
            .find(|t| matches!(t.mode, TriggerMode::ChaosEnsues))
            .expect("ChaosEnsues trigger must parse");
        let execute = trigger.execute.as_ref().expect("execute must be Some");

        // Head effect is Goad.
        assert!(
            matches!(*execute.effect, Effect::Goad { .. }),
            "head effect must be Goad, got {:?}",
            execute.effect
        );

        // The sub-ability lowers the second sentence.
        let sub = execute
            .sub_ability
            .as_ref()
            .expect("Red-Eye must carry a can't-become-tapped sub-ability");
        let Effect::GenericEffect {
            static_abilities,
            duration,
            target,
        } = &*sub.effect
        else {
            panic!(
                "sub-ability effect must be GenericEffect, got {:?}",
                sub.effect
            );
        };
        assert_eq!(
            *target,
            Some(TargetFilter::ParentTarget),
            "GenericEffect must apply to the goaded creatures (ParentTarget)"
        );
        assert_eq!(
            *duration,
            Some(Duration::UntilNextTurnOf {
                player: PlayerScope::Controller
            }),
            "restriction lasts until the controller's next turn"
        );
        let inner: &StaticDefinition = static_abilities
            .iter()
            .find_map(|sd| {
                sd.modifications.iter().find_map(|m| match m {
                    ContinuousModification::GrantStaticAbility { definition } => {
                        Some(&**definition)
                    }
                    _ => None,
                })
            })
            .expect("GenericEffect must grant a static ability");
        assert_eq!(
            inner.mode,
            StaticMode::CantTap,
            "granted static must be CantTap, got {:?}",
            inner.mode
        );
        assert_eq!(inner.mode.kind(), StaticModeKind::CantTap);
    }

    /// Full-card gate: zero `Unimplemented` across every ability, trigger, and
    /// the trigger's sub-ability chain.
    #[test]
    fn ood_sphere_has_no_unimplemented() {
        let parsed = parse_ood_sphere();
        for ability in &parsed.abilities {
            for effect in flatten_chain(ability) {
                assert!(
                    !effect_is_unimplemented(effect),
                    "ability effect is Unimplemented: {effect:?}"
                );
            }
        }
        for trigger in &parsed.triggers {
            if let Some(execute) = trigger.execute.as_ref() {
                for effect in flatten_chain(execute) {
                    assert!(
                        !effect_is_unimplemented(effect),
                        "trigger effect is Unimplemented: {effect:?}"
                    );
                }
            }
        }
    }

    /// Negative: only the declared-as-attackers exemption is captured. A
    /// different "unless" rider must fall through to the strict-failure fallback.
    #[test]
    fn other_unless_rider_falls_through_to_unimplemented() {
        // Reuse the parser via a synthetic trigger whose second sentence carries a
        // non-attacker rider — it must remain Unimplemented (fail closed).
        let parsed = parse_oracle_text(
            "Whenever chaos ensues, goad up to one target creature. Until your next turn, those creatures can't become tapped unless you pay {2}.",
            "Test Plane",
            &[],
            &["Plane".into()],
            &[],
        );
        let trigger = parsed
            .triggers
            .iter()
            .find(|t| matches!(t.mode, TriggerMode::ChaosEnsues))
            .expect("ChaosEnsues trigger must parse");
        let execute = trigger.execute.as_ref().expect("execute must be Some");
        let has_unimplemented = flatten_chain(execute)
            .iter()
            .any(|e| effect_is_unimplemented(e));
        assert!(
            has_unimplemented,
            "a non-attacker 'unless' rider must fail closed to Unimplemented, not be silently dropped"
        );
    }
}
