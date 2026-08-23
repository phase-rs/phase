use nom::branch::alt;
use nom::bytes::complete::{tag, take_till1, take_until};
use nom::character::complete::{multispace0, multispace1, satisfy};
use nom::combinator::{all_consuming, eof, map, not, opt, peek, rest, value, verify};
use nom::multi::separated_list1;
use nom::sequence::{preceded, terminated};
use nom::Parser;

use super::super::oracle_nom::bridge::{nom_on_lower, nom_parse_lower, split_once_on_lower};
use super::super::oracle_nom::duration::{
    parse_duration, parse_for_as_long_as_condition, parse_until_source_exiles_another_card_body,
};
use super::super::oracle_nom::enters_under::{
    parse_leading_control_clause, ControlClausePossessor,
};
use super::super::oracle_nom::error::{oracle_err, OracleError, OracleResult};
use super::super::oracle_nom::primitives as nom_primitives;
use super::super::oracle_nom::quantity as nom_quantity;
use super::super::oracle_quantity::{
    parse_cda_quantity, parse_cda_quantity_with_context, parse_event_context_quantity,
    parse_for_each_clause, parse_for_each_clause_expr, parse_for_each_clause_expr_with_context,
    parse_player_attribute_attr_clause, parse_quantity_ref,
};
use super::super::oracle_target::{
    parse_target, parse_target_with_ctx, parse_that_clause_suffix, parse_type_phrase,
    parse_type_phrase_with_ctx,
};
use super::super::oracle_util::{parse_comparator_prefix, parse_count_expr, strip_after, TextPair};
use crate::parser::oracle_ir::ast::*;
use crate::parser::oracle_ir::context::ParseContext;
use crate::parser::oracle_ir::diagnostic::OracleDiagnostic;
use crate::parser::oracle_ir::effect_chain::{ClauseIr, EffectChainIr};
use crate::types::ability::{
    AbilityCondition, AbilityCost, AbilityDefinition, AbilityKind, AggregateFunction, AttackScope,
    AttackSubject, CastPermissionConstraint, CastingPermission, Comparator, ConjureSource,
    ContinuousModification, ControllerRef, DamageChannel, DamageSource, DelayedTriggerCondition,
    Duration, Effect, EffectScope, ExiledSpellRider, FilterProp, GameRestriction, LibraryPosition,
    ManaSpendPermission, MultiTargetSpec, ObjectScope, PermissionGrantee, PlayerFilter,
    PreventionAmount, PreventionScope, PtValue, QuantityExpr, QuantityRef, RestrictionPlayerScope,
    RoundingMode, SpellStackToGraveyardReplacement, StaticCondition, StaticDefinition,
    SubAbilityLink, TargetChoiceTiming, TargetFilter, TypeFilter, TypedFilter,
};
use crate::types::counter::CounterType;
use crate::types::game_state::{DistributionUnit, TargetSelectionConstraint};
use crate::types::mana::ManaCost;
use crate::types::phase::Phase;
use crate::types::statics::StaticMode;
use crate::types::zones::{EtbTapState, Zone};

// Parse-phase functions from the parent module (oracle_effect/mod.rs).
// These are private to oracle_effect but accessible here as a descendant module.
use super::subject;
use super::{
    each_target_filter_mut, has_typed_target, is_broadcast_population_filter, parse_effect_clause,
    parse_event_context_ref_with_ctx, parse_for_each_object_copy_parts,
    refine_damage_target_remainder, replace_player_anaphor_with_parent_target,
    scan_contains_phrase, target_filter_controller_ref,
};
use crate::game::effects::effect::generic_effect_population_filter;

pub(super) fn rewrite_player_anaphor_targets_in_definition(def: &mut AbilityDefinition) {
    replace_player_anaphor_with_parent_target(def.effect.as_mut());
    if let Some(sub) = def.sub_ability.as_deref_mut() {
        rewrite_player_anaphor_targets_in_definition(sub);
    }
    if let Some(else_ability) = def.else_ability.as_deref_mut() {
        rewrite_player_anaphor_targets_in_definition(else_ability);
    }
}

/// CR 115.10a + CR 120.1 + CR 120.3: Ghyrson-style "that permanent or player"
/// is a non-target damage recipient bound to the raw `DamageDealt.target`.
/// Keep this local to single-source DealDamage lowering so generic event-context
/// refs and EachTarget/EachSource damage stay object-only.
fn parse_damage_event_target_recipient<'a>(
    input: &'a str,
    ctx: &ParseContext,
) -> Option<(TargetFilter, &'a str)> {
    if !ctx.in_trigger {
        return None;
    }
    let lower = input.to_lowercase();
    nom_on_lower(input, &lower, |i| {
        value(
            TargetFilter::EventTarget,
            alt((
                tag::<_, _, OracleError<'_>>("that permanent or player"),
                tag("that permanent or a player"),
            )),
        )
        .parse(i)
    })
}

/// CR 608.2c: True when an ability's primary effect acts on the ability's own
/// source permanent (`TargetFilter::SelfRef`). Self-targeting "If <self status>,
/// A on it. Otherwise, B it." abilities (Repeat Offender) lower the "if" body's
/// "it" to `SelfRef`, which the runtime resolves to `source_id`; the "otherwise"
/// body's "it" is the SAME anaphor (the source), so — applying the rules of
/// English to the whole text (CR 608.2c) — it must resolve the same way.
pub(super) fn definition_targets_self_source(def: &AbilityDefinition) -> bool {
    matches!(def.effect.target_filter(), Some(TargetFilter::SelfRef))
}

/// CR 608.2c: True when a `QuantityExpr` is a bare reference to the
/// immediately-preceding instruction's amount (`EventContextAmount`) — the
/// runtime binding for the "that much" / "that many" anaphor. Used to detect a
/// dangling "that much" in an else branch whose antecedent instruction is
/// skipped on that branch.
fn is_event_context_amount(expr: &QuantityExpr) -> bool {
    matches!(
        expr,
        QuantityExpr::Ref {
            qty: QuantityRef::EventContextAmount
        }
    )
}

/// CR 608.2c: A "stable" antecedent amount is one whose resolution does NOT
/// depend on which conditional branch ran — i.e. it is bound to an object or
/// fixed value established before the branch (e.g. the revealed card's mana
/// value, `ObjectManaValue { Demonstrative }`), not the per-instruction
/// `EventContextAmount` channel. Only such an amount may be propagated into an
/// else branch's "that much" anaphor.
pub(super) fn is_stable_branch_amount(expr: &QuantityExpr) -> bool {
    !is_event_context_amount(expr)
}

/// CR 608.2c: Replace every `EventContextAmount` reference in an else-branch
/// definition tree with the stable antecedent amount `stable`. Applied when the
/// gated ("if") clause's magnitude is a stable quantity (e.g. the revealed
/// card's mana value) and the else branch's "that much" anaphor would otherwise
/// read the per-instruction `EventContextAmount` channel — which is 0 on the
/// else branch because the antecedent instruction was skipped (Caustic Bronco:
/// "You lose life equal to that card's mana value if ~ isn't saddled. Otherwise,
/// each opponent loses that much life."). "That much" refers to the SAME printed
/// quantity as the if branch, so it must resolve to that stable amount on both
/// branches. Recurses through `count_expr` plus `sub_ability` / `else_ability`.
pub(super) fn rewrite_else_event_context_to_stable(
    def: &mut AbilityDefinition,
    stable: &QuantityExpr,
) {
    if let Some(expr) = def.effect.count_expr_mut() {
        if is_event_context_amount(expr) {
            *expr = stable.clone();
        }
    }
    if let Some(sub) = def.sub_ability.as_deref_mut() {
        rewrite_else_event_context_to_stable(sub, stable);
    }
    if let Some(else_ability) = def.else_ability.as_deref_mut() {
        rewrite_else_event_context_to_stable(else_ability, stable);
    }
}

/// CR 608.2c: Rewrite an anaphoric `TargetFilter::ParentTarget` to
/// `TargetFilter::SelfRef` throughout an else-branch definition tree. Used when
/// the gated ("if") clause acts on the source (`SelfRef`) but the lowered
/// else-branch defaulted its "it" anaphor to `ParentTarget`. For a self-targeting
/// activated ability that announces no chosen target, `ParentTarget` resolves
/// against an empty target list (a no-op); the antecedent of the else's "it" is
/// the same source the "if" body acted on, so `SelfRef` is the correct binding
/// (the runtime rewrites `SelfRef` to `source_id`). Only `ParentTarget` is
/// rewritten — every other anaphor (already-resolved targets, `LastCreated`,
/// player anaphors) is left untouched.
pub(super) fn rewrite_else_parent_target_to_self_ref(def: &mut AbilityDefinition) {
    each_target_filter_mut(def.effect.as_mut(), &mut |filter| {
        if matches!(filter, TargetFilter::ParentTarget) {
            *filter = TargetFilter::SelfRef;
        }
    });
    if let Some(sub) = def.sub_ability.as_deref_mut() {
        rewrite_else_parent_target_to_self_ref(sub);
    }
    if let Some(else_ability) = def.else_ability.as_deref_mut() {
        rewrite_else_parent_target_to_self_ref(else_ability);
    }
}

/// CR 608.2c + CR 608.2b: A chained tap/untap anaphor ("untap him"/"untap it")
/// inherits its referent from the active antecedent. When the source itself
/// (`SelfRef` — The Incredible Hulk: "put a +1/+1 counter on him ... untap him")
/// is the active antecedent, a chained single-permanent `SetTapState` whose target
/// lowered to the `ParentTarget` anaphor refers to the source, so rewrite it to
/// `SelfRef` (the runtime then binds it to `source_id`).
///
/// The active antecedent is carried DOWN the sub-ability chain, so an intervening
/// instruction that introduces NO new permanent referent ("... You gain 2 life.
/// Untap him." / Hulk's extra-phase rider) does not break the rewrite — the
/// immediate-child-only earlier version silently no-op'd those. It is reset only
/// when an effect establishes a NEW OBJECT antecedent: a non-`SelfRef`,
/// non-player-scoped target ("destroy target creature. Untap it." — "it" is the
/// creature, not the source). Targetless and player-directed effects (life gain,
/// extra phases, draws) leave the permanent antecedent intact.
///
/// A head with a real or *optional* target (Tyvar Kell: "...up to one target Elf.
/// Untap it.") is NOT `SelfRef`, so its anaphor stays `ParentTarget`: it binds the
/// chosen target, and a DECLINED optional target leaves the target list empty so
/// the sub correctly does nothing (CR 608.2b — the anaphor has no referent).
///
/// Sibling of [`rewrite_else_parent_target_to_self_ref`] for the `sub_ability`
/// chain. Must run at lowering time: by resolution the discriminator is erased
/// (both Hulk and a declined-optional anaphor reach the resolver with the same
/// empty target list), so the head's subject filter — visible only here — is the
/// only thing that tells them apart. Scope is restricted to `Single` (the
/// anaphoric singular) — `All` ("untap all ...") is a population filter.
pub(super) fn patch_self_ref_head_tap_anaphor(def: &mut AbilityDefinition) {
    fn walk(def: &mut AbilityDefinition, carried_self_ref: bool) {
        // Update the active permanent antecedent for THIS node, then apply it to
        // the immediate chained sub before recursing further down the chain.
        let active_self_ref = match def.effect.target_filter() {
            Some(TargetFilter::SelfRef) => true,
            // A new object antecedent (target creature/permanent/...) takes over.
            Some(filter) if !target_filter_is_player_scoped(filter) => false,
            // Player-directed (life/phase/draw) or targetless effects introduce no
            // permanent referent — carry the antecedent through unchanged.
            _ => carried_self_ref,
        };
        if let Some(sub) = def.sub_ability.as_deref_mut() {
            if active_self_ref {
                if let Effect::SetTapState {
                    target: target @ TargetFilter::ParentTarget,
                    scope: EffectScope::Single,
                    ..
                } = sub.effect.as_mut()
                {
                    *target = TargetFilter::SelfRef;
                }
            }
            walk(sub, active_self_ref);
        }
    }
    walk(def, false);
}

/// CR 608.2c: After a head that FREEZES a broadcast population, a chained
/// "then untap them" continuation refers to that population (Lulu, Loyal
/// Hollyphant; Jeskai Ascendancy, issue #6857). Phase-trigger bodies carry
/// `ctx.subject = Any`, so `resolve_it_pronoun` wrongly binds "them" to
/// `SelfRef` (the trigger source), and a spell body defaults it to
/// `ParentTarget`. Rewrite to `TrackedSet(0)` so the runtime binds the
/// published population via `affected_objects_from_events`. Sibling of
/// [`patch_self_ref_head_tap_anaphor`] for the population-head / plural-anaphor
/// polarity.
pub(super) fn patch_population_head_tap_anaphor(def: &mut AbilityDefinition) {
    /// CR 608.2c + CR 611.2c: heads that freeze a broadcast population at
    /// resolution and publish it as the chain tracked set. Mirrors the
    /// publishers in `game/effects/mod.rs::affected_objects_from_events`:
    ///   * `PutCounterAll` -> `CounterAdded` events (CR 122.1 — counters are
    ///     the signal for THIS leg only)          (Lulu, The Fifth Doctor)
    ///   * `PumpAll`       -> `pump::pump_all_affected_objects` (issue #6857)
    ///   * `GenericEffect` -> filter re-enumeration            (issue #6682)
    ///
    /// The broadcast test is `is_broadcast_population_filter`, NOT the
    /// runtime's `generic_effect_affected_uses_inherited_targets`: the latter
    /// does not exclude `SelfRef`, and using it here would rewrite self-scoped
    /// grants whose `SelfRef`/`ParentTarget` tail is already correct.
    fn is_population_publisher(effect: &Effect) -> bool {
        match effect {
            Effect::PutCounterAll { target, .. } | Effect::PumpAll { target, .. } => {
                is_broadcast_population_filter(target)
            }
            // Same authority the runtime publish arm selects with, so a head can
            // never be routed here and then declined there (or vice versa).
            Effect::GenericEffect {
                static_abilities,
                target,
                ..
            } => generic_effect_population_filter(target.as_ref(), static_abilities)
                .is_some_and(is_broadcast_population_filter),
            _ => false,
        }
    }

    fn walk(def: &mut AbilityDefinition, carried_population: bool) {
        let active_population = if is_population_publisher(&def.effect) {
            true
        } else {
            match def.effect.target_filter() {
                Some(filter) if !target_filter_is_player_scoped(filter) => false,
                _ => carried_population,
            }
        };
        if let Some(sub) = def.sub_ability.as_deref_mut() {
            if active_population {
                if let Effect::SetTapState {
                    target,
                    scope: EffectScope::Single,
                    ..
                } = sub.effect.as_mut()
                {
                    // CR 608.2c: under a broadcast-population head the anaphor's
                    // antecedent is that frozen population, whichever resolver
                    // produced the placeholder — the spell-body default
                    // `ParentTarget` (`oracle_target::resolve_pronoun_target`),
                    // the self-subject trigger default `SelfRef`, or the
                    // named-subject trigger default `TriggeringSource`
                    // (`oracle_effect::resolve_it_pronoun`). All three name a
                    // SINGLE referent, and a head that has just frozen a
                    // population is the only live antecedent, so all three
                    // rebind to the published set. `scope: Single` stays in the
                    // pattern above: a mass "untap all …" is a population filter
                    // in its own right, not an anaphor.
                    if matches!(
                        target,
                        TargetFilter::SelfRef
                            | TargetFilter::ParentTarget
                            | TargetFilter::TriggeringSource
                    ) {
                        *target = TargetFilter::TrackedSet {
                            id: crate::types::identifiers::TrackedSetId(0),
                        };
                    }
                }
            }
            walk(sub, active_population);
        }
    }
    walk(def, false);
}

/// CR 608.2c: After a "choose a card …" interactive selection, the chained
/// "… {remove|put} that many counters {from|on} it" continuation's "it" refers to
/// the chosen card. The standalone continuation clause lowers its "it" anaphor to
/// `TargetFilter::SelfRef` (the chain split gives it no parser subject), which the
/// counter resolver would bind to the ability's source object instead of the
/// chosen card. When such a clause is the `sub_ability` of an
/// `Effect::ChooseFromZone`, rebind its target to `ParentTarget` so it inherits
/// the chosen object the `ChooseFromZoneChoice` handler installs as the
/// continuation's target. Amy Pond: "choose a suspended card you own and remove
/// that many time counters from it". `ChooseFromZone` exposes no other object
/// referent, so the rebind is general across the whole "choose a card, then
/// counters {on|from} it" class. Sibling of `patch_self_ref_head_tap_anaphor`.
pub(super) fn patch_choose_from_zone_counter_continuation_target(def: &mut AbilityDefinition) {
    let mut cursor: &mut AbilityDefinition = def;
    loop {
        if matches!(&*cursor.effect, Effect::ChooseFromZone { .. }) {
            if let Some(sub) = cursor.sub_ability.as_deref_mut() {
                match &mut *sub.effect {
                    Effect::RemoveCounter { target, .. } | Effect::PutCounter { target, .. }
                        if matches!(target, TargetFilter::SelfRef) =>
                    {
                        *target = TargetFilter::ParentTarget;
                    }
                    _ => {}
                }
            }
        }
        match cursor.sub_ability.as_deref_mut() {
            Some(next) => cursor = next,
            None => break,
        }
    }
}

/// CR 601.2c + CR 608.2c: Guard a reflexive-target rider against a *declined*
/// optional antecedent target. When an ability declares a variable number of
/// targets that may be zero — "destroy **up to one** target creature"
/// (`multi_target.min_is_fixed_zero()`, CR 601.2c) — and chains a conditional
/// rider whose condition anaphors that target ("**if that creature** wasn't
/// dealt damage this turn, its controller draws two cards"), declining the
/// target leaves the rider's `TargetMatchesFilter` with no antecedent. At
/// runtime that condition falls back to the trigger source (effects/mod.rs), so
/// a `Not`-wrapped rider wrongly fires. Conjoining the rider condition with
/// `HasObjectTarget` (`And{[HasObjectTarget, existing]}`) suppresses the rider
/// when no object target was chosen, while leaving the chosen-target case
/// unchanged (the conjunct is trivially true).
///
/// The optional-target context is threaded DOWN the chain via an inner `walk`: a node
/// whose own `multi_target` is `None` inherits its parent's optionality, so a
/// rider nested deeper than the immediate `sub_ability` — or hanging off an
/// `else_ability` — is still gated against the declined PARENT target (CR 608.2c:
/// read the whole text; the anaphor binds the parent's chosen target, not the
/// intervening instruction). A node that declares its OWN targets establishes a
/// NEW antecedent and recomputes optionality from its own `multi_target`, so a
/// mandatory intervening target (always present) does NOT spuriously gate a rider
/// that anaphors it. Both `sub_ability` and `else_ability` conditions are gated.
///
/// Class-level (Faller's Faithful, Sunpearl Kirin, Zephyr Sentinel, Rescue //
/// Pepper Potts), both polarities. No-op for mandatory-single-target
/// antecedents: those carry `multi_target == None` at the head with no optional
/// ancestor, so optionality is false and the wrapper is never applied. Idempotent
/// — an already-wrapped `And{..}` is not itself a reflexive-target condition, so
/// re-lowering does not double-wrap.
pub(super) fn gate_reflexive_rider_on_declined_optional_target(def: &mut AbilityDefinition) {
    // CR 601.2c + CR 608.2c: wrap a child's reflexive-target rider so a declined
    // optional antecedent target suppresses it; a non-reflexive condition (or no
    // condition) is left untouched.
    fn gate_child_condition(child: &mut AbilityDefinition) {
        if let Some(existing) = child.condition.take() {
            if is_reflexive_target_condition(&existing) {
                child.condition = Some(AbilityCondition::And {
                    conditions: vec![AbilityCondition::HasObjectTarget, existing],
                });
            } else {
                child.condition = Some(existing);
            }
        }
    }
    // Carry the parent's optional-target context down the chain; a node that
    // declares its own targets establishes a NEW antecedent and recomputes it.
    fn walk(def: &mut AbilityDefinition, parent_optional: bool) {
        let optional_here = match def.multi_target.as_ref() {
            Some(mt) => mt.min_is_fixed_zero(),
            None => parent_optional,
        };
        if optional_here {
            if let Some(sub) = def.sub_ability.as_deref_mut() {
                gate_child_condition(sub);
            }
            if let Some(els) = def.else_ability.as_deref_mut() {
                gate_child_condition(els);
            }
        }
        if let Some(sub) = def.sub_ability.as_deref_mut() {
            walk(sub, optional_here);
        }
        if let Some(els) = def.else_ability.as_deref_mut() {
            walk(els, optional_here);
        }
    }
    walk(def, false);
}

/// CR 608.2c: A reflexive-target condition reads the parent's chosen target via
/// an anaphor ("that creature"/"it"/"that much"), so a declined optional target
/// leaves it without an antecedent. `TargetMatchesFilter` (current/LKI target
/// match) and `PreviousEffectAmount` ("that much") are the affected shapes, in
/// either polarity (`Not`-wrapped).
fn is_reflexive_target_condition(cond: &AbilityCondition) -> bool {
    match cond {
        AbilityCondition::TargetMatchesFilter { .. }
        | AbilityCondition::PreviousEffectAmount { .. } => true,
        AbilityCondition::Not { condition } => is_reflexive_target_condition(condition),
        _ => false,
    }
}

/// CR 608.2c: True for `TargetFilter`s that refer to a PLAYER (or set of players),
/// which therefore do NOT establish a new permanent antecedent for a chained
/// tap/untap "him"/"it" anaphor (see [`patch_self_ref_head_tap_anaphor`]).
///
/// Deliberately a NON-exhaustive allow-list: any unlisted filter is treated as a
/// potential new object antecedent, which only ever STOPS a rewrite (leaving the
/// anaphor as `ParentTarget` — the pre-fix behavior), never causes a wrong-object
/// untap. So an omission is safe; a false inclusion would not be, which is why
/// only unambiguously player-referencing variants are listed.
fn target_filter_is_player_scoped(filter: &TargetFilter) -> bool {
    matches!(
        filter,
        TargetFilter::Player
            | TargetFilter::Controller
            | TargetFilter::AllPlayers
            | TargetFilter::DefendingPlayer
            | TargetFilter::ScopedPlayer
            | TargetFilter::TriggeringPlayer
            | TargetFilter::OriginalController
            | TargetFilter::SourceChosenPlayer
            | TargetFilter::ParentTargetController
            | TargetFilter::ParentTargetOwner
            | TargetFilter::TriggeringSpellController
            | TargetFilter::TriggeringSpellOwner
            | TargetFilter::TriggeringSourceController
            | TargetFilter::PostReplacementSourceController
            | TargetFilter::SpecificPlayer { .. }
    )
}

#[cfg(test)]
mod gate_reflexive_rider_tests {
    use super::*;

    /// "if that creature wasn't dealt damage this turn" — the reflexive-target
    /// rider shape (`Not{TargetMatchesFilter{use_lki}}`) that anaphors the parent's
    /// chosen target.
    fn reflexive_rider() -> AbilityCondition {
        AbilityCondition::Not {
            condition: Box::new(AbilityCondition::TargetMatchesFilter {
                filter: TargetFilter::Typed(
                    TypedFilter::default().properties(vec![FilterProp::WasDealtDamageThisTurn]),
                ),
                use_lki: true,
                subject_slot: None,
            }),
        }
    }

    fn draw_effect() -> Effect {
        Effect::Draw {
            count: QuantityExpr::Fixed { value: 2 },
            target: TargetFilter::ParentTargetController,
        }
    }

    /// Leaf rider node carrying `condition`.
    fn leaf_with_condition(condition: AbilityCondition) -> Box<AbilityDefinition> {
        let mut def = AbilityDefinition::new(AbilityKind::Spell, draw_effect());
        def.condition = Some(condition);
        Box::new(def)
    }

    /// A head that declares an OPTIONAL "up to one" target (`min_is_fixed_zero()`).
    fn optional_head() -> AbilityDefinition {
        let mut def = AbilityDefinition::new(AbilityKind::Spell, draw_effect());
        def.multi_target = Some(MultiTargetSpec::up_to(QuantityExpr::Fixed { value: 1 }));
        def
    }

    /// True iff `condition` is the gated form `And{[HasObjectTarget, reflexive_rider]}`.
    fn is_gated(condition: &Option<AbilityCondition>) -> bool {
        matches!(
            condition,
            Some(AbilityCondition::And { conditions })
                if conditions.as_slice()
                    == [AbilityCondition::HasObjectTarget, reflexive_rider()]
        )
    }

    /// Baseline: the immediate `sub_ability` rider under an optional head is gated.
    /// (Passes under both the old direct-child code and the threaded fix.)
    #[test]
    fn direct_rider_under_optional_head_is_gated() {
        let mut head = optional_head();
        head.sub_ability = Some(leaf_with_condition(reflexive_rider()));

        gate_reflexive_rider_on_declined_optional_target(&mut head);

        let sub = head.sub_ability.as_ref().unwrap();
        assert!(
            is_gated(&sub.condition),
            "direct rider must be gated: {:?}",
            sub.condition
        );
    }

    /// DISCRIMINATOR (nested): head[up to one] → sub1[own `multi_target == None`,
    /// non-reflexive `IsYourTurn`] → sub2[reflexive rider]. The rider is two levels
    /// below the optional antecedent. The fix threads the parent's optionality
    /// through sub1 (which declares no own target), so sub2 is gated; the
    /// intervening non-reflexive condition is left untouched. REVERT-PROBE: the old
    /// direct-child recursion recomputes optionality from sub1's own `multi_target`
    /// (`None` → false), so sub2 is NOT gated and this assertion fails.
    #[test]
    fn nested_deeper_rider_under_optional_head_is_gated() {
        let mut sub1 = AbilityDefinition::new(AbilityKind::Spell, draw_effect());
        sub1.condition = Some(AbilityCondition::IsYourTurn);
        sub1.sub_ability = Some(leaf_with_condition(reflexive_rider()));
        let mut head = optional_head();
        head.sub_ability = Some(Box::new(sub1));

        gate_reflexive_rider_on_declined_optional_target(&mut head);

        let sub1 = head.sub_ability.as_ref().unwrap();
        assert_eq!(
            sub1.condition,
            Some(AbilityCondition::IsYourTurn),
            "intervening non-reflexive condition must be left untouched"
        );
        let sub2 = sub1.sub_ability.as_ref().unwrap();
        assert!(
            is_gated(&sub2.condition),
            "deeper rider must be gated with HasObjectTarget (bug: old code lost the guard here): {:?}",
            sub2.condition
        );
    }

    /// DISCRIMINATOR (else): head[up to one] → else_ability[reflexive rider]. The
    /// fix gates the else branch's own reflexive condition. REVERT-PROBE: the old
    /// code only gated `sub_ability` and never touched `else_ability`, so this
    /// assertion fails.
    #[test]
    fn else_branch_rider_under_optional_head_is_gated() {
        let mut head = optional_head();
        head.else_ability = Some(leaf_with_condition(reflexive_rider()));

        gate_reflexive_rider_on_declined_optional_target(&mut head);

        let els = head.else_ability.as_ref().unwrap();
        assert!(
            is_gated(&els.condition),
            "else-branch rider must be gated: {:?}",
            els.condition
        );
    }

    /// NEGATIVE (new mandatory antecedent): head[up to one] → sub1[own EXACT(1)
    /// target — a NEW, always-present antecedent] → sub2[reflexive rider]. sub2's
    /// "that creature" anaphors sub1's mandatory target, which is never declined, so
    /// the rider must NOT be gated. Guards against a naive fix that threads the
    /// parent's optionality unconditionally past a node that establishes its own
    /// target.
    #[test]
    fn mandatory_intervening_target_does_not_gate_deeper_rider() {
        let mut sub1 = AbilityDefinition::new(AbilityKind::Spell, draw_effect());
        sub1.multi_target = Some(MultiTargetSpec::exact(QuantityExpr::Fixed { value: 1 }));
        sub1.sub_ability = Some(leaf_with_condition(reflexive_rider()));
        let mut head = optional_head();
        head.sub_ability = Some(Box::new(sub1));

        gate_reflexive_rider_on_declined_optional_target(&mut head);

        let sub2 = head
            .sub_ability
            .as_ref()
            .unwrap()
            .sub_ability
            .as_ref()
            .unwrap();
        assert_eq!(
            sub2.condition,
            Some(reflexive_rider()),
            "rider under a mandatory own-target antecedent must NOT be gated"
        );
    }

    /// NO-OP: a mandatory single-target head (`multi_target == Some(exact(1))`,
    /// `min_is_fixed_zero()` false, no optional ancestor) does not gate its rider —
    /// the 7 clean S01 mandatory cards are unaffected.
    #[test]
    fn mandatory_head_does_not_gate_rider() {
        let mut head = AbilityDefinition::new(AbilityKind::Spell, draw_effect());
        head.multi_target = Some(MultiTargetSpec::exact(QuantityExpr::Fixed { value: 1 }));
        head.sub_ability = Some(leaf_with_condition(reflexive_rider()));

        gate_reflexive_rider_on_declined_optional_target(&mut head);

        let sub = head.sub_ability.as_ref().unwrap();
        assert_eq!(
            sub.condition,
            Some(reflexive_rider()),
            "mandatory single-target head must not wrap its rider"
        );
    }

    /// NO-OP: a non-reflexive condition under an optional head is left untouched
    /// (the gate only wraps reflexive-target riders).
    #[test]
    fn non_reflexive_rider_under_optional_head_untouched() {
        let mut head = optional_head();
        head.sub_ability = Some(leaf_with_condition(AbilityCondition::IsYourTurn));

        gate_reflexive_rider_on_declined_optional_target(&mut head);

        let sub = head.sub_ability.as_ref().unwrap();
        assert_eq!(
            sub.condition,
            Some(AbilityCondition::IsYourTurn),
            "non-reflexive condition must be left untouched"
        );
    }
}

#[cfg(test)]
mod self_ref_tap_anaphor_tests {
    use super::*;
    use crate::types::ability::TapStateChange;

    /// Builds a `PutCounter{head_target}` head with a chained
    /// `SetTapState{ParentTarget, scope}` untap sub — the shape every chained
    /// tap/untap anaphor lowers to.
    fn put_counter_all_then_untap_chain(head_target: TargetFilter) -> AbilityDefinition {
        let mut def = AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::PutCounterAll {
                counter_type: CounterType::Plus1Plus1,
                count: QuantityExpr::Fixed { value: 1 },
                target: head_target,
            },
        );
        def.sub_ability = Some(Box::new(AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::SetTapState {
                target: TargetFilter::SelfRef,
                scope: EffectScope::Single,
                state: TapStateChange::Untap,
            },
        )));
        def
    }

    fn put_counter_then_untap_chain(
        head_target: TargetFilter,
        sub_scope: EffectScope,
    ) -> AbilityDefinition {
        let mut def = AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::PutCounter {
                counter_type: CounterType::Plus1Plus1,
                count: QuantityExpr::Fixed { value: 1 },
                target: head_target,
            },
        );
        def.sub_ability = Some(Box::new(AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::SetTapState {
                target: TargetFilter::ParentTarget,
                scope: sub_scope,
                state: TapStateChange::Untap,
            },
        )));
        def
    }

    // CR 608.2c: a chained "untap him" anaphor after a `SelfRef`-subject head (The
    // Incredible Hulk: "put a +1/+1 counter on him ... untap him") refers to the
    // source, so the patch rewrites its `ParentTarget` to `SelfRef`.
    #[test]
    fn self_ref_head_tap_anaphor_rewrites_to_self_ref() {
        let mut def = put_counter_then_untap_chain(TargetFilter::SelfRef, EffectScope::Single);
        patch_self_ref_head_tap_anaphor(&mut def);
        let sub = def.sub_ability.expect("sub-ability");
        assert!(
            matches!(
                &*sub.effect,
                Effect::SetTapState {
                    target: TargetFilter::SelfRef,
                    ..
                }
            ),
            "SelfRef-head anaphor must be rewritten to SelfRef, got {:?}",
            sub.effect
        );
    }

    // CR 608.2b: a head with a real/optional target (Tyvar Kell "...up to one
    // target Elf. Untap it.") is NOT `SelfRef`, so the anaphor MUST stay
    // `ParentTarget` — it binds the chosen target, and a declined optional target
    // leaves the target list empty so the sub no-ops. This is exactly the
    // discrimination the rejected bare-`is_empty()` resolver arm lacked (it would
    // have wrongly untapped the source planeswalker on a declined target).
    #[test]
    fn typed_head_tap_anaphor_stays_parent_target() {
        let mut def = put_counter_then_untap_chain(
            TargetFilter::Typed(TypedFilter::default()),
            EffectScope::Single,
        );
        patch_self_ref_head_tap_anaphor(&mut def);
        let sub = def.sub_ability.expect("sub-ability");
        assert!(
            matches!(
                &*sub.effect,
                Effect::SetTapState {
                    target: TargetFilter::ParentTarget,
                    ..
                }
            ),
            "Typed-head anaphor must stay ParentTarget (CR 608.2b), got {:?}",
            sub.effect
        );
    }

    // CR 608.2c + CR 122.1: a chained "untap them" after `PutCounterAll` binds
    // to the countered set, not the trigger source (Lulu, Loyal Hollyphant).
    #[test]
    fn put_counter_all_head_plural_untap_rewrites_to_tracked_set() {
        let mut def = put_counter_all_then_untap_chain(TargetFilter::Typed(
            TypedFilter::creature()
                .controller(ControllerRef::You)
                .properties(vec![FilterProp::Tapped]),
        ));
        patch_population_head_tap_anaphor(&mut def);
        let sub = def.sub_ability.expect("sub-ability");
        assert!(
            matches!(
                &*sub.effect,
                Effect::SetTapState {
                    target: TargetFilter::TrackedSet {
                        id: crate::types::identifiers::TrackedSetId(0)
                    },
                    ..
                }
            ),
            "PutCounterAll-head plural untap must bind TrackedSet(0), got {:?}",
            sub.effect
        );
    }

    // Scope guard: `All` ("untap all ...") is a population filter, never an
    // anaphor — it must not be rewritten even under a `SelfRef` head.
    #[test]
    fn self_ref_head_tap_all_scope_not_rewritten() {
        let mut def = put_counter_then_untap_chain(TargetFilter::SelfRef, EffectScope::All);
        patch_self_ref_head_tap_anaphor(&mut def);
        let sub = def.sub_ability.expect("sub-ability");
        assert!(
            matches!(
                &*sub.effect,
                Effect::SetTapState {
                    target: TargetFilter::ParentTarget,
                    scope: EffectScope::All,
                    ..
                }
            ),
            "All-scope SetTapState must not be rewritten, got {:?}",
            sub.effect
        );
    }

    /// Builds `PutCounter{head_target}` -> `middle` -> `SetTapState{ParentTarget,
    /// Single}` untap — a THREE-node chain to exercise antecedent propagation
    /// across an intervening instruction.
    fn head_middle_untap_chain(head_target: TargetFilter, middle: Effect) -> AbilityDefinition {
        let mut def = AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::PutCounter {
                counter_type: CounterType::Plus1Plus1,
                count: QuantityExpr::Fixed { value: 1 },
                target: head_target,
            },
        );
        let mut middle_def = AbilityDefinition::new(AbilityKind::Spell, middle);
        middle_def.sub_ability = Some(Box::new(AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::SetTapState {
                target: TargetFilter::ParentTarget,
                scope: EffectScope::Single,
                state: TapStateChange::Untap,
            },
        )));
        def.sub_ability = Some(Box::new(middle_def));
        def
    }

    fn untap_of(chain: AbilityDefinition) -> AbilityDefinition {
        *chain
            .sub_ability
            .expect("middle")
            .sub_ability
            .expect("untap")
    }

    // CR 608.2c: an intervening PLAYER-directed instruction (here "you gain 2
    // life") between a `SelfRef` head and the untap does NOT introduce a new
    // permanent referent, so the source antecedent carries through and the untap
    // is still rewritten to `SelfRef`. Discrimination: the immediate-child-only
    // version (and gemini's `target_filter().is_some()` reset) left this as
    // `ParentTarget` — a runtime no-op.
    #[test]
    fn self_ref_head_intermediate_player_effect_still_rewrites() {
        let mut def = head_middle_untap_chain(
            TargetFilter::SelfRef,
            Effect::GainLife {
                amount: QuantityExpr::Fixed { value: 2 },
                player: TargetFilter::Controller,
            },
        );
        patch_self_ref_head_tap_anaphor(&mut def);
        let untap = untap_of(def);
        assert!(
            matches!(
                &*untap.effect,
                Effect::SetTapState {
                    target: TargetFilter::SelfRef,
                    ..
                }
            ),
            "anaphor after SelfRef head + intervening player effect must rewrite to SelfRef, got {:?}",
            untap.effect
        );
    }

    // CR 608.2b/608.2c: an intervening effect that establishes a NEW OBJECT
    // antecedent (here pairing with a chosen creature) resets the antecedent, so a
    // following "untap it" binds THAT object (`ParentTarget`), not the source.
    // This is the target-head negative fixture the maintainer asked for.
    #[test]
    fn self_ref_head_intermediate_object_target_does_not_rewrite() {
        let mut def = head_middle_untap_chain(
            TargetFilter::SelfRef,
            Effect::PairWith {
                target: TargetFilter::Typed(TypedFilter::default()),
            },
        );
        patch_self_ref_head_tap_anaphor(&mut def);
        let untap = untap_of(def);
        assert!(
            matches!(
                &*untap.effect,
                Effect::SetTapState {
                    target: TargetFilter::ParentTarget,
                    ..
                }
            ),
            "anaphor after an intervening object-target effect must stay ParentTarget, got {:?}",
            untap.effect
        );
    }
}

/// CR 608.2c + CR 401.4: After an optional `CastFromZone` from a linked-exile
/// pool (Sanwell, Chaos Wand class), a trailing "put the rest / put the exiled
/// cards … on the bottom" clause must route uncards still linked to the source
/// through `ExiledBySource`, not a `TrackedSet` of library cards.
pub(super) fn normalize_linked_exile_cast_bottom_cleanup(effect: &mut Effect) {
    if let Effect::PutAtLibraryPosition {
        ref mut target,
        ref mut count,
        position,
    } = effect
    {
        if matches!(position, LibraryPosition::Bottom) {
            *target = TargetFilter::ExiledBySource;
            *count = QuantityExpr::Fixed { value: 0 };
        }
    }
}

/// CR 608.2c + CR 701.13a: Head-aware companion to
/// [`is_linked_exile_cast_bottom_cleanup`] for the Jodah, the Unifier class —
/// `ExileFromTopUntil { NextMatches }` → optional `CastFromZone { ParentTarget }`
/// → `PutAtLibraryPosition { Bottom, TrackedSet }`.
///
/// The plain linked-exile gate cannot fire here: neither the `ParentTarget`
/// cast nor the parser-default `TrackedSet` cleanup references
/// `ExiledBySource`, so `is_linked_exile_cast_bottom_cleanup` returns false and
/// the "put the rest on the bottom" step is left addressing the wrong pool (a
/// tracked set nothing publishes) — the whole exiled pile is then stranded.
///
/// The distinguishing feature is the chain HEAD being
/// `ExileFromTopUntil { until: NextMatches }`: those cards are physically in the
/// exile zone, so the cleanup must scan exile, NOT the library. The Dig/look
/// class ("look at the top N, put the rest on the bottom") legitimately keeps
/// the parser-default library-only `TrackedSet` — its rest-cards never left the
/// library — and is excluded because its head is not `ExileFromTopUntil`.
pub(super) fn is_exile_until_cast_bottom_cleanup(
    head_effect: &Effect,
    cast_effect: &Effect,
    cleanup_effect: &Effect,
) -> bool {
    if !matches!(
        head_effect,
        Effect::ExileFromTopUntil {
            until: crate::types::ability::UntilCondition::NextMatches { .. },
            ..
        }
    ) {
        return false;
    }
    let Effect::CastFromZone { target, .. } = cast_effect else {
        return false;
    };
    // The cast anaphors the hit via `ParentTarget`; a cast that already reads
    // `ExiledBySource` is the plain Chaos Wand / Etali shape handled elsewhere.
    if !matches!(target, TargetFilter::ParentTarget) {
        return false;
    }
    matches!(
        cleanup_effect,
        Effect::PutAtLibraryPosition {
            position: LibraryPosition::Bottom,
            target: TargetFilter::TrackedSet { .. } | TargetFilter::TrackedSetFiltered { .. },
            ..
        }
    )
}

/// CR 608.2c + CR 701.13a: Rewrite the Jodah bottom-cleanup to "the rest"
/// semantics — every card this ExileFromTopUntil exiled EXCEPT the hit the
/// player may have cast. `And { ExiledBySource, DistinctFrom { ParentTarget } }`
/// scans the exile zone (via `ExiledBySource`) and, per the Scryfall ruling,
/// excludes the ParentTarget: a DECLINED hit REMAINS IN EXILE, so it must not be
/// swept to the bottom. `DistinctFrom { ParentTarget }` fails open when the
/// cleanup carries no object targets (the no-hit / library-exhausted case),
/// which is exactly right — with no hit, all exiled cards go to the bottom.
pub(super) fn normalize_exile_until_cast_bottom_cleanup(effect: &mut Effect) {
    if let Effect::PutAtLibraryPosition {
        ref mut target,
        ref mut count,
        position,
    } = effect
    {
        if matches!(position, LibraryPosition::Bottom) {
            *target = TargetFilter::And {
                filters: vec![
                    TargetFilter::ExiledBySource,
                    TargetFilter::Typed(TypedFilter::default().properties(vec![
                        FilterProp::DistinctFrom {
                            reference: Box::new(TargetFilter::ParentTarget),
                        },
                    ])),
                ],
            };
            // "All of them" placeholder — mirrors the existing linked-exile
            // normalize; the resolver treats `count: 0` on a Bottom cleanup as
            // "every matching card".
            *count = QuantityExpr::Fixed { value: 0 };
        }
    }
}

pub(super) fn is_spend_mana_as_any_color_rider(clause: &ClauseIr) -> bool {
    let Effect::GenericEffect {
        static_abilities, ..
    } = &clause.parsed.effect
    else {
        return false;
    };
    if static_abilities.len() != 1
        || static_abilities[0].mode
            != (StaticMode::SpendManaAsAnyColor {
                spell_filter: None,
                activation_source_filter: None,
            })
    {
        return false;
    }

    let lower = clause
        .source
        .fragment()
        .unwrap_or_default()
        .to_ascii_lowercase();
    let parsed = all_consuming((
        opt(alt((
            tag::<_, _, OracleError<'_>>("if you cast a spell this way, "),
            tag("if you cast it this way, "),
        ))),
        tag("you may spend mana as though it were mana of any "),
        alt((tag("color"), tag("type"))),
        tag(" to cast "),
        alt((
            tag("it"),
            tag("that spell"),
            tag("a spell this way"),
            tag("spells this way"),
            tag("those spells"),
        )),
        opt(tag(".")),
    ))
    .parse(lower.trim())
    .is_ok();
    parsed
}

pub(super) fn attach_any_color_mana_rider_to_previous_play_from_exile(
    defs: &mut [AbilityDefinition],
) -> bool {
    let Some(previous) = defs.last_mut() else {
        return false;
    };
    let Effect::GrantCastingPermission {
        permission:
            CastingPermission::PlayFromExile {
                mana_spend_permission,
                ..
            },
        ..
    } = previous.effect.as_mut()
    else {
        return false;
    };

    *mana_spend_permission = Some(ManaSpendPermission::AnyTypeOrColor);
    true
}

/// CR 614.1a + CR 608.2n: Fold a "if that spell would be put into a graveyard,
/// [put it on the library / return it to its owner's hand] instead" rider onto
/// the immediately-preceding optional `CastFromZone` as its canonical
/// sub-ability — targeting the cast spell (`ParentTarget`), count 1, the parsed
/// destination. The rider is a CR 608.2n destination-replacement on the *cast
/// spell* (Kylox's Voltstrider → library bottom; the hand variant), NOT a
/// sibling effect and NOT the Sanwell/Chaos-Wand free-cast bottom-cleanup that
/// `is_linked_exile_cast_bottom_cleanup` would otherwise mistake the
/// `PutAtLibraryPosition{Bottom}` for (mis-binding it to `ExiledBySource`,
/// count 0, and duplicating it into a bogus `else_ability`). Building the rider
/// directly here bypasses that generic mis-route.
///
/// The generic singular-spell route intentionally leaves exile to the existing
/// `ChangeZone{Exile, ParentTarget}` anaphor path. The exact plural "a spell
/// cast this way" form is handled separately by
/// `attach_graveyard_redirect_rider_to_prior_free_cast_from_zones`, which is
/// the only route that may attach stack/ParentTarget metadata for that wording.
pub(super) fn attach_graveyard_redirect_rider_to_prior_cast_from_zone(
    defs: &mut [AbilityDefinition],
    dest: SpellStackToGraveyardReplacement,
) -> bool {
    let rider_effect = match dest {
        SpellStackToGraveyardReplacement::Library { position } => Effect::PutAtLibraryPosition {
            target: TargetFilter::ParentTarget,
            count: QuantityExpr::Fixed { value: 1 },
            position,
        },
        SpellStackToGraveyardReplacement::Hand => Effect::ChangeZone {
            destination: Zone::Hand,
            origin: None,
            target: TargetFilter::ParentTarget,
            owner_library: false,
            enter_transformed: false,
            enters_under: None,
            enter_tapped: EtbTapState::Unspecified,
            enters_attacking: false,
            up_to: false,
            enter_with_counters: vec![],
            conditional_enter_with_counters: vec![],
            face_down_profile: None,
            enters_modified_if: None,
        },
        SpellStackToGraveyardReplacement::Exile => return false,
    };
    let Some(prev) = defs.last_mut() else {
        return false;
    };
    if !matches!(&*prev.effect, Effect::CastFromZone { .. }) || prev.sub_ability.is_some() {
        return false;
    }
    let mut rider = AbilityDefinition::new(AbilityKind::Spell, rider_effect);
    rider.sub_link = SubAbilityLink::SequentialSibling;
    prev.sub_ability = Some(Box::new(rider));
    true
}

/// CR 614.1a + CR 608.2g: absorb an exact "a spell cast this way" destination
/// rider into the immediately preceding free-cast window. Unlike the legacy
/// "that spell" form, this rider applies independently to every spell the
/// window casts, so it belongs on `FreeCastFromZones` metadata rather than as a
/// sequential `ParentTarget` sub-ability.
pub(super) fn attach_graveyard_redirect_rider_to_prior_free_cast_from_zones(
    defs: &mut [AbilityDefinition],
    dest: SpellStackToGraveyardReplacement,
) -> bool {
    let Some(prev) = defs.last_mut() else {
        return false;
    };
    let Effect::FreeCastFromZones {
        graveyard_replacement,
        ..
    } = &mut *prev.effect
    else {
        return false;
    };
    *graveyard_replacement = Some(dest);
    true
}

/// CR 601.2f: Detect an "each/a spell cast this way costs {N} more to cast"
/// rider sentence (Lightstall Inquisitor, Invasion of Gobakhan) and return the cost increase. This is
/// a cost-raise scoped to spells cast via the immediately-preceding
/// `PlayFromExile` grant ("this way" = the just-granted exile play), not a
/// global static cost increase — so it folds into the grant's `cast_cost_raise`
/// rather than emitting a standalone `StaticMode::ModifyCost`. Generic over the
/// printed increase (`{1}`, `{2}`, …); the mana symbols are case-insensitive
/// digits in the common generic case.
pub(super) fn cast_cost_raise_rider(clause: &ClauseIr) -> Option<ManaCost> {
    let lower = clause
        .source
        .fragment()
        .unwrap_or_default()
        .to_ascii_lowercase();
    nom_on_lower(
        clause.source.fragment().unwrap_or_default().trim(),
        lower.trim(),
        |i| {
            let (i, _) = alt((
                tag("each spell cast this way costs "),
                tag("a spell cast this way costs "),
            ))
            .parse(i)?;
            let (i, cost) = nom_primitives::parse_mana_cost(i)?;
            let (i, _) = tag(" more to cast").parse(i)?;
            let (i, _) = opt(tag(".")).parse(i)?;
            eof(i)?;
            Ok((i, cost))
        },
    )
    .map(|(cost, _)| cost)
}

fn parses_land_enters_tapped_rider(input: &str) -> bool {
    all_consuming(tag::<_, _, OracleError<'_>>(
        "each land played this way enters tapped",
    ))
    .parse(input)
    .is_ok()
}

/// CR 614.1c: Detect the "each land played this way enters tapped" rider
/// sentence (Lightstall Inquisitor) — "enters tapped" is a CR 614.1c
/// "[permanent] enters ..." replacement. Scoped to lands played via the
/// preceding `PlayFromExile` grant ("this way"), so it folds into the grant's
/// `land_enter_tapped` rather than emitting a board-wide ETB-tapped replacement.
pub(super) fn is_land_enters_tapped_rider(clause: &ClauseIr) -> bool {
    let lower = clause
        .source
        .fragment()
        .unwrap_or_default()
        .to_ascii_lowercase();
    let trimmed = lower.trim().trim_end_matches('.').trim();
    parses_land_enters_tapped_rider(trimmed)
}

pub(super) fn scan_until_next_same_source_exile_invalidation(lower: &str) -> bool {
    nom_primitives::scan_preceded(lower, |i| {
        terminated(parse_until_next_same_source_exile_invalidation, eof).parse(i)
    })
    .is_some()
}

fn parse_until_next_same_source_exile_invalidation(input: &str) -> OracleResult<'_, ()> {
    let (input, _) = tag("until ").parse(input)?;
    let (input, _) = parse_until_source_exiles_another_card_body(input)?;
    let (input, _) = opt(tag(".")).parse(input)?;
    Ok((input, ()))
}

/// Walk the previous def and its `sub_ability` chain for a `PlayFromExile`
/// permission. The grant produced by the compound "exile … and may play that
/// card" chain (Lightstall Inquisitor) lands as a sibling def during the lower
/// loop, but a self-contained "exile …. You may play …" chain (Gonti) nests it
/// as a sub-ability — handle both so the rider absorbs in either shape.
fn find_prev_play_from_exile_permission_mut(
    defs: &mut [AbilityDefinition],
) -> Option<&mut CastingPermission> {
    fn walk(def: &mut AbilityDefinition) -> Option<&mut CastingPermission> {
        let is_pfe = matches!(
            def.effect.as_ref(),
            Effect::GrantCastingPermission {
                permission: CastingPermission::PlayFromExile { .. },
                ..
            }
        );
        if is_pfe {
            if let Effect::GrantCastingPermission { permission, .. } = def.effect.as_mut() {
                return Some(permission);
            }
        }
        def.sub_ability.as_mut().and_then(|sub| walk(sub))
    }
    defs.last_mut().and_then(walk)
}

/// CR 601.2f: Fold an "each spell cast this way costs {N} more" rider into the
/// preceding `PlayFromExile` grant's `cast_cost_raise`.
pub(super) fn attach_cast_cost_raise_to_previous_play_from_exile(
    defs: &mut [AbilityDefinition],
    cost: ManaCost,
) -> bool {
    let Some(CastingPermission::PlayFromExile {
        cast_cost_raise, ..
    }) = find_prev_play_from_exile_permission_mut(defs)
    else {
        return false;
    };
    *cast_cost_raise = Some(cost);
    true
}

/// CR 614.1c: Fold an "each land played this way enters tapped" rider into the
/// preceding `PlayFromExile` grant's `land_enter_tapped`.
pub(super) fn attach_land_enters_tapped_to_previous_play_from_exile(
    defs: &mut [AbilityDefinition],
) -> bool {
    let Some(CastingPermission::PlayFromExile {
        land_enter_tapped, ..
    }) = find_prev_play_from_exile_permission_mut(defs)
    else {
        return false;
    };
    *land_enter_tapped = EtbTapState::Tapped;
    true
}

pub(super) fn is_linked_exile_cast_bottom_cleanup(
    cast_effect: &Effect,
    cleanup_effect: &Effect,
) -> bool {
    let Effect::CastFromZone { target, .. } = cast_effect else {
        return false;
    };
    let Effect::PutAtLibraryPosition {
        target: cleanup_target,
        position,
        ..
    } = cleanup_effect
    else {
        return false;
    };
    matches!(position, LibraryPosition::Bottom)
        && (target.references_exiled_by_source() || cleanup_target.references_exiled_by_source())
}

#[cfg(test)]
mod linked_exile_cleanup_tests {
    use super::*;
    // Only the assembly traversal (now in `assembly.rs`) still uses this type in
    // non-test code, so import it test-locally rather than at module scope.
    use crate::types::ability::CastFromZoneDriver;

    fn cast_from_zone(target: TargetFilter) -> Effect {
        Effect::CastFromZone {
            target,
            without_paying_mana_cost: false,
            mode: crate::types::ability::CardPlayMode::Cast,
            cast_transformed: false,
            alt_ability_cost: None,
            constraint: None,
            duration: None,
            driver: CastFromZoneDriver::LingeringPermission,
            mana_spend_permission: None,
        }
    }

    fn bottom_cleanup() -> Effect {
        Effect::PutAtLibraryPosition {
            target: TargetFilter::Any,
            count: QuantityExpr::Fixed { value: 1 },
            position: LibraryPosition::Bottom,
        }
    }

    #[test]
    fn linked_exile_cleanup_accepts_cast_target_or_cleanup_target_exile_link() {
        let mut cleanup = bottom_cleanup();

        assert!(is_linked_exile_cast_bottom_cleanup(
            &cast_from_zone(TargetFilter::ExiledBySource),
            &cleanup
        ));
        if let Effect::PutAtLibraryPosition { ref mut target, .. } = cleanup {
            *target = TargetFilter::ExiledBySource;
        }
        assert!(is_linked_exile_cast_bottom_cleanup(
            &cast_from_zone(TargetFilter::ParentTarget),
            &cleanup
        ));
        assert!(!is_linked_exile_cast_bottom_cleanup(
            &cast_from_zone(TargetFilter::Any),
            &bottom_cleanup()
        ));
    }
}

/// True for a `TargetFilter` carrying the `IsChosenCreatureType` discriminator.
fn filter_has_chosen_creature_type(filter: &TargetFilter) -> bool {
    matches!(filter, TargetFilter::Typed(f)
        if f.properties.contains(&FilterProp::IsChosenCreatureType))
}

/// The destination gate for "put onto the battlefield instead of putting it into
/// your hand if this spell's additional cost was paid and the revealed card is
/// the chosen type" — `And{ AdditionalCostPaid, TargetMatchesFilter{chosen type} }`.
fn is_chosen_type_battlefield_gate(cond: &AbilityCondition) -> bool {
    match cond {
        AbilityCondition::And { conditions } => {
            conditions
                .iter()
                .any(|c| matches!(c, AbilityCondition::AdditionalCostPaid { .. }))
                && conditions.iter().any(|c| {
                    matches!(c, AbilityCondition::TargetMatchesFilter { filter, .. }
                        if filter_has_chosen_creature_type(filter))
                })
        }
        _ => false,
    }
}

/// CR 608.2c: Rebuild a `Search … reveal … put it into your hand … put onto the
/// battlefield instead if <cost paid> and <revealed card is the chosen type>`
/// chain into the canonical conditional-destination form the runtime's search
/// continuation evaluates: `SearchLibrary → N`, where `N` moves the found card to
/// the battlefield when its `And` gate holds and otherwise (`else_ability`) to
/// the hand, with a `Shuffle` in both branches. The lowered chain reaches this
/// function as a mangled sequence (an unconditional battlefield move, a shuffle,
/// and a trailing `And`-gated battlefield move); it is folded here rather than
/// during clause assembly because the "instead of putting it into your hand"
/// destination-swap is mid-phrase and does not reach the intra-chain instead
/// composer. Scoped to the exact chosen-creature-type gate so no other card's
/// search chain is touched.
pub(super) fn fold_search_choose_type_conditional_destination(def: &mut AbilityDefinition) {
    if !matches!(&*def.effect, Effect::SearchLibrary { .. }) {
        return;
    }
    let mut gate: Option<AbilityCondition> = None;
    let mut move_template: Option<AbilityDefinition> = None;
    let mut shuffle: Option<AbilityDefinition> = None;
    let mut cur = def.sub_ability.as_deref();
    while let Some(node) = cur {
        if move_template.is_none() && matches!(&*node.effect, Effect::ChangeZone { .. }) {
            move_template = Some(node.clone());
        }
        if shuffle.is_none() && matches!(&*node.effect, Effect::Shuffle { .. }) {
            shuffle = Some(node.clone());
        }
        if gate.is_none() {
            if let Some(cond) = &node.condition {
                if is_chosen_type_battlefield_gate(cond)
                    && matches!(
                        &*node.effect,
                        Effect::ChangeZone {
                            destination: Zone::Battlefield,
                            ..
                        }
                    )
                {
                    gate = Some(cond.clone());
                }
            }
        }
        cur = node.sub_ability.as_deref();
    }
    let (Some(gate), Some(mut move_template), Some(mut shuffle)) = (gate, move_template, shuffle)
    else {
        return;
    };
    // Strip any inherited chain wiring from the reused nodes.
    move_template.condition = None;
    move_template.else_ability = None;
    move_template.sub_ability = None;
    shuffle.condition = None;
    shuffle.else_ability = None;
    shuffle.sub_ability = None;

    let set_destination = |node: &mut AbilityDefinition, dest: Zone| {
        if let Effect::ChangeZone { destination, .. } = &mut *node.effect {
            *destination = dest;
        }
    };

    // else branch C: put the found card into hand, then shuffle.
    let mut else_hand = move_template.clone();
    set_destination(&mut else_hand, Zone::Hand);
    else_hand.sub_ability = Some(Box::new(shuffle.clone()));

    // then branch N: put the found card onto the battlefield, then shuffle,
    // gated on the And condition; else_ability is the hand branch.
    let mut then_bf = move_template;
    set_destination(&mut then_bf, Zone::Battlefield);
    then_bf.condition = Some(gate);
    then_bf.sub_ability = Some(Box::new(shuffle));
    then_bf.else_ability = Some(Box::new(else_hand));

    def.sub_ability = Some(Box::new(then_bf));
}

/// Lower a parsed `EffectChainIr` into a single root `AbilityDefinition`.
///
/// Plan 01 §6: the assembly traversal itself now lives in
/// [`super::assembly::assemble_effect_chain`]; this keeps the existing signature
/// and pure `&EffectChainIr -> AbilityDefinition` contract for all callers.
pub(crate) fn lower_effect_chain_ir(ir: &EffectChainIr) -> AbilityDefinition {
    super::assembly::assemble_effect_chain(ir)
}

/// CR 608.2c: The anaphor `ObjectScope::OtherRevealedCard` ("the card revealed by
/// the other player") is only well-formed when its host chain contains a
/// multi-player reveal fan-out — a `RevealTop` whose ability carries a
/// `multi_target` spec (a >=2-player reveal shape). A single-subject reveal
/// provides no "other" anchor, so any OtherRevealedCard-bearing effect is rewritten
/// back to an honest `Effect::Unimplemented` gap. Predicate is `multi_target`
/// PRESENCE (not `min >= 2`) so a future "any number of target players each …"
/// host (`Some { max: None }`, `min: 0`) is not false-negatived; the runtime
/// resolved-to-<2-players case is handled separately by the by-exclusion
/// fail-closed read (→ 0). Parker Luck (`multi_target` present) keeps its parsed
/// `LoseLife`; Keen Duelist ("you and target opponent each", lowered to
/// `RevealTop { player: Any }`, no `multi_target`) stays honest-red until its
/// compound-subject distribution is built.
pub(super) fn gate_other_revealed_card_on_multiplayer_reveal(def: &mut AbilityDefinition) {
    if chain_has_multiplayer_reveal(def) {
        return;
    }
    rewrite_other_revealed_card_to_unimplemented(def);
}

/// True when any def in the chain is a `RevealTop` carrying a `multi_target` spec.
fn chain_has_multiplayer_reveal(def: &AbilityDefinition) -> bool {
    if matches!(&*def.effect, Effect::RevealTop { .. }) && def.multi_target.is_some() {
        return true;
    }
    def.sub_ability
        .as_deref()
        .is_some_and(chain_has_multiplayer_reveal)
        || def
            .else_ability
            .as_deref()
            .is_some_and(chain_has_multiplayer_reveal)
        || def.mode_abilities.iter().any(chain_has_multiplayer_reveal)
}

/// Rewrite every OtherRevealedCard-bearing effect in the chain back to an honest
/// `Effect::Unimplemented` gap. Coverage keys only on the `Unimplemented`
/// variant, so the reconstructed fragment is cosmetic; the def carries no source
/// text on a lowered `LoseLife`, so fall back to the canonical class fragment.
fn rewrite_other_revealed_card_to_unimplemented(def: &mut AbilityDefinition) {
    if effect_reads_other_revealed_card(&def.effect) {
        let fragment = def.description.clone().unwrap_or_else(|| {
            "lose life equal to the mana value of the card revealed by the other player".to_string()
        });
        *def.effect = Effect::unimplemented("lose", fragment);
    }
    if let Some(sub) = def.sub_ability.as_mut() {
        rewrite_other_revealed_card_to_unimplemented(sub);
    }
    if let Some(els) = def.else_ability.as_mut() {
        rewrite_other_revealed_card_to_unimplemented(els);
    }
    for mode in def.mode_abilities.iter_mut() {
        rewrite_other_revealed_card_to_unimplemented(mode);
    }
}

/// CR 608.2c: True when an effect's amount quantity reads
/// `ObjectScope::OtherRevealedCard`. The whole class today is `LoseLife`/`GainLife`
/// "… equal to the mana value of the card revealed by the other player"; extend
/// by adding the hosting effect's amount field here.
fn effect_reads_other_revealed_card(effect: &Effect) -> bool {
    match effect {
        Effect::LoseLife { amount, .. } | Effect::GainLife { amount, .. } => {
            quantity_expr_reads_other_revealed_card(amount)
        }
        _ => false,
    }
}

fn quantity_expr_reads_other_revealed_card(expr: &QuantityExpr) -> bool {
    match expr {
        QuantityExpr::Ref { qty } => quantity_ref_reads_other_revealed_card(qty),
        QuantityExpr::DivideRounded { inner, .. }
        | QuantityExpr::Multiply { inner, .. }
        | QuantityExpr::ClampMin { inner, .. }
        | QuantityExpr::Offset { inner, .. }
        | QuantityExpr::UpTo { max: inner }
        | QuantityExpr::Power {
            exponent: inner, ..
        } => quantity_expr_reads_other_revealed_card(inner),
        QuantityExpr::Sum { exprs } | QuantityExpr::Max { exprs } => {
            exprs.iter().any(quantity_expr_reads_other_revealed_card)
        }
        QuantityExpr::Difference { left, right } => {
            quantity_expr_reads_other_revealed_card(left)
                || quantity_expr_reads_other_revealed_card(right)
        }
        QuantityExpr::Fixed { .. } => false,
    }
}

fn quantity_ref_reads_other_revealed_card(qty: &QuantityRef) -> bool {
    let scope = match qty {
        QuantityRef::ObjectManaValue { scope }
        | QuantityRef::Power { scope }
        | QuantityRef::BasePower { scope }
        | QuantityRef::Toughness { scope }
        | QuantityRef::ObjectColorCount { scope }
        | QuantityRef::ObjectNameWordCount { scope }
        | QuantityRef::ObjectTypelineComponentCount { scope }
        | QuantityRef::CountersOn { scope, .. }
        | QuantityRef::ManaSymbolsInManaCost { scope, .. } => scope,
        _ => return false,
    };
    matches!(scope, ObjectScope::OtherRevealedCard)
}

/// CR 707.10f + CR 608.3f: Fold a spell-copy rider "The copy gains haste and
/// \"<quoted triggered ability>\"" (Choreographed Sparks; Nalfeshnee via its
/// trigger-execute chain) into the preceding `CopySpell.additional_modifications`.
///
/// This is NOT a chained effect: a `CopySpell` puts the copy on the STACK, and
/// it becomes a token only as it resolves (CR 707.10f / CR 608.3f). A chained
/// effect running at CopySpell resolution would act on a stack object, not the
/// resulting permanent. `additional_modifications` is the carrier that rides the
/// copy through the stack→token transition (as Ob Nixilis's RemoveSupertype
/// does) — `apply_spell_copy_modifications` stamps AddKeyword/GrantTrigger onto
/// the copy at creation. Walking the whole assembled tree makes this fire at the
/// `lower_effect_chain_ir` chokepoint for the trigger-execute form (Nalfeshnee),
/// which never passes through the activated-ability `parse_effect_chain` wrappers.
///
/// CR 611.2c deviation (Nalfeshnee): Nalfeshnee's grant is conditional ("If it's
/// a permanent spell"); that condition is dropped upstream, so the rider is
/// appended unconditionally. Practically harmless — Choreographed mode-2 targets
/// a creature spell (always a permanent); a non-permanent copy's haste is inert
/// and its granted end-step-sacrifice trigger never fires (a copy of a
/// non-permanent spell ceases to exist as it resolves, CR 707.10).
pub(super) fn fold_copy_spell_gains_haste_and_quoted_grant(def: &mut AbilityDefinition) {
    if matches!(&*def.effect, Effect::CopySpell { .. }) {
        if let Some(mut sub) = def.sub_ability.take() {
            let folded = (sub.sub_link == SubAbilityLink::SequentialSibling)
                .then(|| match sub.effect.as_ref() {
                    // allow-noncombinator: destructuring an existing Unimplemented to read its description; not a construction.
                    Effect::Unimplemented {
                        description: Some(desc),
                        ..
                    } => parse_copy_gains_haste_and_quoted_grant(desc),
                    _ => None,
                })
                .flatten();
            match folded {
                Some(mods) => {
                    if let Effect::CopySpell {
                        additional_modifications,
                        ..
                    } = &mut *def.effect
                    {
                        additional_modifications.extend(mods);
                    }
                    // Drop the folded Unimplemented, preserving any deeper chain.
                    def.sub_ability = sub.sub_ability.take();
                }
                None => def.sub_ability = Some(sub),
            }
        }
    }
    if let Some(sub) = def.sub_ability.as_mut() {
        fold_copy_spell_gains_haste_and_quoted_grant(sub);
    }
    if let Some(els) = def.else_ability.as_mut() {
        fold_copy_spell_gains_haste_and_quoted_grant(els);
    }
}

/// CR 702.10a + CR 603.1 + CR 701.21a: Decompose "[Tt]he copy gains
/// <keyword(s)> and \"<quoted triggered ability>\"" into `ContinuousModification`s
/// (the granted quoted ability here being the CR 701.21a end-step "sacrifice ~"
/// trigger), reusing the shared keyword-list and quoted-ability classifiers.
/// Returns `None` unless the text has BOTH a keyword grant and a quoted-ability
/// grant, so an unrelated "the copy ..." Unimplemented is never mis-folded.
fn parse_copy_gains_haste_and_quoted_grant(desc: &str) -> Option<Vec<ContinuousModification>> {
    let lower = desc.to_lowercase();
    let tp = TextPair::new(desc, &lower);
    // Strip the "the copy " subject (case-insensitive), leaving "gains <...>" so
    // the shared keyword-clause extractor still sees the "gains" verb.
    // allow-noncombinator: TextPair dual-string prefix strip preserving original case; fixed known subject on a pre-classified clause, not parse-branch dispatch.
    let rest = tp.strip_prefix("the copy ")?.original;
    let mut mods = crate::parser::oracle_static::parse_continuous_modifications(rest);
    let quoted = crate::parser::oracle_static::parse_quoted_ability_modifications(rest);
    if mods.is_empty() || quoted.is_empty() {
        return None;
    }
    mods.extend(quoted);
    Some(mods)
}

/// CR 608.2c + CR 613.1f: A standalone "choose a [type] card exiled with ~"
/// ability — a `ChooseFromZone` from the host's linked-exile set
/// (`ExiledBySource`) with no follow-up consumer — persists its pick as the host's
/// "last chosen card" by appending an `Effect::RememberCard` sub-ability. A choice
/// with no consumer is otherwise a no-op no real card prints; the only cards with
/// this shape feed a companion `TargetFilter::ChosenCard` grant (Koh, the Face
/// Stealer — "has all activated and triggered abilities of the last chosen card").
/// RememberCard reads the resolution chain's published pick via the
/// `TrackedSetId(0)` sentinel (`resolve_tracked_set_sentinel`).
pub(super) fn append_remember_card_to_standalone_exiled_choice(def: &mut AbilityDefinition) {
    if def.sub_ability.is_some() {
        return;
    }
    let from_linked_exile = matches!(
        &*def.effect,
        Effect::ChooseFromZone { filter: Some(f), .. } if filter_mentions_exiled_by_source(f)
    );
    if !from_linked_exile {
        return;
    }
    def.sub_ability = Some(Box::new(AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::RememberCard {
            target: TargetFilter::TrackedSet {
                id: crate::types::identifiers::TrackedSetId(0),
            },
        },
    )));
}

/// Recursively detect a `TargetFilter::ExiledBySource` leaf (possibly nested under
/// `And`/`Or`) — the "exiled with ~" linked-exile marker.
fn filter_mentions_exiled_by_source(filter: &TargetFilter) -> bool {
    match filter {
        TargetFilter::ExiledBySource => true,
        TargetFilter::And { filters } | TargetFilter::Or { filters } => {
            filters.iter().any(filter_mentions_exiled_by_source)
        }
        _ => false,
    }
}

/// CR 115.1: True when a `ChangeZone` clause selects from the battlefield
/// (explicitly or by permanent-type default) rather than a private/off-BF zone.
pub(super) fn change_zone_selects_battlefield_permanent(
    origin: Option<Zone>,
    target: &TargetFilter,
) -> bool {
    if target.is_context_ref() {
        return false;
    }
    if origin.is_some_and(|zone| zone != Zone::Battlefield) {
        return false;
    }
    if let Some(zone) = target.extract_in_zone() {
        return zone == Zone::Battlefield;
    }
    let zones = target.extract_zones();
    if !zones.is_empty() {
        return zones == [Zone::Battlefield];
    }
    matches!(target, TargetFilter::Typed(_))
}

/// CR 115.10a + CR 608.2d: shared ChangeZone stack-vs-resolution classifier.
/// `fragment_lower` must be the moved-object clause / predicate only — never a
/// subject prefix like "Target player …", which would false-positive the
/// `"target "` scan and force Stack.
///
/// Used by `target_choice_timing_for_clause` (`ChangeZone` only — mass
/// `ChangeZoneAll` keeps the historical Stack default there) and by the
/// `"target player" + ChangeZone/ChangeZoneAll` TargetOnly wrap (Strategic
/// Betrayal #6505, Relic of Progenitus #6446).
pub(super) fn change_zone_target_choice_timing(
    origin: Option<Zone>,
    target: &TargetFilter,
    has_multi_target: bool,
    fragment_lower: &str,
) -> TargetChoiceTiming {
    let off_battlefield_origin = origin.is_some_and(|zone| zone != Zone::Battlefield)
        || has_multi_target
            && target
                .extract_zones()
                .iter()
                .any(|zone| *zone != Zone::Battlefield);
    if off_battlefield_origin {
        // Off-BF non-"target " legs (Relic: "exiles a card from their graveyard")
        // are resolution picks; explicit "target cards …" (Memory's Journey) stay Stack.
        if nom_primitives::scan_contains(fragment_lower, "target ") {
            TargetChoiceTiming::Stack
        } else {
            TargetChoiceTiming::Resolution
        }
    } else if nom_primitives::scan_contains(fragment_lower, "target ") {
        TargetChoiceTiming::Stack
    } else if change_zone_selects_battlefield_permanent(origin, target) {
        // CR 115.1: battlefield non-targeted picks (Sothera / Strategic Betrayal
        // edict class) resolve via EffectZoneChoice after player_scope rebinding,
        // not stack targeting.
        // Graveyard/hand/library seeds without "target" (Deadly Cover-Up) keep
        // stack-time selection — their filters carry explicit InZone constraints
        // and origin is None (not off_battlefield_origin above).
        TargetChoiceTiming::Resolution
    } else {
        TargetChoiceTiming::Stack
    }
}

pub(super) fn target_choice_timing_for_clause(clause_ir: &ClauseIr) -> TargetChoiceTiming {
    if let Effect::ChooseCounterKind { target } = &clause_ir.parsed.effect {
        let lower = clause_ir
            .source
            .fragment()
            .unwrap_or_default()
            .to_ascii_lowercase();
        // CR 115.1 + CR 608.2d: "choose a counter on a permanent you
        // control" is an untargeted choice made while the ability resolves.
        // Context references are already bound and need no selection slot.
        if !nom_primitives::scan_contains(&lower, "target ") && !target.is_context_ref() {
            return TargetChoiceTiming::Resolution;
        }
    }
    if let Effect::PutCounter { target, .. } = &clause_ir.parsed.effect {
        let lower = clause_ir
            .source
            .fragment()
            .unwrap_or_default()
            .to_ascii_lowercase();
        // CR 115.10a: an object is a target only if the text uses the literal
        // word "target"; CR 608.2d: an untargeted choice is made "while
        // applying the effect" (at resolution), not at announcement. Was
        // previously scoped to `contains_source_attachment_host()` alone
        // (Equipped/Enchanted-host counters, e.g. "put a loyalty counter on
        // the equipped creature" — deterministic, no player choice). Widened
        // to every untargeted `PutCounter` recipient that isn't already a
        // deterministic `is_context_ref()` shape (SelfRef/ParentTarget/None/…,
        // which resolve automatically regardless of timing) — this is the
        // same generalization `MultiplyCounter` below already applies. Covers
        // "put a keyword counter on any creature you control" (Kathril,
        // Aspect Warper, issue #6321/#6533): each independent instruction in a
        // replicated keyword-counter chain must offer its own untargeted
        // choice at ITS OWN resolution (CR 608.2d), not inherit one shared
        // choice made once when the whole ability went on the stack.
        if !nom_primitives::scan_contains(&lower, "target ") && !target.is_context_ref() {
            return TargetChoiceTiming::Resolution;
        }
    }
    if matches!(clause_ir.parsed.effect, Effect::MultiplyCounter { .. }) {
        let lower = clause_ir
            .source
            .fragment()
            .unwrap_or_default()
            .to_ascii_lowercase();
        if !nom_primitives::scan_contains(&lower, "target ") {
            return TargetChoiceTiming::Resolution;
        }
    }
    // CR 701.26a/b: only single-target tap/untap (legacy `Tap`/`Untap`) takes
    // the resolution-timing branch; the mass scope never declares multi-target.
    if matches!(
        clause_ir.parsed.effect,
        Effect::SetTapState {
            scope: EffectScope::Single,
            ..
        }
    ) && clause_ir.multi_target.is_some()
    {
        let lower = clause_ir
            .source
            .fragment()
            .unwrap_or_default()
            .to_ascii_lowercase();
        if !nom_primitives::scan_contains(&lower, "target ") {
            return TargetChoiceTiming::Resolution;
        }
    }

    // Mass `ChangeZoneAll` stays Stack here (pre-#6446). The TargetOnly wrap
    // may still stamp Resolution on ChangeZoneAll resolution-picks via the
    // shared helper; clause-IR timing must not silently reclassify every
    // off-BF mass move (Bomat Courier / Jace −12 snapshot regressions).
    let Effect::ChangeZone { origin, target, .. } = &clause_ir.parsed.effect else {
        return TargetChoiceTiming::Stack;
    };
    let lower = clause_ir
        .source
        .fragment()
        .unwrap_or_default()
        .to_ascii_lowercase();
    let has_multi_target =
        clause_ir.multi_target.is_some() || clause_ir.parsed.multi_target.is_some();
    change_zone_target_choice_timing(*origin, target, has_multi_target, &lower)
}

/// CR 303.4f: Aura entering by non-spell means — controller chooses the enchanted object.
/// CR 301.5b: Equipment entering attached via "put onto the battlefield attached to" wiring.
/// CR 603.7d: A delayed trigger's source/controller is the parent ability's at creation time.
/// CR 608.2c: Bare "it" anaphor in a later clause binds to the typed referent of an earlier clause.
///
/// Walk the chain and set `forward_result: true` on every `Dig`/`ChangeZone`
/// whose `destination` is `Battlefield` and whose chained sub-ability anchors
/// on the just-moved card. Two anchor shapes are recognized:
///
/// 1. `Attach` sub with a `ZoneChangedThisWay` condition — the Oracle text
///    said "If a[n] [type] is/was put onto the battlefield this way,
///    [attach it]" (Armored Skyhunter, Stonehewer Giant). The just-moved
///    card becomes the attaching object.
/// 2. A non-Attach sub whose own target slot (or a nested
///    GenericEffect/CreateDelayedTrigger inside it) is `SelfRef` — the
///    Oracle text used a bare-"it" anaphor for the just-moved card
///    (Emperor of Bones: "put a creature card exiled with this creature
///    onto the battlefield […]. It gains haste. Sacrifice it at the
///    beginning of the next end step."). The runtime forward_result branch
///    rewrites `sub.source_id` to the moved object, so `SelfRef` in the
///    sub naturally resolves to it.
///
/// Recurses through nested sub-abilities so chains of arbitrary depth
/// (e.g. Skyhunter's Dig → Attach → PutAtLibraryPosition) are covered.
/// CR 122.1 + CR 614.1c: "If a Hero enters this way, it enters with an
/// additional +1/+1 counter on it" riders on a parent battlefield zone change
/// are entry replacement properties, not post-move `PutCounter` subs.
/// CR 608.2c + CR 611.2c: the two-target fight class — "Choose target creature
/// you control and target creature you don't control. … [buff the you-control
/// creature]. Then those creatures fight each other." (Malamet Battle Glyph,
/// Longstalk Brawl, Duel for Dominance, Tail Swipe, Joust, Blizzard Brawl, #4751).
/// Chain descent propagates only the most-recent (opponent) target to later
/// nodes, so the buff's back-reference to "the creature you control" — and any
/// entered-this-turn condition subject — would bind the OPPONENT's creature (or,
/// for `Pump`/`GenericEffect`, an unscoped whole-board target). When the chain
/// declares >= 2 `TargetOnly` object slots, re-key the buff to
/// `ParentTargetSlot { index: 0 }` (the first-declared you-control creature —
/// `try_parse_two_targets` emits it first) and bind a `PutCounter`'s
/// `TargetMatchesFilter` condition to the same slot 0. Covers every buff shape
/// the class uses: `PutCounter{ParentTarget}` (counter cards),
/// `Pump{Any|ParentTarget}` (Tail Swipe / Joust), and a SelfRef-affected
/// `GenericEffect{target:None}` (Blizzard's "gets +N/+M and gains <keyword>").
/// Longstalk's `AdditionalCostPaid` and Duel's count gate are not
/// `TargetMatchesFilter`, so their conditions stay node-local. The reciprocal
/// "those creatures fight each other" object is `ParentTarget` by design
/// (`parse_fight_target`), so the fight itself needs no rekey.
pub(super) fn rewrite_two_target_counter_chain(def: &mut AbilityDefinition) {
    // CR 611.2c + CR 701.14a: gate on the class's ACTUAL signature — a chain that
    // both declares >= 2 typed target slots AND contains an `Effect::Fight`. The
    // slot count alone is a proxy for "two-target declaration"; it says nothing
    // about a fight, so on its own it would let the widened `Pump`/`GenericEffect`
    // rekey fire on any future >= 2-target chain that happens to route an unscoped
    // buff here. Requiring the `Fight` node makes the guard encode the two-target
    // FIGHT class the function is named for, so the rekey is safe by construction
    // rather than by the current pool happening not to trip it (matthewevans
    // review, #4751). All six class cards (Malamet Battle Glyph, Longstalk Brawl,
    // Duel for Dominance, Tail Swipe, Joust, Blizzard Brawl) carry "those
    // creatures fight each other" as a later node in the same chain.
    if count_typed_target_only_slots(def) >= 2 && chain_contains_fight(def) {
        rekey_counter_slot_in_chain(def);
    }
}

/// True if this definition or any node reachable through its `sub_ability` /
/// `else_ability` chain is an `Effect::Fight` (CR 701.14a). The two-target fight
/// class always emits the fight as a later node in the same chain, so a linear
/// descent suffices.
fn chain_contains_fight(def: &AbilityDefinition) -> bool {
    matches!(&*def.effect, Effect::Fight { .. })
        || def.sub_ability.as_deref().is_some_and(chain_contains_fight)
        || def
            .else_ability
            .as_deref()
            .is_some_and(chain_contains_fight)
}

fn count_typed_target_only_slots(def: &AbilityDefinition) -> usize {
    let here = usize::from(matches!(
        &*def.effect,
        Effect::TargetOnly {
            target: TargetFilter::Typed(_)
        }
    ));
    here + def
        .sub_ability
        .as_deref()
        .map_or(0, count_typed_target_only_slots)
        + def
            .else_ability
            .as_deref()
            .map_or(0, count_typed_target_only_slots)
}

fn rekey_counter_slot_in_chain(def: &mut AbilityDefinition) {
    let rekeyed = match &mut *def.effect {
        // CR 608.2c: "put a +1/+1 counter on [the you-control creature]" — the
        // `ParentTarget`/"it" anaphor must bind slot 0 (Malamet / Longstalk /
        // Duel), not the most-recent opponent target.
        Effect::PutCounter { target, .. } if matches!(target, TargetFilter::ParentTarget) => {
            *target = TargetFilter::ParentTargetSlot { index: 0 };
            true
        }
        // CR 611.2c + CR 608.2c: "the creature you control gets +N/+N" — the
        // definite back-reference (Tail Swipe / Joust / Blizzard Brawl, #4751)
        // resolves to slot 0 (the first-declared you-control creature), not the
        // whole battlefield. Only the two-target fight chain routes a `Pump`
        // through here (guarded by `count_typed_target_only_slots >= 2`), where an
        // unscoped `Any`/`ParentTarget` target is always that back-reference.
        Effect::Pump { target, .. }
            if matches!(target, TargetFilter::Any | TargetFilter::ParentTarget) =>
        {
            *target = TargetFilter::ParentTargetSlot { index: 0 };
            true
        }
        // CR 611.2c + CR 613: "the creature you control gets +N/+M and gains
        // <keyword>" lowers to a `GenericEffect` whose per-target static applies
        // to `SelfRef` (the effect's own target). Blizzard Brawl leaves that
        // target unwired (`None`); bind it to slot 0 so the buff lands on the
        // first-declared you-control creature, not nowhere. Guarded on a
        // SelfRef-affected static so a global anthem (`affected: Typed(...)`,
        // `target: None`) is never captured.
        Effect::GenericEffect {
            target,
            static_abilities,
            ..
        } if target.is_none()
            && static_abilities
                .iter()
                .any(|s| matches!(s.affected, Some(TargetFilter::SelfRef))) =>
        {
            *target = Some(TargetFilter::ParentTargetSlot { index: 0 });
            true
        }
        _ => false,
    };
    if rekeyed {
        // CR 608.2c: the counter node's own condition ("if the creature you
        // control entered this turn") must test slot 0, not this node's
        // most-recent (opponent) local target.
        if let Some(AbilityCondition::TargetMatchesFilter { subject_slot, .. }) =
            def.condition.as_mut()
        {
            *subject_slot = Some(0);
        }
    }
    if let Some(sub) = def.sub_ability.as_mut() {
        rekey_counter_slot_in_chain(sub);
    }
    if let Some(els) = def.else_ability.as_mut() {
        rekey_counter_slot_in_chain(els);
    }
}

pub(super) fn fold_enters_this_way_counter_rider(def: &mut AbilityDefinition) {
    let parent_moves_to_battlefield = matches!(
        *def.effect,
        Effect::ChangeZone {
            destination: Zone::Battlefield,
            ..
        } | Effect::Dig {
            destination: Some(Zone::Battlefield),
            ..
        }
    );
    if !parent_moves_to_battlefield {
        if let Some(sub) = def.sub_ability.as_mut() {
            fold_enters_this_way_counter_rider(sub);
        }
        if let Some(else_branch) = def.else_ability.as_mut() {
            fold_enters_this_way_counter_rider(else_branch);
        }
        return;
    }

    let Some(mut sub) = def.sub_ability.take() else {
        return;
    };

    // CR 122.6: counters given as an object enters the battlefield use the
    // same entry-counter representation as counters put on a battlefield object.
    // The parent is itself a battlefield-entry effect, so both legacy
    // destination-agnostic conditions and explicit battlefield-arrival
    // conditions describe this typed entry-counter slot. Other named
    // destinations must remain standalone riders.
    let Some(AbilityCondition::ZoneChangedThisWay {
        filter,
        destination,
    }) = sub.condition.clone()
    else {
        def.sub_ability = Some(sub);
        fold_enters_this_way_counter_rider(def.sub_ability.as_mut().unwrap());
        return;
    };
    if !matches!(destination, None | Some(Zone::Battlefield)) {
        def.sub_ability = Some(sub);
        fold_enters_this_way_counter_rider(def.sub_ability.as_mut().unwrap());
        return;
    }

    if let Effect::PutCounter {
        counter_type,
        count,
        target: TargetFilter::ParentTarget,
    } = &*sub.effect
    {
        if let Effect::ChangeZone {
            conditional_enter_with_counters,
            ..
        } = &mut *def.effect
        {
            conditional_enter_with_counters.push((filter, counter_type.clone(), count.clone()));
            def.sub_ability = sub.sub_ability.take();
            if let Some(nested) = def.sub_ability.as_mut() {
                fold_enters_this_way_counter_rider(nested);
            }
            return;
        }
    }

    def.sub_ability = Some(sub);
    if let Some(sub) = def.sub_ability.as_mut() {
        fold_enters_this_way_counter_rider(sub);
    }
}

/// CR 603.7a + CR 608.2c + CR 702.170c: fold the "If you do, ..." continuation
/// of an "exile [the resolving spell] instead of putting it into [a/your]
/// graveyard as it resolves" clause into the carrier effect's typed `on_exile`
/// rider (`ExiledSpellRider`). Two members:
///   - Feather, the Redeemed: "return it to your hand at the beginning of the
///     next end step" → `ReturnTo { Hand, AtNextPhase { End } }`.
///   - Lilah, Undefeated Slickshot: "it becomes plotted" → `BecomePlotted`.
///
/// The generic at-trigger-resolution lowering is wrong for this class: per
/// CR 603.7a a consequence created "as the result of a replacement effect being
/// applied" exists only once the replacement is APPLIED — i.e. when the spell
/// actually lands in exile during its own stack resolution — not when the
/// trigger resolves. Leaving Feather's `CreateDelayedTrigger` (or Lilah's
/// `GrantCastingPermission { Plotted }`) as an ordinary chained effect would
/// apply it to a spell that was later countered in response (the replacement
/// never applied). The rider routes through the per-object marker so the stack
/// router applies the consequence at replacement-application time.
///
/// Deliberately conservative: any structural mismatch leaves the sub-ability
/// unfolded, so `swallow_check` keeps flagging unrepresented text instead of
/// silently dropping it.
pub(super) fn fold_exile_resolving_rider(def: &mut AbilityDefinition) {
    if matches!(
        *def.effect,
        Effect::ExileResolvingSpellInsteadOfGraveyard { .. }
    ) {
        if let Some(sub) = def.sub_ability.take() {
            if let Some(rider) = detect_exile_resolving_rider(&sub) {
                if let Effect::ExileResolvingSpellInsteadOfGraveyard { on_exile } = &mut *def.effect
                {
                    *on_exile = Some(rider);
                }
                // The continuation is fully represented by the typed rider —
                // consume the sub.
            } else {
                def.sub_ability = Some(sub);
            }
        }
    }
    if let Some(sub) = def.sub_ability.as_mut() {
        fold_exile_resolving_rider(sub);
    }
    if let Some(else_branch) = def.else_ability.as_mut() {
        fold_exile_resolving_rider(else_branch);
    }
}

/// CR 603.7a + CR 702.170c: classify the exile-instead continuation sub-ability
/// into its typed rider, or `None` if it is not a recognized consequence. The
/// per-member matchers stay conservative so unrecognized text is left unfolded
/// for `swallow_check` to flag.
fn detect_exile_resolving_rider(sub: &AbilityDefinition) -> Option<ExiledSpellRider> {
    if is_exile_resolving_return_rider(sub) {
        // CR 603.7a: Feather's return axes — owner's hand, at the beginning of
        // the next end step.
        return Some(ExiledSpellRider::ReturnTo {
            destination: Zone::Hand,
            timing: DelayedTriggerCondition::AtNextPhase { phase: Phase::End },
        });
    }
    if is_exile_resolving_plotted_rider(sub) {
        // CR 702.170c: Lilah's "it becomes plotted".
        return Some(ExiledSpellRider::BecomePlotted);
    }
    None
}

/// Structural match for Lilah's plotted rider: an optionally "if you do"-gated
/// `GrantCastingPermission { Plotted }` on the resolving spell (the
/// `ParentTarget`/`SelfRef` anaphor), granting to the card's owner.
///
/// CR 608.2c: the "if you do" back-reference is absorbed by the plotted-grant
/// continuation grammar (see `parse_becomes_plotted_continuation`), so the
/// grant may carry either no condition or a duplicate optional-effect-performed
/// gate — but never an independent game-state condition, which would make the
/// plot genuinely conditional and wrong to fold to an unconditional rider.
fn is_exile_resolving_plotted_rider(sub: &AbilityDefinition) -> bool {
    if !sub
        .condition
        .as_ref()
        .is_none_or(AbilityCondition::is_optional_effect_performed)
    {
        return false;
    }
    if sub.sub_ability.is_some() || sub.else_ability.is_some() {
        return false;
    }
    matches!(
        &*sub.effect,
        Effect::GrantCastingPermission {
            permission: CastingPermission::Plotted { .. },
            target: TargetFilter::ParentTarget | TargetFilter::SelfRef,
            grantee: PermissionGrantee::ObjectOwner,
        }
    )
}

/// Structural match for the return rider: an "if you do"-gated
/// `CreateDelayedTrigger` at the next end step whose sole body returns the
/// resolving spell (the `ParentTarget`/`SelfRef` anaphor) to its owner's hand.
fn is_exile_resolving_return_rider(sub: &AbilityDefinition) -> bool {
    // CR 608.2c: "If you do" — the optional-effect-performed gate on the rider.
    if !sub
        .condition
        .as_ref()
        .is_some_and(AbilityCondition::is_optional_effect_performed)
    {
        return false;
    }
    if sub.sub_ability.is_some() || sub.else_ability.is_some() {
        return false;
    }
    let Effect::CreateDelayedTrigger {
        condition,
        effect: inner,
        uses_tracked_set,
    } = &*sub.effect
    else {
        return false;
    };
    if *uses_tracked_set {
        return false;
    }
    // CR 603.7a: "at the beginning of the next end step".
    if !matches!(
        condition,
        DelayedTriggerCondition::AtNextPhase { phase: Phase::End }
    ) {
        return false;
    }
    // CR 603.7a: the fold produces an UNCONDITIONAL return — a delayed-trigger
    // body carrying its own condition, else-branch, or continuation would be
    // silently promoted to unconditional if folded, so bail. One tolerated
    // exception: the assembly-pass wrapper lift CLONES (not moves) the outer
    // "if you do" gate, so the inner body legitimately carries a duplicate
    // `is_optional_effect_performed` condition — already enforced on the sub
    // above, hence unconditional once folded.
    if inner.sub_ability.is_some() || inner.else_ability.is_some() {
        return false;
    }
    if !inner
        .condition
        .as_ref()
        .is_none_or(AbilityCondition::is_optional_effect_performed)
    {
        return false;
    }
    // "return it to your hand" — the anaphoric return-to-hand of the spell.
    matches!(
        &*inner.effect,
        Effect::Bounce {
            target: TargetFilter::ParentTarget | TargetFilter::SelfRef,
            destination: None | Some(Zone::Hand),
            ..
        }
    )
}

pub(super) fn rewire_result_anchored_subchain(def: &mut AbilityDefinition) {
    if let Some(sub) = def.sub_ability.as_mut() {
        let sub_is_attach_with_zone_changed_cond = matches!(*sub.effect, Effect::Attach { .. })
            && matches!(
                sub.condition,
                Some(AbilityCondition::ZoneChangedThisWay { .. })
            );
        let parent_moves_to_battlefield = matches!(
            *def.effect,
            Effect::Dig {
                destination: Some(Zone::Battlefield),
                ..
            } | Effect::ChangeZone {
                destination: Zone::Battlefield,
                ..
            } | Effect::Conjure {
                destination: Zone::Battlefield,
                ..
            }
        );
        let attach_anaphor_names_moved_card = parent_moves_to_battlefield
            && rebind_attach_attachment_to_forwarded_source_if_anaphor_names_moved_card(
                &mut sub.effect,
            );
        if parent_moves_to_battlefield
            && (sub_is_attach_with_zone_changed_cond
                || attach_anaphor_names_moved_card
                || sub_targets_moved_card(sub))
        {
            def.forward_result = true;
        }
    }
    if let Some(sub) = def.sub_ability.as_mut() {
        rewire_result_anchored_subchain(sub);
    }
    if let Some(else_branch) = def.else_ability.as_mut() {
        rewire_result_anchored_subchain(else_branch);
    }
}

/// CR 400.7j + CR 608.2c + CR 701.3a: in "<move a card to the battlefield>, then
/// attach it to <referent>", the bare-"it" attachment operand names the card the
/// parent instruction just moved — CR 400.7j lets the rest of that effect find the
/// object it put into a public zone. Encode it as `SelfRef`: the runtime
/// `forward_result` branch rebinds the sub-ability's `source_id` to the moved
/// object, and `change_zone::resolve_forward_result_search_attach_host` gates the
/// pre-entry host stamp on exactly that `SelfRef` encoding.
///
/// Two recipient encodings prove the attachment anaphor is the moved card:
///
/// * `LastCreated` — the recipient is a token this chain created (Ratonhnhaké꞉ton,
///   Forum Filibuster), so the attachment cannot be it.
/// * the SAME filter node as the attachment — the sentence's two referents
///   collapsed onto one anaphor (Sword of the Meek). Attaching an object to
///   itself is a guaranteed no-op whatever the operand resolves to — CR 301.5c
///   ("An Equipment can't equip itself"), CR 301.6 (the same for Fortifications),
///   CR 303.4d ("An Aura can't enchant itself"), and CR 701.3b for anything else
///   — so rebinding can only turn a dead node live; it can never take working
///   behavior away.
///
/// Note the delivery this rebind selects: `SelfRef` also satisfies
/// `change_zone::resolve_forward_result_search_attach_host`'s attachment gate, so
/// the host is stamped as `enter_attached_to` and the card enters already
/// attached. The trailing `Attach` sub still resolves and still emits its
/// `EffectKind::Attach`, so `TriggerMode::Attached` observers are unaffected.
/// It is a CR 301.5c self-attach — the sub's `ParentTarget` host falls back to
/// the source — which `attach::attachment_illegality_projected` rejects before
/// any edit: no state change, no timestamp bump.
///
/// Caller-gated on the parent moving a card to the battlefield. That gate is why
/// this cannot live in `parse_attachment_anaphor` (`imperative.rs`): the parent
/// effect is unknown at parse time, and rebinding there would regress Stonehewer
/// Giant / Quest for the Holy Relic / Armored Skyhunter / Adaptive Armorer, whose
/// "it" names a searched Equipment rather than the source. The same gate keeps the
/// equal-operand pairs under `GainControl` (Ogre Geargrabber, Thieving Skydiver)
/// and under `CopyTokenOf` untouched: no public-zone move, so no CR 400.7j
/// referent to rebind to.
pub(super) fn rebind_attach_attachment_to_forwarded_source_if_anaphor_names_moved_card(
    effect: &mut Effect,
) -> bool {
    let Effect::Attach { attachment, target } = effect else {
        return false;
    };
    // Hoisted so the operand-identity test below can never fire for a
    // `SelfRef`/`SelfRef` pair (Nim Deathmantle, Boonweaver Giant, Hakim,
    // Light-Paws, Magnetic Snuffler, Runed Crown).
    if !matches!(
        attachment,
        TargetFilter::ParentTarget | TargetFilter::TriggeringSource
    ) {
        return false;
    }
    if matches!(target, TargetFilter::LastCreated) || *target == *attachment {
        *attachment = TargetFilter::SelfRef;
        return true;
    }
    false
}

/// CR 608.2c: True when a sub-ability anchors on the just-moved card via
/// the bare-"it" anaphor. Two encodings are recognized:
///
/// - `TargetFilter::SelfRef` — encoded when the anaphor's antecedent is
///   the source itself; the runtime `forward_result` branch rewrites
///   `sub.source_id` to the moved object before resolution, so `SelfRef`
///   resolves to it.
/// - `TargetFilter::ParentTarget` — encoded when the upstream chunk-loop
///   anaphor rewrite (`chain_has_prior_typed_referent` →
///   `replace_target_with_parent`) already redirected the "it" to the
///   parent's chosen-object slot. The parent for this pattern is a
///   `ChangeZone` whose typed target is a compound filter
///   (`And[Typed(<type>), ExiledBySource]`) — a description, not a
///   targeting "target" keyword — so `ability.targets` is empty at
///   resolution time. The runtime `forward_result` branch inserts the
///   moved object into the sub's targets so `ParentTarget` resolves to
///   it.
///
/// Walks the sub's leaf target slot, `GenericEffect`'s grant list
/// (each `StaticDefinition.affected`), `CreateDelayedTrigger`'s inner
/// `AbilityDefinition`, and nested `sub_ability` / `else_ability`.
fn sub_targets_moved_card(sub: &AbilityDefinition) -> bool {
    if matches!(
        sub.effect.target_filter(),
        Some(TargetFilter::SelfRef | TargetFilter::ParentTarget)
    ) {
        return true;
    }
    if let Effect::Conjure { cards, .. } = &*sub.effect {
        if cards.iter().any(|card| {
            matches!(
                &card.source,
                ConjureSource::Duplicate {
                    duplicate_of: TargetFilter::ParentTarget | TargetFilter::SelfRef,
                }
            )
        }) {
            return true;
        }
    }
    if let Effect::GenericEffect {
        static_abilities, ..
    } = &*sub.effect
    {
        if static_abilities.iter().any(|s| {
            matches!(
                s.affected.as_ref(),
                Some(TargetFilter::SelfRef | TargetFilter::ParentTarget)
            )
        }) {
            return true;
        }
    }
    if let Effect::CreateDelayedTrigger { effect, .. } = &*sub.effect {
        if sub_targets_moved_card(effect) {
            return true;
        }
    }
    if let Some(nested) = sub.sub_ability.as_ref() {
        if sub_targets_moved_card(nested) {
            return true;
        }
    }
    if let Some(else_branch) = sub.else_ability.as_ref() {
        if sub_targets_moved_card(else_branch) {
            return true;
        }
    }
    false
}

/// CR 702.33d + CR 608.2c: Resolve "create [N] of those tokens [instead]"
/// anaphoric clauses. The clause refers back to the previous def's token
/// creation effect (either `Token` or `CopyTokenOf`) and reproduces it with
/// a new count. We walk `defs` looking for an `Unimplemented` clause whose
/// description matches the anaphor, and rewrite its effect as a clone of the
/// previous def's effect with the parsed count.
pub(super) fn resolve_those_tokens_anaphors(defs: &mut [AbilityDefinition]) {
    for i in 1..defs.len() {
        let (prev_rest, cur_rest) = defs.split_at_mut(i);
        let prev = &prev_rest[i - 1];
        let cur = &mut cur_rest[0];
        rewrite_those_tokens_from_antecedent(&mut cur.effect, &prev.effect);
    }
}

/// CR 701.60a + CR 608.2c: Resolve the plural population anaphor in
/// "[mass P/T modification to a population]. ... they're no longer suspected"
/// (Eliminate the Impossible: "Creatures your opponents control get -2/-0 ...
/// they're no longer suspected"). The un-suspect body parses to
/// `Unsuspect { ParentTarget, Single }` because "they"/"them" is anaphoric, but
/// the antecedent is a non-targeting `PumpAll` *population* — not an announced
/// target — so `ParentTarget` resolves to nothing. Rebind the un-suspect to the
/// preceding `PumpAll`'s population filter with `All` scope (CR 701.60a removes
/// the designation from every matching permanent). Applying Unsuspect to a
/// non-suspected creature is a no-op, so the redundant "if any of them are
/// suspected" gate the card prints needs no separate condition.
pub(super) fn resolve_populated_unsuspect_anaphors(defs: &mut [AbilityDefinition]) {
    for i in 1..defs.len() {
        let population = match &*defs[i - 1].effect {
            Effect::PumpAll { target, .. } if !matches!(target, TargetFilter::None) => {
                target.clone()
            }
            _ => continue,
        };
        if let Effect::Unsuspect { target, scope } = &mut *defs[i].effect {
            if matches!(target, TargetFilter::ParentTarget) && matches!(scope, EffectScope::Single)
            {
                // CR 701.60a: un-designate every member of the antecedent
                // population.
                *target = population.clone();
                *scope = EffectScope::All;
                // CR 608.2c: represent the printed "if any of them are suspected"
                // gate as an existential over the population restricted to the
                // suspected status. Redundant with the un-suspect no-op, but it
                // makes the condition explicit (and rules-faithful) rather than
                // dropped. `defs[i]` carries no prior condition (the anaphor body
                // parsed conditionless), so this is a pure add.
                if defs[i].condition.is_none() {
                    let mut suspected = population;
                    if let TargetFilter::Typed(typed) = &mut suspected {
                        typed.properties.push(FilterProp::Suspected);
                    }
                    defs[i].condition = Some(AbilityCondition::QuantityCheck {
                        lhs: QuantityExpr::Ref {
                            qty: QuantityRef::ObjectCount { filter: suspected },
                        },
                        comparator: Comparator::GE,
                        rhs: QuantityExpr::Fixed { value: 1 },
                    });
                }
            }
        }
    }
}

/// CR 702.33d + CR 707.10: If `cur` is an `Unimplemented` "create N of those
/// tokens" anaphor, rewrite it as a clone of the `antecedent` token-creation
/// effect with count set to N. No-op when the shapes don't match.
pub(super) fn rewrite_those_tokens_from_antecedent(cur: &mut Effect, antecedent: &Effect) {
    let Some(count) = match_create_of_those_tokens(cur) else {
        return;
    };
    let new_effect = match antecedent {
        Effect::CopyTokenOf {
            target,
            owner,
            enters_attacking,
            tapped,
            extra_keywords,
            additional_modifications,
            ..
        } => Some(Effect::CopyTokenOf {
            target: target.clone(),
            owner: owner.clone(),
            source_filter: None,
            enters_attacking: *enters_attacking,
            tapped: *tapped,
            count: count.clone(),
            extra_keywords: extra_keywords.clone(),
            additional_modifications: additional_modifications.clone(),
        }),
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
            ..
        } => Some(Effect::Token {
            name: name.clone(),
            power: power.clone(),
            toughness: toughness.clone(),
            types: types.clone(),
            colors: colors.clone(),
            keywords: keywords.clone(),
            tapped: *tapped,
            count: count.clone(),
            owner: owner.clone(),
            attach_to: attach_to.clone(),
            enters_attacking: *enters_attacking,
            supertypes: supertypes.clone(),
            static_abilities: static_abilities.clone(),
            enter_with_counters: enter_with_counters.clone(),
        }),
        _ => None,
    };
    if let Some(effect) = new_effect {
        *cur = effect;
    }
}

pub(super) fn rewrite_counter_instead_target_from_antecedent(
    cur: &mut Effect,
    antecedent: &Effect,
) -> bool {
    let Effect::PutCounter {
        target: current_target,
        ..
    } = cur
    else {
        return false;
    };
    if !matches!(current_target, TargetFilter::SelfRef) {
        return false;
    }
    // CR 608.2c + CR 115.1: an instead clause later in the same instruction
    // reuses the original chosen target rather than announcing a new target.
    // Existing attachment-host case — only when the antecedent is itself a `PutCounter`.
    // Preserved verbatim (clone the host filter) so attachment-host cards stay byte-identical.
    if let Effect::PutCounter {
        target: antecedent_target,
        ..
    } = antecedent
    {
        if antecedent_target.contains_source_attachment_host() {
            *current_target = antecedent_target.clone();
            return true;
        }
        match antecedent_target {
            // A printed target is selected once for the root instruction; the
            // override must inherit that selection rather than open a new slot.
            TargetFilter::Typed(_) => *current_target = TargetFilter::ParentTarget,
            // Event and parent anaphors already identify the antecedent object
            // at resolution. Reuse the same reference for a bare "it" override.
            TargetFilter::ParentTarget
            | TargetFilter::ParentTargetSlot { .. }
            | TargetFilter::TriggeringSource => *current_target = antecedent_target.clone(),
            _ => return false,
        }
        return true;
    }
    // FIX A′ — CR 608.2c: an instead-override "Put a +1/+1 counter on it" whose antecedent
    // is a typed-targeted non-counter clause (Throw from the Saddle's "Target creature you
    // control gets +1/+1") anaphors that chosen target (Target1). Bind the override's
    // `SelfRef` counter to `ParentTarget` — a reference to the parent ability's chosen
    // object — NOT a clone of the antecedent's `Typed` filter (which would announce a fresh
    // target). Scoped to `PutCounter{SelfRef}`; demonstrative overrides ("on that creature")
    // are already `ParentTarget` and never reach here.
    if has_typed_target(antecedent) {
        *current_target = TargetFilter::ParentTarget;
        return true;
    }
    false
}

/// Match an `Unimplemented` effect whose description is
/// "create <N> of those tokens" (optionally with a trailing modifier like
/// "that are tapped and attacking" or "instead"). Returns the parsed count.
fn match_create_of_those_tokens(effect: &Effect) -> Option<QuantityExpr> {
    let Effect::Unimplemented { name, description } = effect else {
        return None;
    };
    if name != "create" {
        return None;
    }
    let text = description.as_deref()?;
    let lower = text.to_lowercase();
    let (_, rest) = nom_on_lower(text, &lower, |i| value((), tag("create ")).parse(i))?;
    let rest_lower = rest.to_lowercase();
    let (count, after) = if let Some((_, after)) =
        nom_on_lower(rest, &rest_lower, |i| value((), tag("x ")).parse(i))
    {
        (
            QuantityExpr::Ref {
                qty: QuantityRef::Variable {
                    name: "X".to_string(),
                },
            },
            after,
        )
    } else {
        let (count, after) = crate::parser::oracle_util::parse_number(rest)?;
        (
            QuantityExpr::Fixed {
                value: count as i32,
            },
            after,
        )
    };
    let after = after.trim_start();
    let after_lower = after.to_lowercase();
    let (_, tail) = nom_on_lower(after, &after_lower, |i| {
        value((), tag("of those tokens")).parse(i)
    })?;
    // CR 107.3 / CR 107.3c: when the count is the placeholder X and a trailing
    // ", where X is <expr>" clause defines it, the value of X is defined by the
    // card's own text — bind the count to that clause (e.g. Adipose Offspring's
    // "the sacrificed creature's toughness" → Toughness { CostPaidObject }, The
    // Final Days' "the number of creature cards in your graveyard") rather than
    // to any {X} in the spell's mana cost. Absent the clause, X falls back to
    // the spell's announced {X} (Starnheim Unleashed, Conqueror's Pledge).
    if matches!(
        count,
        QuantityExpr::Ref {
            qty: QuantityRef::Variable { .. }
        }
    ) {
        if let Some(bound) = parse_trailing_where_x_quantity(tail) {
            return Some(bound);
        }
    }
    // Accept end, or a comma/whitespace-prefixed modifier.
    if tail.is_empty() || matches!(tail.chars().next(), Some(' ' | ',' | '.')) {
        Some(count)
    } else {
        None
    }
}

/// CR 107.3c: parse a trailing ", where X is <quantity>" clause into the bound
/// `QuantityExpr` it defines, reusing the shared quantity combinators. Returns
/// `None` when the tail carries no such clause (the count then keeps its prior
/// reading). Event-context quantities ("the sacrificed creature's toughness")
/// are tried before the general CDA quantities so cost-paid-object possessives
/// bind to `ObjectScope::CostPaidObject`.
fn parse_trailing_where_x_quantity(tail: &str) -> Option<QuantityExpr> {
    let lower = tail.to_lowercase();
    // Optional leading separator (", " / " ") then the defining clause keyword,
    // all dispatched via nom combinators.
    let (_, rest) = nom_on_lower(tail, &lower, |i| {
        value((), (opt(tag(",")), multispace0, tag("where x is "))).parse(i)
    })?;
    // Structural trailing-period cleanup on the already clause-delimited
    // quantity text before delegating to the quantity combinators (mirrors
    // `parse_where_x_quantity_expression`).
    let expr = rest.trim().trim_end_matches('.').trim(); // allow-noncombinator: punctuation cleanup, not dispatch
    if expr.is_empty() {
        return None;
    }
    parse_event_context_quantity(expr).or_else(|| parse_cda_quantity(expr))
}

/// CR 611.2a/c + CR 603.7c + CR 111.2 + CR 707.2 + CR 701.36a: Rewrite token
/// anaphors following a token-creating effect.
///
/// Two rewrites, both scoped to defs whose chain contains a prior token
/// creator (`Populate`, `CopyTokenOf`, `Token`):
///
/// 1. `Effect::Unimplemented { description: "<anaphor> <mod>" }`
///    → `GenericEffect { target: Some(LastCreated), static_abilities: [...],
///    duration: Some(Permanent) }` where the modifications are parsed from the
///    verb phrase ("gains haste" / "gets +1/+1" / …). Explicit printed
///    durations are preserved.
///    Recognized anaphor prefixes (longest-first to disambiguate):
///    "the token created this way " / "the tokens created this way "
///    (populate-specific qualifier) and the plain forms "this token " /
///    "that token " / "the token " (covers Pietra, Inalla, and similar
///    token-creators that follow with a generic pronoun rather than the
///    populate-specific phrasing).
///
/// 2. Inside a `CreateDelayedTrigger` whose inner effect references the
///    created token via `TargetFilter::ParentTarget` (currently the
///    imperative parser's "it" / "that creature" default), rewrite that
///    target to `TargetFilter::LastCreated`. At delayed-trigger creation
///    time, `delayed_trigger::resolve` snapshots
///    `state.last_created_token_ids` into the delayed ability's targets.
pub(super) fn resolve_populated_token_anaphors(defs: &mut [AbilityDefinition]) {
    for i in 0..defs.len() {
        let Some(nearest_creator) = defs[..i]
            .iter()
            .rev()
            .find(|d| is_token_creating_effect(&d.effect))
        else {
            continue;
        };
        let token_is_attachable = token_creator_is_attachable(&nearest_creator.effect);
        rewrite_populated_anaphor_in_def(&mut defs[i], token_is_attachable);
    }
}

pub(super) fn is_token_creating_effect(effect: &Effect) -> bool {
    matches!(
        effect,
        Effect::Populate | Effect::Token { .. } | Effect::CopyTokenOf { .. }
    )
}

/// CR 608.2c + CR 701.40a + CR 701.58a + CR 701.62a: Does this clause put a NEW
/// permanent onto the battlefield that a later same-chain anaphor can name?
///
/// A created token and a face-down entry are indistinguishable to the anaphor.
/// Both clauses produce exactly one new permanent and declare NO target, so a
/// following "it" / "that creature" has exactly one possible referent — the
/// thing the clause just made. Manifest (CR 701.40a), manifest dread
/// (CR 701.62a) and cloak (CR 701.58a) all route through the one runtime
/// producer (`game::morph::manifest_card`) and put a 2/2 face-down creature
/// onto the battlefield, exactly as `Effect::Token` puts a token there.
///
/// Keying the chain-referent flag on "token" alone left the face-down producers
/// with no referent, so their anaphor fell through to `ParentTarget` (empty —
/// the producer has no targets) or to the trigger source: Conductive Machete
/// attached to nothing, Weight Room put its counters on the Room (#7531).
pub(super) fn publishes_chain_created_referent(effect: &Effect) -> bool {
    is_token_creating_effect(effect)
        || matches!(
            effect,
            Effect::Manifest { .. } | Effect::ManifestDread | Effect::Cloak { .. }
        )
}

/// CR 603.12 + CR 609.3: Re-link a clause that READS the just-created-token
/// referent published by a clause under an AFFIRMATIVE reflexive gate.
///
/// A reflexive gate ("When you do", "If you do") means the antecedent may not
/// have happened, in which case its clause created no token. A following clause
/// whose only subject is that token ("Put a +1/+1 counter on that token") is
/// then not the next independent instruction (CR 608.2c) — it is an instruction
/// that can do nothing at all (CR 609.3: an effect does only as much as
/// possible). Tagging it `SequentialSibling` makes the resolver's
/// condition-false descent resolve it anyway, and `TargetFilter::LastCreated`
/// resolves against `state.last_created_token_ids`, a GAME-LIFETIME ledger that
/// is never cleared at a resolution boundary — so it would bind a token from an
/// EARLIER resolution.
///
/// Re-linking to `ContinuationStep` makes the clause a resolution step of the
/// instruction it is already attached to — and `gated_instruction_reaches`
/// restricts the pass to the case where that instruction is the gated
/// publisher's own, which is what the printed text means: it resolves when the
/// gate is true and is skipped when it is false. Because the whole clause moves,
/// every referent-reading position inside it moves with it — there is no
/// per-effect read-position list to keep in sync.
///
/// Deliberately narrow: only a clause that reads the referent the gated clause
/// PUBLISHES, and that is not separated from it by an independent instruction,
/// is re-linked. A genuinely independent tail after a reflexive gate still
/// resolves (Springheart Nantuko's "If you didn't create a token this way"
/// complement; Scion of the Ur-Dragon's "Then shuffle", CR 701.23 + CR 701.24;
/// Localized Destruction's "Destroy all creatures").
///
/// This pass and the referent walk that seeds the binding
/// (`parser::oracle_effect::chain_prior_referent_is_created_token`) are two
/// halves of one rule and must not diverge: the walk predicts THIS pass's
/// acceptance before assembly, from the same two authorities —
/// `oracle_ir::ast::sub_link_after_boundary` and
/// [`instruction_spine_is_continuation`] — so a seed cannot land on a shape this
/// pass declines for a reason either authority can see. Widening either half
/// without the other re-opens the stale-`LastCreated` bind. The prediction's
/// blind spot (assembly-time `SequentialSibling` minters) is enumerated on
/// [`instruction_spine_is_continuation`].
///
/// The producer half is [`publishes_chain_created_referent`] — the SAME
/// predicate `chain_prior_referent_is_created_token` seeds `LastCreated` from,
/// and the same one `clone_would_transplant_gated_referent` re-asks. Three
/// passes, one question: a producer that can seed the referent must also be
/// able to relink its consumer, or a gated face-down producer seeds
/// `LastCreated` and then leaves the consumer a `SequentialSibling` that reads
/// the game-lifetime ledger when the gate is false.
pub(super) fn relink_gated_token_referent_consumers(defs: &mut [AbilityDefinition]) {
    for i in 0..defs.len() {
        let Some(publisher) = defs[..i]
            .iter()
            .rposition(|d| publishes_chain_created_referent(&d.effect))
        else {
            continue;
        };
        if !defs[publisher]
            .condition
            .as_ref()
            .is_some_and(AbilityCondition::is_affirmative_reflexive_gate)
        {
            continue;
        }
        if !gated_instruction_reaches(&defs[publisher..i]) {
            continue;
        }
        if defs[i].sub_link == SubAbilityLink::SequentialSibling
            && ability_reads_last_created(&defs[i])
        {
            defs[i].sub_link = SubAbilityLink::ContinuationStep;
        }
    }
}

/// CR 608.2c: Is the clause following `slice` still inside the gated
/// publisher's own instruction?
///
/// `slice[0]` is the gated publisher; the remaining entries are the clauses
/// between it and the candidate consumer. `sub_link` describes the link to the
/// IMMEDIATELY preceding node, not to the publisher, and the resolver's
/// condition-false descent (`game::effects::resolve_ability_chain`) walks
/// `sub_ability` from the gated node and resolves the FIRST node whose
/// `sub_link` is `SequentialSibling` — together with that node's entire
/// sub-chain. So re-tagging a consumer that sits behind an intervening
/// `SequentialSibling` would only make it a continuation step of THAT sibling:
/// the descent selects the sibling and resolves the consumer anyway, changing
/// the link for nothing. Requiring an unbroken continuation path is what makes
/// the re-tag mean what `SubAbilityLink::ContinuationStep` says it means.
///
/// Each node's own within-clause spine is checked too, via
/// [`instruction_spine_is_continuation`], because the chain assembler appends the
/// next clause to the DEEPEST `sub_ability`, so an internal `SequentialSibling`
/// rider also sits on the descent path.
fn gated_instruction_reaches(slice: &[AbilityDefinition]) -> bool {
    slice.iter().enumerate().all(|(idx, def)| {
        (idx == 0 || def.sub_link == SubAbilityLink::ContinuationStep)
            && instruction_spine_is_continuation(def)
    })
}

/// CR 608.2c: Is every node of this definition's own within-clause spine a
/// `ContinuationStep`?
///
/// Shared by the two passes that must agree on "an unbroken continuation path
/// runs from the gated publisher to the consumer": `gated_instruction_reaches`
/// (above, over assembled `AbilityDefinition`s) and the referent walk
/// `parser::oracle_effect::chain_prior_referent_is_created_token` (over
/// `ClauseIr::parsed.sub_ability`, which is the same `AbilityDefinition` spine
/// before assembly appends the following clause to its deepest node).
///
/// Not vacuous even though no shipped card exercises it today: PARSE-TIME
/// builders mint an internal `SequentialSibling` rider directly and hand it back
/// inside a `ParsedEffectClause` — [`try_parse_bidirectional_prevent`] here and
/// `oracle_effect::mod::try_parse_exile_play_grant_with_play_prohibition` — so
/// such a spine can reach both callers.
///
/// SCOPE, stated so the seeder's use of it is not read as a proof: this sees the
/// parse-time spine only. Three ASSEMBLY-time sites mint a `SequentialSibling`
/// that no `ClauseIr` carries and that the referent walk therefore cannot
/// predict. Each would make `gated_instruction_reaches` stricter than the walk
/// predicted, i.e. leave a `LastCreated` bind the re-link does not protect:
///
/// * [`attach_graveyard_redirect_rider_to_prior_cast_from_zone`] and
///   `absorb_last_created_riders` — each needs an `Effect::CastFromZone` /
///   `Effect::FlipCoins` antecedent, and the second MOVES its rider inside the
///   coin effect, off the top level entirely.
/// * `oracle_effect::mod::attach_repeat_process_keywords`, which pushes cloned
///   TOP-LEVEL siblings rather than a within-clause rider, and clones the
///   template's target VERBATIM. Closed at its binding site: `assembly.rs`
///   declines the binding when [`clone_would_transplant_gated_referent`] holds,
///   and that predicate decides by running THIS pass over the def vector the
///   clone would land in. So every clone that exists is one this pass either
///   re-tagged onto the gated instruction's continuation path or found honest
///   on its own (self-gated, or reading no gated referent at all).
///
/// The backstop for all three is the invariant "no `SequentialSibling` node
/// reads `TargetFilter::LastCreated`". Two tests carry it, and NEITHER is a
/// corpus sweep — read them for what they cover before relying on them:
/// `bbfu9_no_stale_last_created_bind` asserts it over a FROZEN list of the 20
/// cards whose AST this change moved, embedded verbatim (it cannot see a card
/// that acquires the shape later), and
/// `repeat_process_directive_never_joins_a_continuation_path` asserts it over
/// the repeat-process grammar's own fixtures.
pub(super) fn instruction_spine_is_continuation(def: &AbilityDefinition) -> bool {
    let mut cursor = def.sub_ability.as_deref();
    while let Some(node) = cursor {
        if node.sub_link != SubAbilityLink::ContinuationStep {
            return false;
        }
        cursor = node.sub_ability.as_deref();
    }
    true
}

/// CR 111.1: Does this ability (or anything nested inside it) read the
/// just-created-token referent `TargetFilter::LastCreated`? Walks the whole
/// definition — target filter (including composite wrappers), `GenericEffect`
/// grant recipients, a `CreateDelayedTrigger`'s inner definition, modal modes,
/// and the within-clause sub/else chain — so the answer does not depend on an
/// enumeration of which `Effect` variants can carry the referent.
fn ability_reads_last_created(def: &AbilityDefinition) -> bool {
    fn filter_reads(filter: &TargetFilter) -> bool {
        match filter {
            TargetFilter::LastCreated => true,
            TargetFilter::And { filters } | TargetFilter::Or { filters } => {
                filters.iter().any(filter_reads)
            }
            TargetFilter::Not { filter } | TargetFilter::TrackedSetFiltered { filter, .. } => {
                filter_reads(filter)
            }
            TargetFilter::ChosenDamageSource { filter } => {
                filter.as_deref().is_some_and(filter_reads)
            }
            TargetFilter::None
            | TargetFilter::Any
            | TargetFilter::Player
            | TargetFilter::Controller
            | TargetFilter::SourceController
            | TargetFilter::ControllerAndControlledPermanents { .. }
            | TargetFilter::Opponent
            | TargetFilter::SelfRef
            | TargetFilter::GrantingObject
            | TargetFilter::SourceOrPaired
            | TargetFilter::Typed(..)
            | TargetFilter::StackAbility { .. }
            | TargetFilter::StackSpell
            | TargetFilter::SpecificObject { .. }
            | TargetFilter::SpecificPlayer { .. }
            | TargetFilter::PlayerWhoChoseLabel { .. }
            | TargetFilter::Neighbor { .. }
            | TargetFilter::ScopedPlayer
            | TargetFilter::AttachedTo
            | TargetFilter::LastRevealed
            | TargetFilter::LastZoneChanged
            | TargetFilter::CostPaidObject
            | TargetFilter::ChosenCard
            | TargetFilter::TrackedSet { .. }
            | TargetFilter::ExiledBySource
            | TargetFilter::ExiledCardByIndex { .. }
            | TargetFilter::TriggeringSpellController
            | TargetFilter::TriggeringSpellOwner
            | TargetFilter::TriggeringPlayer
            | TargetFilter::TriggeringSource
            | TargetFilter::EventTarget
            | TargetFilter::TriggeringSourceController
            | TargetFilter::ParentTarget
            | TargetFilter::ParentTargetSlot { .. }
            | TargetFilter::ParentTargetController
            | TargetFilter::ParentTargetOwner
            | TargetFilter::SourceChosenPlayer
            | TargetFilter::OriginalController
            | TargetFilter::OriginalSource
            | TargetFilter::PostReplacementSourceController
            | TargetFilter::PostReplacementDamageSource
            | TargetFilter::PostReplacementDamageTarget
            | TargetFilter::PostReplacementDamageTargetOwner
            | TargetFilter::DefendingPlayer
            | TargetFilter::HasChosenName
            | TargetFilter::Named { .. }
            | TargetFilter::Owner
            | TargetFilter::AllPlayers => false,
        }
    }
    if def.effect.target_filter().is_some_and(filter_reads) {
        return true;
    }
    match &*def.effect {
        Effect::CreateDelayedTrigger { effect, .. } if ability_reads_last_created(effect) => {
            return true;
        }
        Effect::GenericEffect {
            static_abilities, ..
        } if static_abilities
            .iter()
            .any(|s| s.affected.as_ref().is_some_and(filter_reads)) =>
        {
            return true;
        }
        _ => {}
    }
    def.sub_ability
        .as_deref()
        .is_some_and(ability_reads_last_created)
        || def
            .else_ability
            .as_deref()
            .is_some_and(ability_reads_last_created)
        || def.mode_abilities.iter().any(ability_reads_last_created)
}

/// CR 603.12: Would replicating `defs[template]` at the TAIL of `defs`
/// transplant a gated publisher's just-created-token referent to a slot the
/// resolver can reach without that token?
///
/// `oracle_effect::mod::attach_repeat_process_keywords` ("Repeat this process
/// for …") clones its template VERBATIM — target included — and pushes the
/// clones at the end of `defs`. That is position-independent unless the template
/// reads `TargetFilter::LastCreated`, which is a CHAIN-CONTEXT referent:
/// [`relink_gated_token_referent_consumers`] keeps such a read honest only while
/// an unbroken continuation path runs from the gated clause that published it,
/// and a clone landing off that path keeps `SubAbilityLink::SequentialSibling`.
/// The resolver's condition-false descent then resolves the clone anyway, and
/// `state.last_created_token_ids` is a game-lifetime ledger — so on a false gate
/// it binds a token from an EARLIER resolution.
///
/// Two early returns bound the question, and neither is a guess about position:
///
/// * a template that reads no `LastCreated` carries no chain-context referent at
///   all — nothing to transplant, wherever the clone lands;
/// * an UNGATED nearest publisher creates its token unconditionally during this
///   resolution, so the read is live at any position — that is BASE behaviour
///   and not a hazard.
///
/// The remaining question — "is the clone honest where it lands?" — is not
/// re-derived here. It is ASKED, by building the def
/// [`super::attach_repeat_process_keywords`] will push
/// ([`super::repeat_process_clone_shape`], the shared authority for that shape)
/// and running [`relink_gated_token_referent_consumers`] over the result. The
/// clone is honest if either answer comes back yes:
///
/// * the re-link re-tags it `ContinuationStep`, so it is a resolution step of
///   the gated instruction and the condition-false descent never selects it; or
/// * it is SELF-GATED — [`AbilityDefinition::is_self_gated_reflexive`] — so the
///   descent's own false-condition skip drops it wherever it sits.
///
/// Running the pass rather than predicting it is what makes the answer exact.
/// The prediction has to model the pass's ORDER (the template is re-tagged
/// before the clone is examined, so a `Sentence`-joined template that is still
/// `SequentialSibling` at this point must be treated as if it were not) and the
/// pass's choice of publisher for the clone (a LATER token creator becomes the
/// clone's own nearest publisher). Both were hand-modelled before and both are
/// now simply what the pass does.
///
/// The probe is not byte-for-byte the finished chain, in two ways, and neither
/// changes the answer — measured on purpose-built fixtures, not argued:
///
/// * the probe DEF: the clone the caller actually pushes differs only in its
///   `counter_type` and in keyword payloads rewritten inside `QuantityCheck` /
///   `TargetHasKeywordInstead` / `SourceLacksKeyword`
///   (`super::rewrite_ability_condition_keyword`) — no field either answer reads.
/// * the probe VECTOR: later passes APPEND defs after this binding runs, so the
///   vector here is a PREFIX of the finished chain (a fixture ending "… Repeat
///   this process for first strike. Then create a Soldier token." is examined at
///   4 defs and finishes at 6), and the caller pushes one clone per listed
///   keyword where this pushes one. Both are benign: the re-link's verdict for a
///   node is a function of the defs BEFORE it, which appends leave index-stable,
///   and every clone is a copy of the same template landing consecutively on the
///   same path — two-keyword fixtures emit or decline both clones together,
///   never split.
pub(super) fn clone_would_transplant_gated_referent(
    defs: &[AbilityDefinition],
    template: usize,
) -> bool {
    if !ability_reads_last_created(&defs[template]) {
        return false;
    }
    let Some(publisher) = defs[..template]
        .iter()
        .rposition(|d| publishes_chain_created_referent(&d.effect))
    else {
        return false;
    };
    if !defs[publisher]
        .condition
        .as_ref()
        .is_some_and(AbilityCondition::is_affirmative_reflexive_gate)
    {
        return false;
    }
    let mut probe = defs.to_vec();
    probe.push(super::repeat_process_clone_shape(&defs[template]));
    relink_gated_token_referent_consumers(&mut probe);
    let landed = probe.last().expect("pushed just above");
    landed.sub_link == SubAbilityLink::SequentialSibling && !landed.is_self_gated_reflexive()
}

/// CR 301.5 + CR 303.4: True when the nearest preceding token creator makes
/// an Equipment or Aura token. Used to prefer the `attachment` slot for the
/// post-token anaphor rewrite (`rewrite_parent_target_to_last_created`): in
/// U.S.Agent, John Walker's "create ... Equipment ... token ... Attach it to
/// ~", the created token is the attachment. If that slot is explicit, the
/// target-side anaphor remains eligible for the normal fallback rewrite.
fn token_creator_is_attachable(effect: &Effect) -> bool {
    matches!(
        effect,
        Effect::Token { types, .. }
            if types.iter().any(|t| t.eq_ignore_ascii_case("Equipment") || t.eq_ignore_ascii_case("Aura"))
    )
}

/// True for the misbound recipient of a post-token-creation grant: the bare
/// pronoun "it" (`SelfRef`, the imperative default) or the plural "those
/// tokens" (`TrackedSet` sentinel `id 0`, which the plural-anaphor path emits
/// assuming a published set). Neither is correct after a token creator, which
/// publishes `last_created_token_ids` (→ `LastCreated`), not a tracked set —
/// so both must be rebound. A concretely-numbered `TrackedSet` (a real prior
/// published set) is left untouched.
fn is_post_token_misbound_grant_recipient(filter: &TargetFilter) -> bool {
    matches!(
        filter,
        TargetFilter::SelfRef
            | TargetFilter::TrackedSet {
                id: crate::types::identifiers::TrackedSetId(0),
            }
    )
}

/// Rebind a `GenericEffect` grant's misbound recipient(s) to `LastCreated`.
/// Handles both the singular bare-"it" (`SelfRef`) grant ("create a token …
/// It gains haste") and the plural "those tokens gain haste" grant whose
/// recipient the plural-anaphor path defaulted to the `TrackedSet(0)` sentinel
/// (Mirror March #5966). No-op for any other effect, and for grants that
/// already name a concrete recipient — only the source-defaulted references are
/// the misbound anaphor.
fn rebind_self_ref_grant_to_last_created(effect: &mut Effect) {
    let Effect::GenericEffect {
        static_abilities,
        target,
        duration,
        ..
    } = effect
    else {
        return;
    };
    let mut rebound = false;
    for static_def in static_abilities.iter_mut() {
        if static_def
            .affected
            .as_ref()
            .is_some_and(is_post_token_misbound_grant_recipient)
        {
            static_def.affected = Some(TargetFilter::LastCreated);
            rebound = true;
        }
    }
    if rebound
        && (target.is_none()
            || target
                .as_ref()
                .is_some_and(is_post_token_misbound_grant_recipient))
    {
        *target = Some(TargetFilter::LastCreated);
    }
    if rebound && duration.is_none() {
        *duration = Some(Duration::Permanent);
    }
}

/// Walk an ability definition, rewriting the populated-token anaphor at
/// whichever level it appears. Recurses into `CreateDelayedTrigger.effect` so
/// the "sacrifice it" pattern inside a delayed trigger also rewrites.
fn rewrite_populated_anaphor_in_def(def: &mut AbilityDefinition, token_is_attachable: bool) {
    if let Some(new_effect) =
        rewrite_token_created_this_way_unimplemented(&def.effect, def.duration.clone())
    {
        *def.effect = new_effect;
        def.duration = None;
        return;
    }

    rewrite_populated_anaphor_in_effect(&mut def.effect, token_is_attachable);
    // CR 608.2c + CR 701.36a: recurse into sub_ability chains so anaphoric
    // rewrites apply to sibling followups (Fractal Harness PutCounter/Attach).
    if let Some(sub) = def.sub_ability.as_mut() {
        rewrite_populated_anaphor_in_def(sub, token_is_attachable);
    }
}

/// CR 111.3 + CR 702.6a: Intrinsic token statics (Equipment tokens with Equip,
/// Urza's Saga Construct-style explicit permanent grants) belong on the token's
/// own `static_abilities`. Transient resolution-time grants — keyword pumps and
/// `GrantTrigger` installs such as Rite of the Raging Storm (#3297) — must
/// remain sibling `GenericEffect`s targeting `LastCreated`.
fn token_it_has_grant_should_fold_into_statics(
    token_effect: &Effect,
    static_abilities: &[StaticDefinition],
    duration: &Option<Duration>,
) -> bool {
    if static_abilities.iter().any(|static_def| {
        static_def
            .modifications
            .iter()
            .any(|m| matches!(m, ContinuousModification::GrantTrigger { .. }))
    }) {
        return false;
    }

    if matches!(duration, Some(Duration::Permanent)) {
        return true;
    }

    matches!(
        token_effect,
        Effect::Token { types, .. }
            if types
                .iter()
                .any(|t| t.eq_ignore_ascii_case("equipment"))
    )
}

pub(super) fn fold_token_it_has_grants_into_token_statics(def: &mut AbilityDefinition) {
    if !matches!(&*def.effect, Effect::Token { .. }) {
        return;
    }
    let Some(grant_box) = def.sub_ability.take() else {
        return;
    };
    let grant = *grant_box;
    if grant.sub_link != SubAbilityLink::SequentialSibling {
        def.sub_ability = Some(Box::new(grant));
        return;
    }
    // CR 116.2c: `end_cost: None` is a REQUIREMENT of the fold, not a wildcard.
    // Folding a grant's statics into the token clause would discard the grant's
    // own effect node — and with it any pay-to-end permission riding on it. A
    // grant that carries one falls to the non-folding branch below, keeping the
    // permission attached to the effect that installs it.
    let Effect::GenericEffect {
        static_abilities,
        duration,
        target,
        end_cost: None,
    } = grant.effect.as_ref()
    else {
        def.sub_ability = Some(Box::new(grant));
        return;
    };
    let token_scoped = target.as_ref().is_none_or(|t| {
        matches!(
            t,
            TargetFilter::LastCreated | TargetFilter::ParentTarget | TargetFilter::SelfRef
        )
    });
    if !token_scoped
        || !token_it_has_grant_should_fold_into_statics(
            def.effect.as_ref(),
            static_abilities,
            duration,
        )
    {
        def.sub_ability = Some(Box::new(grant));
        return;
    }

    if let Effect::Token {
        static_abilities: token_statics,
        ..
    } = &mut *def.effect
    {
        for mut static_def in static_abilities.clone() {
            if matches!(
                static_def.affected,
                Some(
                    TargetFilter::LastCreated | TargetFilter::ParentTarget | TargetFilter::SelfRef
                )
            ) {
                static_def.affected = Some(TargetFilter::SelfRef);
            }
            token_statics.push(static_def);
        }
    }

    def.sub_ability = grant.sub_ability;
}

/// Walk an effect, rewriting the populated-token anaphor at whichever level
/// it appears. Recurses into `CreateDelayedTrigger.effect` so the "sacrifice
/// it" pattern inside a delayed trigger also rewrites.
fn rewrite_populated_anaphor_in_effect(effect: &mut Effect, token_is_attachable: bool) {
    // Case 1: bare Unimplemented anaphor at the top level (e.g., "the token
    // created this way gains haste").
    if let Some(new_effect) = rewrite_token_created_this_way_unimplemented(effect, None) {
        *effect = new_effect;
        return;
    }

    // Case 2: CreateDelayedTrigger whose inner ability references the token
    // via ParentTarget. Rewrite to LastCreated and recurse into the inner
    // effect for any nested anaphors.
    if let Effect::CreateDelayedTrigger { effect: inner, .. } = effect {
        rewrite_parent_target_to_last_created(&mut inner.effect, token_is_attachable);
        // CR 603.7c + CR 608.2c (issue #4601): a PHASE-triggered token-copier
        // (Mishra, Eminent One — "At the beginning of combat on your turn,
        // create a token …, Sacrifice it at the beginning of the next end step")
        // has no triggering object, so the bare-"it" delayed cleanup lowers to
        // `SelfRef` (the source) rather than `ParentTarget`/`TriggeringSource`.
        // In this gated post-token scope the antecedent is the created token.
        rewrite_delayed_cleanup_self_ref_to_last_created(&mut inner.effect);
        rewrite_populated_anaphor_in_effect(&mut inner.effect, token_is_attachable);
    }

    // Case 3: a bare "it gains/gets X" grant that parsed to a `GenericEffect`
    // targeting `SelfRef` (the imperative parser's default for the bare pronoun
    // "it") — directly after a token-creating effect, "it" is the created token
    // (God-Pharaoh's Gift: "create a token … It gains haste"). Rebind to the
    // just-created token.
    rebind_self_ref_grant_to_last_created(effect);

    // Case 4 (CR 301.5b + CR 122.6a): imperative followups like Fractal Harness's
    // "attach this Equipment to it" parse "it" as ParentTarget (Self-ETB trigger
    // subject). After a token creator in the same chain, rewrite to LastCreated —
    // on the `attachment` slot when the created token is itself an Equipment/Aura
    // (U.S.Agent, John Walker: "Attach it to ~"), or the `target` slot otherwise.
    rewrite_parent_target_to_last_created(effect, token_is_attachable);
}

/// If `effect` is `Unimplemented { description: "<anaphor> <verb-phrase>" }`,
/// try to parse the verb phrase as a continuous modification set and return
/// a replacement `GenericEffect`. Returns `None` when the shape doesn't
/// match so the caller leaves the effect untouched.
///
/// CR 611.2c + CR 603.7c: Recognized anaphor prefixes resolve to the
/// just-created token via `TargetFilter::LastCreated`. The longer
/// populate-specific phrases ("the token(s) created this way ") MUST be
/// tried before the plain "the token " prefix to avoid the latter
/// shadowing the qualified forms when both could match.
pub(crate) fn rewrite_token_created_this_way_unimplemented(
    effect: &Effect,
    clause_duration: Option<Duration>,
) -> Option<Effect> {
    let Effect::Unimplemented { description, .. } = effect else {
        return None;
    };
    let text = description.as_deref()?;
    let lower = text.to_lowercase();
    // Anaphor prefixes — longest-first so "the token created this way "
    // wins over the bare "the token " when both could match. Plain forms
    // ("this/that/the token ") cover token-creators (Pietra, Inalla,
    // Ghired) that refer to the just-created token without the
    // populate-specific qualifier.
    let mut anaphor = alt((
        tag::<&str, &str, ()>("the token created this way "),
        tag("the tokens created this way "),
        tag("this token "),
        tag("that token "),
        tag("the tokens "),
        tag("the token "),
    ));
    let (rest, _matched) = anaphor.parse(lower.as_str()).ok()?;
    let (mod_text, duration) = strip_trailing_duration(rest.trim());
    let mods = crate::parser::oracle_static::parse_continuous_modifications(mod_text);
    if mods.is_empty() {
        return None;
    }
    let static_def = StaticDefinition::continuous()
        .affected(TargetFilter::LastCreated)
        .modifications(mods)
        .description(text.to_string());
    Some(Effect::GenericEffect {
        static_abilities: vec![static_def],
        duration: duration.or(clause_duration).or(Some(Duration::Permanent)),
        target: Some(TargetFilter::LastCreated),
        end_cost: None,
    })
}

/// CR 608.2c + CR 701.20: True when this effect publishes a revealed or
/// zone-changed subject at resolution — i.e. it populates the
/// `last_revealed_ids` / `last_zone_changed_ids` trackers that
/// `AbilityCondition::RevealedHasCardType` reads. When a prior clause in a
/// chain is such a publisher, a following "if it's a [type]" gate refers to
/// THAT card (Goblin Guide: reveal-then-conditional-recall), so the
/// `RevealedHasCardType` reading is correct and must not be rewritten to a
/// `TargetMatchesFilter` parent-target reading. Reveal-class effects populate
/// `last_revealed_ids` directly; zone-change-class effects emit `ZoneChanged`
/// events that populate `last_zone_changed_ids`.
pub(super) fn effect_publishes_revealed_subject(effect: &Effect) -> bool {
    matches!(
        effect,
        // Reveal-class (populate last_revealed_ids).
        Effect::Reveal { .. }
            | Effect::RevealTop { .. }
            | Effect::RevealHand { .. }
            | Effect::Dig { .. }
            | Effect::ExileFromTopUntil { .. }
            | Effect::Clash
            | Effect::TurnFaceUp { .. }
            // Zone-change-class (emit ZoneChanged → last_zone_changed_ids).
            | Effect::ChangeZone { .. }
            | Effect::ChangeZoneAll { .. }
            | Effect::ExileTop { .. }
            | Effect::Mill { .. }
            | Effect::SearchLibrary { .. }
    )
}

/// Rewrite any `TargetFilter::ParentTarget` sitting in the target slot of
/// an effect to `TargetFilter::LastCreated`. This is the runtime bridge for
/// "sacrifice it at the beginning of the next end step" (Determined
/// Iteration) and related delayed-trigger anaphors: the imperative parser
/// emits ParentTarget for bare "it", but in the populate context the
/// antecedent is the newly created token, not a parent ability's target.
///
/// CR 608.2k: Scope is narrow — this runs only inside the inner effect of a
/// `CreateDelayedTrigger` whose enclosing chain contains a token-creating
/// effect. Within that scope, `ParentTarget` reflects the imperative
/// parser's bare-pronoun fallback ("sacrifice it" / "exile it" / …) rather
/// than a real parent target slot, so rewriting to `LastCreated` is safe.
/// `ChangeZone` is included because Inalla-style "Exile it at the beginning
/// of the next end step" lowers to `ChangeZone { destination: Exile,
/// target: ParentTarget }`.
fn definition_contains_choose_damage_source(def: &AbilityDefinition) -> bool {
    if matches!(&*def.effect, Effect::ChooseDamageSource { .. }) {
        return true;
    }
    def.sub_ability
        .as_deref()
        .is_some_and(definition_contains_choose_damage_source)
        || def
            .else_ability
            .as_deref()
            .is_some_and(definition_contains_choose_damage_source)
}

/// CR 609.7a + CR 608.2c (#5601): A resolution chain that chose a damage source
/// but whose head `Effect::ChooseDamageSource` was flattened away during
/// lowering still leaves a bound `ChosenDamageSource` anaphor in whichever
/// branch spelled the source out. Desperate Gambit ("Choose a source you
/// control and flip a coin. … the next time *that source* would deal damage …,
/// it deals double … . … the next time *it* would deal damage …, prevent that
/// damage.") lowers the head choice to a bare target selection, so the win
/// branch's `CreateDamageReplacement { source_filter: ChosenDamageSource }` is
/// the only surviving marker of the chosen-source context. That surviving
/// binding is the signal that a *sibling* bare-"it" prevention/replacement in
/// the SAME chain co-refers with the chosen source (the two "it"s share one
/// antecedent, CR 608.2c) and must be threaded too. Detecting it lets the
/// existing `SelfRef` → `ChosenDamageSource` rewrite fire even though the head
/// no longer matches [`definition_contains_choose_damage_source`].
fn definition_contains_chosen_damage_source_binding(def: &AbilityDefinition) -> bool {
    fn effect_binds_chosen(effect: &Effect) -> bool {
        match effect {
            Effect::CreateDamageReplacement { source_filter, .. } => {
                matches!(source_filter, Some(TargetFilter::ChosenDamageSource { .. }))
            }
            Effect::PreventDamage {
                damage_source_filter,
                ..
            } => matches!(
                damage_source_filter,
                Some(TargetFilter::ChosenDamageSource { .. })
            ),
            Effect::FlipCoin {
                win_effect,
                lose_effect,
                ..
            } => {
                win_effect
                    .as_deref()
                    .is_some_and(definition_contains_chosen_damage_source_binding)
                    || lose_effect
                        .as_deref()
                        .is_some_and(definition_contains_chosen_damage_source_binding)
            }
            _ => false,
        }
    }
    effect_binds_chosen(&def.effect)
        || def
            .sub_ability
            .as_deref()
            .is_some_and(definition_contains_chosen_damage_source_binding)
        || def
            .else_ability
            .as_deref()
            .is_some_and(definition_contains_chosen_damage_source_binding)
}

/// CR 609.7a + CR 608.2c: When a resolution chain begins with
/// `ChooseDamageSource`, bare "it" in a coin-flip one-shot prevention branch
/// co-refers with the chosen source — rewrite `SelfRef` to `ChosenDamageSource`.
fn rewrite_oneshot_selfref_to_chosen_in_effect(effect: &mut Effect) {
    match effect {
        Effect::PreventDamage {
            damage_source_filter,
            ..
        } if matches!(damage_source_filter, Some(TargetFilter::SelfRef)) => {
            *damage_source_filter = Some(TargetFilter::ChosenDamageSource { filter: None });
        }
        Effect::CreateDamageReplacement { source_filter, .. }
            if matches!(source_filter, Some(TargetFilter::SelfRef)) =>
        {
            *source_filter = Some(TargetFilter::ChosenDamageSource { filter: None });
        }
        Effect::FlipCoin {
            win_effect,
            lose_effect,
            ..
        } => {
            if let Some(win) = win_effect.as_deref_mut() {
                rewrite_oneshot_selfref_to_chosen_in_def(win);
            }
            if let Some(lose) = lose_effect.as_deref_mut() {
                rewrite_oneshot_selfref_to_chosen_in_def(lose);
            }
        }
        _ => {}
    }
}

fn rewrite_oneshot_selfref_to_chosen_in_def(def: &mut AbilityDefinition) {
    rewrite_oneshot_selfref_to_chosen_in_effect(&mut def.effect);
    if let Some(sub) = def.sub_ability.as_deref_mut() {
        rewrite_oneshot_selfref_to_chosen_in_def(sub);
    }
    if let Some(else_def) = def.else_ability.as_deref_mut() {
        rewrite_oneshot_selfref_to_chosen_in_def(else_def);
    }
}

pub(super) fn thread_chosen_damage_source_into_oneshot_effects(defs: &mut [AbilityDefinition]) {
    // CR 609.7a (#5601): fire when either the head `ChooseDamageSource` survives
    // OR a `ChosenDamageSource` anaphor was already bound in a branch (the head
    // was flattened during lowering, Desperate Gambit) — both prove the chain
    // chose a damage source, so a sibling bare-"it" `SelfRef` must be threaded.
    if !defs.iter().any(|def| {
        definition_contains_choose_damage_source(def)
            || definition_contains_chosen_damage_source_binding(def)
    }) {
        return;
    }
    for def in defs.iter_mut() {
        rewrite_oneshot_selfref_to_chosen_in_effect(&mut def.effect);
        if let Some(sub) = def.sub_ability.as_deref_mut() {
            rewrite_oneshot_selfref_to_chosen_in_def(sub);
        }
        if let Some(else_def) = def.else_ability.as_deref_mut() {
            rewrite_oneshot_selfref_to_chosen_in_def(else_def);
        }
    }
}

pub(super) fn rewrite_parent_target_to_last_created(
    effect: &mut Effect,
    token_is_attachable: bool,
) {
    match effect {
        Effect::GenericEffect {
            static_abilities,
            target,
            duration,
            ..
        } => {
            // CR 608.2c + CR 611.2c: In token-creator followups ("that token
            // gains haste"), the GenericEffect's application authority is the
            // just-created token set, not the parent copied/source object.
            // Rewrite both possible application slots because the runtime
            // intentionally prefers a StaticDefinition's inherited `affected`
            // reference over the outer GenericEffect target.
            let mut rebound = false;
            for static_def in static_abilities {
                if matches!(
                    static_def.affected,
                    Some(
                        TargetFilter::ParentTarget
                            | TargetFilter::SelfRef
                            | TargetFilter::TriggeringSource
                            | TargetFilter::LastCreated
                    )
                ) {
                    static_def.affected = Some(TargetFilter::LastCreated);
                    rebound = true;
                }
            }
            if matches!(
                target,
                Some(
                    TargetFilter::ParentTarget
                        | TargetFilter::SelfRef
                        | TargetFilter::TriggeringSource
                        | TargetFilter::LastCreated
                )
            ) {
                *target = Some(TargetFilter::LastCreated);
                rebound = true;
            } else if rebound && target.is_none() {
                *target = Some(TargetFilter::LastCreated);
            }
            if rebound && duration.is_none() {
                *duration = Some(Duration::Permanent);
            }
        }
        Effect::Attach {
            attachment,
            target,
        } => {
            if token_is_attachable
                && matches!(
                    attachment,
                    TargetFilter::ParentTarget | TargetFilter::TriggeringSource
                )
            {
                // CR 301.5 + CR 303.4: the just-created token is itself an
                // Equipment/Aura, and the attachment slot is the anaphor in
                // U.S.Agent's "Attach it to ~". Rebind that slot and leave the
                // explicit host alone.
                *attachment = TargetFilter::LastCreated;
            } else if matches!(
                target,
                TargetFilter::SelfRef | TargetFilter::ParentTarget | TargetFilter::TriggeringSource
            ) {
                // CR 608.2c + CR 301.5b: after a token creator, "attach this
                // Equipment to it" may have resolved the host pronoun through
                // the source-default `SelfRef` path before this gated
                // post-token pass.
                *target = TargetFilter::LastCreated;
            }
        }
        Effect::Sacrifice { target, .. }
        | Effect::Destroy { target, .. }
        | Effect::Bounce { target, .. }
        // CR 701.26a/b: only single-target tap/untap carries a rewritable target.
        | Effect::SetTapState {
            scope: EffectScope::Single,
            target,
            ..
        }
        | Effect::Pump { target, .. }
        // CR 603.7c + CR 608.2c (issue #4601 review): a delayed cleanup that
        // puts the temporary token on top/bottom of a library ("… put it on the
        // bottom of its owner's library at the beginning of the next end step")
        // lowers its bare-"it" to `ParentTarget`/`TriggeringSource` just like the
        // other move/cleanup forms — rebind to the created token.
        | Effect::PutAtLibraryPosition { target, .. } => {
            // CR 603.7c + CR 608.2c: inside an ETB-triggered token-copier (e.g.
            // Flameshadow Conjuring / Inalla: "create a token that's a copy of
            // that creature. … Exile it at the beginning of the next end step"),
            // the trigger sets the effect's subject to the *entering* creature,
            // so the bare-"it" pronoun lowers to `TriggeringSource` rather than
            // `ParentTarget`. In this gated post-token scope the antecedent of
            // "it"/"that token" is the newly created token, so both fallback
            // anaphors rewrite to `LastCreated`. (The `CopyTokenOf` copy source
            // is structurally absent from these arms, so it stays
            // `TriggeringSource` — the token is still a copy of the entering
            // creature.)
            if matches!(
                target,
                TargetFilter::ParentTarget | TargetFilter::TriggeringSource
            ) {
                *target = TargetFilter::LastCreated;
            }
        }
        Effect::ChangeZone { target, origin, .. }
            if matches!(
                target,
                TargetFilter::ParentTarget | TargetFilter::TriggeringSource
            ) =>
        {
            // CR 603.7c: In the gated post-token scope, singular "it"/"that
            // token" anaphors refer to the one just-created token and must
            // still be on the battlefield at cleanup — bind `LastCreated`.
            // Plural "those tokens" is already rewritten to `TrackedSet` by
            // `rewrite_parent_targets_to_tracked_set` (Saheeli -7, Twinflame);
            // do not stomp it here — `LastCreated` only snapshots the last
            // token in a multi-token batch (issue #5972).
            rewrite_change_zone_cleanup_to_last_created(target, origin);
        }
        Effect::ChangeZone {
            target: TargetFilter::TrackedSet { .. },
            origin,
            ..
        } => {
            // CR 603.7c: plural token cleanup stays on `TrackedSet`; stamp the
            // battlefield as the expected origin at firing time (issue #5972).
            origin.get_or_insert(Zone::Battlefield);
        }
        _ => {}
    }
}

/// CR 603.7c + CR 608.2c (issue #4601): the `SelfRef` companion to
/// [`rewrite_parent_target_to_last_created`], for the inner effect of a
/// `CreateDelayedTrigger` in the gated post-token-creator scope. A PHASE-
/// triggered token-copier ("At the beginning of combat on your turn, create a
/// token …, Sacrifice it at the beginning of the next end step" — Mishra,
/// Eminent One) has no triggering object, so the imperative parser lowers the
/// bare-"it" delayed cleanup to `SelfRef` (the source) instead of
/// `ParentTarget`/`TriggeringSource`. The antecedent is still the just-created
/// token, so rebind to `LastCreated`.
///
/// Scope is deliberately limited to the **destructive cleanup** effects that
/// remove/move the temporary token (`Sacrifice`/`Destroy`/`Bounce`/
/// `ChangeZone`/`PutAtLibraryPosition`). `Pump`/`Attach`/`SetTapState` are
/// excluded: there a delayed `SelfRef` ("~ gets +1/+1 until end of turn") more
/// plausibly means the source, so leaving it as `SelfRef` is correct.
fn rewrite_delayed_cleanup_self_ref_to_last_created(effect: &mut Effect) {
    match effect {
        Effect::Sacrifice { target, .. }
        | Effect::Destroy { target, .. }
        | Effect::Bounce { target, .. }
        // CR 603.7c (issue #4601 review): a delayed cleanup that puts the
        // temporary token on top/bottom of a library ("… put it on the bottom
        // of its owner's library at the beginning of the next end step") has the
        // same "it" anaphor — bind it to the created token, not the source.
        | Effect::PutAtLibraryPosition { target, .. }
            if matches!(target, TargetFilter::SelfRef) =>
        {
            *target = TargetFilter::LastCreated;
        }
        Effect::ChangeZone { target, origin, .. } if matches!(target, TargetFilter::SelfRef) => {
            rewrite_change_zone_cleanup_to_last_created(target, origin);
        }
        _ => {}
    }
}

fn rewrite_change_zone_cleanup_to_last_created(
    target: &mut TargetFilter,
    origin: &mut Option<Zone>,
) {
    *target = TargetFilter::LastCreated;
    // CR 603.7c: A delayed triggered ability affects a referenced object only
    // if that object remains in the zone it is expected to be in when the
    // delayed trigger resolves.
    origin.get_or_insert(Zone::Battlefield);
}

/// CR 603.7c: Sentence splitting can leave a WheneverEvent delayed trigger's
/// token-creating inner effect and its end-step cleanup delayed trigger as
/// sibling `sub_ability` links on the activated ability. Rewire the cleanup
/// under the token creator so it registers when the WheneverEvent fires, not
/// at activation time (Dalkovan Encampment, Encore sacrifice riders).
pub(super) fn nest_whenever_this_turn_token_cleanup_delayed_trigger(def: &mut AbilityDefinition) {
    let cleanup_sub = match def.sub_ability.take() {
        Some(sub) => sub,
        None => return,
    };

    let inner = match &mut *def.effect {
        Effect::CreateDelayedTrigger {
            condition: DelayedTriggerCondition::WheneverEvent { .. },
            effect: inner,
            ..
        } => inner,
        _ => {
            def.sub_ability = Some(cleanup_sub);
            return;
        }
    };

    let is_token_cleanup = matches!(
        &*cleanup_sub.effect,
        Effect::CreateDelayedTrigger {
            condition: DelayedTriggerCondition::AtNextPhase { .. },
            effect: cleanup_effect,
            ..
        } if matches!(
            &*cleanup_effect.effect,
            Effect::Sacrifice { .. } | Effect::ChangeZone { .. } | Effect::Destroy { .. }
        )
    );
    if !is_token_cleanup || !is_token_creating_effect(&inner.effect) {
        def.sub_ability = Some(cleanup_sub);
        return;
    }

    let mut cleanup_sub = cleanup_sub;
    let remaining_sibling_chain = cleanup_sub
        .sub_ability
        .as_ref()
        .is_some_and(|sub| sub.sub_link == SubAbilityLink::SequentialSibling)
        .then(|| cleanup_sub.sub_ability.take())
        .flatten();
    if let Effect::CreateDelayedTrigger {
        effect: cleanup_effect,
        ..
    } = &mut *cleanup_sub.effect
    {
        rewrite_parent_target_to_last_created(&mut cleanup_effect.effect, false);
    }

    let mut cursor = inner.as_mut();
    while cursor.sub_ability.is_some() {
        cursor = cursor
            .sub_ability
            .as_mut()
            .expect("sub_ability checked above");
    }
    cursor.sub_ability = Some(cleanup_sub);
    def.sub_ability = remaining_sibling_chain;
}

/// True when a def is a rider clause bound to the just-created tokens
/// (`TargetFilter::LastCreated`) — e.g. the "Those tokens gain haste" grant and
/// the "Exile them at the beginning of the next end step" delayed cleanup that
/// follow a token-creating `FlipCoinUntilLose` win clause (Mirror March #5966).
/// Used as the absorb predicate that folds these into the per-win `win_effect`.
fn def_is_last_created_rider(def: &AbilityDefinition) -> bool {
    effect_targets_last_created(&def.effect)
}

/// Append consecutive `LastCreated` riders after `defs[index]` to `head` so a
/// coin resolver runs the complete token-producing chain once per won flip.
fn absorb_last_created_riders(
    defs: &mut Vec<AbilityDefinition>,
    index: usize,
    head: &mut AbilityDefinition,
) {
    while index + 1 < defs.len() && def_is_last_created_rider(&defs[index + 1]) {
        let mut rider = defs.remove(index + 1);
        rider.kind = AbilityKind::Spell;
        rider.sub_link = SubAbilityLink::SequentialSibling;
        super::append_to_deepest_sub_ability(head, Some(Box::new(rider)));
    }
}

/// True when `effect` (directly or through a `CreateDelayedTrigger` wrapper)
/// targets `TargetFilter::LastCreated`. The `GenericEffect` arm inspects both
/// the effect-level `target` and each static grant's `affected` recipient, since
/// a "those tokens gain haste" grant carries `LastCreated` on the static, not
/// the effect target.
fn effect_targets_last_created(effect: &Effect) -> bool {
    match effect {
        Effect::GenericEffect {
            static_abilities,
            target,
            ..
        } => {
            target.as_ref() == Some(&TargetFilter::LastCreated)
                || static_abilities
                    .iter()
                    .any(|s| s.affected.as_ref() == Some(&TargetFilter::LastCreated))
        }
        Effect::CreateDelayedTrigger { effect: inner, .. } => {
            effect_targets_last_created(&inner.effect)
        }
        other => other.target_filter() == Some(&TargetFilter::LastCreated),
    }
}

/// CR 705: Post-process parsed ability defs to consolidate coin flip conditional
/// branches into their parent `FlipCoin` effect.
///
/// Pattern: a bare `FlipCoin { win: None, lose: None }` followed by one or more
/// `FlipCoin { win: Some(..), lose: None }` / `FlipCoin { win: None, lose: Some(..) }`
/// defs produced by the "if you win/lose the flip" intercept in `parse_effect_clause`.
pub(super) fn consolidate_die_and_coin_defs(defs: &mut Vec<AbilityDefinition>, _kind: AbilityKind) {
    let mut i = 0;
    while i < defs.len() {
        // CR 705: Consolidate coin flip branches. CR 705.2: the bare flip carries
        // the `flipper` (which player flips); the following branch-only flips are
        // stubs with the default `Controller` flipper, so preserve the bare flip's
        // flipper rather than the stubs'.
        if let Effect::FlipCoin {
            win_effect: None,
            lose_effect: None,
            flipper,
        } = &*defs[i].effect
        {
            let flipper = flipper.clone();
            let mut win = None;
            let mut lose = None;
            let mut j = i + 1;
            while j < defs.len() && (win.is_none() || lose.is_none()) {
                match &*defs[j].effect {
                    Effect::FlipCoin {
                        win_effect: Some(w),
                        lose_effect: None,
                        ..
                    } if win.is_none() => {
                        win = Some(w.clone());
                        j += 1;
                    }
                    Effect::FlipCoin {
                        win_effect: None,
                        lose_effect: Some(l),
                        ..
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
                    flipper,
                };
                defs.drain(i + 1..j);
            }
        }

        // CR 705.2 + CR 603.7c: Consolidate FlipCoinUntilLose with its per-win
        // clause chain. Absorb the win head (e.g. the token-creating copy clause)
        // PLUS any trailing rider clauses that reference the just-created tokens
        // (`LastCreated`) — "Those tokens gain haste", "Exile them …", already
        // rebound to `LastCreated` by the earlier `resolve_populated_token_anaphors`
        // pass — into ONE win_effect chain. They must live INSIDE win_effect
        // because `finish_until_lose` runs win_effect once per win and each
        // `CopyTokenOf` overwrites `state.last_created_token_ids`; left as post-loop
        // siblings they would grant haste to / exile only the final win's token
        // (Mirror March #5966). Riders that do NOT reference `LastCreated` stay
        // top-level siblings — the predicate is the reach guard against
        // over-absorbing an unrelated following clause.
        if matches!(&*defs[i].effect, Effect::FlipCoinUntilLose { .. }) && i + 1 < defs.len() {
            let mut head = defs.remove(i + 1);
            absorb_last_created_riders(defs, i, &mut head);
            *defs[i].effect = Effect::FlipCoinUntilLose {
                win_effect: Box::new(head),
            };
        }

        // CR 705: Consolidate FlipCoins with its following effect clause — the
        // "for each heads …" / "skips their next X turns where X is the number of
        // coins that came up heads" sentence. Like FlipCoinUntilLose, trailing
        // `LastCreated` riders must join the per-head chain, rather than apply
        // only to the final token created by the loop.
        if let Effect::FlipCoins {
            win_effect: None,
            lose_effect: None,
            count,
            flipper,
        } = &*defs[i].effect
        {
            if i + 1 < defs.len() {
                let count = count.clone();
                let flipper = flipper.clone();
                let mut next = defs.remove(i + 1);
                absorb_last_created_riders(defs, i, &mut next);
                *defs[i].effect = Effect::FlipCoins {
                    count,
                    win_effect: Some(Box::new(next)),
                    lose_effect: None,
                    flipper,
                };
            }
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

/// Strip optional-effect prefixes, returning whether the effect is optional,
/// which opponent-may scope applies (if any), and an implicit player_scope to
/// propagate to the containing ability (set when the prefix itself carries a
/// per-player iteration, e.g. "each opponent may").
///
/// CR 608.2d + CR 603.2: "each opponent may X" differs from "any opponent
/// may X" — every opponent independently decides yes/no, rather than first
/// accept wins. It lowers to `optional: true` + `player_scope: Opponent`:
/// the outer `player_scope` iteration rebinds controller to each opponent,
/// and each scoped clone enters the standard OptionalEffectChoice prompt.
pub(super) fn strip_optional_effect_prefix(
    text: &str,
) -> (
    bool,
    Option<crate::types::ability::OpponentMayScope>,
    Option<PlayerFilter>,
    String,
) {
    crate::parser::clause_shell::peel_optional_slots(text)
}

/// CR 107.1: Detect and strip a trailing "a number of times equal to the
/// difference" repeat suffix — an integer repeat count, not the CR 609.3 "do as
/// much as possible" rule. On success returns the suffix-free head; the
/// match itself confirms the difference-repeat pattern.
///
/// `strip_repeat_count_suffix` only recognizes numeric / `twice` / `three
/// times` repeats via `parse_count_expr`, so this dedicated combinator owns
/// the difference variant — it both detects and consumes the full suffix in
/// one `terminated(take_until(..), tag(..))` operation.
pub(super) fn split_difference_repeat_suffix(text: &str) -> Option<&str> {
    const SUFFIX: &str = " a number of times equal to the difference";
    nom::sequence::terminated(take_until::<_, _, OracleError<'_>>(SUFFIX), tag(SUFFIX))
        .parse(text)
        .ok()
        .map(|(_, head)| head)
}

/// CR 107.1: Strip "for each [X], " prefix from effect text. The iteration count
/// is an integer per-each quantity (plain count templating), not the CR 609.3
/// "do as much as possible" rule.
/// Returns the QuantityExpr for the iteration count and the remaining text.
/// "For as long as" is NOT matched (different construct — duration, not iteration).
/// CR 606.3: Recognize The Chain Veil's printed second-ability pattern,
/// "for each planeswalker you control, you may activate one of its loyalty
/// abilities once this turn as though none of its loyalty abilities have been
/// activated this turn." This belongs to `strip_for_each_prefix` solely to
/// bail out — the grant is a single per-controller cap raise, not a per-iteration
/// repeat. The actual `Effect::GrantExtraLoyaltyActivations` mapping lives in
/// `imperative::parse_grant_extra_loyalty_activations`.
fn is_chain_veil_for_each_grant(lower: &str) -> bool {
    nom_primitives::scan_contains(
        lower,
        "for each planeswalker you control, you may activate one of its loyalty abilities once this turn",
    )
}

pub(crate) fn strip_for_each_prefix(text: &str) -> (Option<QuantityExpr>, String) {
    let (repeat_for, _, rest) = strip_for_each_prefix_with_difference(text);
    (repeat_for, rest)
}

/// CR 608.2c + CR 208.4b: Peel a leading `for each` prefix while preserving
/// comparison provenance from the parser product. The optional binding is
/// produced only by the dedicated controller-scoped `PowerExceedsBase` parser
/// arm; it is not inferred by searching an arbitrary filter tree later.
pub(crate) fn strip_for_each_prefix_with_difference(
    text: &str,
) -> (Option<QuantityExpr>, Option<QuantityExpr>, String) {
    let lower = text.to_lowercase();
    if let Some(((), rest)) = nom_on_lower(text, &lower, |i| value((), tag("for each ")).parse(i)) {
        let rest_lower = &lower[text.len() - rest.len()..];
        if let Ok((remainder, clause)) =
            terminated(take_until(", "), tag::<_, _, OracleError<'_>>(", ")).parse(rest_lower)
        {
            let parsed_comparison = nom_quantity::parse_for_each_clause_ref_with_difference(clause)
                .ok()
                .and_then(|(rest, parsed)| rest.is_empty().then_some(parsed));
            let parsed_clause = parsed_comparison
                .clone()
                .map(|(qty, difference)| (qty, Some(difference)))
                .or_else(|| parse_for_each_clause(clause).map(|qty| (qty, None)));
            if let Some((qty, difference)) = parsed_clause {
                // CR 105.1: "for each color among [X], add one mana of that color"
                // must NOT be split into (repeat_for, "add one mana of that color").
                // The "that color" anaphors the per-iteration color, not the
                // source's `ChosenAttribute::Color`. Let the full text flow
                // through to `try_parse_for_each_color_mana_public` which emits a
                // single `ManaProduction::DistinctColorsAmongPermanents` mana
                // ability (a DIFFERENT enum from the `QuantityRef` matched here).
                if matches!(qty, QuantityRef::DistinctColorsAmong { .. })
                    && remainder
                        .trim_end_matches('.')
                        .trim()
                        .eq_ignore_ascii_case("add one mana of that color")
                {
                    return (None, None, text.to_string());
                }
                let mut copy_ctx = ParseContext::default();
                if parse_for_each_object_copy_parts(text, &lower, &mut copy_ctx).is_some() {
                    return (None, None, text.to_string());
                }
                // CR 606.3: The Chain Veil's "For each planeswalker you control,
                // you may activate one of its loyalty abilities once this turn..."
                // is parsed as a single Effect::GrantExtraLoyaltyActivations —
                // the "for each planeswalker" preamble names the beneficiaries
                // (every planeswalker the controller controls gets +1 cap), not
                // a repeat count. Bailing out keeps the residual text intact so
                // the imperative dispatch can recognize the full pattern.
                if is_chain_veil_for_each_grant(&lower) {
                    return (None, None, text.to_string());
                }
                let offset = text.len() - remainder.len();
                return (
                    Some(QuantityExpr::Ref { qty }),
                    difference,
                    text[offset..].to_string(),
                );
            }
        }
    }
    (None, None, text.to_string())
}

#[cfg(test)]
mod difference_binding_tests {
    use super::strip_for_each_prefix_with_difference;

    #[test]
    fn comparison_parser_product_carries_difference_provenance() {
        let (repeat_for, difference, rest) = strip_for_each_prefix_with_difference(
            "for each creature you control with power greater than that creature's base power, put a counter",
        );
        assert!(repeat_for.is_some());
        assert!(difference.is_some());
        assert_eq!(rest, "put a counter");
    }

    #[test]
    fn nested_not_and_or_properties_do_not_bind_difference() {
        for text in [
            "for each creature you control with not power greater than that creature's base power, put a counter",
            "for each creature you control with power greater than that creature's base power or flying, put a counter",
        ] {
            let (_, difference, _) = strip_for_each_prefix_with_difference(text);
            assert!(difference.is_none(), "nested property must not bind: {text}");
        }
    }
}

/// CR 705.2: Strip the redundant `"for each flip you won, "` (Mirror March)
/// quantifier from a coin-flip win clause. Unlike `strip_for_each_prefix`, this
/// carries NO iteration count: `FlipCoinUntilLose`/`FlipCoins` already run their
/// `win_effect` once per win (`finish_until_lose`), so lifting the count into a
/// `repeat_for` loop would double-apply it. Dropping the quantifier lets the
/// bare imperative ("create a token that's a copy of that creature") reach the
/// `CopyTokenOf` combinator. The `"flip(s) you won"` noun is not a countable
/// `parse_for_each_clause` clause, so `strip_for_each_prefix` cannot handle it.
/// Anchored nom strip — never a substring dispatch.
pub(crate) fn strip_redundant_flip_win_quantifier(text: &str) -> Option<String> {
    let lower = text.to_lowercase();
    let ((), rest) = nom_on_lower(text, &lower, |i| {
        let (i, _) = tag::<_, _, OracleError<'_>>("for each ").parse(i)?;
        let (i, _) = alt((tag("flips"), tag("flip"))).parse(i)?;
        let (i, _) = tag(" you ").parse(i)?;
        let (i, _) = alt((tag("won"), tag("win"))).parse(i)?;
        let (i, _) = tag(", ").parse(i)?;
        Ok((i, ()))
    })?;
    Some(rest.to_string())
}

/// CR 107.1: Parse an anchored `for each <clause>` multiplier for an effect's
/// count. The multiplier scales the base count by an integer per-each quantity
/// (the game uses only integers), so this is plain count templating, not the
/// CR 609.3 "do as much as possible" rule.
///
/// Single authority for "attach trailing for-each multiplier", shared across
/// quantity-taking verbs whose own quantity parser has already returned the
/// exact remainder where the multiplier is allowed. The count parser leaves
/// quantity nouns such as `card`/`cards` in the remainder, so this accepts that
/// draw-count noun axis before the marker. Returns `None` when the remainder
/// does not begin with an allowed multiplier shape or the clause does not parse
/// — never silently substitutes `Fixed(1)`.
pub(super) fn parse_for_each_multiplier_prefix(text: &str) -> Option<QuantityExpr> {
    let lower = text.to_lowercase();
    let ((), for_each_clause) = nom_on_lower(text, &lower, |input| {
        let (rest, _) = multispace0.parse(input)?;
        let (rest, _) = opt(terminated(
            alt((
                tag::<_, _, OracleError<'_>>("cards"),
                tag::<_, _, OracleError<'_>>("card"),
            )),
            multispace1,
        ))
        .parse(rest)?;
        let (rest, _) = tag("for each ").parse(rest)?;
        Ok((rest, ()))
    })?;
    let clause_lower = for_each_clause.to_lowercase();
    parse_for_each_clause_expr(clause_lower.trim_end_matches('.').trim())
}

pub(super) fn parse_for_each_opponent_target_fanout_clause(
    text: &str,
    repeat_for: Option<&QuantityExpr>,
    stripped_multi_target: Option<&MultiTargetSpec>,
    ctx: &ParseContext,
) -> Option<(ParsedEffectClause, MultiTargetSpec, ParseContext)> {
    if !matches!(
        repeat_for,
        Some(QuantityExpr::Ref {
            qty: QuantityRef::PlayerCount {
                filter: PlayerFilter::Opponent
            }
        })
    ) {
        return None;
    }

    let mut scoped_ctx = ctx.clone();
    scoped_ctx.relative_player_scope = Some(ControllerRef::TargetPlayer);
    let clause = parse_effect_clause(text, &mut scoped_ctx);
    if !is_per_opponent_target_fanout_clause(&clause) {
        return None;
    }

    Some((
        clause,
        MultiTargetSpec::bounded_expr(
            stripped_multi_target
                .map(|spec| spec.min.clone())
                .unwrap_or_else(|| QuantityExpr::Fixed {
                    value: per_opponent_target_fanout_min(text) as i32,
                }),
            QuantityExpr::Ref {
                qty: QuantityRef::PlayerCount {
                    filter: PlayerFilter::Opponent,
                },
            },
        ),
        scoped_ctx,
    ))
}

fn is_per_opponent_target_fanout_clause(clause: &ParsedEffectClause) -> bool {
    if matches!(
        clause.effect,
        Effect::Choose { .. }
            | Effect::ChooseCard { .. }
            | Effect::CopyTokenOf { .. }
            | Effect::TargetOnly { .. }
    ) {
        return false;
    }
    clause.effect.target_filter().is_some_and(|filter| {
        target_filter_controller_ref(filter) == Some(ControllerRef::TargetPlayer)
            && (target_filter_is_single_object_target(filter)
                || target_filter_is_explicit_target_player_graveyard_card(filter))
    })
}

/// CR 115.1a + CR 108.3: The per-opponent fanout normally targets battlefield
/// objects. This is the sole nonbattlefield exception: an explicit typed card
/// target in a paired opponent's graveyard. An `Or` is allowed only when every
/// branch independently carries that complete binding; it must not inherit the
/// controller, ownership, or zone restriction from a sibling. This keeps "that
/// player's graveyard" tied to the immediately preceding player target instead
/// of broadly enabling all nonbattlefield fanout filters.
pub(super) fn target_filter_is_explicit_target_player_graveyard_card(
    filter: &TargetFilter,
) -> bool {
    match filter {
        TargetFilter::Typed(tf) => {
            tf.controller == Some(ControllerRef::TargetPlayer)
                && !tf.type_filters.is_empty()
                && tf.properties.contains(&FilterProp::Owned {
                    controller: ControllerRef::TargetPlayer,
                })
                && tf.properties.contains(&FilterProp::InZone {
                    zone: Zone::Graveyard,
                })
        }
        TargetFilter::Or { filters } => {
            !filters.is_empty()
                && filters
                    .iter()
                    .all(target_filter_is_explicit_target_player_graveyard_card)
        }
        _ => false,
    }
}

pub(crate) fn target_filter_is_single_object_target(filter: &TargetFilter) -> bool {
    let zones = filter.extract_zones();
    if !zones.is_empty() && zones.iter().any(|zone| *zone != Zone::Battlefield) {
        return false;
    }

    match filter {
        TargetFilter::Typed(tf) => !tf.type_filters.is_empty(),
        TargetFilter::And { filters } | TargetFilter::Or { filters } => {
            filters.iter().all(target_filter_is_single_object_target)
        }
        TargetFilter::Not { filter } => target_filter_is_single_object_target(filter),
        _ => false,
    }
}

/// #5994: whether the per-opponent fanout slot is optional (min 0) or
/// mandatory (min 1), for verbs that fall through to this detector because
/// they aren't in `MULTI_TARGET_VERBS` (e.g. "put", "gain control of") — a
/// `MULTI_TARGET_VERBS` verb like "exile" takes its min from
/// `stripped_multi_target` upstream and never reaches this function. Scans at
/// word boundaries for an "up to N target …" quantifier anywhere in the
/// clause, not just immediately after the verb, so one detector covers every
/// non-`MULTI_TARGET_VERBS` verb instead of each needing its own hardcoded
/// prefix (the prior version only recognized "gain control of "). This does
/// NOT recognize "any number of target …" — that arm lives in
/// `strip_leading_quantifier`, which this function doesn't call; no card in
/// the per-opponent-fanout class currently uses that form. Reusing
/// `strip_optional_target_prefix` (rather than the bare `strip_leading_quantifier`
/// used by `MULTI_TARGET_VERBS`) is the safety property this relies on: it only
/// accepts a quantifier immediately followed by "target "/"other target "/
/// "another target ", so it can't misfire on a resource-count quantifier that
/// happens to precede the object noun (e.g. "put up to three +1/+1 counters on
/// target creature" — the quantity there modifies the counters, not the
/// target, and the "target " guard declines it).
fn per_opponent_target_fanout_min(text: &str) -> usize {
    let lower = text.to_ascii_lowercase();
    let found_optional_target_slot =
        nom_primitives::scan_at_word_boundaries(lower.as_str(), |input| {
            match strip_optional_target_prefix(input) {
                (rest, Some(spec)) if spec.min_is_fixed_zero() => Ok((rest, ())),
                _ => Err(nom::Err::Error(OracleError::new(
                    input,
                    nom::error::ErrorKind::Fail,
                ))),
            }
        })
        .is_some();
    if found_optional_target_slot {
        0
    } else {
        1
    }
}

/// CR 107.1: Strip trailing "for each [quantity]" repeat suffixes whose base
/// action should be repeated rather than have an embedded amount replaced. The
/// repeat count is an integer per-each quantity (count templating), not the
/// CR 609.3 "do as much as possible" rule.
/// CR 707.10 + CR 608.2c: Strip Zada's trailing "each copy targets a different
/// one of those creatures" rider before lifting the `for each` repeat suffix.
/// Returns whether the rider was present so chain lowering can stamp
/// `RetargetEachCopyToIterationMember` even after sentence splitting.
fn parse_zada_distinct_copy_target_rider_clause(i: &str) -> OracleResult<'_, ()> {
    all_consuming((
        tag("each copy targets a different one of those creatures"),
        opt(tag(".")),
        multispace0::<_, OracleError<'_>>,
    ))
    .parse(i)
    .map(|(rest, _)| (rest, ()))
}

pub(super) fn strip_each_copy_targets_distinct_member_suffix(text: &str) -> (bool, String) {
    let lower = text.to_ascii_lowercase();
    if let Some(((consumed, ()), _remainder)) = nom_on_lower(text, &lower, |input| {
        let before = input.len();
        let (rest, _) = terminated(
            take_until("each copy targets a different one of those creatures"),
            parse_zada_distinct_copy_target_rider_clause,
        )
        .parse(input)?;
        Ok((rest, (before - rest.len(), ())))
    }) {
        (
            true,
            text[..consumed]
                .trim_end()
                .trim_end_matches(|c: char| c == '.' || c.is_whitespace())
                .to_string(),
        )
    } else {
        (false, text.to_string())
    }
}

/// CR 707.10 + CR 115.1: Zada's `for each other creature ... the spell could
/// target` repeat count implies each copy targets a distinct iteration member.
fn filter_has_could_be_targeted_by_triggering_spell(filter: &TargetFilter) -> bool {
    match filter {
        TargetFilter::Typed(tf) => tf
            .properties
            .iter()
            .any(|p| matches!(p, FilterProp::CouldBeTargetedByTriggeringSpell)),
        TargetFilter::Or { filters } | TargetFilter::And { filters } => filters
            .iter()
            .any(filter_has_could_be_targeted_by_triggering_spell),
        TargetFilter::Not { filter } => filter_has_could_be_targeted_by_triggering_spell(filter),
        _ => false,
    }
}

/// CR 707.10 + CR 608.2c: True when a clause chunk is only Zada's distinct-copy
/// target rider (already stripped at chain level when possible; this absorbs a
/// residual standalone sentence after period splitting).
pub(super) fn recognize_zada_copy_distinct_target_rider(lower: &str) -> bool {
    all_consuming(parse_zada_distinct_copy_target_rider_clause)
        .parse(lower.trim())
        .is_ok()
}

/// CR 707.10 + CR 115.1: Zada's `for each other creature ... the spell could
/// target` repeat count implies each copy targets a distinct iteration member.
pub(super) fn zada_repeat_for_implies_distinct_copy_targets(qty: &QuantityExpr) -> bool {
    let QuantityExpr::Ref {
        qty: QuantityRef::ObjectCount { filter },
    } = qty
    else {
        return false;
    };
    filter_has_could_be_targeted_by_triggering_spell(filter)
}

/// Split a clause at the first " for each " boundary. Returns the base byte-length
/// (an offset into the ORIGINAL text — lowercasing is byte-length-preserving for the
/// ASCII Oracle corpus) and the lowercase tail after " for each ". The single split
/// authority shared by `strip_for_each_repeat_suffix` and `for_each_repeatable_repeat_for`.
fn split_for_each_suffix(text: &str) -> Option<(usize, String)> {
    let lower = text.to_lowercase();
    let (rest, base) = take_until::<_, _, OracleError<'_>>(" for each ")
        .parse(lower.as_str())
        .ok()?;
    let (tail, _) = tag::<_, _, OracleError<'_>>(" for each ")
        .parse(rest)
        .ok()?;
    Some((base.len(), tail.to_string()))
}

pub(super) fn strip_for_each_repeat_suffix(text: &str) -> (Option<QuantityExpr>, String) {
    let (_, text) = strip_each_copy_targets_distinct_member_suffix(text);
    if let Some((base_len, tail)) = split_for_each_suffix(&text) {
        if let Ok((_, qty)) = all_consuming(terminated(
            nom_quantity::parse_for_each_clause_ref,
            opt(tag::<_, _, OracleError<'_>>(".")),
        ))
        .parse(tail.as_str())
        {
            // The repeat-suffix lift is restricted to quantities whose consumer
            // is `CopySpell`: commander casts, trigger-bound spell history, and
            // Zada's distinct-copy object count. A player-set `PlayerCount` is
            // deliberately NOT admitted here — that class routes through the
            // fieldless-Investigate seam via `for_each_repeatable_repeat_for`.
            if matches!(
                &qty,
                QuantityRef::CommanderCastFromCommandZoneCount
                    | QuantityRef::SpellsCastBeforeTriggeringSpell { .. }
            ) || zada_repeat_for_implies_distinct_copy_targets(&QuantityExpr::Ref {
                qty: qty.clone(),
            }) {
                return (
                    Some(QuantityExpr::Ref { qty }),
                    text[..base_len].trim_end().to_string(),
                );
            }
        }
    }
    (None, text)
}

/// CR 701.16a + CR 608.2c: Lift a trailing "[once] for each ⟨set⟩" multiplier off a
/// fieldless keyword-action effect (Investigate has no count slot) into a `repeat_for`.
/// Restricted to the per-each MEMBER-COUNT class — a count of the players or objects the
/// "for each" ranges over: `PlayerCount` (including its nested `PlayerAttribute` filter,
/// e.g. Wojek's comparative hand size) and `ObjectCount` (e.g. Serene Sleuth's goaded
/// creatures). Contextual amount-refs (`FilteredTrackedSetSize` / `TrackedSetSize` /
/// `PreviousEffectAmount` / `EventContextAmount`) are deliberately NOT lifted, and the
/// match is fail-closed (an unrecognized ref leaves the Investigate bare). This matters
/// because such refs co-occur with a leading Fixed multiplier the single `repeat_for`
/// slot cannot represent: Tamiyo Meets the Story Circle's "investigate TWICE for each
/// card discarded this way" would otherwise lift the per-each `FilteredTrackedSetSize`
/// and silently DROP the "twice" (N Clues instead of 2×N). The runtime repeat_for driver
/// resolves either admitted member-count generically (one Clue per member). One shape —
/// a class-membership guard, not per-family handling.
pub(super) fn for_each_repeatable_repeat_for(text: &str) -> Option<QuantityExpr> {
    let (_, tail) = split_for_each_suffix(text)?;
    match parse_for_each_clause(&tail) {
        Some(qty @ (QuantityRef::PlayerCount { .. } | QuantityRef::ObjectCount { .. })) => {
            Some(QuantityExpr::Ref { qty })
        }
        _ => None,
    }
}

/// CR 107.1: Strip "twice" / "three times" / "N times" suffix to produce a
/// `repeat_for` count — an integer repeat multiplier (count templating), not the
/// CR 609.3 "do as much as possible" rule. Unified with `strip_for_each_prefix`
/// at the chain level so the base action is parsed normally and the resolver
/// loops it N times.
pub(crate) fn strip_repeat_count_suffix(text: &str) -> (Option<QuantityExpr>, String) {
    let lower = text.to_lowercase();
    let suffixes: &[(&str, i32)] = &[
        (" twice", 2),
        (" three times", 3),
        (" four times", 4),
        (" five times", 5),
    ];
    for &(suffix, count) in suffixes {
        if let Ok((_, base)) = terminated(
            take_until::<_, _, OracleError<'_>>(suffix),
            nom::combinator::all_consuming(tag(suffix)),
        )
        .parse(lower.as_str())
        {
            return (
                Some(QuantityExpr::Fixed { value: count }),
                text[..base.len()].to_string(),
            );
        }
    }
    if let Ok((_, base)) = terminated(
        take_until::<_, _, OracleError<'_>>(" times"),
        nom::combinator::all_consuming(tag(" times")),
    )
    .parse(lower.as_str())
    {
        if let Some(space_idx) = base.rfind(' ') {
            let qty_text = text[space_idx + 1..text.len() - " times".len()].trim();
            if let Some((qty, remainder)) = parse_count_expr(qty_text) {
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
/// "Each other player sacrifices a creature" → (Some(Opponent), "sacrifice a creature")
/// "Each player draws a card" → (Some(All), "draw a card")
pub(crate) fn strip_player_scope_subject(text: &str) -> (Option<PlayerFilter>, String) {
    let (scope, stripped) = strip_linked_exile_owner_subject(text);
    if scope.is_some() {
        return (scope, stripped);
    }
    strip_each_player_subject(text)
}

/// Parse the player anchor in an "each player other than ⟨anchor⟩" subject into
/// the `PlayerFilter` whose population is excluded. Composable `alt()` so future
/// anchors ("you", "that player") slot in without new `PlayerFilter` variants.
fn parse_excluded_player_anchor(i: &str) -> OracleResult<'_, PlayerFilter> {
    alt((
        // CR 109.4 + CR 608.2h: "its controller" = controller of the targeted
        // permanent named earlier in the spell (the exiled object for Fractured
        // Identity), resolved via `PlayerFilter::ParentObjectTargetController`.
        value(
            PlayerFilter::ParentObjectTargetController,
            tag("its controller"),
        ),
    ))
    .parse(i)
}

pub(super) fn strip_each_player_subject(text: &str) -> (Option<PlayerFilter>, String) {
    // CR 701.9a + CR 608.2c: Reserve only the exact Kroxa/Strongarm
    // mandatory-FILTERED decline-tail grammar for its dedicated dispatcher.
    // A broad `who didn't` reservation also captures unrelated relative clauses
    // (for example, sacrifice and choose), changing their parser routes even
    // though this dispatcher intentionally supports discard only.
    if strip_each_scope_who_didnt_verb_filter_this_way_subject(text).is_some() {
        return (None, text.to_string());
    }

    let lower = text.to_lowercase();
    let scope_rest = nom_on_lower(text, &lower, |i| {
        alt((
            value(
                PlayerFilter::HighestSpeed,
                tag("each player with the highest speed among players "),
            ),
            value(PlayerFilter::Opponent, tag("each other player ")),
            // CR 102.2 + CR 603.2: "each of that player's opponents" — the
            // caster's opponents (mandatory variant), fanned out per-player.
            // Apostrophe variants: ASCII ' and curly U+2019 '.
            value(
                PlayerFilter::OpponentOfTriggeringPlayer,
                tag("each of that player's opponents "),
            ),
            value(
                PlayerFilter::OpponentOfTriggeringPlayer,
                tag("each of that player\u{2019}s opponents "),
            ),
            value(PlayerFilter::Opponent, tag("each opponent ")),
            // CR 608.2c + CR 109.4 + CR 608.2h: "each player other than <ref>" —
            // all players except the anchor's player (resolved with last-known
            // information when the anchor object has left the battlefield, e.g.
            // Fractured Identity's exiled permanent). Placed before the bare
            // "each player " arm so the longer prefix wins.
            map(
                preceded(
                    tag("each player other than "),
                    terminated(parse_excluded_player_anchor, tag(" ")),
                ),
                |anchor| PlayerFilter::AllExcept {
                    exclude: Box::new(anchor),
                },
            ),
            value(PlayerFilter::All, tag("each player ")),
            // CR 101.4 + CR 608.2c: comma-prefixed per-player imperative scope —
            // "For each player, <imperative> ... that player controls" (Curse of
            // Fenric I). The more-specific "for each player, you choose"/"choose
            // ... in that player's zone" handlers run earlier in the dispatcher,
            // so only the bare imperative residual reaches here.
            value(PlayerFilter::All, tag("for each player, ")),
        ))
        .parse(i)
    });
    let Some((scope, rest)) = scope_rest else {
        return (None, text.to_string());
    };

    // CR 611.2a + CR 400.7i: "each player may play/cast …" is a per-grantee
    // casting permission (`try_parse_per_grantee_play_grant`), not a player-scoped
    // imperative subject. Stripping "each player " leaves "may play …", which
    // misroutes to `Effect::CastFromZone` instead of `GrantCastingPermission`.
    let rest_lower = rest.trim_start().to_lowercase();
    if alt((tag::<_, _, OracleError<'_>>("may play "), tag("may cast ")))
        .parse(rest_lower.as_str())
        .is_ok()
    {
        return (None, text.to_string());
    }

    // CR 311.7 + CR 607.2d / CR 607.2m (by analogy): "each player who last chose
    // <A> chooses <B>, and vice versa" (Two Streams Facility's chaos swap) is a
    // symmetric per-player anchor swap owned by `parse_swap_chosen_labels`, NOT a
    // player-scoped imperative subject. Stripping "each player " would leave "who
    // last chose …", which misroutes to `Unimplemented { who }`. Bail with
    // `(None, full_text)` so the whole clause survives for the dedicated handler
    // (mirrors the "may play"/"may cast" bail above).
    if tag::<_, _, OracleError<'_>>("who last chose ")
        .parse(rest_lower.as_str())
        .is_ok()
    {
        return (None, text.to_string());
    }

    // CR 608.2c + CR 608.2d + CR 701.21a: "for each player, you choose …" (Tragic
    // Arrogance → CategoryChooserScope::ControllerForAll) and "for each player,
    // choose … in that player's graveyard/zone" (Breach the Multiverse →
    // ChooseFromZone { zone_owner: EachPlayer }) have DEDICATED dispatchers that
    // must own these shapes. The chunk-loop cascade can reach this subject-strip
    // before those dispatchers, so a "choose"-headed residual must survive as
    // `(None, full_text)` for the dedicated handler. Ordering invariant.
    if alt((tag::<_, _, OracleError<'_>>("choose "), tag("you choose ")))
        .parse(rest_lower.as_str())
        .is_ok()
    {
        return (None, text.to_string());
    }

    // CR 109.4 + CR 109.5: A "who controls [comparator] [count] [type-phrase]"
    // relative clause restricts the player set to those whose controlled-permanent
    // count satisfies the comparison (Thornbow Archer: "each opponent who doesn't
    // control an Elf loses 1 life"; Heidegger: "each opponent who controls more
    // creatures than you"). The clause must be consumed and reflected in the
    // scope — silently dropping it over-applies the effect to every player.
    if let Some((controls_scope, after_clause)) = strip_controls_permanent_clause(&scope, rest) {
        let deconjugated = subject::deconjugate_verb(&after_clause);
        return (Some(controls_scope), deconjugated);
    }

    // CR 122.1 + CR 122.2: "who has/have N or more <kind> counters" restricts
    // the player set to those whose per-candidate counter total meets the
    // threshold (Ixhel, Scion of Atraxa: "each opponent who has three or more
    // poison counters exiles …"; Glissa's Retriever quantity path shares the
    // same attr-clause grammar via `parse_player_attribute_attr_clause`).
    if let Some((attr_scope, after_clause)) = strip_player_attribute_clause(&scope, rest) {
        let deconjugated = subject::deconjugate_verb(&after_clause);
        return (Some(attr_scope), deconjugated);
    }

    // CR 101.4 + CR 608.2d: "who [didn't] chose/choose the highest/lowest
    // number" restricts the player set to those whose secretly-chosen number
    // matches (or fails to match) the cross-player extremum — Wheel of
    // Misfortune's "each player who didn't choose the lowest number discards
    // their hand, then draws seven cards", Life at Stake's "each player who
    // chose the highest number loses that much life". Sibling of the attribute
    // clause above and consumed on the same terms.
    if let Some((chosen_scope, after_clause)) = strip_chosen_number_clause(&scope, rest) {
        let deconjugated = subject::deconjugate_verb(&after_clause);
        return (Some(chosen_scope), deconjugated);
    }

    // CR 608.2c + CR 109.5: A "who [verb]ed … this way" relative clause after
    // "each player" / "each opponent" restricts the affected set to the players
    // who performed the tracked action during THIS resolution (Kwain, Itinerant
    // Meddler: "each player who drew a card this way gains 1 life" — only players
    // who actually drew gain the life, so an opponent who declined the optional
    // draw or had an empty library is excluded). Like the "who controls" /
    // attribute clauses above, the relative clause MUST be consumed and reflected
    // in the scope; dropping it would over-apply the effect to every player.
    if let Some((action_scope, after_clause)) = strip_performed_action_this_way_clause(&scope, rest)
    {
        let deconjugated = subject::deconjugate_verb(&after_clause);
        return (Some(action_scope), deconjugated);
    }

    // CR 508.6 + CR 104.3e: A "[source] attacked this turn" relative clause after
    // "each player" / "each opponent" restricts the affected set to the players
    // the ability source creature attacked this turn — Angel of Destiny: "each
    // player this creature attacked this turn loses the game". Resolved as the
    // source-specific `OpponentAttacked { Source, ThisTurn }`, which excludes the
    // controller and avoids widening to players attacked by other creatures.
    // Like the "who controls" clause above, the relative clause MUST be consumed
    // and reflected in the scope; dropping it would over-apply the loss to every
    // player (the bug behind issue #1599). General over the predicate verb —
    // "loses the game", "loses N life", etc. all compose.
    let rest_attacked_lower = rest.to_lowercase();
    if let Some(((), after_clause)) = nom_on_lower(rest, &rest_attacked_lower, |i| {
        let (i, _) = alt((tag("this creature "), tag("~ "), tag("it "))).parse(i)?;
        value((), tag("attacked this turn ")).parse(i)
    }) {
        let deconjugated = subject::deconjugate_verb(after_clause);
        return (
            Some(PlayerFilter::OpponentAttacked {
                subject: AttackSubject::Source,
                scope: AttackScope::ThisTurn,
            }),
            deconjugated,
        );
    }

    // Guard: static restriction predicates ("can't", "cannot", "don't", "may only",
    // "may not") belong to the static parser, not the imperative effect pipeline.
    // Intercepting them here would produce Unimplemented instead of typed static modes.
    let rest_lower = rest.trim().to_lowercase();
    if alt((
        tag::<_, _, OracleError<'_>>("can't"),
        tag("cannot"),
        tag("don't"),
        tag("may only"),
        tag("may not"),
        tag("may cast"),
        // CR 101.3 + CR 109.5: Reserve the relative-clause shape "who can't" /
        // "who cannot" for the Plaguecrafter-class subject-only decline-tail
        // dispatcher (`strip_each_scope_who_cant_subject` in
        // `parse_effect_clause_inner`). The dispatcher runs AFTER this
        // function returns, so we must return `(None, text)` for these
        // shapes — otherwise we'd strip `each player ` and leave
        // `who can't …` orphaned to be misclassified as a static
        // restriction. This is load-bearing for the dispatch contract, not
        // a defensive escape.
        tag("who can't"),
        tag("who cannot"),
        // CR 118.12 + CR 608.2c: Reserve the relative-clause shape "who
        // doesn't" / "who does not" for the Wernog-class subject-only
        // OPTIONAL-decline tail dispatcher (`strip_each_scope_who_doesnt_subject`
        // in `parse_effect_clause_inner`). This guard runs AFTER the
        // `strip_controls_permanent_clause` consumer above, which
        // already absorbs the "who doesn't control <type>" static-board shape
        // (Thornbow Archer → ControlsCount) because that combinator requires a
        // "control " verb after "doesn't". So a bare "who doesn't loses 1 life"
        // (no "control") reaches here and must survive as `(None, full_text)`
        // for the dispatcher — ordering invariant, not a defensive escape.
        tag("who doesn't"),
        tag("who does not"),
        // CR 118.12 + CR 608.2d + CR 109.5: Reserve the positive relative clause
        // "who does" for the subject-only OPTIONAL-ACCEPT consequence-tail
        // dispatcher (`strip_each_scope_who_does_subject` in
        // `parse_effect_clause_inner` — The Second Doctor, City Hall). The
        // "who doesn't" / "who does not" tags above already reserve the decline
        // forms; this arm reserves the accept form. Every arm of this `alt`
        // returns the same `(None, full_text)` reservation, so listing order is
        // for readability, not correctness — but `who does` is listed AFTER the
        // longer `who doesn't`/`who does not` tags to mirror the grammar.
        tag("who does"),
        // CR 119.3 + CR 701.55a: "each opponent who lost N or more life this
        // turn faces a villainous choice" is a restricted chooser phrase, not
        // a normal per-player imperative. Preserve the full subject so the
        // `ChooseOneOf` parser can emit a PlayerAttribute chooser instead of
        // broadening the choice to every opponent.
        tag("who lost"),
    ))
    .parse(rest_lower.as_str())
    .is_ok()
    {
        return (None, text.to_string());
    }

    let rest_condition_lower = rest.to_lowercase();
    if let Some(((), conditioned_rest)) = nom_on_lower(rest, &rest_condition_lower, |i| {
        value((), tag("with no cards in hand ")).parse(i)
    }) {
        let deconjugated = subject::deconjugate_verb(conditioned_rest);
        return (
            Some(scope),
            format!("if you have no cards in hand, {deconjugated}"),
        );
    }

    // CR 608.2c: A leading manner/continuation adverb after a resolved
    // player-scope subject carries no AST weight — strip it via `tag()` so the
    // residual deconjugates and dispatches normally.
    //
    // - "also" ("each opponent also discards a card") is the additive connector
    //   also handled for self-ref subjects in `parse_effect_clause_inner`.
    // - CR 101.4 + CR 608.2d: "secretly" (Wheel of Misfortune's "each player
    //   secretly chooses a number 0 or greater"; Menacing Ogre's "each player
    //   secretly chooses a number") marks the choice as hidden from the other
    //   choosers. That is a VISIBILITY property, enforced at the state-filtering
    //   seam (`game::visibility` keeps each player's `ChosenAttribute::Number`
    //   private to that player), not a distinct effect — so the choice itself
    //   parses exactly like an open one.
    let rest = nom_on_lower(rest, &rest_condition_lower, |i| {
        value((), alt((tag("also "), tag("secretly ")))).parse(i)
    })
    .map(|((), after)| after)
    .unwrap_or(rest);

    // Deconjugate the verb: "discards" → "discard", "draws" → "draw"
    let deconjugated = subject::deconjugate_verb(rest);
    (Some(scope), deconjugated)
}

/// CR 101.3 + CR 118.12 + CR 109.5: Strip a leading "each <scope> who can't /
/// cannot, <body>" subject-only mandatory-impossible decline-tail. Returns the
/// player scope and the body text. The body's recipient (e.g. Discard.target)
/// must be rewritten Controller → ScopedPlayer by the caller; the body's
/// condition must be stamped Not { current_scope_succeeded() }; the preceding
/// clause's boundary must be retargeted Sentence → Then. Caller responsibilities
/// — this combinator only does subject + scope detection.
///
/// Parallel to `strip_for_each_opponent_who_doesnt` (prepositional + optional);
/// fills the subject-only + mandatory-impossible quadrant of the 2×2 matrix.
pub(super) fn strip_each_scope_who_cant_subject(text: &str) -> Option<(PlayerFilter, String)> {
    let lower = text.to_lowercase();
    nom_on_lower(text, &lower, |i| {
        let (i, scope) = alt((
            value(PlayerFilter::Opponent, tag("each other player who ")),
            value(PlayerFilter::Opponent, tag("each opponent who ")),
            value(PlayerFilter::All, tag("each player who ")),
        ))
        .parse(i)?;
        let (i, _) = alt((tag("can't"), tag("cannot"))).parse(i)?;
        let (i, _) = preceded(opt(tag(",")), opt(multispace1)).parse(i)?;
        Ok((i, scope))
    })
    .map(|(scope, rest)| (scope, rest.to_string()))
}

/// CR 118.12 + CR 608.2d + CR 109.5: Strip a leading "each <scope> who doesn't /
/// does not, <body>" subject-only OPTIONAL-decline tail. Returns the player scope
/// and the body text. The body's recipient (e.g. LoseLife.target) must be
/// rewritten Controller → ScopedPlayer by the caller; the body's condition must
/// be stamped Not { effect_performed() } (the CR 118.12 "doesn't" branch reading
/// OptionalEffectPerformed); the preceding clause's boundary must be retargeted
/// Sentence → Then. Caller responsibilities — this combinator only does subject +
/// scope detection.
///
/// PARALLEL INVERSE to `strip_each_scope_who_cant_subject` (subject-only +
/// mandatory-impossible): this fills the subject-only + optional-decline cell of
/// the 2×2 decline matrix (Wernog, Rider's Chaplain: "each opponent may
/// investigate. Each opponent who doesn't loses 1 life."). Matches ONLY
/// doesn't/does not; the can't/cannot arm stays with `strip_each_scope_who_cant_subject`.
pub(super) fn strip_each_scope_who_doesnt_subject(text: &str) -> Option<(PlayerFilter, String)> {
    let lower = text.to_lowercase();
    nom_on_lower(text, &lower, |i| {
        let (i, scope) = alt((
            value(PlayerFilter::Opponent, tag("each other player who ")),
            value(PlayerFilter::Opponent, tag("each opponent who ")),
            value(PlayerFilter::All, tag("each player who ")),
        ))
        .parse(i)?;
        let (i, _) = alt((tag("doesn't"), tag("does not"))).parse(i)?;
        let (i, _) = preceded(opt(tag(",")), opt(multispace1)).parse(i)?;
        Ok((i, scope))
    })
    .map(|(scope, rest)| (scope, rest.to_string()))
}

/// CR 118.12 + CR 608.2d + CR 109.5: Strip a leading "each <scope> who does,
/// <body>" subject-only OPTIONAL-ACCEPT consequence tail. Returns the player
/// scope and the body text. The body's recipient must be rebound
/// Controller/ParentTargetedPlayer → ScopedPlayer by the caller; the body's
/// condition must be stamped `effect_performed()` (the CR 118.12 "does" accept
/// branch reading OptionalEffectPerformed); the preceding clause's boundary must
/// be retargeted Sentence → Then. Caller responsibilities — this combinator only
/// does subject + scope detection.
///
/// POSITIVE/ACCEPT TWIN of `strip_each_scope_who_doesnt_subject` (subject-only +
/// optional-decline): this fills the subject-only + optional-ACCEPT cell of the
/// decline matrix (The Second Doctor: "each player may draw a card. Each opponent
/// who does can't attack you …"; City Hall: "each player may create two tapped
/// Treasure tokens. Each player who does can't attack you …"; Step Between
/// Worlds: "Each player may shuffle …. Each player who does draws seven cards.").
///
/// SELF-CORRECT against the negative cells: "does" is a strict prefix of
/// "doesn't"/"does not", so an `not(alt((tag("n't"), tag(" not"))))` word-boundary
/// guard rejects those forms in isolation — correctness does NOT depend on
/// dispatch-arm ordering (the `who doesn't` arm running first is defense-in-depth,
/// not a requirement).
pub(super) fn strip_each_scope_who_does_subject(text: &str) -> Option<(PlayerFilter, String)> {
    let lower = text.to_lowercase();
    nom_on_lower(text, &lower, |i| {
        let (i, scope) = alt((
            value(PlayerFilter::Opponent, tag("each other player who ")),
            value(PlayerFilter::Opponent, tag("each opponent who ")),
            value(PlayerFilter::All, tag("each player who ")),
        ))
        .parse(i)?;
        // CR 118.12 accept branch: match "does" only when it is NOT the prefix of
        // "doesn't" / "does not" (those are the decline cell, owned by
        // `strip_each_scope_who_doesnt_subject`).
        let (i, _) = terminated(tag("does"), not(alt((tag("n't"), tag(" not"))))).parse(i)?;
        let (i, _) = preceded(opt(tag(",")), opt(multispace1)).parse(i)?;
        Ok((i, scope))
    })
    .map(|(scope, rest)| (scope, rest.to_string()))
}

/// CR 701.9a + CR 608.2c + CR 109.5: Strip a leading "each <scope> who
/// didn't / did not <verb> a [filter] this way, <body>" subject-only
/// mandatory-FILTERED decline-tail. Returns the player scope, the filter the
/// scoped player's own zone change failed to match, and the body text.
///
/// Sibling of `strip_each_scope_who_cant_subject` (mandatory-IMPOSSIBLE: the
/// action couldn't happen at all) and `strip_each_scope_who_doesnt_subject`
/// (OPTIONAL-decline): this fills the mandatory-FILTERED cell, where the
/// scoped player's mandatory action always happens but the body only cares
/// whether the moved object matched a filter (Kroxa, Titan of Death's Hunger:
/// "each opponent discards a card, then each opponent who didn't discard a
/// nonland card this way loses 3 life"). The gate reads
/// `ZoneChangedThisWay { filter }` (CR 608.2c) rather than a pass/fail signal
/// — an opponent who discarded nothing (empty hand) still "didn't discard a
/// nonland card", matching the official ruling that Kroxa's life loss still
/// applies to an opponent with no cards in hand.
///
/// Verb is scoped to "discard" — the only verb this exact "who didn't <verb>
/// a [filter] this way" relative-clause construction is verified against
/// (Kroxa, Titan of Death's Hunger — opponent scope, nonland filter; and
/// Strongarm Tactics — all-players scope, creature filter: "Each player
/// discards a card. Then each player who didn't discard a creature card
/// this way loses 4 life."). `ZoneChangedThisWay` itself is verb-agnostic
/// (it reads `last_zone_changed_ids`, which sacrifice/exile populate
/// identically to discard), so widening the `alt()` to those verbs is a
/// one-line change once a card actually prints that construction —
/// deferred rather than speculatively added ahead of a verified card.
pub(super) fn strip_each_scope_who_didnt_verb_filter_this_way_subject(
    text: &str,
) -> Option<(PlayerFilter, TargetFilter, String)> {
    let lower = text.to_lowercase();
    nom_on_lower(text, &lower, |i| {
        let (i, scope) = alt((
            value(PlayerFilter::Opponent, tag("each other player who ")),
            value(PlayerFilter::Opponent, tag("each opponent who ")),
            value(PlayerFilter::All, tag("each player who ")),
        ))
        .parse(i)?;
        let (i, _) = alt((tag("didn't "), tag("did not "))).parse(i)?;
        let (i, _) = tag("discard ").parse(i)?;
        let (i, _) = alt((tag("a "), tag("an "))).parse(i)?;
        let (filter, after_filter) = parse_type_phrase(i);
        if matches!(filter, TargetFilter::Any) {
            return Err(oracle_err(i));
        }
        let (i, _) = tag("this way").parse(after_filter.trim_start())?;
        let (i, _) = preceded(opt(tag(",")), opt(multispace1)).parse(i)?;
        Ok((i, (scope, filter)))
    })
    .map(|((scope, filter), rest)| (scope, filter, rest.to_string()))
}

/// CR 608.2e + CR 608.2c + CR 101.3: Strip a leading "For each opponent who
/// doesn't / does not / can't / cannot, " decline-tail prefix. Two shapes:
///
/// - **Optional-decline** (`doesn't` / `does not`): Braids-class. The parent is
///   "each opponent may <optional action>"; the body runs once per opponent
///   who declined the optional action. Returns `AbilityCondition::effect_performed()` —
///   caller wraps in `Not { IfYouDo }` so the body fires on the decline branch
///   (CR 118.12 optional-cost branch + CR 608.2d).
/// - **Mandatory-impossible** (`can't` / `cannot`): Refurbished-Familiar-class.
///   The parent is "each opponent <bare imperative>"; the body runs once per
///   opponent who couldn't perform the action (empty hand for discard, no
///   permanent to sacrifice, etc.). Returns
///   `AbilityCondition::current_scope_succeeded()` — caller wraps in `Not` so
///   the body fires on the mandatory-impossible branch (CR 101.3 +
///   CR 118.12 mandatory-cost branch).
///
/// The matched-arm condition is returned alongside the residual body so the
/// caller can stamp the right gate on the sub_ability. The `tag()`/`alt()`
/// chain is both the detector and the consumer — no
/// `contains()`/`starts_with()`.
pub(super) fn strip_for_each_opponent_who_doesnt(text: &str) -> Option<(String, AbilityCondition)> {
    let lower = text.to_lowercase();
    nom_on_lower(text, &lower, |i| {
        alt((
            value(
                AbilityCondition::effect_performed(),
                preceded(
                    alt((
                        tag("for each opponent who doesn't"),
                        tag("for each opponent who does not"),
                    )),
                    preceded(opt(tag(",")), opt(multispace1)),
                ),
            ),
            value(
                AbilityCondition::current_scope_succeeded(),
                preceded(
                    alt((
                        tag("for each opponent who can't"),
                        tag("for each opponent who cannot"),
                    )),
                    preceded(opt(tag(",")), opt(multispace1)),
                ),
            ),
        ))
        .parse(i)
    })
    .map(|(cond, rest)| (rest.to_string(), cond))
}

/// CR 109.5 + CR 115.10: Within a "for each opponent who doesn't" decline body,
/// "that player" is the scoped (per-iteration) opponent and "you" is the printed
/// ability controller. Rewrite a recipient-bearing effect's recipient so it
/// rebinds correctly inside the surrounding `player_scope: Opponent` iteration:
/// - `TriggeringPlayer` → `ScopedPlayer` ("that player" event-context anaphor)
/// - `ParentTargetController` → `ScopedPlayer` ("that player" parsed as the
///   controller of the parent `Sacrifice(opponent)` node's target — which is
///   the declining opponent's own permanent, i.e. the scoped opponent)
/// - `Controller` → `OriginalController` ("you" — the fixed printed controller)
/// - an undirected `LoseLife { target: None }` → `Some(ScopedPlayer)` — the live
///   card data drops the "that player" subject, but inside a decline body an
///   undirected life loss IS "that player" by CR 109.5 context.
pub(super) fn rebind_decline_body_recipient(effect: &mut Effect) {
    fn rebind(filter: &mut TargetFilter) {
        match filter {
            TargetFilter::TriggeringPlayer | TargetFilter::ParentTargetController => {
                *filter = TargetFilter::ScopedPlayer
            }
            TargetFilter::Controller => *filter = TargetFilter::OriginalController,
            _ => {}
        }
    }
    match effect {
        Effect::LoseLife { target, .. } => match target {
            Some(filter) => rebind(filter),
            None => *target = Some(TargetFilter::ScopedPlayer),
        },
        Effect::Draw { target, .. }
        | Effect::Discard { target, .. }
        | Effect::Mill { target, .. } => rebind(target),
        Effect::Token { owner, .. } => rebind(owner),
        _ => {}
    }
}

/// CR 109.5: Walk a decline-body chain (`effect` + every `sub_ability`
/// descendant) and apply `rebind` to each node's `effect`. Single shared
/// walker; the per-quadrant mapping is supplied as the leaf rebinder.
///
/// Used by both the prepositional decline path
/// (`rebind_decline_body_recipient`: `Controller → OriginalController`) and
/// the subject-only decline path (`rebind_subject_only_body_recipient`:
/// `Controller → ScopedPlayer`). Replaces the previous byte-for-byte
/// duplicated `rebind_decline_body_recipients` / `rebind_subject_only_body_recipients`
/// pair — the two walkers differed only in which leaf function they called.
pub(super) fn rebind_clause_recipients_with(
    clause: &mut ParsedEffectClause,
    rebind: impl Fn(&mut Effect),
) {
    rebind(&mut clause.effect);
    let mut cursor = clause.sub_ability.as_deref_mut();
    while let Some(node) = cursor {
        rebind(&mut node.effect);
        cursor = node.sub_ability.as_deref_mut();
    }
}

/// CR 109.5 + CR 101.3: Inside a subject-only "each <scope> who can't, <body>"
/// decline-tail, the body's implicit recipient binds to the SCOPED player (the
/// one who couldn't perform the predicate), not to the printed ability
/// controller. Rewrite Controller → ScopedPlayer.
///
/// PARALLEL INVERSE to `rebind_decline_body_recipient`: this rewrites
/// `Controller → ScopedPlayer` (subject-only "each X who can't"), whereas
/// the prepositional walker rewrites `Controller → OriginalController`
/// ("for each opponent who doesn't" — "you" stays "you" inside an
/// Opponent-scoped iteration).
///
/// Same five-variant surface: `{ LoseLife, Draw, Discard, Mill, Token }`.
/// `Sacrifice` is NOT covered (it carries its own target on the parent node).
pub(super) fn rebind_subject_only_body_recipient(effect: &mut Effect) {
    fn rebind(filter: &mut TargetFilter) {
        if matches!(filter, TargetFilter::Controller) {
            *filter = TargetFilter::ScopedPlayer;
        }
    }
    match effect {
        Effect::LoseLife { target, .. } => match target {
            Some(filter) => rebind(filter),
            None => *target = Some(TargetFilter::ScopedPlayer),
        },
        Effect::Draw { target, .. }
        | Effect::Discard { target, .. }
        | Effect::Mill { target, .. } => rebind(target),
        Effect::Token { owner, .. } => rebind(owner),
        // CR 109.5: inside "each <scope> who does, <body>", an AddRestriction
        // consequence ("can't attack you … during their next turn" — The Second
        // Doctor, City Hall) affects the SCOPED player. The shared body recognizer
        // (`try_parse_that_player_cant_attack_prohibition`) emits the parent-target
        // placeholder; rebind it to `ScopedPlayer` so it resolves to the
        // per-iteration player, not a (nonexistent) parent target.
        Effect::AddRestriction {
            restriction:
                GameRestriction::ProhibitActivity {
                    affected_players, ..
                },
        } => {
            if matches!(
                affected_players,
                RestrictionPlayerScope::ParentTargetedPlayer
                    | RestrictionPlayerScope::TargetedPlayer
            ) {
                *affected_players = RestrictionPlayerScope::ScopedPlayer;
            }
        }
        _ => {}
    }
}

/// CR 109.4 + CR 109.5: Parse the shared "who controls [comparator] [count]
/// [type-phrase]" control predicate — the comparison axis (presence or
/// comparative) plus the controlled-permanent filter.
/// Returns `(Comparator, QuantityExpr, TargetFilter, remainder)` where
/// `remainder` is the text after the consumed object sub-phrase, or `None` when
/// no control predicate is present (or the object resolves to the
/// everything-matching `TargetFilter::Any`, which must not silently match every
/// permanent).
///
/// Three presence/comparison classes are recognized as a single parameterized
/// `(Comparator, QuantityExpr)` pair:
/// - "controls"/"control" → `(GE, Fixed(1))` (at least one matching permanent).
/// - "doesn't/does not/don't/do not control" → `(EQ, Fixed(0))` (none).
/// - "controls/control more <type> than you" → `(GT, Ref(ObjectCount {
///   filter: <type>.controller(You) }))` — strictly more than the effect
///   controller's own count of the same type (CR 109.5 — "you" is the controller
///   of the object the ability is on). The carried `filter` is the BARE type
///   (no controller axis); the per-candidate control relationship is enforced at
///   runtime by `player_control_count_compares`.
///
/// The object sub-phrase ("an Elf", "a creature with power 4 or greater")
/// delegates to the shared `parse_type_phrase_with_ctx` combinator — no bespoke
/// string matching. This is the DRY core shared by the "each opponent who
/// controls …" subject path (`strip_controls_permanent_clause`) and the "the
/// number of opponents who control …" quantity path (`oracle_quantity.rs`).
pub(crate) fn parse_controls_permanent_object<'a>(
    rest: &'a str,
    ctx: &mut ParseContext,
) -> Option<(Comparator, QuantityExpr, TargetFilter, &'a str)> {
    let lower = rest.to_lowercase();
    // Comparative form tried FIRST: "who controls more <type> than you".
    // Mirrors `oracle_nom::condition::parse_that_player_controls_more_comparison`:
    // consume the verb prefix, then split the original-case remainder on
    // " than you" so the isolated type text and the trailing remainder both stay
    // in original case. `split_once_on_lower` is a structural boundary lookup
    // (permitted), not parsing dispatch.
    if let Some(((), after_verb)) = nom_on_lower(rest, &lower, |i| {
        let (i, _) = tag("who ").parse(i)?;
        let (i, _) = alt((tag("controls more "), tag("control more "))).parse(i)?;
        Ok((i, ()))
    }) {
        let after_verb_lower = after_verb.to_lowercase();
        if let Some((type_text, comparative_remainder)) =
            split_once_on_lower(after_verb, &after_verb_lower, " than you")
        {
            let (bare_filter, _) = parse_type_phrase_with_ctx(type_text, ctx);
            if matches!(bare_filter, TargetFilter::Any) {
                return None;
            }
            // CR 109.5: the controller's own count uses a `You`-controlled filter.
            let you_count = match &bare_filter {
                TargetFilter::Typed(tf) => QuantityExpr::Ref {
                    qty: QuantityRef::ObjectCount {
                        filter: TargetFilter::Typed(tf.clone().controller(ControllerRef::You)),
                    },
                },
                // Non-typed filters cannot carry a controller axis; reject rather
                // than silently mis-counting.
                _ => return None,
            };
            return Some((
                Comparator::GT,
                you_count,
                bare_filter,
                comparative_remainder,
            ));
        }
    }

    // CR 109.4 + CR 109.5: superlative "who controls the most <type>" — each
    // player tied for the GREATEST count of <type> permanents they control. The
    // cross-player extremum reuses `QuantityRef::ControlledByEachPlayer { Max }`
    // (the same building block the quantity path
    // `parse_controlled_by_extremum_player` uses). Placed BEFORE the bare-presence
    // "controls " arm so "controls the most creatures" is not mis-parsed as
    // presence (GE, 1) leaving "the most creatures" unconsumed. GE vs the Max is
    // equivalent to EQ here (CR 107.1 integers: no player's count exceeds the max)
    // and selects exactly the tied-for-most set; all-equal (incl. all-at-0) means
    // everyone, per the Tectonic Hellion "if everyone controls the same number,
    // everyone …" ruling.
    if let Some(((), after_verb)) = nom_on_lower(rest, &lower, |i| {
        let (i, _) = tag("who ").parse(i)?;
        let (i, _) = alt((tag("controls the most "), tag("control the most "))).parse(i)?;
        Ok((i, ()))
    }) {
        let (filter, remainder) = parse_type_phrase_with_ctx(after_verb, ctx);
        // Honest-red guard: reject Any / content-empty filters so a type phrase
        // that fails to parse stays Unimplemented rather than building a bogus
        // extremum. Both `filter` sides stay BARE (controller-less): the resolvers
        // (`player_control_count_compares` and `ControlledByEachPlayer`) apply the
        // per-player controller gate themselves — a `.controller(You)` here would
        // double-gate and mis-count.
        let has_content = matches!(&filter, TargetFilter::Typed(tf)
            if !tf.type_filters.is_empty() || !tf.properties.is_empty());
        if !has_content {
            return None;
        }
        let count = QuantityExpr::Ref {
            qty: QuantityRef::ControlledByEachPlayer {
                filter: filter.clone(),
                aggregate: AggregateFunction::Max,
            },
        };
        return Some((Comparator::GE, count, filter, remainder));
    }

    // "who controls " / "who doesn't control " — one alt() arm per presence axis.
    // Both singular ("each opponent who controls") and plural ("opponents who
    // control") subject-verb agreement forms are accepted: the present/absent
    // axis is identical regardless of grammatical number. Negative forms are
    // longest-match-first so "doesn't/does not/don't/do not control" win before
    // the bare affirmative; "controls " precedes "control " so the singular form
    // is not split. `(GE, Fixed(1))` ≡ old `Controls` (count >= 1);
    // `(EQ, Fixed(0))` ≡ old `ControlsNone` (count == 0).
    let ((comparator, count), after_verb) = nom_on_lower(rest, &lower, |i| {
        preceded(
            tag("who "),
            alt((
                value(
                    (Comparator::EQ, QuantityExpr::Fixed { value: 0 }),
                    tag("doesn't control "),
                ),
                value(
                    (Comparator::EQ, QuantityExpr::Fixed { value: 0 }),
                    tag("does not control "),
                ),
                value(
                    (Comparator::EQ, QuantityExpr::Fixed { value: 0 }),
                    tag("don't control "),
                ),
                value(
                    (Comparator::EQ, QuantityExpr::Fixed { value: 0 }),
                    tag("do not control "),
                ),
                value(
                    (Comparator::GE, QuantityExpr::Fixed { value: 1 }),
                    tag("controls "),
                ),
                value(
                    (Comparator::GE, QuantityExpr::Fixed { value: 1 }),
                    tag("control "),
                ),
            )),
        )
        .parse(i)
    })?;
    // The object sub-phrase is consumed by the shared type-phrase combinator.
    let (filter, remainder) = parse_type_phrase_with_ctx(after_verb, ctx);
    if matches!(filter, TargetFilter::Any) {
        return None;
    }
    Some((comparator, count, filter, remainder))
}

/// CR 109.4 + CR 109.5: Strip a "who controls [comparator] [count]
/// [type-phrase]" relative clause that follows an "each opponent"/"each player"
/// subject. Returns the `PlayerFilter::ControlsCount` scope (carrying the base
/// subject's relation, the controlled-permanent filter, and the comparator/count
/// pair) and the verb-phrase remainder. Returns `None` when no control clause is
/// present.
///
/// Delegates the control predicate to the shared
/// `parse_controls_permanent_object` core; this function adds the subject-path
/// concerns: deriving the relation from the base subject and enforcing a
/// non-empty verb-phrase residual.
fn strip_controls_permanent_clause(
    base: &PlayerFilter,
    rest: &str,
) -> Option<(PlayerFilter, String)> {
    use crate::types::ability::PlayerRelation;
    // The base subject only contributes its player relation; HighestSpeed and
    // any non-relational base are out of scope for a controls qualifier.
    let relation = match base {
        PlayerFilter::Opponent => PlayerRelation::Opponent,
        PlayerFilter::All => PlayerRelation::All,
        _ => return None,
    };
    // Match today's no-ctx behaviour for the subject path.
    let mut ctx = ParseContext::default();
    let (comparator, count, filter, remainder) = parse_controls_permanent_object(rest, &mut ctx)?;
    let verb_phrase = remainder.trim_start();
    if verb_phrase.is_empty() {
        return None;
    }
    Some((
        PlayerFilter::ControlsCount {
            relation,
            filter,
            comparator,
            count: Box::new(count),
        },
        verb_phrase.to_string(),
    ))
}

/// CR 122.1 + CR 122.2 + CR 402.1 + CR 403.3: Strip a "who has/have N or more
/// <attribute>" relative clause after an "each opponent"/"each player" subject.
/// Covers counters, hand size, cards drawn, and battlefield-entry predicates via
/// `parse_player_attribute_attr_clause`. Returns `PlayerFilter::PlayerAttribute`
/// and the verb-phrase remainder.
fn strip_player_attribute_clause(
    base: &PlayerFilter,
    rest: &str,
) -> Option<(PlayerFilter, String)> {
    use crate::types::ability::PlayerRelation;
    let relation = match base {
        PlayerFilter::Opponent => PlayerRelation::Opponent,
        PlayerFilter::All => PlayerRelation::All,
        _ => return None,
    };
    let lower = rest.to_lowercase();
    let ((attr, count), remainder) =
        nom_on_lower(rest, &lower, parse_player_attribute_attr_clause)?;
    let verb_phrase = remainder.trim_start();
    if verb_phrase.is_empty() {
        return None;
    }
    Some((
        PlayerFilter::PlayerAttribute {
            relation,
            attr: Box::new(attr),
            comparator: Comparator::GE,
            value: Box::new(QuantityExpr::Fixed { value: count }),
        },
        verb_phrase.to_string(),
    ))
}

/// CR 101.4 + CR 608.2d: a player-set restriction keyed on the number each
/// player secretly chose during this resolution. Returns the comparator and the
/// extremum to compare against:
///
/// - `"who chose the highest number"` → `(EQ, Max)` — Life at Stake.
/// - `"who didn't choose the lowest number"` → `(NE, Min)` — Wheel of Misfortune.
/// - `"with the highest number"` → `(EQ, Max)` — Menacing Ogre's participial form.
/// - `"who chose that number"` → `(EQ, anaphor)` — Wheel of Misfortune's damage
///   recipient, where "that number" refers back to the extremum the same clause
///   already named as its amount.
///
/// Composed by axis (polarity × verb form × extremum), not enumerated as
/// permutations, so a new phrasing on any one axis costs one `tag`. `anaphor`
/// supplies the referent for "that number"; `None` disables that arm, which is
/// correct wherever no extremum is in scope to anaphor back to.
/// A trailing `" of "` disqualifies EVERY arm: "the highest number OF cards in
/// hand" is a counting phrase over a population, not a reference to a chosen
/// number. This is the same guard the quantity-side `parse_extreme_chosen_number_ref`
/// carries — without it here, "choose an opponent with the highest number of
/// cards in hand" binds a chosen-number comparison to a card that has no choice
/// in it, the Custodi Peacekeeper failure one layer over.
pub(crate) fn parse_chosen_number_restriction(
    i: &str,
    anaphor: Option<AggregateFunction>,
) -> OracleResult<'_, (Comparator, AggregateFunction)> {
    terminated(
        parse_chosen_number_restriction_body(anaphor),
        not(tag(" of ")),
    )
    .parse(i)
}

fn parse_chosen_number_restriction_body(
    anaphor: Option<AggregateFunction>,
) -> impl FnMut(&str) -> OracleResult<'_, (Comparator, AggregateFunction)> {
    move |i: &str| {
        alt((
            map(
                (
                    tag("who "),
                    alt((
                        value(
                            Comparator::NE,
                            alt((tag("didn't "), tag("did not "), tag("doesn't "))),
                        ),
                        nom::combinator::success(Comparator::EQ),
                    )),
                    alt((tag("chose "), tag("chooses "), tag("choose "))),
                    tag("the "),
                    nom_quantity::parse_chosen_number_extremum,
                    nom_quantity::parse_chosen_number_noun,
                ),
                |(_, comparator, _, _, aggregate, ())| (comparator, aggregate),
            ),
            map(
                terminated(
                    preceded(tag("with the "), nom_quantity::parse_chosen_number_extremum),
                    nom_quantity::parse_chosen_number_noun,
                ),
                |aggregate| (Comparator::EQ, aggregate),
            ),
            nom::combinator::map_opt(
                terminated(
                    tag("who chose that"),
                    nom_quantity::parse_chosen_number_noun,
                ),
                move |_| anaphor.map(|aggregate| (Comparator::EQ, aggregate)),
            ),
        ))
        .parse(i)
    }
}

/// CR 101.4 + CR 608.2d: the `PlayerFilter` selecting the players whose
/// secretly-chosen number compares (under `comparator`) to the cross-player
/// extremum of the same scalar.
///
/// Reuses the existing parameterized [`PlayerFilter::PlayerAttribute`] rather
/// than minting a "chose the highest number" variant: the per-candidate scalar
/// is [`QuantityRef::PlayerChosenNumber`] under `ScopedPlayer` (read off each
/// candidate by `effects::candidate_player_scalar`), and the threshold is the
/// same reference under `AllPlayers { aggregate }`. "Didn't choose the lowest"
/// is therefore just `Comparator::NE` — no negation wrapper, no `AllExcept`.
pub(crate) fn chosen_number_player_filter(
    relation: crate::types::ability::PlayerRelation,
    comparator: Comparator,
    aggregate: AggregateFunction,
) -> PlayerFilter {
    use crate::types::ability::PlayerScope;
    PlayerFilter::PlayerAttribute {
        relation,
        attr: Box::new(QuantityRef::PlayerChosenNumber {
            player: PlayerScope::ScopedPlayer,
        }),
        comparator,
        value: Box::new(QuantityExpr::Ref {
            qty: QuantityRef::PlayerChosenNumber {
                player: PlayerScope::AllPlayers {
                    aggregate,
                    exclude: None,
                },
            },
        }),
    }
}

/// CR 101.4 + CR 608.2d + CR 109.5: Strip a chosen-number relative clause after
/// an "each player" / "each opponent" subject ("Each player who didn't choose
/// the lowest number discards their hand"). Returns the narrowed scope and the
/// verb-phrase remainder. Structural sibling of `strip_player_attribute_clause`:
/// same `PlayerAttribute` shape, different per-candidate scalar. Like every
/// relative clause in this dispatcher the clause MUST be consumed and reflected
/// in the scope — dropping it would apply the effect to every player.
fn strip_chosen_number_clause(base: &PlayerFilter, rest: &str) -> Option<(PlayerFilter, String)> {
    use crate::types::ability::PlayerRelation;
    let relation = match base {
        PlayerFilter::Opponent => PlayerRelation::Opponent,
        PlayerFilter::All => PlayerRelation::All,
        _ => return None,
    };
    let lower = rest.to_lowercase();
    let ((comparator, aggregate), remainder) =
        nom_on_lower(rest, &lower, |i| parse_chosen_number_restriction(i, None))?;
    let verb_phrase = remainder.trim_start();
    if verb_phrase.is_empty() {
        return None;
    }
    Some((
        chosen_number_player_filter(relation, comparator, aggregate),
        verb_phrase.to_string(),
    ))
}

/// CR 608.2c + CR 109.5: Strip a "who [verb]ed … this way" relative clause after
/// an "each opponent"/"each player" subject. Returns
/// `PlayerFilter::PerformedActionThisWay` (carrying the base subject's relation
/// and the performed action, keyed at runtime on the `player_actions_this_way`
/// ledger that each settled search/investigate/draw populates) and the
/// verb-phrase remainder. Returns `None` when no such clause is present.
///
/// The this-way verb table is delegated whole to `parse_who_action_this_way`
/// (oracle_quantity.rs) — the same authority the quantity path
/// (`parse_action_this_way`) uses — so search, investigate, and draw stay one
/// building block across both the quantity and subject scopes. This function
/// adds only the subject-path concerns: deriving the relation from the base
/// subject and enforcing a non-empty verb-phrase residual. Kwain, Itinerant
/// Meddler ("each player who drew a card this way gains 1 life") is the
/// subject-scope sibling of Cut a Deal's quantity-path "for each opponent who
/// drew a card this way".
fn strip_performed_action_this_way_clause(
    base: &PlayerFilter,
    rest: &str,
) -> Option<(PlayerFilter, String)> {
    use crate::types::ability::PlayerRelation;
    let relation = match base {
        PlayerFilter::Opponent => PlayerRelation::Opponent,
        PlayerFilter::All => PlayerRelation::All,
        PlayerFilter::Controller
        | PlayerFilter::DefendingPlayer
        | PlayerFilter::OpponentLostLife
        | PlayerFilter::OpponentGainedLife
        | PlayerFilter::HasLostTheGame
        | PlayerFilter::OpponentDealtDamage { .. }
        | PlayerFilter::OpponentAttacked { .. }
        | PlayerFilter::OpponentAttackingEnchantedPlayer
        | PlayerFilter::AllExcept { .. }
        | PlayerFilter::HighestSpeed
        | PlayerFilter::ZoneChangedThisWay
        | PlayerFilter::PerformedActionThisWay { .. }
        | PlayerFilter::OwnersOfCardsExiledBySource
        | PlayerFilter::TriggeringPlayer
        | PlayerFilter::OpponentOtherThanTriggering
        | PlayerFilter::OpponentOfTriggeringPlayer
        | PlayerFilter::OpponentOfTriggeringPlayerNotAttacked
        | PlayerFilter::VotedFor { .. }
        | PlayerFilter::ParentObjectTargetController
        | PlayerFilter::ControlsCount { .. }
        | PlayerFilter::PlayerAttribute { .. }
        | PlayerFilter::ChosenPlayer { .. }
        | PlayerFilter::ParentObjectTargetOwner
        | PlayerFilter::TrackedSetPossessor { .. } => return None,
    };
    let (remainder, action) =
        crate::parser::oracle_quantity::parse_who_action_this_way(rest).ok()?;
    let verb_phrase = remainder.trim_start();
    if verb_phrase.is_empty() {
        return None;
    }
    Some((
        PlayerFilter::PerformedActionThisWay { relation, action },
        verb_phrase.to_string(),
    ))
}

fn strip_linked_exile_owner_subject(text: &str) -> (Option<PlayerFilter>, String) {
    let lower = text.to_lowercase();
    let scope_rest = nom_on_lower(text, &lower, |i| {
        alt((
            value(
                PlayerFilter::OwnersOfCardsExiledBySource,
                tag::<_, _, OracleError<'_>>("the exiled card's owner "),
            ),
            value(
                PlayerFilter::OwnersOfCardsExiledBySource,
                tag("the exiled cards' owners "),
            ),
            // CR 406.2 + CR 610.3: "the owner of each card exiled with <source> "
            // — the source-linked exile cleanup subject (Trial of a Time Lord IV:
            // "the owner of each card exiled with ~ puts that card on the bottom
            // of their library"). The self-ref token is `~` after normalization,
            // or the literal "this saga" pre-normalization; compose the prefix
            // with the source token rather than verbatim-matching the card name.
            value(
                PlayerFilter::OwnersOfCardsExiledBySource,
                preceded(
                    tag("the owner of each card exiled with "),
                    (alt((tag("~"), tag("this saga"))), tag(" ")),
                ),
            ),
        ))
        .parse(i)
    });
    let Some((scope, rest)) = scope_rest else {
        return (None, text.to_string());
    };

    let rest_lower = rest.trim().to_lowercase();
    if alt((
        tag::<_, _, OracleError<'_>>("can't"),
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

    (Some(scope), subject::deconjugate_verb(rest))
}

/// Parse the player noun used by damage-to-players phrases.
/// Shared by simple `each player/opponent` damage routing and compound
/// `each opponent and each creature ...` damage clauses.
pub(super) fn parse_damage_player_scope(
    input: &str,
) -> nom::IResult<&str, PlayerFilter, OracleError<'_>> {
    alt((
        value(
            PlayerFilter::Opponent,
            alt((tag::<_, _, OracleError<'_>>("opponent"), tag("foe"))),
        ),
        value(PlayerFilter::All, tag("player")),
    ))
    .parse(input)
}

/// Parse an exact `each player` / `each opponent` / `each foe` / `each other opponent`
/// / `each other player` damage scope.
/// Returns `None` for compound phrases so dedicated compound parsers can handle them.
///
/// CR 120.3 + CR 603.2c: "each other opponent" anaphors back to the triggering
/// opponent named in the preceding "deals combat damage to an opponent" clause,
/// so the dispatch routes to `OpponentOtherThanTriggering` (a `PlayerFilter`
/// variant that excludes both the controller and the triggering player).
/// "each other player" excludes the controller (the only "other" antecedent
/// available outside trigger context) and reduces to plain `Opponent`.
pub(crate) fn parse_damage_each_player_scope(text: &str) -> Option<PlayerFilter> {
    let (filter, rest) = parse_damage_each_player_scope_with_remainder(text)?;
    rest.chars()
        .all(|c| c.is_ascii_whitespace() || c.is_ascii_punctuation())
        .then_some(filter)
}

/// CR 101.4 + CR 120.3 + CR 608.2d: an all-consuming damage-recipient scope
/// narrowed by a chosen-number relative clause — "each player who chose that
/// number" (Wheel of Misfortune), "each player who chose the highest number".
///
/// `anaphor` is the extremum the enclosing clause already named as its damage
/// amount, which is what an anaphoric "that number" refers to; pass `None` where
/// the amount is not a chosen-number extremum, and the anaphoric arm declines
/// (so the clause falls through to the unnarrowed scopes instead of silently
/// binding the wrong referent).
fn parse_damage_each_chosen_number_scope(
    text: &str,
    anaphor: Option<AggregateFunction>,
) -> Option<PlayerFilter> {
    use crate::types::ability::PlayerRelation;
    let (rest, base) = preceded(tag("each "), parse_damage_player_scope)
        .parse(text)
        .ok()?;
    let relation = match base {
        PlayerFilter::Opponent => PlayerRelation::Opponent,
        PlayerFilter::All => PlayerRelation::All,
        _ => return None,
    };
    let (rest, (comparator, aggregate)) =
        preceded(multispace1, |i| parse_chosen_number_restriction(i, anaphor))
            .parse(rest)
            .ok()?;
    rest.chars()
        .all(|c| c.is_ascii_whitespace() || c.is_ascii_punctuation())
        .then(|| chosen_number_player_filter(relation, comparator, aggregate))
}

/// CR 608.2c: the cross-player extremum a quantity names, when it is one. This
/// is the referent an anaphoric "that number" in the same clause points back to
/// — read off the already-parsed AST rather than re-matching Oracle text.
fn chosen_number_extremum_of(amount: &QuantityExpr) -> Option<AggregateFunction> {
    match amount {
        QuantityExpr::Ref {
            qty:
                QuantityRef::PlayerChosenNumber {
                    player: crate::types::ability::PlayerScope::AllPlayers { aggregate, .. },
                },
        } => Some(*aggregate),
        _ => None,
    }
}

/// CR 120.2b + CR 120.3 + CR 102.2: leading "each opponent/player/foe/other
/// opponent/other player" damage scope, returning the matched filter AND the
/// unconsumed remainder. Unlike `parse_damage_each_player_scope` it is NOT
/// all-consuming — used only by the multi-target damage CHAIN primary, which
/// hands the trailing " and M damage to ..." segment back to the loop.
fn parse_damage_each_player_scope_with_remainder(text: &str) -> Option<(PlayerFilter, &str)> {
    let (rest, filter) = preceded(
        tag("each "),
        alt((
            value(
                PlayerFilter::OpponentOtherThanTriggering,
                alt((
                    tag::<_, _, OracleError<'_>>("other opponent"),
                    tag("other foe"),
                )),
            ),
            value(PlayerFilter::Opponent, tag("other player")),
            parse_damage_player_scope,
        )),
    )
    .parse(text)
    .ok()?;
    Some((filter, rest))
}

pub(super) fn strip_leading_duration(text: &str) -> Option<(Duration, &str)> {
    let lower = text.to_lowercase();
    // Leading "<duration>, <effect>" — the phrase→`Duration` mapping is owned
    // by the single duration grammar (`oracle_nom::duration::parse_duration`);
    // this wrapper owns only the leading position and the ", " clause split.
    if let Some((duration, rest)) = nom_on_lower(text, &lower, |i| {
        terminated(parse_duration, tag(", ")).parse(i)
    }) {
        return Some((duration, rest.trim()));
    }

    // CR 611.2b: "For as long as [condition], [effect]" — leading duration
    // prefix. The condition is bounded by the first ", " (the generic branch
    // above can't split it because the condition grammar is clause-final);
    // its mapping is delegated to the duration grammar's condition table.
    if let Ok((rest, _)) = tag::<_, _, OracleError<'_>>("for as long as ").parse(lower.as_str()) {
        // Split "condition, effect_body" on the first ", " delimiter.
        if let Ok((effect_body, condition_text)) =
            terminated(take_until(", "), tag::<_, _, OracleError<'_>>(", ")).parse(rest)
        {
            if let Ok((_, dur)) = parse_for_as_long_as_condition(condition_text) {
                let prefix_len = "for as long as ".len() + condition_text.len() + ", ".len();
                return Some((dur, text[prefix_len..].trim()));
            }
            let _ = effect_body; // consumed by combinator; unused here
        }
    }

    None
}

pub(crate) fn strip_trailing_duration(text: &str) -> (&str, Option<Duration>) {
    // Oracle sentences often end with a period before duration stripping runs
    // (e.g. Shifting Woodland: "... until end of turn. Activate only if ...").
    let text = text.trim();
    let duration_text = text.trim_end_matches('.').trim();
    let lower = duration_text.to_lowercase();
    if target_relative_clause_owns_suffix(lower.as_str())
        || player_lookback_relative_clause_owns_suffix(lower.as_str())
        || cant_be_activated_clause_owns_tapped_suffix(lower.as_str())
    {
        return (text, None);
    }
    // CR 611.2 + CR 611.2b: trailing duration clause. The phrase→`Duration`
    // mapping is owned by the single duration grammar
    // (`oracle_nom::duration::parse_duration`); this wrapper owns only WHERE
    // the clause sits — a word-boundary scan for the position whose remainder
    // is entirely a duration phrase — plus two disambiguation guards: a bare
    // duration phrase with no preceding clause is not a suffix, and a
    // "this turn" suffix can be owned by a per-turn quantity clause instead
    // (for example, "where X is the number of tokens you created this turn"),
    // in which case it belongs to the quantity grammar, not to the outer
    // effect duration.
    if let Some((before, duration, _)) =
        nom_primitives::scan_preceded(&lower, |i| terminated(parse_duration, eof).parse(i))
    {
        let quantity_owns_suffix = all_consuming(tag::<_, _, OracleError<'_>>("this turn"))
            .parse(&lower[before.len()..])
            .is_ok()
            && quantity_clause_owns_this_turn_suffix(&lower);
        if !before.is_empty() && !quantity_owns_suffix {
            return (
                duration_text[..before.len()]
                    .trim_end()
                    .trim_end_matches(',')
                    .trim(),
                Some(duration),
            );
        }
    }

    // CR 611.2a: Duration mid-clause before a trailing conjunct, variable
    // definition, or alternative expiry (", or " / ", where " boundaries).
    // End-of-string durations are handled above; the text after the duration
    // phrase is intentionally dropped, preserving the legacy table behavior.
    // Do NOT treat " unless " as a boundary here — unless-pay parsers
    // (`try_parse_unless_player_have_deal_damage`, `extract_resolution_unless_pay_modifier`)
    // own that tail and must see the full phrase.
    if let Some((before, duration, _)) = nom_primitives::scan_preceded(&lower, |i| {
        terminated(
            parse_duration,
            peek(alt((
                tag::<_, _, OracleError<'_>>(", or "),
                tag(", where "),
            ))),
        )
        .parse(i)
    }) {
        // CR 400.7 + CR 700.4: A "this turn" that belongs to a per-turn VALUE
        // quantity in the preceding clause ("loses life equal to the total power
        // of Daleks that died this turn, or destroy all non-Dalek creatures") is
        // NOT an effect duration — stripping it here would amputate the ", or …"
        // alternative-effect branch of a binary choice. Mirror the end-of-string
        // handler's quantity-ownership guard so both strippers defer to the
        // quantity grammar identically.
        let this_turn_end = before.len() + "this turn".len();
        let quantity_owns_suffix =
            lower.get(before.len()..this_turn_end).is_some_and(|seg| {
                all_consuming(tag::<_, _, OracleError<'_>>("this turn"))
                    .parse(seg)
                    .is_ok()
            }) && quantity_clause_owns_this_turn_suffix(&lower[..this_turn_end]);
        if !before.is_empty() && !quantity_owns_suffix {
            return (duration_text[..before.len()].trim_end(), Some(duration));
        }
    }

    (text, None)
}

fn quantity_clause_owns_this_turn_suffix(lower: &str) -> bool {
    where_x_quantity_clause_owns_this_turn_suffix(lower)
        || for_each_quantity_clause_owns_this_turn_suffix(lower)
        || value_quantity_clause_owns_this_turn_suffix(lower)
}

/// CR 400.7 + CR 700.4: True when the trailing " this turn" is part of a dynamic
/// VALUE quantity (e.g. "loses life equal to the total power of Daleks that died
/// this turn") rather than an effect duration. The end-of-string and mid-clause
/// duration strippers both consult this guard so a per-turn quantity's "this
/// turn" is never amputated as an outer `UntilEndOfTurn`. Generalizes the
/// `where x is` / `for each` ownership checks to the "equal to <quantity ...
/// this turn>" form by reusing the shared `parse_quantity_ref` building block:
/// the quantity owns the suffix iff some word-boundary tail of the clause parses
/// as a `QuantityRef` that consumes exactly through " this turn".
fn value_quantity_clause_owns_this_turn_suffix(lower: &str) -> bool {
    // The clause spans from the start through the first " this turn" suffix.
    // Anchor on the LAST " this turn" — that is the suffix the duration stripper
    // is testing (the trailing one for the end-of-string handler, the one before
    // ", or "/", where " for the mid-clause handler, since callers slice their
    // input to end there). An earlier per-turn quantity ("where X is the life
    // you've lost this turn, then … +1/+1 this turn") must NOT mask the OUTER
    // trailing duration on a later clause.
    // allow-noncombinator: anchor slice on the last " this turn" for the scan_at_word_boundaries word-boundary scan below (Pattern 5), not parsing dispatch
    let Some(idx) = lower.rfind(" this turn") else {
        return false;
    };
    let clause = &lower[..idx + " this turn".len()];
    // Scan word boundaries (via the shared `scan_at_word_boundaries` combinator)
    // for a tail that parses fully as a dynamic quantity ending at " this turn";
    // the quantity owns the suffix iff one exists. `parse_quantity_ref` is a
    // whole-string match, so a successful tail necessarily consumes through
    // " this turn" (the end of `clause`). Mirrors the `where_x` / `for_each`
    // ownership helpers, generalized to any `QuantityRef`.
    nom_primitives::scan_at_word_boundaries(clause, |i| match parse_quantity_ref(i) {
        Some(_) => Ok((i, ())),
        None => Err(nom::Err::Error(OracleError::new(
            i,
            nom::error::ErrorKind::Fail,
        ))),
    })
    .is_some()
}

fn where_x_quantity_clause_owns_this_turn_suffix(lower: &str) -> bool {
    let Ok((where_clause, _)) = preceded(
        take_until::<_, _, OracleError<'_>>("where x is "),
        tag::<_, _, OracleError<'_>>("where x is "),
    )
    .parse(lower) else {
        return false;
    };
    let normalized = where_clause.trim();
    let Ok((_, quantity_before_this_turn)) = all_consuming(terminated(
        take_until::<_, _, OracleError<'_>>(" this turn"),
        tag::<_, _, OracleError<'_>>(" this turn"),
    ))
    .parse(normalized) else {
        return false;
    };
    let expression_end = quantity_before_this_turn.len() + " this turn".len();
    parse_where_x_quantity_expression(&normalized[..expression_end]).is_some()
}

fn for_each_quantity_clause_owns_this_turn_suffix(lower: &str) -> bool {
    let Ok((for_each_clause, _)) = preceded(
        take_until::<_, _, OracleError<'_>>(" for each "),
        tag::<_, _, OracleError<'_>>(" for each "),
    )
    .parse(lower) else {
        return false;
    };
    let normalized = for_each_clause.trim();
    let Ok((_, quantity_before_this_turn)) = all_consuming(terminated(
        take_until::<_, _, OracleError<'_>>(" this turn"),
        tag::<_, _, OracleError<'_>>(" this turn"),
    ))
    .parse(normalized) else {
        return false;
    };
    let expression_end = quantity_before_this_turn.len() + " this turn".len();
    parse_for_each_clause(&normalized[..expression_end]).is_some()
}

/// CR 608.2i + CR 611.2a: True when the trailing " this turn" belongs to a
/// controller-scoped *player look-back* relative clause on the target — e.g.
/// Admiral Beckett Brass's "target nonland permanent controlled by a player who
/// was dealt combat damage by three or more Pirates this turn" — rather than
/// being the effect's own duration. Without this guard `strip_trailing_duration`
/// amputates the "this turn" and (wrongly) stamps `Duration::UntilEndOfTurn` on
/// a control-change that is in fact permanent (CR 611.2a: a continuous
/// effect with no stated duration lasts until end of the game).
///
/// This is the "who"-introduced sibling of `target_relative_clause_owns_suffix`
/// (which recognizes "that"-introduced object-property clauses). It is
/// deliberately SELF-STANDING — it does NOT delegate to `parse_that_clause_suffix`
/// (whose vocabulary is object-property clauses like "that's enchanted", not the
/// "controlled by a player who <look-back verb>" player shape). It recognizes the
/// clause STRUCTURE (which owns the suffix), not its full semantics — the target
/// scope itself remains an over-broad coverage gap, but the duration is correct.
///
/// The positional discipline mirrors the quantity-ownership guards: the guard
/// fires only when the relative clause consumes *through* the trailing " this
/// turn" to end-of-input. A genuine OUTER duration after the relative clause
/// ("… who lost life this turn until end of turn") leaves a non-empty remainder,
/// so the guard declines and the real duration still strips.
pub(crate) fn player_lookback_relative_clause_owns_suffix(input: &str) -> bool {
    // Anchor on the LAST " who " so an earlier "who" (in an unrelated preceding
    // clause) cannot mask the outer duration on a later clause.
    // allow-noncombinator: rfind anchors the word-boundary slice for the nom scan below (Pattern 5), not parsing dispatch.
    let Some(who_idx) = input.rfind(" who ") else {
        return false;
    };
    let after_who = &input[who_idx + " who ".len()..];
    // A player look-back relative clause: a look-back verb phrase, then a tail
    // that ends exactly at " this turn" (the suffix the stripper is testing).
    // `alt` returns the first success, so longer matches must precede their own
    // prefixes ("was dealt combat damage " before "was dealt "), else the prefix
    // shadows the longer branch into dead code.
    let lookback_verb = alt((
        tag::<_, _, OracleError<'_>>("was dealt combat damage "),
        tag("was dealt "),
        tag("were dealt "),
        tag("lost life "),
        tag("gained life "),
        tag("has lost life "),
        tag("has gained life "),
        tag("controls "),
    ));
    let Ok((rest, _)) = preceded(lookback_verb, take_until(" this turn"))
        .and(tag::<_, _, OracleError<'_>>(" this turn"))
        .parse(after_who)
    else {
        return false;
    };
    // The relative clause must own the suffix: nothing but optional punctuation
    // may follow, so an outer duration ("… until end of turn") is not suppressed.
    (
        multispace0,
        opt(alt((tag::<_, _, OracleError<'_>>("."), tag(",")))),
        multispace0,
        eof,
    )
        .parse(rest)
        .is_ok()
}

/// CR 611.2b + CR 110.5 + CR 602.5: A "[subject] activated abilities can't be
/// activated for as long as &lt;it|that …&gt; remains tapped" restriction owns its
/// tapped-bound duration — the CantBeActivated arm
/// (`subject::tapped_bound_prohibition_duration`) binds it to the grant's TARGET
/// (`IsTapped { scope: Target }`), not the source. `strip_trailing_duration`
/// must therefore NOT peel the suffix (the generic duration grammar maps the
/// anaphoric "it remains tapped" to the SOURCE, the wrong object for this
/// clause). Scoped to the CantBeActivated class so ordinary control/copy "for as
/// long as it remains tapped" durations still strip normally (Braided Net).
fn cant_be_activated_clause_owns_tapped_suffix(input: &str) -> bool {
    let mentions_cant_be_activated = nom_primitives::scan_contains(input, "can't be activated")
        || nom_primitives::scan_contains(input, "can\u{2019}t be activated");
    mentions_cant_be_activated
        && nom_primitives::scan_at_word_boundaries(input, |i: &str| {
            let (i, _) = tag::<_, _, OracleError<'_>>("for as long as ").parse(i)?;
            let (i, _) = alt((
                tag("it"),
                tag("that creature"),
                tag("that permanent"),
                tag("that artifact"),
                tag("~"),
            ))
            .parse(i)?;
            let (i, _) = tag(" remains tapped").parse(i)?;
            Ok((i, ()))
        })
        .is_some()
}

fn target_relative_clause_owns_suffix(input: &str) -> bool {
    let Ok((relative_clause, _)) = take_until::<_, _, OracleError<'_>>(" that ").parse(input)
    else {
        return false;
    };
    let Some((_, consumed)) = parse_that_clause_suffix(relative_clause, None) else {
        return false;
    };
    let remaining = &relative_clause[consumed..];
    (
        multispace0,
        opt(alt((tag::<_, _, OracleError<'_>>("."), tag(",")))),
        multispace0,
        eof,
    )
        .parse(remaining)
        .is_ok()
}

/// CR 603.7a: Strip temporal suffix indicating a delayed trigger condition.
/// Parallel to `strip_trailing_duration()` but for one-shot deferred effects.
/// Duration = "effect is active during this period"; DelayedTriggerCondition = "fire once at this
/// future point".
///
/// CR 505.1: "your next main phase" binds the trigger to the ability's
/// controller — the `player` field is a compile-time placeholder
/// (`PlayerId(0)`) rewritten to `ability.controller` at resolution time in
/// `effects::delayed_trigger::resolve`. Mirrors the existing
/// `RestrictionScope::SourcesControlledBy` placeholder pattern.
pub(super) fn strip_temporal_suffix(text: &str) -> (&str, Option<DelayedTriggerCondition>) {
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
        // CR 603.7a: "the next turn's upkeep" is the natural-language variant
        // of "the next upkeep" — both reference the very next upkeep step that
        // occurs (Arcane Denial, Bag of Holding family; ~15 cards).
        (
            " at the beginning of the next turn's upkeep",
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
        // CR 505.1: Precombat main phase of the controller. "Your" binds
        // `player` to the ability's controller; resolved at resolve time.
        (
            " at the beginning of your next main phase",
            DelayedTriggerCondition::AtNextPhaseForPlayer {
                phase: Phase::PreCombatMain,
                player: crate::types::player::PlayerId(0),
                gate: crate::types::ability::TurnGate::None,
            },
        ),
        // CR 505.1 + CR 603.7a: Symmetric to the prefix form at
        // `strip_temporal_prefix`. Greasefang's "return it to its owner's hand
        // at the beginning of your next end step" uses this suffix shape; the
        // player placeholder is rewritten to `ability.controller` at resolve
        // time alongside the main-phase and upkeep variants.
        (
            " at the beginning of your next end step",
            DelayedTriggerCondition::AtNextPhaseForPlayer {
                phase: Phase::End,
                player: crate::types::player::PlayerId(0),
                gate: crate::types::ability::TurnGate::None,
            },
        ),
        // CR 513.2 + CR 603.7a: reordered "…, sacrifice that token at the
        // beginning of the end step on your next turn" (Kav Landseeker). Suffix
        // companion of the prefix arm. Skip-current via `AfterCreationTurn`
        // (rewritten to `After(creation_turn)` at resolve). Diverges from the
        // Greasefang "your next end step" suffix above ("the end step on" vs
        // "your next end step"); neither suffix is a tail of the other.
        (
            " at the beginning of the end step on your next turn",
            DelayedTriggerCondition::AtNextPhaseForPlayer {
                phase: Phase::End,
                player: crate::types::player::PlayerId(0),
                gate: crate::types::ability::TurnGate::AfterCreationTurn,
            },
        ),
        // CR 603.7a + CR 104.3e: anaphoric "that turn's end step" — the extra
        // turn granted by the parent clause (the controller's next turn), so
        // the controller's next end step. Suffix companion of the prefix arm
        // in `strip_temporal_prefix`. Used by Final Fortune / Last Chance /
        // Warrior's Oath / Chance for Glory.
        (
            " at the beginning of that turn's end step",
            DelayedTriggerCondition::AtNextPhaseForPlayer {
                phase: Phase::End,
                player: crate::types::player::PlayerId(0),
                gate: crate::types::ability::TurnGate::None,
            },
        ),
        (
            " at the beginning of your next upkeep",
            DelayedTriggerCondition::AtNextPhaseForPlayer {
                phase: Phase::Upkeep,
                player: crate::types::player::PlayerId(0),
                gate: crate::types::ability::TurnGate::None,
            },
        ),
        // CR 514.3a + CR 603.7a: "at the beginning of the next cleanup step"
        // (Bounty of the Hunt and the class of temporary-counter effects).
        (
            " at the beginning of the next cleanup step",
            DelayedTriggerCondition::AtNextPhase {
                phase: Phase::Cleanup,
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
pub(crate) fn strip_temporal_prefix(text: &str) -> (&str, Option<DelayedTriggerCondition>) {
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
            // CR 505.1 + CR 603.7a: "your next" binds the phase to the ability's
            // controller. `PlayerId(0)` is a placeholder rewritten at resolution
            // time in `effects::delayed_trigger::resolve`.
            value(
                DelayedTriggerCondition::AtNextPhaseForPlayer {
                    phase: Phase::Upkeep,
                    player: crate::types::player::PlayerId(0),
                    gate: crate::types::ability::TurnGate::None,
                },
                tag("at the beginning of your next upkeep, "),
            ),
            value(
                DelayedTriggerCondition::AtNextPhaseForPlayer {
                    phase: Phase::End,
                    player: crate::types::player::PlayerId(0),
                    gate: crate::types::ability::TurnGate::None,
                },
                tag("at the beginning of your next end step, "),
            ),
            // CR 513.2 + CR 603.7a: "at the beginning of the end step on your
            // next turn" — the WotC "skip the current turn's end step" wording
            // (Kav Landseeker). Distinct from "your next end step" above (which
            // fires the current end step, CR 513.2 does not back it up): this
            // arm diverges at "the end step on" vs "your next end step" and must
            // NOT be shadowed by it. `AfterCreationTurn` is rewritten to
            // `After(creation_turn)` in effects::delayed_trigger::resolve so the
            // current turn's end step is skipped.
            value(
                DelayedTriggerCondition::AtNextPhaseForPlayer {
                    phase: Phase::End,
                    player: crate::types::player::PlayerId(0),
                    gate: crate::types::ability::TurnGate::AfterCreationTurn,
                },
                tag("at the beginning of the end step on your next turn, "),
            ),
            // CR 603.7a + CR 104.3e: "at the beginning of that turn's end step"
            // is the anaphoric form used by the extra-turn-with-a-cost cards
            // (Final Fortune, Last Chance, Warrior's Oath, Chance for Glory):
            // "Take an extra turn after this one. At the beginning of that
            // turn's end step, you lose the game." "That turn" is the just-
            // granted extra turn — the controller's next turn — so this is the
            // controller's next end step, identical to the "your next end step"
            // arm above. PlayerId(0) is rewritten to ability.controller at
            // resolve time.
            value(
                DelayedTriggerCondition::AtNextPhaseForPlayer {
                    phase: Phase::End,
                    player: crate::types::player::PlayerId(0),
                    gate: crate::types::ability::TurnGate::None,
                },
                tag("at the beginning of that turn's end step, "),
            ),
            // CR 505.1 + CR 603.7a: "your next main phase" → PreCombatMain.
            // PlayerId(0) rewritten to ability.controller at resolve time
            // in effects::delayed_trigger::resolve.
            value(
                DelayedTriggerCondition::AtNextPhaseForPlayer {
                    phase: Phase::PreCombatMain,
                    player: crate::types::player::PlayerId(0),
                    gate: crate::types::ability::TurnGate::None,
                },
                tag("at the beginning of your next main phase, "),
            ),
            // CR 500.8 + CR 603.7a: "at the beginning of that combat" refers to an
            // additional combat phase just scheduled by the parent effect
            // (e.g., Moraug, Fury of Akoum's landfall trigger). The additional
            // combat is pushed as the very next phase, so we fire on the next
            // BeginCombat.
            value(
                DelayedTriggerCondition::AtNextPhase {
                    phase: Phase::BeginCombat,
                },
                tag("at the beginning of that combat, "),
            ),
            // CR 511.2 + CR 603.7a: "At this turn's next end of combat, …"
            // fires at the end-of-combat step of the current turn.
            // Covers Triton Tactics, Glyph of Doom, Gaze of the Gorgon,
            // Venomous Breath, and the full class of spells that schedule
            // an end-of-combat effect during resolution.
            value(
                DelayedTriggerCondition::AtNextPhase {
                    phase: Phase::EndCombat,
                },
                tag("at this turn's next end of combat, "),
            ),
            // CR 511.2 + CR 603.7a: bare "at end of combat, …" prefix — the
            // companion of the existing suffix arm in `strip_temporal_suffix`.
            // An attack/combat trigger whose effect body is deferred to the
            // end-of-combat step (Fortune, Loyal Steed: "Whenever Fortune
            // attacks while saddled, at end of combat, exile it and …").
            value(
                DelayedTriggerCondition::AtNextPhase {
                    phase: Phase::EndCombat,
                },
                tag("at end of combat, "),
            ),
            // CR 514.3a + CR 603.7a: "at the beginning of the next cleanup step, "
            value(
                DelayedTriggerCondition::AtNextPhase {
                    phase: Phase::Cleanup,
                },
                tag("at the beginning of the next cleanup step, "),
            ),
            // CR 603.7a + CR 701.31 + CR 901.11: inline delayed triggered ability
            // keyed to any planeswalk ("When a player planeswalks, …"). Mirrors The
            // Pandorica's `WhenNextEvent { Untaps, or LeavesBattlefield, Persistent }`
            // delayed trigger, differing only in the trigger event.
            // `Persistent` (CR 603.7b): no stated duration → survives across turns
            // until the next planeswalk. The stripped body ("those permanents phase
            // in") then parses to `Effect::PhaseIn { target: ParentTarget }`, and the
            // chosen permanents are frozen into `ability.targets` at delayed-trigger
            // creation by the existing `parent_target_snapshot` path.
            value(
                DelayedTriggerCondition::WhenNextEvent {
                    trigger: Box::new(crate::types::ability::TriggerDefinition::new(
                        crate::types::triggers::TriggerMode::Planeswalked {
                            role: crate::types::triggers::PlaneswalkRole::Any,
                        },
                    )),
                    or_trigger: None,
                    lifetime: crate::types::ability::DelayedTriggerLifetime::Persistent,
                },
                tag("when a player planeswalks, "),
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
pub(super) fn extract_put_counter_multi_target(text: &str) -> Option<MultiTargetSpec> {
    let lower = text.to_lowercase();
    let after = [
        "counter on up to ",
        "counters on up to ",
        "counter on each of up to ",
        "counters on each of up to ",
    ]
    .into_iter()
    .find_map(|marker| strip_after(&lower, marker))?;
    let (_, max) = parse_multi_target_count_expr(after).ok()?;
    Some(MultiTargetSpec::up_to(max))
}

pub(crate) fn extract_exact_target_multi_target(text: &str) -> Option<MultiTargetSpec> {
    let lower = text.to_lowercase();
    for verb in MULTI_TARGET_VERBS {
        let mut parser = terminated(tag::<_, _, OracleError<'_>>(*verb), tag(" "));
        let Ok((after_verb, _)) = parser.parse(lower.as_str()) else {
            continue;
        };
        let (count, _) = strip_exact_target_prefix(after_verb)?;
        return Some(MultiTargetSpec::exact(count));
    }
    None
}

/// CR 115.1d: Recover bounded multi-target counts from imperative text where the
/// verb precedes the count phrase — "return one or two target permanent cards
/// from your graveyard" (Trystan's Command mode 2). The targeted-action parser
/// strips the count via `parse_target` but does not attach `MultiTargetSpec`.
pub(crate) fn extract_bounded_target_multi_target(text: &str) -> Option<MultiTargetSpec> {
    let lower = text.to_lowercase();
    for verb in MULTI_TARGET_VERBS {
        let Ok((after_verb, _)) =
            terminated(tag::<_, _, OracleError<'_>>(*verb), tag(" ")).parse(lower.as_str())
        else {
            continue;
        };
        for (prefix, min, max) in [
            ("one or two ", 1usize, 2usize),
            ("one, two, or three ", 1, 3),
        ] {
            if let Ok((after_prefix, _)) = tag::<_, _, OracleError<'_>>(prefix).parse(after_verb) {
                if tag::<_, _, OracleError<'_>>("target ")
                    .parse(after_prefix)
                    .is_ok()
                {
                    return Some(MultiTargetSpec::fixed(min, max));
                }
            }
        }
    }
    None
}

/// CR 115.1d: Recover "up to N target …" from imperative text where the verb
/// precedes the count phrase — "tap up to four target permanents" (Elder
/// Deep-Fiend). The targeted-action parser strips the count via
/// `strip_optional_target_prefix` but does not attach `MultiTargetSpec`.
pub(crate) fn extract_optional_target_multi_target(text: &str) -> Option<MultiTargetSpec> {
    let lower = text.to_lowercase();
    for verb in MULTI_TARGET_VERBS {
        let Ok((after_verb, _)) =
            terminated(tag::<_, _, OracleError<'_>>(*verb), tag(" ")).parse(lower.as_str())
        else {
            continue;
        };
        let (_, multi_target) = strip_optional_target_prefix(after_verb);
        if multi_target.is_some() {
            return multi_target;
        }
    }
    None
}

/// CR 115.1d: Recover "verb up to N <filter>" when the phrase omits the word
/// "target" — "untap up to five lands" (Peregrine Drake). Delegates to
/// `strip_any_number_quantifier`, which is the single authority for that shape.
pub(crate) fn extract_verb_up_to_multi_target(text: &str) -> Option<MultiTargetSpec> {
    let (_, multi_target) = strip_any_number_quantifier(text);
    multi_target
}

/// CR 115.1: the "controlled by different players" target-set constraint phrase.
/// Single source of truth shared by the detector
/// (`parse_controlled_by_different_players_target_constraint`) and the per-slot
/// stripper (`strip_controlled_by_different_players` →
/// `try_parse_exchange_control_targets`).
pub(crate) const CONTROLLED_BY_DIFFERENT_PLAYERS: &str = " controlled by different players";

/// Locate the `CONTROLLED_BY_DIFFERENT_PLAYERS` constraint with a `take_until`
/// combinator and return the span BEFORE it (trimmed). Returns `None` when the
/// constraint is absent, so callers keep the original span. Composed from the
/// shared constraint phrase so the detector and the stripper can never drift.
pub(crate) fn strip_controlled_by_different_players(span: &str) -> Option<&str> {
    take_until::<_, _, OracleError<'_>>(CONTROLLED_BY_DIFFERENT_PLAYERS)
        .parse(span)
        .ok()
        .map(|(_, before)| before.trim_end())
}

pub(super) fn parse_controlled_by_different_players_target_constraint(text: &str) -> bool {
    let lower = text.to_lowercase();
    let mut parser = preceded(
        take_until::<_, _, OracleError<'_>>(CONTROLLED_BY_DIFFERENT_PLAYERS),
        tag(CONTROLLED_BY_DIFFERENT_PLAYERS),
    );
    parser.parse(lower.as_str()).is_ok()
}

/// CR 115.1 + CR 601.2c + CR 400.1: Detect target-set constraints that require
/// all chosen objects to come from one player's zone pile, currently the printed
/// "from a single graveyard" class.
pub(super) fn parse_same_zone_owner_target_constraint(
    text: &str,
) -> Option<TargetSelectionConstraint> {
    let lower = text.to_lowercase();
    let mut parser = preceded(
        take_until::<_, _, OracleError<'_>>("from a single graveyard"),
        tag("from a single graveyard"),
    );
    parser
        .parse(lower.as_str())
        .ok()
        .map(|_| TargetSelectionConstraint::SameZoneOwner {
            zone: Zone::Graveyard,
        })
}

/// CR 202.3 + CR 115.1: Detect a "with total mana value <N|X> or less" target-set
/// constraint anywhere in the clause and build the typed
/// `TargetSelectionConstraint::TotalManaValue`. Literal numbers stay fixed;
/// X remains a variable placeholder for the where-X form (Ancient Brass Dragon)
/// so `apply_where_x_*` later rebinds it to the die-result `EventContextAmount`.
///
/// Target side accepts only the "or less" (LE) comparator — see
/// `validate_target_constraints` / the parser strip in `oracle_effect/mod.rs`
/// for why GE is never emitted for targeting.
pub(super) fn parse_total_mana_value_target_constraint(
    text: &str,
) -> Option<TargetSelectionConstraint> {
    let lower = text.to_lowercase();
    let (_, (value, comparator), _) = nom_primitives::scan_preceded(lower.as_str(), |input| {
        preceded(
            tag::<_, _, OracleError<'_>>("with total mana value "),
            (
                nom_quantity::parse_quantity_expr_number,
                alt((
                    value(Comparator::LE, tag(" or less")),
                    value(Comparator::GE, tag(" or greater")),
                )),
            ),
        )
        .parse(input)
    })?;
    if comparator != Comparator::LE {
        return None;
    }
    Some(TargetSelectionConstraint::TotalManaValue {
        comparator: Comparator::LE,
        value,
    })
}

pub(super) fn extract_deal_damage_multi_target(text: &str) -> Option<MultiTargetSpec> {
    let lower = text.to_lowercase();
    let after_each_of = strip_after(&lower, "damage to each of ")?;
    if let Some((remainder, spec)) = strip_bounded_targets_placeholder(after_each_of) {
        if remainder.is_empty() {
            return Some(spec);
        }
    }
    let (_, multi_target) = strip_optional_target_prefix(after_each_of);
    multi_target
}

/// CR 115.1d + CR 613.4d: Recover the `MultiTargetSpec` for the prepositional
/// SwitchPT form ("switch the power and toughness of <subject>"). The
/// imperative parser strips "each of" and "any number of" so `parse_target`
/// sees a bare target phrase; this helper rebuilds the spec from the original
/// text. Mirrors `extract_double_counter_multi_target` — the only axis of
/// variation is the verb prefix.
pub(super) fn extract_switch_pt_multi_target(text: &str) -> Option<MultiTargetSpec> {
    let lower = text.to_lowercase();
    let (_, target_text) = preceded(
        tag::<_, _, OracleError<'_>>("switch the power and toughness of "),
        rest,
    )
    .parse(lower.as_str())
    .ok()?;
    // The distribution prefix "each of " is optional ("switch ... of each of
    // any number of target creatures" vs "switch ... of any number of target
    // creatures"); both surface the same MultiTargetSpec.
    let after_each_of = tag::<_, _, OracleError<'_>>("each of ")
        .parse(target_text)
        .map(|(rest, _)| rest)
        .unwrap_or(target_text);
    if let Ok((after_any_number, _)) =
        tag::<_, _, OracleError<'_>>("any number of ").parse(after_each_of)
    {
        if alt((
            tag::<_, _, OracleError<'_>>("target "),
            tag("other target "),
            tag("another target "),
        ))
        .parse(after_any_number)
        .is_ok()
        {
            return Some(MultiTargetSpec::unlimited(0));
        }
    }
    let (_, multi_target) = strip_optional_target_prefix(after_each_of);
    multi_target
}

pub(super) fn extract_double_counter_multi_target(text: &str) -> Option<MultiTargetSpec> {
    let lower = text.to_lowercase();
    // CR 701.10e (#5247): "double the number of <descriptor> counter(s) on
    // <target>" — the descriptor is either "each kind of" or a TYPED counter
    // ("+1/+1 counters", "charge counters", …). Consume the descriptor up to the
    // "counter(s) on " that introduces the target, so a typed counter surfaces
    // the same `MultiTargetSpec` as the "each kind of" form. Without this, Kinetic
    // Ooze ("double the number of +1/+1 counters on any number of other target
    // creatures") drops its "any number of … target creatures" bound and binds a
    // single required target — the ETB then panics with "Unused selected target
    // slots" (p0). The singular "counter on" arm preserves the "each kind of
    // counter on" form unchanged.
    let (target_text, _) = preceded(
        tag::<_, _, OracleError<'_>>("double the number of "),
        alt((
            (
                take_until::<_, _, OracleError<'_>>(" counters on "),
                tag(" counters on "),
            ),
            (
                take_until::<_, _, OracleError<'_>>(" counter on "),
                tag(" counter on "),
            ),
        )),
    )
    .parse(lower.as_str())
    .ok()?;
    if let Ok((after_any_number, _)) =
        tag::<_, _, OracleError<'_>>("any number of ").parse(target_text)
    {
        if alt((
            tag::<_, _, OracleError<'_>>("target "),
            tag("other target "),
            tag("another target "),
        ))
        .parse(after_any_number)
        .is_ok()
        {
            return Some(MultiTargetSpec::unlimited(0));
        }
    }
    let (_, multi_target) = strip_optional_target_prefix(target_text);
    multi_target
}

/// CR 115.1d + CR 122.1: Recover `MultiTargetSpec` for "remove … from each of
/// any number of <type>". The imperative parser strips the distribution prefix
/// so `parse_type_phrase` sees a bare filter; rebuild the spec from the
/// original text (parallel to `extract_switch_pt_multi_target`).
pub(super) fn extract_remove_counter_multi_target(text: &str) -> Option<MultiTargetSpec> {
    let lower = text.to_lowercase();
    if strip_after(&lower, "from each of any number of ").is_some() {
        return Some(MultiTargetSpec::unlimited(0));
    }
    None
}

/// CR 115.4 + CR 115.1a: The noun class of a "to each of ⟨count⟩ ⟨noun⟩" head.
///
/// Not a `bool`: the two arms are different rules with different downstream
/// handling. CR 115.4 gives a bare plural "two targets" the damage target class
/// (creature, player, planeswalker, or battle) ⇒ `TargetFilter::Any`, while
/// CR 115.1a's "two target ⟨type⟩" defers to `parse_target_with_ctx` for the
/// printed filter.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EachOfTargetNoun {
    /// Bare plural `targets` — CONSUMED by the combinator; the caller supplies
    /// `TargetFilter::Any` itself.
    AnyTargets,
    /// `[other |another ]target ⟨type⟩` — NOT consumed; `parse_target_with_ctx`
    /// needs the full phrase including the `target ` article.
    Typed,
}

/// CR 601.2c: Bounded announced count ("one or two", "one, two, or three").
/// Composes the trailing noun off `BOUNDED_TARGET_CARDINALITIES` exactly as
/// that constant's doc comment prescribes, so a future cardinality is added in
/// one place.
fn parse_bounded_target_cardinality(input: &str) -> OracleResult<'_, MultiTargetSpec> {
    for &(stem, min, max) in BOUNDED_TARGET_CARDINALITIES {
        if let Ok((rest, _)) = tag::<_, _, OracleError<'_>>(stem).parse(input) {
            return Ok((rest, MultiTargetSpec::fixed(min, max)));
        }
    }
    Err(oracle_err(input))
}

/// CR 601.2c: Optional announced count ("up to two", "up to X"). The count
/// vocabulary is delegated to `parse_multi_target_count_expr`, the single
/// authority for digits/English numerals/`x`/dynamic quantities.
fn parse_optional_target_cardinality(input: &str) -> OracleResult<'_, MultiTargetSpec> {
    map(
        preceded(tag("up to "), parse_multi_target_count_expr),
        MultiTargetSpec::up_to,
    )
    .parse(input)
}

/// CR 601.2c: Exact announced count ("two", "X"). "Once the number of targets
/// the spell has is determined, that number doesn't change."
fn parse_exact_target_cardinality(input: &str) -> OracleResult<'_, MultiTargetSpec> {
    map(parse_multi_target_count_expr, MultiTargetSpec::exact).parse(input)
}

/// CR 115.4 + CR 115.1a: The noun following the announced count.
///
/// The bare-plural arm is fenced with `not(satisfy(char::is_alphanumeric))` so
/// `targets` never matches inside a longer word — the same in-file form as the
/// `" tapped"` fence below. `not` never consumes, so no `peek` wrapper is
/// needed, and `satisfy` errors on empty input, so end-of-input is covered
/// without an `eof` arm.
///
/// The typed arm is `peek`-only: `parse_target_with_ctx` must still see the
/// `target `/`other target `/`another target ` article. `other`/`another` is a
/// lexical modifier here, and `FilterProp::Another` is applied downstream by
/// `parse_target_with_ctx` — the same grandfathered handling as
/// `strip_optional_target_prefix`. Note the asymmetry: the bare-plural arm does
/// NOT accept "other targets" (Drakuseth's "up to two other targets"), matching
/// the pre-existing behaviour exactly rather than adding new leniency.
fn parse_each_of_target_noun(input: &str) -> OracleResult<'_, EachOfTargetNoun> {
    alt((
        value(
            EachOfTargetNoun::AnyTargets,
            terminated(tag("targets"), not(satisfy(char::is_alphanumeric))),
        ),
        value(
            EachOfTargetNoun::Typed,
            peek(alt((
                tag("target "),
                tag("other target "),
                tag("another target "),
            ))),
        ),
    ))
    .parse(input)
}

/// CR 601.2c + CR 115.4: The parameterized "⟨cardinality⟩ ⟨noun⟩" head that
/// follows "to each of ". Spans the cardinality × noun matrix (bounded /
/// optional / exact × bare-plural / typed) that two single-leaf strippers
/// previously covered one cell each.
///
/// One printed cell is deliberately excluded: `other`/`another` on the
/// BARE-PLURAL arm ("each of up to two other targets", Drakuseth, Maw of
/// Flames). On the typed arm the modifier survives into `parse_target_with_ctx`
/// as `FilterProp::Another`, but the bare-plural arm consumes the noun and
/// synthesizes `TargetFilter::Any`, so there is nowhere for the CR 115.3
/// cross-slot distinctness to land — accepting it would silently drop the
/// constraint. Drakuseth's clause is dropped upstream today anyway, so this
/// changes nothing for it; the cell is left to whoever threads that constraint.
///
/// Each `alt` arm is a COMPLETE `(cardinality, multispace1, noun)` tuple so a
/// noun failure backtracks the whole arm: `"one or two targets"` would
/// otherwise have its leading `"one"` eaten by the exact arm. Bounded runs
/// first as defence in depth; the tuple shape is what makes the ordering
/// non-load-bearing.
///
/// Returns the ORIGINAL-CASE remainder so `parse_target_with_ctx` still sees
/// printed casing.
fn parse_each_of_target_distribution(
    after_each_of: &str,
) -> Option<(MultiTargetSpec, EachOfTargetNoun, &str)> {
    let lower = after_each_of.to_ascii_lowercase();
    let ((spec, noun), remainder) = nom_on_lower(after_each_of, lower.as_str(), |input| {
        map(
            alt((
                (
                    parse_bounded_target_cardinality,
                    multispace1,
                    parse_each_of_target_noun,
                ),
                (
                    parse_optional_target_cardinality,
                    multispace1,
                    parse_each_of_target_noun,
                ),
                (
                    parse_exact_target_cardinality,
                    multispace1,
                    parse_each_of_target_noun,
                ),
            )),
            |(spec, _, noun)| (spec, noun),
        )
        .parse(input)
    })?;
    // Return the remainder UNTRIMMED. A leading space is the compound-boundary
    // marker every consumer of `try_parse_damage_with_remainder` keys on —
    // `try_split_damage_compound` matches `tag(" and ")` and explicitly does not
    // trim. Only the typed arm needs a phrase starting at `target `, so it trims
    // at its own call site.
    Some((spec, noun, remainder))
}

/// CR 601.2c + CR 115.4: "⟨source⟩ deals N damage to each of ⟨count⟩ ⟨noun⟩".
///
/// CR 601.2c fixes the announced target COUNT; CR 115.4 fixes the target CLASS
/// for the bare-plural noun (creature, player, planeswalker, or battle). The
/// count is recorded on `ctx.pending_damage_multi_target` so the filter and the
/// count come from ONE parse rather than from a second text scan that can
/// disagree with it.
pub(super) fn parse_each_of_up_to_damage_target<'a>(
    target_phrase: &'a str,
    ctx: &mut ParseContext,
) -> Option<(TargetFilter, &'a str)> {
    let lower = target_phrase.to_lowercase();
    let (after_each_of_lower, _) = tag::<_, _, OracleError<'_>>("each of ")
        .parse(lower.as_str())
        .ok()?;
    let consumed = lower.len() - after_each_of_lower.len();
    let after_each_of = &target_phrase[consumed..];
    let (spec, noun, remainder) = parse_each_of_target_distribution(after_each_of)?;
    ctx.pending_damage_multi_target = Some(spec);
    Some(match noun {
        EachOfTargetNoun::AnyTargets => (TargetFilter::Any, remainder),
        EachOfTargetNoun::Typed => {
            // Only this arm trims: `parse_target_with_ctx` must see a phrase
            // starting at `target `/`other target `/`another target `.
            let (target, rest) = parse_target_with_ctx(remainder.trim_start(), ctx);
            refine_damage_target_remainder(target, rest)
        }
    })
}

/// Verbs where "any number of" / "up to N" modifies the target set (CR 115.1d),
/// not a resource count (counters, life, etc.).
///
/// `sacrifice` is intentionally excluded: per CR 701.21a a player sacrifices
/// their own permanents by choice during resolution — sacrifice never targets.
/// "Sacrifice any number of <filter>" is a variable-count choice resolved via
/// `EffectZoneChoice` (CR 107.1c), modeled as `Effect::Sacrifice { count:
/// UpTo(ObjectCount), min_count: 0 }` by `parse_one_or_more_sacrifice` — not a
/// `MultiTargetSpec`. Routing it through this list would strip the quantifier
/// and collapse the count to a fixed 1 (issue #458).
const MULTI_TARGET_VERBS: &[&str] = &[
    "exile", "tap", "untap", "goad", "detain", "return", "destroy", "choose",
];

/// CR 115.1d + CR 601.2d: The bounded target-cardinality lists. The stem carries
/// the bare `" or "` that enumerates a target COUNT, never a disjunction of two
/// clauses; each list carries its `(min, max)` count bounds. This is the single
/// authority for the vocabulary — the complete set measured against the full
/// pool (`AtomicCards.json`, 34,632 cards). Every consumer composes the trailing
/// noun it needs off the stem (`" targets"`, `" target "`, `" target"`) rather
/// than re-spelling the list, so a future cardinality ("one, two, three, or
/// four …") is added here once. Consumers: `strip_bounded_targets_placeholder`,
/// `strip_bounded_target_prefix`, `subject::…each-of`, `oracle_target`'s
/// bare-count target arm, and the binary-choice splitter's divide/distribute
/// bail (which keys on this cardinality axis — shared by CR 601.2d's "damage or
/// counters" halves — rather than on a distribution verb).
pub(crate) const BOUNDED_TARGET_CARDINALITIES: &[(&str, usize, usize)] =
    &[("one or two", 1, 2), ("one, two, or three", 1, 3)];

/// CR 115.1d + CR 601.2c: Strip exact target-count prefix before a targeted
/// phrase. "two target creatures" and "X target creatures" both set the exact
/// number of targets, unlike "up to X target creatures".
pub(crate) fn strip_exact_target_prefix(lower: &str) -> Option<(QuantityExpr, &str)> {
    let (rest, count) = parse_exact_target_count_expr(lower).ok()?;
    let rest = rest.trim_start();
    if alt((tag::<_, _, OracleError<'_>>("target "), tag("target,")))
        .parse(rest)
        .is_ok()
    {
        Some((count, rest))
    } else {
        None
    }
}

fn parse_exact_target_count_expr(input: &str) -> OracleResult<'_, QuantityExpr> {
    alt((
        value(
            QuantityExpr::Ref {
                qty: QuantityRef::Variable {
                    name: "X".to_string(),
                },
            },
            tag("x"),
        ),
        value(QuantityExpr::Fixed { value: 1 }, tag("one ")),
        value(QuantityExpr::Fixed { value: 2 }, tag("two ")),
        value(QuantityExpr::Fixed { value: 3 }, tag("three ")),
        value(QuantityExpr::Fixed { value: 4 }, tag("four ")),
        value(QuantityExpr::Fixed { value: 5 }, tag("five ")),
        value(QuantityExpr::Fixed { value: 6 }, tag("six ")),
    ))
    .parse(input)
}

/// CR 115.1d: Bare target-count placeholders after "each of" — "one or two
/// targets" (Prismari Charm: "deals 1 damage to each of one or two targets").
/// Returns the unconsumed remainder and a bounded `MultiTargetSpec` with min ≥ 1.
fn strip_bounded_targets_placeholder(text: &str) -> Option<(&str, MultiTargetSpec)> {
    let lower = text.to_ascii_lowercase();
    for &(stem, min, max) in BOUNDED_TARGET_CARDINALITIES {
        if let Ok((rest, _)) =
            (tag::<_, _, OracleError<'_>>(stem), tag(" targets")).parse(lower.as_str())
        {
            let consumed = lower.len() - rest.len();
            return Some((
                text[consumed..].trim_start(),
                MultiTargetSpec::fixed(min, max),
            ));
        }
    }
    None
}

/// CR 115.1d: "one or two target X" / "one, two, or three target X" before a
/// targeted phrase (Electrolyze: "among one or two target creatures and/or
/// players").
fn strip_bounded_target_prefix(text: &str) -> Option<(&str, MultiTargetSpec)> {
    let lower = text.to_ascii_lowercase();
    for &(stem, min, max) in BOUNDED_TARGET_CARDINALITIES {
        if let Ok((rest, _)) =
            (tag::<_, _, OracleError<'_>>(stem), tag(" target ")).parse(lower.as_str())
        {
            let consumed = lower.len() - rest.len();
            return Some((
                text[consumed..].trim_start(),
                MultiTargetSpec::fixed(min, max),
            ));
        }
    }
    None
}

fn strip_distribute_among_target_quantifier<'a>(
    text: &'a str,
    pool: &QuantityExpr,
) -> (&'a str, Option<MultiTargetSpec>) {
    let target_lower = text.to_lowercase();
    if let Ok((rest, _)) =
        tag::<_, _, OracleError<'_>>("any number of ").parse(target_lower.as_str())
    {
        let skip = target_lower.len() - rest.len();
        return (&text[skip..], Some(multi_target_for_distribute_among(pool)));
    }
    if let Some((rest, spec)) = strip_bounded_targets_placeholder(text) {
        return (rest, Some(spec));
    }
    if let Some((rest, spec)) = strip_bounded_target_prefix(text) {
        return (rest, Some(spec));
    }
    strip_optional_target_prefix(text)
}

/// Strip optional target-count prefixes before a targeted phrase.
/// For spells, CR 115.1a + CR 115.6 + CR 601.2c: the caster announces
/// zero through the stated maximum legal targets as the spell is cast.
pub(crate) fn strip_optional_target_prefix(text: &str) -> (&str, Option<MultiTargetSpec>) {
    let lower = text.to_ascii_lowercase();
    let Ok((after_up_to, _)) = tag::<_, _, OracleError<'_>>("up to ").parse(lower.as_str()) else {
        return (text, None);
    };
    let Ok((remainder, max)) = parse_multi_target_count_expr(after_up_to) else {
        return (text, None);
    };
    let consumed = lower.len() - remainder.len();
    let rest = text[consumed..].trim_start();
    let rest_lower = rest.to_ascii_lowercase();
    if alt((
        tag::<_, _, OracleError<'_>>("target "),
        tag("other target "),
        tag("another target "),
    ))
    .parse(rest_lower.as_str())
    .is_err()
    {
        return (text, None);
    }
    (rest, Some(MultiTargetSpec::up_to(max)))
}

pub(crate) fn parse_multi_target_count_expr(input: &str) -> OracleResult<'_, QuantityExpr> {
    alt((
        value(
            QuantityExpr::Ref {
                qty: QuantityRef::Variable {
                    name: "X".to_string(),
                },
            },
            tag("x"),
        ),
        nom_quantity::parse_quantity_expr_number,
        nom_quantity::parse_quantity,
    ))
    .parse(input)
}

/// CR 115.1d: Strip a leading "any number of "/"up to N " quantifier from
/// text that immediately follows a `MULTI_TARGET_VERBS` verb. Pure slice
/// arithmetic — the returned remainder is a true subslice of `after_verb`,
/// never a freshly-allocated string — so callers that must hand a remainder
/// back through an external input lifetime (e.g. `try_parse_verb_and_target`'s
/// `Option<(_, &'a str)>` return shape) can use it directly. Shared core for
/// `strip_any_number_quantifier` below, which additionally re-attaches the
/// verb prefix into a single owned string for its own (different) callers.
pub(super) fn strip_leading_quantifier(after_verb: &str) -> (&str, Option<MultiTargetSpec>) {
    let lower = after_verb.to_ascii_lowercase();
    if let Ok((rest, _)) = tag::<_, _, OracleError<'_>>("any number of ").parse(lower.as_str()) {
        let consumed = lower.len() - rest.len();
        return (&after_verb[consumed..], Some(MultiTargetSpec::unlimited(0)));
    }
    if let Ok((after_up_to, _)) = tag::<_, _, OracleError<'_>>("up to ").parse(lower.as_str()) {
        if let Ok((remainder, max)) = parse_multi_target_count_expr(after_up_to) {
            let consumed = lower.len() - remainder.len();
            return (
                after_verb[consumed..].trim_start(),
                Some(MultiTargetSpec::up_to(max)),
            );
        }
    }
    (after_verb, None)
}

/// CR 115.1d: Strip "any number of" or "up to N" quantifier from imperative text.
/// Only applies to verbs where the quantifier modifies target selection.
pub(super) fn strip_any_number_quantifier(text: &str) -> (String, Option<MultiTargetSpec>) {
    let lower = text.to_lowercase();
    let tp = TextPair::new(text, &lower);
    let verb = lower.split_whitespace().next().unwrap_or("");
    if !MULTI_TARGET_VERBS.contains(&verb) {
        return (text.to_string(), None);
    }

    let verb_end = lower.find(' ').map(|i| i + 1).unwrap_or(0);
    let (verb_tp, after_verb_tp) = tp.split_at(verb_end);

    let (rest, spec) = strip_leading_quantifier(after_verb_tp.original);
    match spec {
        Some(spec) => (format!("{}{}", verb_tp.original, rest), Some(spec)),
        None => (text.to_string(), None),
    }
}

/// Strip "to the battlefield [under X's control]" and similar destination phrases.
/// Returns the remaining target text and the destination zone (if battlefield).
/// Result of parsing a "return ... to <zone>" destination phrase.
pub(super) struct ReturnDestination {
    pub(super) zone: Zone,
    pub(super) transformed: bool,
    // CR 110.2a (docs/MagicCompRules.txt:618): the battlefield-entry control
    // clause AS WRITTEN — raw syntax, deliberately unbound. A destination
    // stripper sees only the destination phrase, never the moved object's
    // filter or the enclosing `ParseContext`, so it cannot resolve a
    // third-person anaphor ("under their control") without guessing. Binding
    // happens at the caller via `bind_control_clause`, where both are in scope.
    // `None` means the effect stated nothing otherwise (CR 110.2's default).
    pub(super) control: Option<ControlClausePossessor>,
    // CR 614.1: "tapped" — enters the battlefield tapped.
    pub(super) enter_tapped: bool,
    // CR 508.4: "tapped and attacking" — enters attacking.
    pub(super) enters_attacking: bool,
    // CR 122.1 + CR 122.6: Counters placed on the returned object as it enters.
    pub(super) enter_with_counters: Vec<(CounterType, QuantityExpr)>,
    // CR 708.2a + CR 708.3: "face down" — the object is turned face down before
    // it enters (CR 708.3). The default vanilla-2/2 profile is refined by a
    // trailing "It's a <type> ..." sentence (Yedora's "It's a Forest land.")
    // via the `FaceDownProfileSpec` continuation.
    pub(super) face_down: bool,
}

/// A single battlefield-entry rider parsed from the tail after the destination
/// phrase. Each variant is one independent flag; the scanner OR-accumulates a
/// sequence of them into [`BattlefieldRiders`].
#[derive(Clone, Copy)]
enum BattlefieldRider {
    // CR 614.1: enters the battlefield tapped.
    Tapped,
    // CR 708.2a + CR 708.3: turned face down before it enters.
    FaceDown,
    // CR 712.14a: put onto the battlefield "transformed" — enters with its
    // back face up.
    Transformed,
    // CR 508.4: enters tapped and attacking (attacking flag; the accompanying
    // "tapped" word, when present, is a separate `Tapped` rider).
    Attacking,
}

/// OR-accumulated battlefield-entry riders.
#[derive(Default, Clone, Copy)]
struct BattlefieldRiders {
    enter_tapped: bool,
    face_down: bool,
    transformed: bool,
    enters_attacking: bool,
}

/// Match a single battlefield-entry rider, preceded by an optional connector
/// (" and" / ","). The connector + rider are matched atomically: if no rider
/// follows the connector the `preceded` fails and consumes nothing (including
/// the connector), so a non-rider tail (", then exile it") stops the scan
/// cleanly. " tapped" carries a word-boundary guard so it does not match a
/// longer word with the same prefix.
fn parse_one_battlefield_rider(input: &str) -> OracleResult<'_, BattlefieldRider> {
    preceded(
        opt(alt((tag(" and"), tag(",")))),
        alt((
            // CR 508.4: "tapped and attacking" is the connector ("tapped" +
            // "and") feeding the `Attacking` rider on the next iteration; the
            // standalone words are matched here. " face down" before " tapped"
            // so the longer phrase wins when both could start a match.
            value(BattlefieldRider::FaceDown, tag(" face down")),
            value(
                BattlefieldRider::Tapped,
                terminated(tag(" tapped"), not(satisfy(|c: char| c.is_alphanumeric()))),
            ),
            value(BattlefieldRider::Transformed, tag(" transformed")),
            value(BattlefieldRider::Attacking, tag(" attacking")),
        )),
    )
    .parse(input)
}

/// Scan trailing battlefield-entry riders that may appear in any order after the
/// destination phrase ("to the battlefield under your control face down and
/// tapped"). The legacy destination table only encodes a fixed set of
/// contiguous rider permutations; this scanner picks up whatever riders the
/// table left on `after_destination`, OR-ing each into the flag accumulator.
/// Returns the unconsumed remainder and the accumulated riders.
///
/// CR 614.1 (tapped) + CR 708.3 (face down) + CR 712.14a (transformed) + CR
/// 508.4 (attacking) are all independent entry conditions, so order doesn't
/// matter.
fn strip_trailing_battlefield_riders(after_destination: &str) -> (&str, BattlefieldRiders) {
    let mut remaining = after_destination;
    let mut riders = BattlefieldRiders::default();
    while let Ok((rest, rider)) = parse_one_battlefield_rider(remaining) {
        match rider {
            BattlefieldRider::Tapped => riders.enter_tapped = true,
            BattlefieldRider::FaceDown => riders.face_down = true,
            BattlefieldRider::Transformed => riders.transformed = true,
            BattlefieldRider::Attacking => riders.enters_attacking = true,
        }
        remaining = rest;
    }
    (remaining, riders)
}

/// Detect "return ... to <zone>" destination phrase, including "transformed" flag.
/// Thin wrapper over [`strip_return_destination_ext_with_remainder`] for call sites
/// that discard the attach-host remainder (unit tests + legacy helpers).
#[allow(dead_code)] // exercised from `oracle_effect/tests.rs` (cfg(test) sibling)
pub(super) fn strip_return_destination_ext(text: &str) -> (&str, Option<ReturnDestination>) {
    let (target, dest, _) = strip_return_destination_ext_with_remainder(text);
    (target, dest)
}

type ReturnDestinationPattern = (
    &'static str,
    Zone,
    bool,
    Option<ControlClausePossessor>,
    bool,
    bool,
);

pub(super) fn strip_return_destination_ext_with_remainder(
    text: &str,
) -> (&str, Option<ReturnDestination>, &str) {
    let lower = text.to_lowercase();
    // Ordered longest-first to avoid partial matches.
    // "transformed" variants must come before their non-transformed counterparts.
    // Tuples: (phrase, zone, transformed, control, enter_tapped, enters_attacking)
    // CR 110.2a (docs/MagicCompRules.txt:618): the `control` column is the
    // parser-table carrier for whatever control clause the row's phrase already
    // spells out — `Some(You)` for "under your control", `Some(Owner)` for every
    // "under <its|their|his|her> owner('s|s') control" spelling (CR 110.2 @ :616,
    // which restates the default rather than overriding it), `None` otherwise.
    // Non-battlefield rows are always `None`: CR 110.1 (:614) gives a controller
    // only to permanents. Rows whose phrase carries no clause fall through to the
    // `parse_leading_control_clause` pass below, which picks up the third-person
    // forms the table never enumerated.
    // Ordered longest-first; compound patterns must precede their shorter substrings.
    let patterns: &[ReturnDestinationPattern] = &[
        // Tapped + transformed + owner's control (compound, longest)
        (
            " to the battlefield tapped and transformed under its owner's control",
            Zone::Battlefield,
            true,
            Some(ControlClausePossessor::Owner),
            true,
            false,
        ),
        // Transformed + your control
        (
            " to the battlefield transformed under your control",
            Zone::Battlefield,
            true,
            Some(ControlClausePossessor::You),
            false,
            false,
        ),
        // Transformed + owner's control variants
        (
            " to the battlefield transformed under their owners' control",
            Zone::Battlefield,
            true,
            Some(ControlClausePossessor::Owner),
            false,
            false,
        ),
        (
            " to the battlefield transformed under its owner's control",
            Zone::Battlefield,
            true,
            Some(ControlClausePossessor::Owner),
            false,
            false,
        ),
        (
            " to the battlefield transformed under his owner's control",
            Zone::Battlefield,
            true,
            Some(ControlClausePossessor::Owner),
            false,
            false,
        ),
        (
            " to the battlefield transformed under her owner's control",
            Zone::Battlefield,
            true,
            Some(ControlClausePossessor::Owner),
            false,
            false,
        ),
        (
            " to the battlefield transformed",
            Zone::Battlefield,
            true,
            None,
            false,
            false,
        ),
        // CR 508.4: Tapped and attacking (must precede shorter "tapped" variants)
        (
            " to the battlefield tapped and attacking",
            Zone::Battlefield,
            false,
            None,
            true,
            true,
        ),
        (
            " onto the battlefield tapped and attacking",
            Zone::Battlefield,
            false,
            None,
            true,
            true,
        ),
        // CR 508.4: bare "attacking" without tapped (Senu, Keen-Eyed Protector).
        (
            " onto the battlefield attacking",
            Zone::Battlefield,
            false,
            None,
            false,
            true,
        ),
        (
            " to the battlefield attacking",
            Zone::Battlefield,
            false,
            None,
            false,
            true,
        ),
        // Tapped + control variants (must precede shorter "tapped" and "under X control")
        (
            " to the battlefield tapped under their owners' control",
            Zone::Battlefield,
            false,
            Some(ControlClausePossessor::Owner),
            true,
            false,
        ),
        (
            " to the battlefield tapped under its owner's control",
            Zone::Battlefield,
            false,
            Some(ControlClausePossessor::Owner),
            true,
            false,
        ),
        (
            " to the battlefield tapped under your control",
            Zone::Battlefield,
            false,
            Some(ControlClausePossessor::You),
            true,
            false,
        ),
        // Simple control variants
        (
            " to the battlefield under their owners' control",
            Zone::Battlefield,
            false,
            Some(ControlClausePossessor::Owner),
            false,
            false,
        ),
        (
            " to the battlefield under its owner's control",
            Zone::Battlefield,
            false,
            Some(ControlClausePossessor::Owner),
            false,
            false,
        ),
        // CR 110.2: "under your control" — controller override.
        (
            " to the battlefield under your control",
            Zone::Battlefield,
            false,
            Some(ControlClausePossessor::You),
            false,
            false,
        ),
        // CR 614.1: "tapped" — enters tapped.
        (
            " to the battlefield tapped",
            Zone::Battlefield,
            false,
            None,
            true,
            false,
        ),
        (
            " to the battlefield",
            Zone::Battlefield,
            false,
            None,
            false,
            false,
        ),
        // "onto" variants
        (
            " onto the battlefield under your control",
            Zone::Battlefield,
            false,
            Some(ControlClausePossessor::You),
            false,
            false,
        ),
        (
            " onto the battlefield tapped",
            Zone::Battlefield,
            false,
            None,
            true,
            false,
        ),
        (
            " onto the battlefield",
            Zone::Battlefield,
            false,
            None,
            false,
            false,
        ),
        // Hand destinations
        (
            " to its owner's hand",
            Zone::Hand,
            false,
            None,
            false,
            false,
        ),
        (
            " to their owner's hand",
            Zone::Hand,
            false,
            None,
            false,
            false,
        ),
        (
            " to their owners' hands",
            Zone::Hand,
            false,
            None,
            false,
            false,
        ),
        (" to their hand", Zone::Hand, false, None, false, false),
        (" to your hand", Zone::Hand, false, None, false, false),
        // Graveyard destinations
        (
            " to its owner's graveyard",
            Zone::Graveyard,
            false,
            None,
            false,
            false,
        ),
        (
            " to their owner's graveyard",
            Zone::Graveyard,
            false,
            None,
            false,
            false,
        ),
        (
            " to their owners' graveyards",
            Zone::Graveyard,
            false,
            None,
            false,
            false,
        ),
        (
            " to your graveyard",
            Zone::Graveyard,
            false,
            None,
            false,
            false,
        ),
        // Command-zone destinations
        (
            " to the command zone",
            Zone::Command,
            false,
            None,
            false,
            false,
        ),
        // NOTE: Library destinations ("to the top/bottom of owner's library") are
        // intentionally NOT handled here. They require PutAtLibraryPosition (positional
        // placement without shuffling), not ChangeZone (which auto-shuffles).
    ];
    // CR 708.3: "face down" is turned on before the permanent enters the
    // battlefield, so the word sits immediately after "the battlefield" (and
    // before any control clause): "... to the battlefield face down under its
    // owner's control" (Yedora). The destination table is keyed on contiguous
    // phrases, so a face-down return is recognized by matching the phrase with
    // " face down" present and recording the rider. Rather than cross-product
    // every control/tapped row with a face-down twin, we try each row a second
    // time with " face down" spliced in right after "the battlefield".
    for (phrase, zone, transformed, row_control, enter_tapped, enters_attacking) in patterns {
        // Prefer the face-down variant (" to the battlefield face down ...") when
        // the text carries it; otherwise fall back to the plain destination row.
        let face_down_phrase = phrase
            // allow-noncombinator: structural construction of a face-down table-key variant from a static phrase, not parsing dispatch of input text (dispatch is the lower.rfind below, matching this existing rfind-table parser)
            .strip_prefix(" to the battlefield")
            .map(|rest| format!(" to the battlefield face down{rest}"));
        let (phrase_len, face_down, pos) = match face_down_phrase
            .as_deref()
            // allow-noncombinator: positional table scan in this pre-existing rfind-keyed destination parser; mirrors the existing `lower.rfind(phrase)` row dispatch, extended for the face-down variant
            .and_then(|fd| lower.rfind(fd).map(|p| (fd.len(), p)))
        {
            Some((len, pos)) => (len, true, Some(pos)),
            None => (phrase.len(), false, lower.rfind(phrase)),
        };
        if let Some(pos) = pos {
            // Local, OR-able copies of this row's battlefield-entry flags. The
            // legacy table only encodes a fixed set of contiguous rider
            // permutations; `strip_trailing_battlefield_riders` picks up any
            // order-independent riders the table left behind (Missy's "... under
            // your control face down and tapped").
            let mut face_down = face_down;
            let mut transformed = *transformed;
            let mut enter_tapped = *enter_tapped;
            let mut enters_attacking = *enters_attacking;
            // Byte offset (into both `lower` and `text`) just past the consumed
            // destination phrase and any trailing riders. Riders are pure-ASCII
            // and case-invariant, so the lowercase advance is valid into `text`
            // exactly as the pre-existing `pos + phrase_len` indexing already
            // assumes.
            let mut entry_offset = pos + phrase_len;
            // CR 110.2a (docs/MagicCompRules.txt:618): one control-clause
            // authority, two possible positions — inside the matched table
            // phrase, or trailing it. Declared OUTSIDE the battlefield block
            // because it is read at the `ReturnDestination` construction below,
            // which EVERY row reaches (including the hand/graveyard/command
            // rows, whose `control` is always `None` per CR 110.1 @ :614).
            // `*row_control` is a `Copy` read out of the `&'static` table row.
            let mut control: Option<ControlClausePossessor> = *row_control;
            // CR 122.6 (:1208): putting counters on an object includes giving
            // counters to it as it enters the battlefield. Battlefield-entry
            // riders, the control
            // clause and the "with … counter(s)" clause are INDEPENDENT entry
            // conditions printed in any order ("under your control face down and
            // tapped", "tapped and with two stun counters on it"). Consume them
            // as one order-independent run to a fixpoint rather than as a fixed
            // riders → control → riders pass sequence.
            //
            // CONSUMING (advancing `entry_offset`) rather than excising a span
            // out of the middle is what keeps the remainder a true SUFFIX, so an
            // instruction printed AFTER the entry clauses stays reachable by
            // normal clause processing. The previous code truncated the
            // remainder at the counter clause's start offset and so discarded
            // everything past it — Heart-Shaped Herb's "…with three +1/+1
            // counters on it and you become the monarch" lost the monarch
            // instruction outright, and any trailing control clause vanished
            // with it.
            let mut enter_with_counters: Vec<(CounterType, QuantityExpr)> = Vec::new();
            loop {
                let before = entry_offset;
                if *zone == Zone::Battlefield {
                    let (rider_rest, riders) =
                        strip_trailing_battlefield_riders(&lower[entry_offset..]);
                    face_down |= riders.face_down;
                    transformed |= riders.transformed;
                    enter_tapped |= riders.enter_tapped;
                    enters_attacking |= riders.enters_attacking;
                    entry_offset = lower.len() - rider_rest.len();
                    // CR 110.2a: the table enumerates only the first- and
                    // owner-person spellings, so a third-person clause ("under
                    // their control", "under that player's control") survives on
                    // the tail. Consume it here so it is neither dropped from
                    // the destination NOR re-emitted as a dangling remainder.
                    if control.is_none() {
                        if let Ok((rest, p)) = parse_leading_control_clause(&lower[entry_offset..])
                        {
                            control = Some(p);
                            entry_offset = lower.len() - rest.len();
                        }
                    }
                }
                // CR 122.1: the counter rider is zone-agnostic here — the
                // pre-loop scan it replaces ran for every destination row, not
                // just the battlefield ones, so it stays outside the gate above.
                if enter_with_counters.is_empty() {
                    if let Ok((rest, counters)) =
                        parse_leading_enter_counters_clause(&lower[entry_offset..])
                    {
                        enter_with_counters = counters;
                        entry_offset = lower.len() - rest.len();
                    }
                }
                if entry_offset == before {
                    break;
                }
            }
            // A true suffix of the ORIGINAL-case `text`: everything the entry
            // clauses did not consume, in printed order. Riders, control clauses
            // and counter types are pure ASCII and case-invariant, so advancing
            // the offset on `lower` indexes `text` identically — the same
            // invariant the pre-existing `pos + phrase_len` indexing assumes.
            let original_after_destination = &text[entry_offset..];
            return (
                text[..pos].trim(),
                Some(ReturnDestination {
                    zone: *zone,
                    transformed,
                    control,
                    enter_tapped,
                    enters_attacking,
                    enter_with_counters,
                    face_down,
                }),
                original_after_destination,
            );
        }
    }
    (text, None, "")
}

/// Detect "return to <zone> <target>" destination phrases.
pub(super) fn strip_leading_return_destination_ext(
    text: &str,
) -> (&str, Option<ReturnDestination>) {
    let lower = text.to_lowercase();
    if let Ok((rest, dest)) = parse_leading_return_destination(lower.as_str()) {
        let consumed = lower.len() - rest.len();
        return (text[consumed..].trim(), Some(dest));
    }

    (text, None)
}

fn parse_leading_return_destination(input: &str) -> OracleResult<'_, ReturnDestination> {
    alt((
        parse_leading_battlefield_return_destination,
        parse_leading_hand_return_destination,
        parse_leading_graveyard_return_destination,
        parse_leading_command_return_destination,
    ))
    .parse(input)
}

fn parse_leading_battlefield_return_destination(
    input: &str,
) -> OracleResult<'_, ReturnDestination> {
    let (input, _) = alt((
        tag::<_, _, OracleError<'_>>("to the battlefield"),
        tag("onto the battlefield"),
    ))
    .parse(input)?;
    // CR 708.3: "face down" is applied before entry, so it precedes the
    // tapped/transformed/control modifiers.
    let (input, face_down) = alt((
        value(true, tag::<_, _, OracleError<'_>>(" face down")),
        value(false, tag("")),
    ))
    .parse(input)?;
    // (transformed, enter_tapped, enters_attacking)
    let (input, modifier) = alt((
        value((true, true, false), tag(" tapped and transformed")),
        value((true, false, false), tag(" transformed")),
        value((false, true, true), tag(" tapped and attacking")),
        value((false, true, false), tag(" tapped")),
        value((false, false, false), tag("")),
    ))
    .parse(input)?;
    // CR 110.2a (docs/MagicCompRules.txt:618): parse the control clause (or its
    // absence) as raw syntax. The four hand-picked literal arms this replaces
    // recognized only "under your control", "under their owners' control" and
    // "under its owner's control"; the singular "under their/his/her owner's
    // control" spellings fell through to the empty arm and their residue leaked
    // into the TARGET text. The shared combinator recognizes every printed
    // owner spelling plus the third-person forms.
    let (input, control) = opt(parse_leading_control_clause).parse(input)?;
    let (input, _) = tag(" ").parse(input)?;
    Ok((
        input,
        ReturnDestination {
            zone: Zone::Battlefield,
            transformed: modifier.0,
            control,
            enter_tapped: modifier.1,
            enters_attacking: modifier.2,
            enter_with_counters: vec![],
            face_down,
        },
    ))
}

fn parse_leading_hand_return_destination(input: &str) -> OracleResult<'_, ReturnDestination> {
    let (input, _) = alt((
        tag::<_, _, OracleError<'_>>("to its owner's hand "),
        tag("to their owner's hand "),
        tag("to their owners' hands "),
        tag("to their hand "),
        tag("to your hand "),
    ))
    .parse(input)?;
    Ok((
        input,
        ReturnDestination {
            zone: Zone::Hand,
            transformed: false,
            control: None,
            enter_tapped: false,
            enters_attacking: false,
            enter_with_counters: vec![],
            face_down: false,
        },
    ))
}

fn parse_leading_graveyard_return_destination(input: &str) -> OracleResult<'_, ReturnDestination> {
    let (input, _) = alt((
        tag::<_, _, OracleError<'_>>("to its owner's graveyard "),
        tag("to their owner's graveyard "),
        tag("to their owners' graveyards "),
        tag("to your graveyard "),
    ))
    .parse(input)?;
    Ok((
        input,
        ReturnDestination {
            zone: Zone::Graveyard,
            transformed: false,
            control: None,
            enter_tapped: false,
            enters_attacking: false,
            enter_with_counters: vec![],
            face_down: false,
        },
    ))
}

fn parse_leading_command_return_destination(input: &str) -> OracleResult<'_, ReturnDestination> {
    let (input, _) = tag("to the command zone ").parse(input)?;
    Ok((
        input,
        ReturnDestination {
            zone: Zone::Command,
            transformed: false,
            control: None,
            enter_tapped: false,
            enters_attacking: false,
            enter_with_counters: vec![],
            face_down: false,
        },
    ))
}

/// CR 601.2d: Cap "any number of" target selection to the distribution pool.
/// Without this, the controller can select more permanents than counters or
/// damage and the assign step deadlocks (each chosen target must receive at
/// least one). "Any number" always permits zero targets, even when the pool is
/// fixed and positive (Stolen Goodies).
fn multi_target_for_distribute_among(distribution_amount: &QuantityExpr) -> MultiTargetSpec {
    let (inner, _) = distribution_amount.peel_up_to();
    MultiTargetSpec::bounded_expr(QuantityExpr::Fixed { value: 0 }, inner.clone())
}

/// CR 601.2d: The keywords that introduce a divided/distributed *damage* effect.
/// Single authority for the "is this a distribution clause?" membership test —
/// extend here, never inline at a call site.
///
/// One caller: `try_parse_distribute_damage` bounds its Pattern-B quantity slice
/// with this set. (The binary-choice splitter `try_parse_choose_one_of_inline`
/// formerly also consulted it to bail, but that coupling — and the set-drift
/// hazard it warned about — is gone: the splitter now keys its bail on the
/// target-cardinality list `BOUNDED_TARGET_CARDINALITIES`, the axis CR 601.2d
/// shares across both its "damage or counters" halves. That axis guards the
/// counter-distribution templating ("Distribute three +1/+1 counters among one,
/// two, or three target creatures") this damage-verb set never could, so the two
/// no longer need to agree.)
pub(super) const DISTRIBUTION_KEYWORDS: [&str; 2] =
    ["divided as you choose among", "divided evenly"];

/// CR 601.2d: Parse "deal N damage divided as you choose among [targets]" and
/// "deal N damage distributed among [targets]" → Effect::DealDamage with distribute flag.
///
/// Also handles "deal N damage divided evenly, rounded down, among [targets]" which uses
/// the same Effect but signals even-split (the engine treats this as a pre-set distribution).
pub(super) fn try_parse_distribute_damage(lower: &str, text: &str) -> Option<ParsedEffectClause> {
    let tp = TextPair::new(text, lower);
    // Scan word-by-word for "deals " or "deal " verb.
    let (pos, verb_len) = {
        let mut scan = lower;
        let mut offset = 0usize;
        loop {
            if tag::<_, _, OracleError<'_>>("deals ").parse(scan).is_ok() {
                break (offset, 6usize);
            }
            if tag::<_, _, OracleError<'_>>("deal ").parse(scan).is_ok() {
                break (offset, 5usize);
            }
            // allow-noncombinator: word-boundary advance in scan loop (Pattern 5)
            let i = scan.find(' ')?;
            offset += i + 1;
            scan = &scan[i + 1..];
        }
    };
    let (_, after_tp) = tp.split_at(pos + verb_len);

    let (amount, rest_tp) = if let Some((qty, rem)) = parse_count_expr(after_tp.lower) {
        // Pattern A: "[qty] damage divided/distributed among …"
        if tag::<_, _, OracleError<'_>>("damage").parse(rem).is_ok() {
            let skip = after_tp.lower.len() - rem.len() + "damage".len();
            let (_, rest) = after_tp.split_at(skip);
            (qty, rest)
        } else {
            return None;
        }
    } else if let Ok((after_prefix, _)) =
        tag::<_, _, OracleError<'_>>("damage equal to ").parse(after_tp.lower)
    {
        // Pattern B: "damage equal to [qty] divided/distributed among …"
        // CR 601.2d: the quantity follows the "equal to" phrase and is a dynamic
        // reference (e.g., "its power" — Emberwilde Captain), so it routes through
        // the CDA quantity layer rather than the fixed/X-only `parse_count_expr`.
        // The quantity slice is the text between "equal to " and the distribution
        // keyword; the distribution phrase is then located in `rest` below exactly
        // as in Pattern A.
        let after_prefix_offset = after_tp.lower.len() - after_prefix.len();
        let (_, rest) = after_tp.split_at(after_prefix_offset);
        let qty_end = DISTRIBUTION_KEYWORDS
            .iter()
            // allow-noncombinator: structural slice bound, not parsing dispatch — locate
            // the earliest distribution keyword so `parse_cda_quantity` receives only the
            // quantity phrase. The dispatch on *which* distribution kind applies is done
            // by the `distribute_kind` combinator block below; this only bounds the slice.
            .filter_map(|kw| rest.lower.find(kw))
            .min()?;
        let qty_text = rest.lower[..qty_end].trim();
        let qty = parse_cda_quantity(qty_text)?;
        (qty, rest)
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

    // CR 115.1d: Detect the target-count quantifier before the target phrase.
    let (stripped_target_text, multi_target) =
        strip_distribute_among_target_quantifier(target_text, &amount);
    let (target, _) = parse_target(stripped_target_text);

    Some(ParsedEffectClause {
        effect: Effect::DealDamage {
            amount,
            target,
            damage_source: None,
            excess: None,
        },
        duration: None,
        sub_ability: None,
        distribute: Some(distribute_kind),
        multi_target,
        condition: None,
        optional: false,
        unless_pay: None,
    })
}

/// CR 601.2d: Parse "distribute N [type] counters among [targets]"
/// → Effect::PutCounter with distribute flag set.
pub(super) fn try_parse_distribute_counters(lower: &str, text: &str) -> Option<ParsedEffectClause> {
    // "distribute " = 11 bytes; "distributes " = 12 bytes. Capture matched length for
    // the expected_min sanity check. Both infinitive and 3rd-person forms appear in Oracle text.
    let (after_lower, verb_len): (&str, usize) = {
        let mut verb_alt = alt((
            tag::<_, _, OracleError<'_>>("distributes "),
            tag::<_, _, OracleError<'_>>("distribute "),
        ));
        if let Ok((rest, matched)) = verb_alt.parse(lower) {
            (rest, matched.len())
        } else {
            return None;
        }
    };
    let (count_expr, rest_lower) =
        if let Ok((rest, _)) = tag::<_, _, OracleError<'_>>("up to ").parse(after_lower) {
            let (inner, rest) = parse_count_expr(rest)?;
            (QuantityExpr::up_to(inner), rest)
        } else {
            parse_count_expr(after_lower)?
        };

    // CR 122.1 + CR 122.1b: shared counter-type combinator handles multi-word
    // keyword counter names. Keyword counters aren't a printed distribute
    // target today (CR 122.1b keyword counters are placed singly), but the
    // shared combinator costs nothing and future-proofs the parser.
    let (after_type_raw, counter_type) =
        nom_primitives::parse_counter_type_typed(rest_lower).ok()?;
    let type_end = rest_lower.len() - after_type_raw.len();

    // Require "counter(s)" immediately after the counter type word.
    let after_type = after_type_raw.trim_start();
    let counter_word_len = if tag::<_, _, OracleError<'_>>("counters")
        .parse(after_type)
        .is_ok()
    {
        "counters".len()
    } else if tag::<_, _, OracleError<'_>>("counter")
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
    let (stripped_target, multi_target) =
        strip_distribute_among_target_quantifier(target_text, &count_expr);
    let (target, _) = parse_target(stripped_target);

    // Verify the "among" comes after the counter word (sanity guard against false matches).
    let expected_min =
        verb_len + (after_lower.len() - rest_lower.len()) + type_end + counter_word_len;
    if among_pos < expected_min {
        return None;
    }
    let _ = counter_word_len; // used above

    let counter_name = counter_type.as_str().into_owned();
    Some(ParsedEffectClause {
        effect: Effect::PutCounter {
            counter_type,
            count: count_expr,
            target,
        },
        duration: None,
        sub_ability: None,
        distribute: Some(DistributionUnit::Counters(counter_name)),
        multi_target,
        condition: None,
        optional: false,
        unless_pay: None,
    })
}

/// CR 601.2d + CR 615.7: Parse "prevent [qty] damage divided/distributed among [targets]"
/// → Effect::PreventDamage with distribute flag. Called from the Prevent intercept arm
/// in `lower_imperative_family_ast` before the standard prevent resolver.
pub(super) fn try_parse_prevent_distribute(text: &str) -> Option<ParsedEffectClause> {
    let lower = text.to_lowercase();
    // Quick-reject: require a distribution marker before spending effort on parsing.
    if !scan_contains_phrase(&lower, "distributed among")
        && !scan_contains_phrase(&lower, "divided as you choose among")
    {
        return None;
    }
    // Parse "prevent " prefix via nom combinator.
    let (after_prevent, _) = tag::<_, _, OracleError<'_>>("prevent ")
        .parse(lower.as_str())
        .ok()?;
    // CR 615.7: prevention shields are printed as "prevent the next N damage …".
    // Strip the optional "the next "/"next " quantifier before the count so the
    // shared `parse_count_expr` sees a bare quantity. `opt` makes both the
    // "the next" and the determiner-less "next" forms parse, and leaves the input
    // untouched for "prevent N damage" (no quantifier).
    let (after_quantifier, _) = opt(alt((
        tag::<_, _, OracleError<'_>>("the next "),
        tag::<_, _, OracleError<'_>>("next "),
    )))
    .parse(after_prevent)
    .unwrap_or((after_prevent, None));
    // Parse the prevention amount.
    let (qty, rem) = parse_count_expr(after_quantifier)?;
    // CR 615.7: Require "damage" immediately after the quantity.
    let (after_damage, _) = tag::<_, _, OracleError<'_>>("damage").parse(rem).ok()?;

    // Locate the distribution keyword using TextPair-style strip_after.
    let tp = TextPair::new(text, &lower);
    // Reconstruct byte offset into after_damage in the lower string.
    let after_damage_offset = lower.len() - after_damage.len();
    let (_, after_damage_tp) = tp.split_at(after_damage_offset);

    let target_tp = after_damage_tp
        .strip_after("divided as you choose among ")
        .or_else(|| after_damage_tp.strip_after("distributed among "))?;
    let target_text = target_tp.original.trim();

    let (stripped_target, multi_target) =
        strip_distribute_among_target_quantifier(target_text, &qty);
    let (target, _) = parse_target(stripped_target);

    // Convert the parsed QuantityExpr to PreventionAmount.
    // CR 615.7: Fixed amounts use Next(n); dynamic amounts use amount_dynamic.
    let (amount, amount_dynamic) = match &qty {
        QuantityExpr::Fixed { value } => (PreventionAmount::Next(*value as u32), None),
        _ => (PreventionAmount::All, Some(qty)),
    };

    Some(ParsedEffectClause {
        effect: Effect::PreventDamage {
            amount,
            amount_dynamic,
            target,
            scope: PreventionScope::AllDamage,
            damage_source_filter: None,
            prevention_duration: None,
        },
        duration: None,
        sub_ability: None,
        distribute: Some(DistributionUnit::Damage),
        multi_target,
        condition: None,
        optional: false,
        unless_pay: None,
    })
}

/// CR 615.1a + CR 608.2c: Parse "prevent [amount] [combat] damage
/// that would be dealt to and dealt by <anaphor> this turn" — the
/// bidirectional shield CR 615's AND-only shield semantics cannot express as
/// one node (`Effect::PreventDamage`'s `target` + `damage_source_filter`
/// combine with AND semantics: recipient==X AND source==Y, never recipient==X
/// OR source==X). Splits into two independent `PreventDamage` nodes chained via
/// `SequentialSibling` — a recipient-scoped ("to") shield and a
/// source-scoped-only ("by") shield with no recipient restriction. Called from
/// the Prevent intercept arm in `lower_imperative_family_ast` BEFORE
/// `try_parse_prevent_distribute` — mutually exclusive markers in the corpus
/// (no card combines "distributed among" with "dealt to and dealt by").
/// Issue #1094 (Maze of Ith).
pub(super) fn try_parse_bidirectional_prevent(
    text: &str,
    parent_target_available: bool,
) -> Option<ParsedEffectClause> {
    let lower = text.to_lowercase();
    // Quick-reject via the bidirectional marker before spending parse effort.
    if !scan_contains_phrase(&lower, "dealt to and dealt by") {
        return None;
    }
    // Parse "prevent " prefix and position `rest` just past it.
    let (rest, _) = tag::<_, _, OracleError<'_>>("prevent ")
        .parse(lower.as_str())
        .ok()?;

    // CR 615: scope — combat damage only vs all damage (mirrors
    // `parse_prevent_effect`'s own scope detection exactly).
    let scope = if nom_primitives::scan_contains(rest, "combat damage") {
        PreventionScope::CombatDamage
    } else {
        PreventionScope::AllDamage
    };

    // CR 615.1a: shared amount detection (issue #1094 factored this out of
    // `parse_prevent_effect` so both paths agree).
    let amount = super::imperative::parse_prevention_amount(rest);

    // CR 511.2 + CR 615: trailing duration window ("this turn" -> end of turn).
    let prevention_duration =
        nom_primitives::scan_preceded(rest, parse_duration).map(|(_, d, _)| d);

    // CR 608.2c + CR 615: isolate the anaphor phrase following the
    // bidirectional marker and resolve it via the same recipient resolution
    // `parse_prevent_effect` uses (chosen target anaphor / any other
    // recognized filter — Energy Arc's "those creatures" resolves via
    // `parse_target`'s `TrackedSet` dispatch). `None` when no tier
    // recognizes it — a standalone "dealt to and dealt by that creature"
    // with no prior target-selecting clause must NOT split into ParentTarget
    // shields.
    let anaphor_tp = TextPair::new(text, &lower).strip_after("dealt to and dealt by ")?;
    let anaphor_filter =
        super::imperative::resolve_prevent_recipient(anaphor_tp, parent_target_available)?;

    // CR 615: the recipient ("to") shield — scoped to the chosen creature as
    // the damage RECIPIENT (target: ParentTarget, no source restriction).
    let to_effect = Effect::PreventDamage {
        amount,
        amount_dynamic: None,
        target: anaphor_filter.clone(),
        scope,
        damage_source_filter: None,
        prevention_duration: prevention_duration.clone(),
    };

    // CR 615: the source-only ("by") shield — scoped to the chosen creature as
    // the damage SOURCE (target: Any, damage_source_filter: ParentTarget). A
    // SequentialSibling: an independent following instruction in the same
    // resolution, not a per-event rider.
    let mut by_ability = AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::PreventDamage {
            amount,
            amount_dynamic: None,
            target: TargetFilter::Any,
            scope,
            damage_source_filter: Some(anaphor_filter),
            prevention_duration,
        },
    );
    by_ability.sub_link = SubAbilityLink::SequentialSibling;

    Some(ParsedEffectClause {
        effect: to_effect,
        duration: None,
        sub_ability: Some(Box::new(by_ability)),
        distribute: None,
        multi_target: None,
        condition: None,
        optional: false,
        unless_pay: None,
    })
}

/// Thin wrapper around `try_parse_damage_with_remainder` for callers that don't
/// need the remainder (e.g., `parse_cost_resource_ast`). The remainder is only
/// safely discardable when `try_split_damage_compound` has already run and found
/// no compound connector.
pub(super) fn try_parse_damage(lower: &str, text: &str, ctx: &mut ParseContext) -> Option<Effect> {
    let (effect, _remainder) = try_parse_damage_with_remainder(text, lower, ctx)?;
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
pub(super) fn try_parse_damage_with_remainder<'a>(
    text: &'a str,
    lower: &'a str,
    ctx: &mut ParseContext,
) -> Option<(Effect, &'a str)> {
    // Match: "~ deals N damage to {target}" / "deal N damage to {target}"
    // and variable forms like "deal that much damage" or
    // "deal damage equal to its power".
    // Scan word-by-word for "deals " or "deal " verb.
    let (pos, verb_len) = {
        let mut scan = lower;
        let mut offset = 0usize;
        loop {
            if tag::<_, _, OracleError<'_>>("deals ").parse(scan).is_ok() {
                break (offset, 6usize);
            }
            if tag::<_, _, OracleError<'_>>("deal ").parse(scan).is_ok() {
                break (offset, 5usize);
            }
            // allow-noncombinator: word-boundary advance in scan loop (Pattern 5)
            let i = scan.find(' ')?;
            offset += i + 1;
            scan = &scan[i + 1..];
        }
    };
    let after = &text[pos + verb_len..];
    let after_lower = &lower[pos + verb_len..];

    let (amount, after_target) = if let Some((qty, rest)) = parse_count_expr(after_lower) {
        if tag::<_, _, OracleError<'_>>("damage").parse(rest).is_ok() {
            (qty, &after[after.len() - rest.len() + "damage".len()..])
        } else {
            return None;
        }
    } else if let Ok((rem, _)) =
        tag::<_, _, OracleError<'_>>("twice that much damage").parse(after_lower)
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
    } else if let Ok((rem, _)) = alt((
        tag::<_, _, OracleError<'_>>("that much damage"),
        // CR 120.1: "that amount of damage" is the synonym used when the
        // antecedent reads "N damage" rather than "this much damage" (Fear of
        // Burning Alive: "deals that amount of damage to target creature that
        // player controls"). Both anaphors resolve to the just-dealt amount.
        tag("that amount of damage"),
    ))
    .parse(after_lower)
    {
        let consumed = after_lower.len() - rem.len();
        (
            QuantityExpr::Ref {
                qty: QuantityRef::EventContextAmount,
            },
            &after[consumed..],
        )
    } else if let Ok((rest, _)) = tag::<_, _, OracleError<'_>>("damage to ").parse(after_lower) {
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
            let target_phrase = target_phrase.trim();
            // CR 508.5: "defending player" in an attacking creature's ability
            // identifies the player that creature is attacking. Bind local
            // third-person quantity refs ("they control") to that player. This
            // is intentionally scoped to the literal parsed recipient phrase.
            let references_defending_player =
                nom::combinator::all_consuming(tag::<_, _, OracleError<'_>>("defending player"))
                    .parse(target_phrase)
                    .is_ok();
            // CR 120.3: "deals damage to each player equal to the number of [X]
            // THEY control" — the third-person "they" binds to the iterating
            // player (DamageEachPlayer resolves per recipient), NOT the caster.
            // Classify the recipient scope BEFORE parsing the amount so the
            // count's controller threads to `ScopedPlayer` (Acidic Soil).
            let each_player_scope = parse_damage_each_player_scope(target_phrase).is_some();
            // Parse amount using existing helpers
            let qty = crate::parser::oracle_quantity::parse_event_context_quantity(amount_phrase)
                .or_else(|| {
                    if references_defending_player {
                        ctx.with_player_scope(ControllerRef::DefendingPlayer, |amount_ctx| {
                            parse_cda_quantity_with_context(amount_phrase, amount_ctx)
                        })
                    } else if each_player_scope {
                        ctx.with_player_scope(ControllerRef::ScopedPlayer, |amount_ctx| {
                            parse_cda_quantity_with_context(amount_phrase, amount_ctx)
                        })
                    } else {
                        parse_cda_quantity_with_context(amount_phrase, ctx)
                    }
                });
            if let Some(qty) = qty {
                // Route based on target phrase
                if target_phrase == "itself" {
                    // CR 608.2k: When the recipient is "itself", an anaphoric
                    // "its <characteristic>" means that target's value. Only the
                    // pronoun `Anaphoric` is rebound (across every per-object
                    // characteristic) — an explicit possessive ("the sacrificed
                    // creature's power", `CostPaidObject`) or a demonstrative
                    // ("that creature's toughness", `Demonstrative`) keeps its
                    // fixed referent.
                    let mut qty = qty;
                    super::rebind_anaphoric_object_scope(
                        &mut qty,
                        crate::types::ability::ObjectScope::Target,
                    );
                    return Some((
                        Effect::DealDamage {
                            amount: qty,
                            target: TargetFilter::ParentTarget,
                            damage_source: Some(DamageSource::Target),
                            excess: None,
                        },
                        "",
                    ));
                } else if tag::<_, _, OracleError<'_>>("each ")
                    .parse(target_phrase)
                    .is_ok()
                {
                    if let Some((target, remainder)) =
                        parse_each_of_up_to_damage_target(target_phrase, ctx)
                    {
                        return Some((
                            Effect::DealDamage {
                                amount: qty,
                                target,
                                damage_source: None,
                                excess: None,
                            },
                            remainder,
                        ));
                    }
                    // "each player" → DamageEachPlayer (per-player varying damage)
                    // "each creature" → DamageAll (uniform damage to objects)
                    // "each foe" — archaic synonym for opponent (friend/foe cards)
                    if let Some(player_filter) = parse_damage_each_player_scope(target_phrase) {
                        return Some((
                            Effect::DamageEachPlayer {
                                amount: qty,
                                player_filter,
                            },
                            "",
                        ));
                    }
                    let (filter, remainder) = parse_target_with_ctx(target_phrase, ctx);
                    let (filter, remainder) = refine_damage_target_remainder(filter, remainder);
                    // CR 119.2 + CR 120.3: "[N] damage to each creature and each
                    // player" — composite scope. The "each creature" parse
                    // captures the object filter; the trailing "and each player"
                    // (or variants) carries the player scope. Lift it into
                    // player_filter so DamageAll covers both audiences uniformly
                    // (Pompeii, Volcanic Eruption, etc.).
                    let trimmed = remainder.trim_start_matches([',', ' ']);
                    let trimmed_lower = trimmed.to_lowercase();
                    let player_filter = tag::<_, _, OracleError<'_>>("and ")
                        .parse(trimmed_lower.as_str())
                        .ok()
                        .and_then(|(after_and, _)| parse_damage_each_player_scope(after_and));
                    let leftover = if player_filter.is_some() {
                        ""
                    } else {
                        remainder.trim()
                    };
                    if !leftover.is_empty() {
                        ctx.push_diagnostic(OracleDiagnostic::IgnoredRemainder {
                            text: leftover.into(),
                            parser: "damage-all".into(),
                            line_index: 0,
                        });
                    }
                    return Some((
                        Effect::DamageAll {
                            amount: qty,
                            target: filter,
                            player_filter,
                            damage_source: None,
                        },
                        "",
                    ));
                } else if parse_source_chosen_player_damage_target(target_phrase) {
                    return Some((
                        Effect::DealDamage {
                            amount: qty,
                            target: TargetFilter::SourceChosenPlayer,
                            damage_source: None,
                            excess: None,
                        },
                        "",
                    ));
                } else if let Some((target, ecr_rem)) =
                    parse_damage_event_target_recipient(target_phrase, ctx)
                {
                    return Some((
                        Effect::DealDamage {
                            amount: qty,
                            target,
                            damage_source: None,
                            excess: None,
                        },
                        ecr_rem,
                    ));
                } else if let Some((target, ecr_rem)) =
                    parse_event_context_ref_with_ctx(target_phrase, ctx)
                {
                    let (target, ecr_rem) = refine_damage_target_remainder(target, ecr_rem);
                    #[cfg(debug_assertions)]
                    assert_no_compound_remainder(ecr_rem, target_phrase);
                    return Some((
                        Effect::DealDamage {
                            amount: qty,
                            target,
                            damage_source: None,
                            excess: None,
                        },
                        ecr_rem,
                    ));
                } else {
                    let (target, remainder) = parse_target(target_phrase);
                    let (target, remainder) = refine_damage_target_remainder(target, remainder);
                    if !remainder.trim().is_empty() {
                        ctx.push_diagnostic(OracleDiagnostic::IgnoredRemainder {
                            text: remainder.trim().into(),
                            parser: "deal-damage".into(),
                            line_index: 0,
                        });
                    }
                    return Some((
                        Effect::DealDamage {
                            amount: qty,
                            target,
                            damage_source: None,
                            excess: None,
                        },
                        "",
                    ));
                }
            }
        }
        return None;
    } else if let Ok((rem, _)) = tag::<_, _, OracleError<'_>>("damage equal to ").parse(after_lower)
    {
        let consumed = after_lower.len() - rem.len();
        let amount_text = &after[consumed..];
        let amount_lower = amount_text.to_lowercase();
        let (_, before_to) = take_until::<_, _, OracleError<'_>>(" to ")
            .parse(amount_lower.as_str())
            .ok()?;
        let qty_text = amount_text[..before_to.len()].trim();
        // CR 120.1 + CR 601.2c + CR 208.1 + CR 608.2: Multi-source per-power
        // damage — "(each) deal damage equal to their power to <recipient>". The
        // plural possessive "their power" (vs. the singular "its power" handled by
        // the single-source one-sided-fight path) marks the variable-count source
        // set established by the subject ("up to N / any number of target
        // creatures you control") or by the prior sentence ("They each ..."). Each
        // source deals damage equal to ITS OWN power (CR 208.1 modifiable
        // characteristic, CR 608.2 read at resolution), so the amount is the
        // per-object `Power{Anaphoric}` (rebound to `Target` by
        // `wrap_target_subject_damage` for the direct subject form, or by the
        // one-sided-fight prepend for the "They each ..." back-reference) and the
        // source is `EachTarget`. Allies at Last, Coordinated Clobbering, Terrific
        // Team-Up. (Graceful Takedown's compound source set is now supported via
        // `EachDealsDamageEqualToPower`'s `extra_source` group.)
        if let Some(clause) =
            try_parse_each_source_power_damage(qty_text, amount_text, before_to, ctx)
        {
            return Some(clause);
        }
        // CR 120.1: The amount of a "deals damage equal to <qty>" clause may be a
        // dynamic count ("the number of creatures you control" — Ajani, Nacatl
        // Avenger). Mirror the sibling "damage to <target> equal to <amount>"
        // branch: try the event-context refs first, then fall back to the general
        // CDA quantity parser (`the number of … you control`, `your life total`,
        // …). Without this fallback the phrase degrades to a raw `Variable`, which
        // resolves to 0 at runtime — the damage silently no-ops.
        let qty =
            crate::parser::oracle_quantity::parse_event_context_quantity(qty_text).or_else(|| {
                crate::parser::oracle_quantity::parse_cda_quantity_with_context(qty_text, ctx)
            });
        let qty = match qty {
            Some(qty) => qty,
            // CR 120.1 + CR 202.3: The typed quantity parsers declined this
            // amount. Only the spell variable "X" resolves through the
            // `Variable` runtime path (`quantity.rs` — `name == "X"`, or a named
            // choice); any OTHER unrecognized phrase ("the total mana value of
            // those exiled cards", Ensnared by the Mara) would be stored
            // verbatim and silently resolve to 0 damage. Storing raw Oracle text
            // as a `Variable` name is the prohibited verbatim-text-in-parser
            // smell, so strict-fail instead: return `None` here, letting the
            // effect lower to `Effect::Unimplemented` so coverage honestly flags
            // the branch as unsupported rather than dealing the wrong (zero)
            // amount. Reaching a resolvable model ("those exiled cards" as a
            // typed exiled-this-resolution mana-value aggregate) is a future
            // building block; until then coverage waits on the strict-failure
            // tag rather than masking the gap.
            None if qty_text.eq_ignore_ascii_case("x") => QuantityExpr::Ref {
                qty: QuantityRef::Variable {
                    name: "X".to_string(),
                },
            },
            None => return None,
        };
        (qty, &amount_text[before_to.len() + 4..])
    } else {
        return None;
    };

    // CR 107.1a: A trailing ", rounded up" / ", rounded down" qualifier sits
    // BETWEEN the "damage" noun and the " to <target>" preposition (e.g.,
    // Banshee — "deals half X damage, rounded down, to any target"). Consume
    // it from `after_target` and propagate the typed RoundingMode onto the
    // already-parsed DivideRounded amount. Necessary because `parse_count_expr`
    // sees only "half X" before the literal "damage" tag fires; the rounding
    // qualifier never reaches the inner combinator.
    let (amount, after_target) = absorb_trailing_rounding_suffix(amount, after_target);

    let after_to = {
        let s = after_target.trim();
        let (rest, _) = opt(tag::<_, _, OracleError<'_>>("to ")).parse(s).unwrap();
        rest.trim()
    };
    // CR 107.3i + CR 120.3: Trim a trailing "where X is <expr>" binding from
    // the recipient phrase before classification. The binding has already been
    // captured at chunk-build time and re-applied via
    // `apply_where_x_ability_expression`; leaving it in the recipient phrase
    // would cause `parse_damage_each_player_scope`'s exact-match check to
    // reject "each player, where X is the number of descent counters on ~",
    // forcing a fall-through to `DamageAll{Typed{empty}}`. Repro: Descent into
    // Avernus. The strip is local to classification — it doesn't disturb the
    // outer chunk-level where-X handling (Token P/T, Pump, SkipNextTurn).
    let after_to_lower_full = after_to.to_lowercase();
    let after_to_for_classification = {
        let tp = TextPair::new(after_to, &after_to_lower_full);
        let (stripped, _) = strip_trailing_where_x(tp);
        // `stripped.original` is a prefix slice of `after_to` (TextPair::new
        // requires byte-length equality, preserved for ASCII). Re-slice
        // `after_to` by the stripped length to keep the outer lifetime.
        &after_to[..stripped.original.len()]
    };
    if tag::<_, _, OracleError<'_>>("each ")
        .parse(after_to_for_classification)
        .is_ok()
    {
        if let Some((target, rem)) =
            parse_each_of_up_to_damage_target(after_to_for_classification, ctx)
        {
            return Some((
                Effect::DealDamage {
                    amount: amount.clone(),
                    target,
                    damage_source: None,
                    excess: None,
                },
                rem,
            ));
        }
        // CR 101.4 + CR 120.3: "… to each player who chose that number" (Wheel of
        // Misfortune). The recipient set is keyed on the secretly-chosen numbers,
        // and the anaphoric "that number" points back to the extremum THIS clause
        // already named as its amount — resolved structurally from the parsed
        // `amount`, never by re-reading the Oracle phrase. Tried before the plain
        // each-player scope so the relative clause is consumed rather than left as
        // a remainder that would widen the damage to every player.
        if let Some(player_filter) = parse_damage_each_chosen_number_scope(
            after_to_for_classification,
            chosen_number_extremum_of(&amount),
        ) {
            return Some((
                Effect::DamageEachPlayer {
                    amount,
                    player_filter,
                },
                "",
            ));
        }
        if let Some(player_filter) = parse_damage_each_player_scope(after_to_for_classification) {
            return Some((
                Effect::DamageEachPlayer {
                    amount,
                    player_filter,
                },
                "",
            ));
        }
        // CR 120.2b + CR 120.3: multi-target chain whose FIRST segment is an
        // each-player scope with a repeated-amount continuation (Dagger Caster:
        // "deals 1 damage to each opponent and 1 damage to each creature your
        // opponents control"). The all-consuming arm above rejected it because
        // the continuation isn't punctuation-only; emit DamageEachPlayer for the
        // player half and hand the continuation back to the chain loop (CR 120.2b
        // independent events). NOT the " and each " compound (caught upstream by
        // the compound parser); the chain joins two separately-amounted segments.
        if let Some((player_filter, rem)) =
            parse_damage_each_player_scope_with_remainder(after_to_for_classification)
        {
            let consumed = after_to_for_classification.len() - rem.len();
            let rem_full = &after_to[consumed..];
            return Some((
                Effect::DamageEachPlayer {
                    amount,
                    player_filter,
                },
                rem_full,
            ));
        }
        let (target, rem) = parse_target_with_ctx(after_to_for_classification, ctx);
        let (target, rem) = refine_damage_target_remainder(target, rem);
        // CR 119.2 + CR 120.3: Composite "each <object> and each <player>"
        // (Chandra's Ignition: "to each other creature and each opponent"). The
        // object filter is captured above; if the remainder begins with
        // "and <player-scope>", lift it into `player_filter` so DamageAll covers
        // both audiences uniformly instead of silently dropping the player half.
        // Mirrors the lift in the simpler "deals N damage to each X and each Y"
        // dispatch upstream (Pompeii, Goblin Chainwhirler, Hurricane class).
        let trimmed = rem.trim_start_matches([',', ' ']);
        let trimmed_lower = trimmed.to_lowercase();
        let player_filter = tag::<_, _, OracleError<'_>>("and ")
            .parse(trimmed_lower.as_str())
            .ok()
            .and_then(|(after_and, _)| parse_damage_each_player_scope(after_and));
        let rem_out = if player_filter.is_some() { "" } else { rem };
        return Some((
            Effect::DamageAll {
                amount,
                target,
                player_filter,
                damage_source: None,
            },
            rem_out,
        ));
    }

    // CR 120.3: "itself" — the source creature is both damage source and recipient.
    let after_to_lower = after_to.to_lowercase();
    if after_to_lower == "itself"
        || tag::<_, _, OracleError<'_>>("itself ")
            .parse(after_to_lower.as_str())
            .is_ok()
    {
        return Some((
            Effect::DealDamage {
                amount,
                target: TargetFilter::ParentTarget,
                damage_source: Some(DamageSource::Target),
                excess: None,
            },
            "",
        ));
    }

    // CR 607.2d: Resolve source-linked persisted "the chosen player" before
    // generic target parsing, where that phrase has different meanings.
    if parse_source_chosen_player_damage_target(after_to) {
        return Some((
            Effect::DealDamage {
                amount: amount.clone(),
                target: TargetFilter::SourceChosenPlayer,
                damage_source: None,
                excess: None,
            },
            "",
        ));
    }

    // CR 115.10a + CR 120.1 + CR 120.3: Check Ghyrson-style mixed event
    // recipients before generic event-context references. This is DealDamage
    // local because `EventTarget` may be a player only for the raw damage
    // recipient carried by DamageDealt.
    if let Some((target, ecr_rem)) = parse_damage_event_target_recipient(after_to, ctx) {
        return Some((
            Effect::DealDamage {
                amount: amount.clone(),
                target,
                damage_source: None,
                excess: None,
            },
            ecr_rem,
        ));
    }

    // CR 608.2k: Check for event-context references before standard target parsing.
    if let Some((target, ecr_rem)) = parse_event_context_ref_with_ctx(after_to, ctx) {
        let (target, ecr_rem) = refine_damage_target_remainder(target, ecr_rem);
        return Some((
            Effect::DealDamage {
                amount: amount.clone(),
                target,
                damage_source: None,
                excess: None,
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
                excess: None,
            },
            "",
        ));
    }

    // CR 603.2b + CR 608.2c: A bare player anaphor recipient ("them" / "they")
    // in a player-scoped trigger body ("At the beginning of each player's
    // upkeep, ~ deals N damage to them") follows the player scope established
    // by the trigger condition — the player whose upkeep it is. The generic
    // pronoun resolver treats bare "them" as an object anaphor and binds it to
    // `ParentTarget`, which has no referent here, so the damage hits no one
    // (Roiling Vortex, issue #2891).
    if let Some(target) = resolve_player_anaphor_damage_recipient(after_to, ctx) {
        return Some((
            Effect::DealDamage {
                amount,
                target,
                damage_source: None,
                excess: None,
            },
            "",
        ));
    }

    let (after_to, multi_target) = strip_optional_target_prefix(after_to);
    if let Some(spec) = multi_target {
        ctx.pending_damage_multi_target = Some(spec);
    }
    let (target, rem) = parse_target_with_ctx(after_to, ctx);
    let (target, rem) = refine_damage_target_remainder(target, rem);
    let rem = trim_dangling_target_word(rem);
    Some((
        Effect::DealDamage {
            amount,
            target,
            damage_source: None,
            excess: None,
        },
        rem,
    ))
}

/// CR 120.1 + CR 601.2c + CR 208.1 + CR 608.2: Parse the multi-source per-power
/// damage tail "their power to <recipient>" (plural possessive). Returns a
/// `DealDamage` whose source is `DamageSource::EachTarget` — every leading object
/// target (the source set chosen by the subject / prior sentence) deals damage
/// equal to ITS OWN power to the shared recipient. The amount is `Power{Anaphoric}`,
/// rebound to `Target` downstream (`wrap_target_subject_damage` for the direct
/// subject form; the one-sided-fight prepend for the "They each ..." back-ref).
///
/// Returns `None` for the singular "its power" form (handled by the existing
/// single-source one-sided-fight path) and for any non-power amount, so the
/// caller's general quantity dispatch is untouched.
///
/// `amount_text` is the original-case slice immediately following "damage equal
/// to "; `before_to` is the lowercase amount slice up to the " to " preposition.
/// The recipient phrase is the original-case tail past `before_to`, with the
/// " to " separator consumed by a `tag` combinator (mirroring the caller's
/// `take_until(" to ")` split) rather than a hard-coded byte offset.
fn try_parse_each_source_power_damage<'a>(
    qty_text: &str,
    amount_text: &'a str,
    before_to: &str,
    ctx: &mut ParseContext,
) -> Option<(Effect, &'a str)> {
    // CR 208.1 + CR 608.2: "their power" / "their toughness" — the per-object
    // characteristic (modifiable, read at resolution) of each source in the set.
    // Bound directly to `ObjectScope::Target`: the `EachTarget` resolver
    // re-resolves the amount against a single-element target slice per source, so
    // `Power{Target}` reads each member's OWN value. This is
    // correct for both the direct subject form (sources prepended ahead of the
    // recipient) and the "They each ..." back-reference (the prior sentence's
    // chosen set is prepended at resolution) — neither needs the anaphoric
    // pronoun rebind the single-source one-sided-fight path relies on.
    let qty = nom_parse_lower(qty_text, |i| {
        all_consuming(alt((
            value(
                QuantityExpr::Ref {
                    qty: QuantityRef::Power {
                        scope: ObjectScope::Target,
                    },
                },
                tag("their power"),
            ),
            value(
                QuantityExpr::Ref {
                    qty: QuantityRef::Toughness {
                        scope: ObjectScope::Target,
                    },
                },
                tag("their toughness"),
            ),
        )))
        .parse(i)
    })?;

    // `before_to` is the lowercase length-equivalent of the amount prefix, so
    // `amount_text[before_to.len()..]` is the original-case tail beginning at the
    // " to " preposition. Consume that separator with a `tag` combinator (the
    // recipient is the parse remainder) instead of a hard-coded byte offset.
    let (_, recipient_tail) = preceded(tag::<_, _, OracleError<'_>>(" to "), rest)
        .parse(&amount_text[before_to.len()..])
        .ok()?;
    let recipient_text = recipient_tail.trim();
    if recipient_text.is_empty() {
        return None;
    }

    // CR 115.1: The shared recipient is a single targeted object ("target
    // creature an opponent controls" / "target creature you don't control").
    // Event-context refs first (mirrors the single-source path), then the
    // general target parser.
    let (target, rem) =
        if let Some((target, ecr_rem)) = parse_event_context_ref_with_ctx(recipient_text, ctx) {
            refine_damage_target_remainder(target, ecr_rem)
        } else {
            let (target, rem) = parse_target_with_ctx(recipient_text, ctx);
            refine_damage_target_remainder(target, rem)
        };
    let rem = trim_dangling_target_word(rem);

    Some((
        Effect::DealDamage {
            amount: qty,
            target,
            damage_source: Some(DamageSource::EachTarget),
            excess: None,
        },
        rem,
    ))
}

/// CR 603.2b + CR 608.2c: Resolve a bare player-anaphor damage recipient
/// ("them" / "they") to the player the trigger's `relative_player_scope`
/// established, mirroring how the "that player" event-context anaphor resolves.
///
/// Returns `None` for any recipient that is not the bare anaphor, and for
/// contexts with neither a player scope nor a player-actor trigger subject — so
/// the caller's generic target parse (and the object "them" → `ParentTarget`
/// anaphor used by, e.g., "destroy them") is left untouched. The scope mapping
/// matches `that_player_library_filter`: `ScopedPlayer` (per-player phase
/// triggers) stays `ScopedPlayer`; the triggering-event and target-player scopes
/// resolve to `TriggeringPlayer`; attack triggers resolve to the
/// `DefendingPlayer`. When no explicit scope is set, a player-actor trigger
/// subject ("an opponent draws a card") makes "them"/"they" the triggering
/// player — the same subject fallback `that_player_library_filter` uses
/// (Razorkin Needlehead, issue #2869).
fn resolve_player_anaphor_damage_recipient(
    after_to: &str,
    ctx: &ParseContext,
) -> Option<TargetFilter> {
    let trimmed = after_to.trim().trim_end_matches(['.', ',', ';']).trim();
    let lower = trimmed.to_lowercase();
    let is_player_anaphor = nom_parse_lower(&lower, |input| {
        all_consuming(value(
            (),
            alt((tag::<_, _, OracleError<'_>>("them"), tag("they"))),
        ))
        .parse(input)
    })
    .is_some();
    if !is_player_anaphor {
        return None;
    }
    // "Attack enchanted player" trigger scope resolves a bare "them"/"they" damage
    // recipient to the defender captured at attack declaration — the
    // shared single-authority binding (`enchanted_player_anaphor_filter`) so this
    // resolver stays complete for the whole curse class (e.g. "~ deals 2 damage to
    // them"), not just the subject verb forms handled in subject.rs.
    if let Some(filter) =
        super::subject::enchanted_player_anaphor_filter(ctx.relative_player_scope.as_ref())
    {
        return Some(filter);
    }
    // CR 608.2c + CR 109.4: "Choose an opponent …. ~ deals that much damage to
    // them." — the recipient is the player the earlier `Choose(Player)` clause
    // selected, carried on `relative_player_scope` across the sentence boundary.
    // Same single-authority binding the subject-position "they" anaphor uses
    // (`resolve_they_pronoun`), so both pronoun positions in this card class
    // (Itazura, Lingering Wick; Gluntch, the Bestower) name the same player.
    if let Some(filter) =
        super::subject::chosen_player_anaphor_filter(ctx.relative_player_scope.as_ref())
    {
        return Some(filter);
    }
    match ctx.relative_player_scope {
        Some(ControllerRef::ScopedPlayer) => Some(TargetFilter::ScopedPlayer),
        Some(ControllerRef::ParentTargetController) => Some(TargetFilter::ParentTargetController),
        Some(ControllerRef::ParentTargetOwner) => Some(TargetFilter::ParentTargetOwner),
        Some(ControllerRef::TriggeringPlayer) | Some(ControllerRef::TargetPlayer) => {
            Some(TargetFilter::TriggeringPlayer)
        }
        Some(ControllerRef::DefendingPlayer) => Some(TargetFilter::DefendingPlayer),
        // CR 608.2k: No explicit player scope — fall back to the trigger
        // subject. A player-actor subject (a bare player filter: empty type
        // filters with a controller ref, e.g. "an opponent draws a card", or
        // `TargetFilter::Player`) makes "them"/"they" the triggering player,
        // not the object the generic pronoun resolver would bind. An
        // object-typed subject (non-empty type filters) keeps that object
        // anaphor. Mirrors `that_player_library_filter`'s subject fallback.
        _ => match &ctx.subject {
            Some(TargetFilter::Typed(tf))
                if tf.type_filters.is_empty() && tf.controller.is_some() =>
            {
                Some(TargetFilter::TriggeringPlayer)
            }
            Some(TargetFilter::Player) => Some(TargetFilter::TriggeringPlayer),
            _ => None,
        },
    }
}

/// CR 607.2d + CR 608.2c + CR 120.1: In damage-recipient grammar, singular
/// "the chosen player" refers to the source object's linked persisted choice
/// (Stuffy Doll class). Kept local to damage parsing so generic target parsing
/// preserves selected-set and resolution-scoped chosen-player meanings.
fn parse_source_chosen_player_damage_target(input: &str) -> bool {
    let lower = input.trim().trim_end_matches('.').to_lowercase();
    let parsed = nom::combinator::all_consuming(value(
        (),
        tag::<_, _, OracleError<'_>>("the chosen player"),
    ))
    .parse(lower.as_str())
    .is_ok();
    parsed
}

/// CR 115.1: `parse_target_with_ctx` consumes "another " but leaves the bare
/// noun "target" in the remainder when no type word follows ("another target,"
/// — Cone of Flame's continuation segments). The trailing word is structural
/// punctuation between the target phrase and the next clause boundary; strip
/// it so downstream chain detection lines up the comma boundary cleanly.
pub(super) fn trim_dangling_target_word(rem: &str) -> &str {
    let trimmed = rem.trim_start_matches([' ']);
    let lower = trimmed.to_lowercase();
    if let Ok((rest_lower, _)) = tag::<_, _, OracleError<'_>>("target").parse(lower.as_str()) {
        // Boundary check: the "target" must be a complete word (followed by
        // EOF, comma, period, or whitespace). Otherwise we'd corrupt phrases
        // like "targeted" / "targets" that legitimately start the remainder.
        if rest_lower.is_empty()
            || rest_lower.starts_with([',', '.'])
            || rest_lower.starts_with(char::is_whitespace)
        {
            return &trimmed["target".len()..];
        }
    }
    rem
}

/// CR 107.1a: A `, rounded up` / `, rounded down` qualifier may appear
/// AFTER the "damage" noun and BEFORE the recipient phrase (Banshee,
/// Spinal Embrace class). When present, propagate the typed
/// [`RoundingMode`] onto a `DivideRounded` amount and consume the suffix
/// from the post-amount remainder so downstream classification ("to <target>")
/// sees a clean string.
///
/// Returns the (possibly updated) amount and the post-suffix remainder.
/// Non-fractional amounts are returned untouched — the suffix only attaches to
/// `DivideRounded` shapes per CR 107.1a; if it appears against a fixed amount
/// it would be malformed Oracle text and we leave it for the recipient parser
/// to surface as `IgnoredRemainder`.
pub(super) fn absorb_trailing_rounding_suffix(
    amount: QuantityExpr,
    after_target: &str,
) -> (QuantityExpr, &str) {
    let trimmed = after_target.trim_start();
    let trimmed_lower = trimmed.to_lowercase();
    let parsed = alt((
        value(
            RoundingMode::Up,
            tag::<_, _, OracleError<'_>>(", rounded up"),
        ),
        value(RoundingMode::Down, tag(", rounded down")),
        value(RoundingMode::Up, tag(", round up")),
        value(RoundingMode::Down, tag(", round down")),
    ))
    .parse(trimmed_lower.as_str());
    let Ok((rest_lower, rounding)) = parsed else {
        return (amount, after_target);
    };
    let consumed = trimmed_lower.len() - rest_lower.len();
    // After consuming the rounding suffix, any immediately following ", " is
    // the boundary delimiter between the rounding qualifier and the
    // recipient phrase ("damage, rounded down, to any target"). Strip it so
    // the downstream "to <target>" classifier sees a clean prefix instead of
    // ", to any target". The comma + space is structural punctuation, not
    // dispatch — the dispatch already happened above.
    let rest = trimmed[consumed..].trim_start_matches(',').trim_start();
    let amount = match amount {
        QuantityExpr::DivideRounded {
            inner,
            divisor,
            rounding: _,
        } => QuantityExpr::DivideRounded {
            inner,
            divisor,
            rounding,
        },
        other => other,
    };
    (amount, rest)
}

fn parse_pump_modifier_phrase(input: &str) -> OracleResult<'_, (PtValue, PtValue)> {
    let (rest, _) = opt(alt((
        tag::<_, _, OracleError<'_>>("an additional "),
        tag("additional "),
    )))
    .parse(input)?;
    let (rest, token) =
        take_till1(|c: char| c.is_whitespace() || c == ',' || c == '.').parse(rest)?;
    let (power, toughness) = parse_pt_modifier(token)
        .ok_or_else(|| nom::Err::Error(OracleError::new(token, nom::error::ErrorKind::Verify)))?;
    Ok((rest, (power, toughness)))
}

pub(crate) fn try_parse_pump(lower: &str, _text: &str) -> Option<Effect> {
    // Match "+N/+M", "+X/+0", "-X/-X", etc.
    let (_, (power, toughness), _) = nom_primitives::scan_preceded(lower, |input| {
        preceded(
            alt((
                tag::<_, _, OracleError<'_>>("gets "),
                tag::<_, _, OracleError<'_>>("get "),
            )),
            parse_pump_modifier_phrase,
        )
        .parse(input)
    })?;
    Some(Effect::Pump {
        power,
        toughness,
        target: TargetFilter::Any,
    })
}

#[cfg(test)]
pub(crate) fn parse_pump_clause(predicate: &str) -> Option<(PtValue, PtValue, Option<Duration>)> {
    parse_pump_clause_with_context(predicate, &ParseContext::default())
}

pub(crate) fn parse_pump_clause_with_context(
    predicate: &str,
    ctx: &ParseContext,
) -> Option<(PtValue, PtValue, Option<Duration>)> {
    let predicate_lower = predicate.to_lowercase();
    let predicate_tp = TextPair::new(predicate, &predicate_lower);
    let (without_where, where_x_expression) = strip_trailing_where_x(predicate_tp);
    // Strip "for each [clause]" suffix before duration extraction.
    let (without_for_each, for_each_qty) =
        strip_trailing_for_each_clause_expr(without_where.original, ctx);
    let (without_duration, duration) = strip_trailing_duration(without_for_each);
    let lower = without_duration.to_lowercase();

    let (_, (power, toughness)) = (|input| {
        let (rest, _) = alt((
            tag::<_, _, OracleError<'_>>("gets "),
            tag::<_, _, OracleError<'_>>("get "),
        ))
        .parse(input)?;
        let (rest, pt) = parse_pump_modifier_phrase(rest)?;
        let (rest, _) = multispace0.parse(rest)?;
        let (rest, _) = opt(terminated(
            alt((tag::<_, _, OracleError<'_>>(","), tag("."))),
            multispace0,
        ))
        .parse(rest)?;
        let (rest, _) = eof.parse(rest)?;
        Ok::<_, nom::Err<OracleError<'_>>>((rest, pt))
    })(lower.as_str())
    .ok()?;
    // CR 107.3c: if the clause defines X but we cannot represent the definition,
    // this pump clause does not lower — fail the parse rather than fabricate a
    // dead placeholder. The line then falls through to the gap path and is
    // reported honestly instead of resolving as a silent +0/+0 no-op.
    let power = apply_where_x_expression(power, where_x_expression.as_deref())?;
    let toughness = apply_where_x_expression(toughness, where_x_expression.as_deref())?;

    // CR 613.4c: Compose with "for each" quantity to produce dynamic PtValue.
    let (power, toughness) = if let Some(quantity) = for_each_qty {
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
/// and the parsed QuantityExpr (if any). Handles both "until end of turn for each X"
/// (duration already stripped) and bare "for each X".
fn strip_trailing_for_each_clause_expr<'a>(
    text: &'a str,
    ctx: &ParseContext,
) -> (&'a str, Option<QuantityExpr>) {
    let lower = text.to_lowercase();
    if let Some(pos) = lower.rfind(" for each ") {
        let clause_text = lower[pos + " for each ".len()..].trim_end_matches('.');
        if let Some(quantity) = parse_for_each_clause_expr_with_context(clause_text, ctx) {
            return (text[..pos].trim(), Some(quantity));
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

/// CR 107.3i + CR 107.3m: Compute, for each chunk, the `where X is <expr>`
/// binding that applies to its enclosing sentence. Sibling clauses of the same
/// sentence share the binding so that "target player loses X life and you gain
/// X life, where X is the greatest power among creatures you control" resolves
/// both X references to the same expression.
///
/// Groups chunks by `ClauseBoundary::Sentence` (Comma/Then/None continue the
/// current sentence). The returned Vec has the same length as `chunks`; each
/// entry is the binding of that chunk's sentence, or `None` if no sibling in
/// the sentence contains a "where X is" suffix.
pub(super) fn compute_sentence_where_x(chunks: &[ClauseChunk]) -> Vec<Option<String>> {
    let mut out = vec![None; chunks.len()];
    let mut group_start = 0usize;
    for (idx, chunk) in chunks.iter().enumerate() {
        let ends_sentence = matches!(chunk.boundary_after, Some(ClauseBoundary::Sentence) | None);
        if ends_sentence {
            // Close the group [group_start..=idx]: scan for a where-X binding.
            let binding = chunks[group_start..=idx].iter().find_map(|c| {
                let lower = c.text.to_lowercase();
                let (_, expr) = strip_trailing_where_x(TextPair::new(&c.text, &lower));
                expr
            });
            if binding.is_some() {
                for slot in &mut out[group_start..=idx] {
                    *slot = binding.clone();
                }
            }
            group_start = idx + 1;
        }
    }
    // CR 107.3i: Normally, all instances of X on an object have the same value
    // at any given time. The first pass binds per-sentence-group; this second
    // pass forward-fills subsequent sentences with no own binding so X
    // references in later sentences (e.g. Thassa's Oracle's "If X is greater
    // than or equal to the number of cards in your library, ...") resolve to
    // the earlier binding. A later sentence with its own binding shadows.
    let mut current: Option<String> = None;
    for slot in out.iter_mut() {
        match slot {
            Some(_) => current = slot.clone(),
            None => *slot = current.clone(),
        }
    }
    out
}

pub(crate) fn strip_trailing_where_x<'a>(tp: TextPair<'a>) -> (TextPair<'a>, Option<String>) {
    for needle in [", where x is ", " where x is "] {
        if let Some((before, after)) = tp.split_around(needle) {
            // CR 608.2c: A where-X binding can precede further instructions in
            // the same resolution. Bound the expression structurally, not by
            // enumerating the verbs that may start the next instruction.
            let mut after_clause = after;
            if let Some((clause, _)) = after.split_around(". ") {
                after_clause = clause;
            }
            after_clause = structurally_bound_where_x_clause(after_clause);
            let expression = after_clause
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

fn structurally_bound_where_x_clause<'a>(clause: TextPair<'a>) -> TextPair<'a> {
    let clause = clause.trim_start().trim_end_matches('.').trim_end();
    // CR 613.4c: a "+X/+Y" pump binds each axis to its own quantity via
    // "<X quantity>, and Y is <Y quantity>" (Aspect of Wolf). The "and Y is …"
    // is a continuation of the SAME binding, not a new instruction, so keep the
    // whole clause when both halves independently parse as where-X quantities —
    // `parse_dynamic_pt_in_text` then splits it back and assigns each half to its
    // axis. Guarded on both halves parsing so a genuine "…, and <verb>" next
    // instruction still falls through to the comma-bounding below.
    if let Ok((_, (x_part, y_part))) = nom_primitives::split_once_on(clause.lower, ", and y is ") {
        if parse_where_x_quantity_expression(x_part).is_some()
            && parse_where_x_quantity_expression(y_part).is_some()
        {
            return clause;
        }
    }
    let mut has_comma = false;
    let mut best_end = None;

    for (idx, _) in clause.lower.match_indices(',') {
        has_comma = true;
        let candidate = clause.slice(0, idx).trim_end();
        if !candidate.is_empty() && parse_where_x_quantity_expression(candidate.original).is_some()
        {
            best_end = Some(candidate.len());
        }
    }

    if let Some(expr) = parse_where_x_quantity_expression(clause.original) {
        let is_constraint = matches!(
            expr,
            QuantityExpr::Ref {
                qty: QuantityRef::Variable { ref name },
            } if name == "X"
        );
        if !is_constraint || best_end.is_none() || !has_comma {
            best_end = Some(clause.len());
        }
    }

    best_end
        .map(|end| clause.slice(0, end).trim_end())
        .unwrap_or(clause)
}

pub(super) fn strip_leading_sequence_connector(text: &str) -> &str {
    let trimmed = text.trim_start();

    if trimmed.eq_ignore_ascii_case("then") {
        return "";
    }

    // Try to strip a leading sequence connector using nom alt().
    // Mixed case requires explicit variants since nom tag() is exact-match.
    // CR 608.2c: "Also" is an additive sequence connector at clause start
    // (Beast Mode); strip like "then"/"and". Position-0 only — mid-sentence
    // "also" (e.g. Repulsor Blast's "it also deals") is never reached here.
    match alt((
        tag::<_, _, OracleError<'_>>("Then, "),
        tag("Then "),
        tag("then, "),
        tag("then "),
        tag("and "),
        tag("And "),
        tag("Also, "),
        tag("Also "),
        tag("also, "),
        tag("also "),
    ))
    .parse(trimmed)
    {
        Ok((rest, _)) => rest,
        Err(_) => trimmed,
    }
}

/// CR 107.3c: A "where X is …" clause DEFINES the value of X in the ability's
/// text — the controller does not choose it. Bind the X placeholder to the typed
/// quantity the clause names.
///
/// Returns `None` when the clause defines X but the parser cannot represent that
/// definition. That is a PARSE FAILURE and callers MUST surface it through
/// `Effect::unimplemented`; they must never fabricate a substitute value.
///
/// This function previously fell back to `PtValue::Variable("<raw oracle text>")`.
/// That fallback was a silent lie: `resolve_variable_pt` (game/effects/pump.rs)
/// dispatches only `X`/`-X` and returns `None` for any other content, so
/// `pt_modifications` pushed NO `ContinuousModification` at all and the pump
/// resolved as a +0/+0 no-op — while the raw text still rendered as a supported
/// dynamic quantity in the coverage report. The node was well-typed and
/// completely dead. Honest failure is the only correct answer here.
fn apply_where_x_expression(value: PtValue, where_x_expression: Option<&str>) -> Option<PtValue> {
    match (value, where_x_expression) {
        (PtValue::Variable(alias), Some(expression)) if alias.eq_ignore_ascii_case("X") => {
            parse_where_x_quantity_expression(expression).map(PtValue::Quantity)
        }
        (PtValue::Variable(alias), Some(expression)) if alias.eq_ignore_ascii_case("-X") => {
            parse_where_x_quantity_expression(expression).map(|inner| {
                PtValue::Quantity(QuantityExpr::Multiply {
                    factor: -1,
                    inner: Box::new(inner),
                })
            })
        }
        // CR 107.3i: an X-bearing P/T slot does not always reach here as
        // `PtValue::Variable("X")`. When the clause grammar has already lowered the slot to
        // a quantity, the unbound placeholder survives one level down, inside the
        // expression tree — Tivash, Gloom Summoner's "create an X/X black Demon creature
        // token with flying, where X is the amount of life you gained this turn" lowers to
        // `PtValue::Quantity(Ref { Variable("X") })`, not `PtValue::Variable("X")`.
        // Matching only the bare-placeholder shape left that X unbound, and the token
        // entered as an 0/0 (dying immediately to CR 704.5f) while the face still rendered
        // as supported. Recurse so the where-clause owns every X in the slot, at whatever
        // depth it sits; `apply_where_x_quantity_expression` is a no-op on a slot that
        // holds no X, so a concrete P/T is left untouched.
        (PtValue::Quantity(quantity), Some(_)) => {
            apply_where_x_quantity_expression(quantity, where_x_expression).map(PtValue::Quantity)
        }
        (value, _) => Some(value),
    }
}

/// CR 608.2c + CR 615.1a + CR 615.4: Collapse the "deal N damage … then prevent X
/// of that damage" idiom (Power Leak, Errant Minion) into a single computed-amount
/// `DealDamage` node.
///
/// Why collapse rather than reorder: prevention effects must already exist as a
/// replacement shield *before* the damage event, and cannot retroactively unwind
/// damage that has already been dealt (CR 615.1a / CR 615.4 — "can't go back in
/// time"). A `DealDamage` immediately followed by a `SequentialSibling`
/// `PreventDamage` deals its damage first and leaves a dangling, mistimed shield;
/// worse, a numeric `PreventionAmount::Next(n)` shield deplete per damage event
/// (CR 615.7), so any unconsumed capacity leaks onto a later, unrelated damage
/// event to the same recipient this turn. Folding the arithmetic into the damage
/// amount up front (max(N − X, 0)) is the only shape that yields the printed net
/// damage with no residual shield. This mirrors the shipped precedent of folding
/// "Destroy … It can't be regenerated" into one `Effect::Destroy { cant_regenerate }`
/// node rather than two effect nodes (CR 608.2c: later text modifies earlier text).
///
/// The rewrite fires ONLY on the exact structural shape — all five guards must hold
/// together, so it is a category rewrite (any "deal N then prevent the paid-mana
/// amount" card), never a card-name special case:
/// 1. this node's effect is `DealDamage { amount: Fixed(n), .. }`;
/// 2. its `sub_ability` is a `SequentialSibling`;
/// 3. that sub's effect is a blanket where-X prevention shield:
///    `PreventDamage { target: Any, damage_source_filter: None,
///    prevention_duration: None, scope: AllDamage, amount_dynamic: Some(expr), .. }`.
///
/// On a match the damage amount becomes `max(n − X, 0)` (`ClampMin { Offset {
/// Multiply(-1, expr), n }, 0 }` — CR 107.1b: a negative computed result uses 0),
/// the original `target`/`damage_source`/`excess` are preserved unchanged, and the
/// prevention node is spliced out, promoting anything that followed it (none exists
/// for Power Leak/Errant Minion today, but a future trailing rider is not dropped).
/// Recurses so the idiom is folded wherever it sits in the chain (e.g. beneath the
/// "that player may pay any amount of mana" `PayCost` head for Power Leak).
pub(super) fn fold_deal_damage_then_prevent_into_computed_amount(def: &mut AbilityDefinition) {
    // Guard 1: this node deals a fixed amount of damage.
    let n = match def.effect.as_ref() {
        Effect::DealDamage {
            amount: QuantityExpr::Fixed { value },
            ..
        } => *value,
        _ => {
            recurse_fold_deal_damage_then_prevent(def);
            return;
        }
    };

    // Guards 2 + 3: an immediately-following SequentialSibling that is the exact
    // blanket where-X prevention shield. Extract its dynamic prevention amount.
    let folded_expr = match def.sub_ability.as_ref() {
        Some(next) if next.sub_link == SubAbilityLink::SequentialSibling => {
            match next.effect.as_ref() {
                Effect::PreventDamage {
                    target: TargetFilter::Any,
                    damage_source_filter: None,
                    prevention_duration: None,
                    scope: PreventionScope::AllDamage,
                    amount_dynamic: Some(expr),
                    ..
                } => Some(expr.clone()),
                _ => None,
            }
        }
        _ => None,
    };

    let Some(expr) = folded_expr else {
        recurse_fold_deal_damage_then_prevent(def);
        return;
    };

    // CR 615.1a + CR 107.1b: net damage is max(n − X, 0). Preserve every other
    // DealDamage field (target already correctly TriggeringPlayer, plus
    // damage_source / excess) by mutating only the amount in place.
    if let Effect::DealDamage { amount, .. } = def.effect.as_mut() {
        *amount = QuantityExpr::ClampMin {
            inner: Box::new(QuantityExpr::Offset {
                inner: Box::new(QuantityExpr::Multiply {
                    factor: -1,
                    inner: Box::new(expr),
                }),
                offset: n,
            }),
            minimum: 0,
        };
    }

    // Splice out the PreventDamage node, promoting whatever followed it (if any).
    let promoted = def
        .sub_ability
        .as_mut()
        .and_then(|prevent_node| prevent_node.sub_ability.take());
    def.sub_ability = promoted;

    // Continue walking: the promoted chain (or any nested branch) may itself
    // contain the idiom.
    recurse_fold_deal_damage_then_prevent(def);
}

/// Recurse the fold into a definition's `sub_ability` chain. Kept separate so the
/// early-return arms above and the post-rewrite tail all share one walk.
fn recurse_fold_deal_damage_then_prevent(def: &mut AbilityDefinition) {
    if let Some(sub) = def.sub_ability.as_mut() {
        fold_deal_damage_then_prevent_into_computed_amount(sub);
    }
}

/// CR 601.2h + CR 106.4: Recognize the "the [total ]amount of mana [<payer> ]paid
/// this way" where-X binding phrase across its known surface variants. Composed
/// along its grammar axes rather than enumerating one `tag()` literal per card:
///
/// - fixed `"the "` lead,
/// - optional `"total "` qualifier (Join Forces cards),
/// - fixed `"amount of mana "` head — deliberately mana-scoped so it can never
///   match the `{E}` (energy) "paid this way" family (CR 106 vs CR 122),
/// - optional payer-subject clause (`"that player "` / `"they "` / bare),
/// - fixed `"paid this way"` tail.
///
/// Operates on already-lowercased input. Callers require an empty remainder.
fn parse_amount_of_mana_paid_this_way(input: &str) -> OracleResult<'_, ()> {
    let (input, _) = tag("the ").parse(input)?;
    let (input, _) = opt(tag("total ")).parse(input)?;
    let (input, _) = tag("amount of mana ").parse(input)?;
    let (input, _) = opt(alt((tag("that player "), tag("they ")))).parse(input)?;
    let (input, _) = tag("paid this way").parse(input)?;
    Ok((input, ()))
}

pub(crate) fn parse_where_x_quantity_expression(where_x_expression: &str) -> Option<QuantityExpr> {
    let expression = where_x_expression.trim().trim_end_matches('.');
    let expression_lower = expression.to_ascii_lowercase();
    // CR 702.51c + CR 603.3: Knight-Errant of Eos reads the number of
    // creatures that convoked the spell which became this permanent. The
    // casting pipeline preserves that count through the zone change, so the
    // ETB reveal filter can use the existing source-relative quantity.
    if all_consuming(preceded(
        tag::<_, _, OracleError<'_>>("the number of creatures that convoked "),
        alt((tag("this creature"), tag("~"))),
    ))
    .parse(expression_lower.as_str())
    .is_ok()
    {
        return Some(QuantityExpr::Ref {
            qty: QuantityRef::ConvokedCreatureCount,
        });
    }
    // CR 107.3i + CR 608.2g: Within a single resolution, X has one value used
    // everywhere it appears. Join Forces ("Each player draws X cards, where
    // X is the total amount of mana paid this way") binds X to the total
    // payments accumulated by the upstream `PayCost { Mana { X } }` loop:
    // `engine_resolution_choices::handle_resolution_choice` stamps the
    // accumulated total onto the chained `chosen_x` slot at each
    // `PayAmountChoice` round-trip. Normalizing the phrase to
    // `QuantityRef::Variable("X")` lets the existing X-resolution machinery
    // do the rest — this is also the one-line fix that unblocks Collective
    // Voyage (#131), Alliance of Arms, Shared Trauma, and Mana-Charged
    // Dragon, since all five Join Forces cards share this binding phrase.
    // CR 601.2h + CR 106.4: The "amount of mana … paid this way" family binds X
    // to the mana accumulated by the upstream `PayAmountChoice` loop regardless
    // of the surface phrasing. Rather than one literal per card, compose the
    // shared structural axes: the fixed "the " lead, an optional "total "
    // qualifier (Join Forces cards — Alliance of Arms, Collective Voyage,
    // Mana-Charged Dragon, Minds Aglow, Shared Trauma), the fixed
    // "amount of mana " head, an optional payer-subject clause ("that player " —
    // Power Leak / Errant Minion; "they " — Liege of the Hollows; or bare), and
    // the fixed "paid this way" tail. The head is deliberately kept mana-scoped
    // ("amount of mana", never a resource-generic capture) so it structurally
    // cannot match the energy variants ("amount of {E} paid this way" — CR 106
    // vs CR 122), which are handled elsewhere.
    if parse_amount_of_mana_paid_this_way(expression_lower.as_str())
        .is_ok_and(|(rest, ())| rest.is_empty())
    {
        return Some(QuantityExpr::Ref {
            qty: QuantityRef::Variable {
                name: "X".to_string(),
            },
        });
    }
    // CR 107.3i + CR 608.2g: "where X is less than or equal to <bound>" is a
    // constraint on the player's chosen X (not a definition of X's exact
    // value). Well of Lost Dreams pays {X} mana and draws X cards; the bound
    // only limits what the player may choose — the actual drawn count is the
    // amount paid (resolved via `chosen_x`). Preserving Variable("X") lets the
    // existing PayAmountChoice → chosen_x → draw machinery work correctly.
    if parse_comparator_prefix(expression_lower.as_str())
        .is_some_and(|(_, bound)| !bound.trim().is_empty())
    {
        return Some(QuantityExpr::Ref {
            qty: QuantityRef::Variable {
                name: "X".to_string(),
            },
        });
    }
    if let Ok((rest_lower, (n, sign))) = (
        nom_primitives::parse_number,
        alt((
            value(1i32, tag::<_, _, OracleError<'_>>(" plus ")),
            value(-1i32, tag(" minus ")),
        )),
    )
        .parse(expression_lower.as_str())
    {
        let consumed = expression_lower.len() - rest_lower.len();
        if let Some(inner) = parse_where_x_quantity_expression(&expression[consumed..]) {
            let inner = if sign < 0 {
                QuantityExpr::Multiply {
                    factor: -1,
                    inner: Box::new(inner),
                }
            } else {
                inner
            };
            let offset = QuantityExpr::Offset {
                inner: Box::new(inner),
                offset: n as i32,
            };
            // CR 107.1b: "where X is N minus …" can resolve negative; damage
            // and other effect-result quantities use zero instead (The Rack).
            return Some(if sign < 0 {
                QuantityExpr::ClampMin {
                    inner: Box::new(offset),
                    minimum: 0,
                }
            } else {
                offset
            });
        }
    }
    if let Some(expr) = parse_where_x_cards_named_in_all_graveyards(expression) {
        return Some(expr);
    }
    if let Some(expr) = parse_where_x_kicker_count(expression) {
        return Some(expr);
    }
    if let Some(expr) = parse_where_x_scry_look_count(expression) {
        return Some(expr);
    }
    if let Some(expr) = parse_where_x_exiled_card_power(expression_lower.as_str()) {
        return Some(expr);
    }
    let lower = expression.to_ascii_lowercase();
    if tag::<_, _, OracleError<'_>>("the number of times ")
        .parse(lower.as_str())
        .is_ok()
    {
        return None;
    }
    // CR 608.2c + CR 115.10a + CR 202.3: "that card's mana value" in a "where X
    // is …" binding is anaphoric, not targeted. The revealed/looked-at card is
    // an affected object introduced by an earlier instruction in the SAME
    // ability (e.g. Twilight Prophet's "reveal the top card … Each opponent
    // loses X life … where X is that card's mana value") — CR 115.10a: it is
    // NOT a target (no "target" word), so it must resolve against the anaphoric
    // referent, not the empty target slot. `parse_cda_quantity` (below) would
    // hard-map "that card's mana value" to `ObjectScope::Target` (see
    // `oracle_target::parse_mana_value_reference_qty`), which reads only the
    // target slot and yields 0 at runtime. Route ONLY the literal "card"
    // possessive through `parse_event_context_quantity`, which classifies the
    // demonstrative referent as `ObjectScope::Demonstrative` (resolved via
    // `effect_context_object` — the revealed card, LKI-snapshotted before it
    // moves zones, CR 202.3 mana value). "card" is the only unambiguously-safe
    // referent: unlike "creature"/"permanent"/"planeswalker" (which are correct
    // `Target` for targeted where-X cards like Feeding Grounds) it is never a
    // battlefield target here. "spell" is explicitly excluded — its current
    // `EventSource` binding (Draining Whelk class) must be preserved, and
    // `parse_event_context_quantity` would instead emit `Demonstrative` for it.
    // Restricted to the mana-value property only (CR 202.3), never power /
    // toughness.
    if is_that_card_mana_value_where_x(expression_lower.as_str()) {
        // Pass the already-trimmed `expression` (trailing `.` stripped at the top
        // of this fn), not the raw `where_x_expression`: the guard matches the
        // trimmed phrase, so a punctuation-bearing input like "that card's mana
        // value." must resolve through the same trimmed text or the demonstrative
        // binding would fall back to `None` and the bug would survive.
        return parse_event_context_quantity(expression);
    }
    // CDA-quantity classification takes precedence: it is the more specific
    // where-X interpreter (object counts, "that spell's mana value",
    // "the number of age counters on this enchantment", etc.).
    if let Some(expr) = parse_cda_quantity(where_x_expression) {
        return Some(expr);
    }
    // CR 107.3i: Keep the compositional nom quantity grammar available to
    // where-X bindings after the more-specific CDA interpreter has declined
    // them. This supplies a single X value for past-tense event-subject forms
    // such as "the number of counters it had" without a card-name-specific
    // token parser.
    if let Ok((_, qty)) = nom_quantity::parse_quantity_ref_complete(expression_lower.as_str()) {
        return Some(QuantityExpr::Ref { qty });
    }
    // CR 107.3i + CR 115.1: Some where-X definitions spell the count as
    // "the number of <for-each clause>" where the clause itself may need a
    // target player ("Islands target opponent controls"). Keep that grammar in
    // the shared where-X interpreter so every effect family gets the same
    // `ControllerRef::TargetPlayer` quantity binding.
    if let Some(expr) = parse_where_x_number_of_for_each_clause(expression_lower.as_str()) {
        return Some(expr);
    }
    // CR 107.3f + CR 113.7: "where X is [printed card name]'s power" refers to the
    // ability source (Halana and Alena, Partners). Must precede
    // `parse_event_context_quantity`, which only recognizes anaphoric/participle
    // possessives.
    if let Some(expr) = parse_where_x_printed_name_possessive_stat(expression_lower.as_str()) {
        return Some(expr);
    }
    // CR 706.2 + CR 706.4: "where X is the result" of a die roll / coin flip
    // binds X to the rolled value via the shared `EventContextAmount` channel
    // (the same one inline "you gain life equal to the result" cards use). This
    // is a FALLBACK below `parse_cda_quantity` — `parse_event_context_quantity`
    // has a broad `parse_quantity_ref` fallback that would otherwise mis-classify
    // CDA-handled phrases, so CDA must win first. `parse_cda_quantity` returns
    // `None` for the bare die-result phrase (see `cda_quantity_returns_none_for_the_result`),
    // so this fallback is what binds Ancient Bronze Dragon's "where X is the result".
    crate::parser::oracle_quantity::parse_event_context_quantity(where_x_expression)
}

/// CR 107.3c + CR 608.2h: "where X is the power of the exiled card" DEFINES X as
/// the power of the card exiled by this ability's source — Bishop of Binding
/// ("Whenever this creature attacks, target Vampire gets +X/+X until end of
/// turn, where X is the power of the exiled card") and Redemptor Dreadnought.
/// CR 608.2h: the exiled card is read via last known information, which is what
/// `QuantityRef::ExiledCardPower` resolves against (game/quantity.rs).
///
/// `index: 0` is the first (and, for this class, only) card the source exiled.
fn parse_where_x_exiled_card_power(expression_lower: &str) -> Option<QuantityExpr> {
    all_consuming(value(
        QuantityExpr::Ref {
            qty: QuantityRef::ExiledCardPower { index: 0 },
        },
        tag::<_, _, OracleError<'_>>("the power of the exiled card"),
    ))
    .parse(expression_lower)
    .ok()
    .map(|(_, expr)| expr)
}

/// CR 608.2c + CR 202.3: Match EXACTLY `that card's mana value` (or its
/// `converted mana cost` synonym; CR 202.3 defines the mana value) — the
/// anaphoric "that card's MV"
/// where-X referent. Matches only the literal `card` possessive (never `spell`,
/// `creature`, `permanent`, or `planeswalker`) and only the mana-value property
/// (never power/toughness). Callers route a positive match through
/// `parse_event_context_quantity` so the referent classifies as
/// `ObjectScope::Demonstrative` (CR 115.10a: not a target).
fn is_that_card_mana_value_where_x(expression_lower: &str) -> bool {
    all_consuming(preceded(
        tag::<_, _, OracleError<'_>>("that card's "),
        alt((tag("mana value"), tag("converted mana cost"))),
    ))
    .parse(expression_lower)
    .is_ok()
}

/// CR 107.3f + CR 113.7: Printed-name possessive in a where-X binding
/// ("Halana and Alena's power" → `Power { scope: Source }`). Determiner-led
/// forms ("the sacrificed creature's power", "~'s power") are rejected here and
/// handled by `parse_cda_quantity` / `parse_event_context_quantity` upstream.
fn parse_where_x_printed_name_possessive_stat(expression_lower: &str) -> Option<QuantityExpr> {
    let blocked_prefix = alt((
        tag::<_, _, OracleError<'_>>("that "),
        tag("the "),
        tag("target "),
        tag("its "),
        tag("this "),
        tag("sacrificed "),
        tag("discarded "),
        tag("destroyed "),
        tag("exiled "),
        tag("milled "),
        tag("revealed "),
        tag("targeted "),
        tag("entered "),
        tag("~"),
    ));
    let non_empty = |subject: &str| subject.chars().any(|c| !c.is_whitespace());
    let possessive_stat = alt((
        map(
            (
                verify(take_until::<_, _, OracleError<'_>>("'s power"), non_empty),
                tag("'s power"),
            ),
            |(_, _)| QuantityRef::Power {
                scope: ObjectScope::Source,
            },
        ),
        map(
            (
                verify(
                    take_until::<_, _, OracleError<'_>>("'s toughness"),
                    non_empty,
                ),
                tag("'s toughness"),
            ),
            |(_, _)| QuantityRef::Toughness {
                scope: ObjectScope::Source,
            },
        ),
    ));
    let (_, qty) = all_consuming(preceded(not(blocked_prefix), possessive_stat))
        .parse(expression_lower)
        .ok()?;
    Some(QuantityExpr::Ref { qty })
}

fn parse_where_x_number_of_for_each_clause(expression_lower: &str) -> Option<QuantityExpr> {
    let (clause, _) = tag::<_, _, OracleError<'_>>("the number of ")
        .parse(expression_lower)
        .ok()?;
    parse_for_each_clause_expr(clause)
}

fn parse_where_x_cards_named_in_all_graveyards(where_x_expression: &str) -> Option<QuantityExpr> {
    let lower = where_x_expression.to_ascii_lowercase();
    let (rest, name_lower) = preceded(
        tag::<_, _, OracleError<'_>>("the number of cards named "),
        take_until(" in all graveyards"),
    )
    .parse(lower.as_str())
    .ok()?;
    let (rest, _) = tag::<_, _, OracleError<'_>>(" in all graveyards")
        .parse(rest)
        .ok()?;
    let (rest, _) = opt(tag::<_, _, OracleError<'_>>(" as you cast this spell"))
        .parse(rest)
        .ok()?;
    if !rest.is_empty() || name_lower.trim().is_empty() {
        return None;
    }
    let name_offset = lower.find(name_lower)?;
    let name = where_x_expression[name_offset..name_offset + name_lower.len()].trim();
    Some(QuantityExpr::Ref {
        qty: QuantityRef::ObjectCount {
            filter: TargetFilter::Typed(TypedFilter {
                type_filters: vec![TypeFilter::Card],
                controller: None,
                properties: vec![
                    FilterProp::Named {
                        name: name.to_string(),
                    },
                    FilterProp::InZone {
                        zone: Zone::Graveyard,
                    },
                ],
            }),
        },
    })
}

fn parse_where_x_kicker_count(where_x_expression: &str) -> Option<QuantityExpr> {
    let lower = where_x_expression.to_ascii_lowercase();
    let (_, qty) = nom_quantity::parse_kicker_count_where_x_expression(lower.as_str()).ok()?;
    Some(QuantityExpr::Ref { qty })
}

/// CR 701.22a: "where X is the number of cards looked at while scrying this
/// way" (Elrond, Master of Healing) binds X to the effective (post-clamp)
/// look count of the scry that fired the enclosing "whenever you scry"
/// trigger — `QuantityRef::TriggeringScryLookCount`, resolved per-trigger
/// from that trigger's own preserved scry event. Delegates to the composed
/// grammar in `oracle_nom::quantity` (shared with the general quantity-ref
/// channel); the where-X binder additionally requires the phrase to own the
/// entire expression.
fn parse_where_x_scry_look_count(where_x_expression: &str) -> Option<QuantityExpr> {
    let lower = where_x_expression.to_ascii_lowercase();
    let (rest, qty) = nom_quantity::parse_scry_look_count_ref(lower.as_str()).ok()?;
    rest.is_empty().then_some(QuantityExpr::Ref { qty })
}

/// CR 107.3c: A "where X is …" clause DEFINES the value of X in the ability's
/// text — the controller does not choose it. Bind every X reference in the
/// quantity channel to the typed quantity the clause names.
///
/// Returns `None` when the clause defines X but the parser cannot represent that
/// definition. That is a PARSE FAILURE and callers MUST surface it through
/// `Effect::unimplemented`; they must never fabricate a substitute value.
///
/// This function previously fell back to
/// `QuantityRef::Variable { name: "<raw oracle text>" }`. That fallback was a
/// silent lie, and it is the quantity-channel twin of the `PtValue::Variable`
/// lie removed in the P/T channel: `game/quantity.rs` dispatches the non-`"X"`
/// `Variable` arm through `state.last_named_choice` and `.unwrap_or(0)`, so the
/// quantity read 0 — or, worse, an unrelated number left behind by some earlier
/// "choose a number" — while the raw text still rendered as a supported dynamic
/// quantity in the coverage report. Porcuparrot dealt 0 damage; Abby made 0
/// tokens. Every such node was well-typed and completely dead. Honest failure is
/// the only correct answer here.
///
/// Note that `None` is returned ONLY when the node actually carries an X
/// reference (bare `Variable("X")` or `CostXPaid`) that this clause was supposed
/// to bind. A node with no X reference is returned unchanged as `Some`, so an
/// unrepresentable where-X clause on an ability that never uses X cannot poison
/// that ability.
pub(super) fn apply_where_x_quantity_expression(
    value: QuantityExpr,
    where_x_expression: Option<&str>,
) -> Option<QuantityExpr> {
    Some(match value {
        // CR 107.3i: Generic "X is N or more" condition parsing defaults to
        // CostXPaid for X-cost spells, but a surrounding "where X is ..." clause
        // is the more specific binding and must own every X reference in the
        // ability, including later-sentence rider conditions.
        QuantityExpr::Ref {
            qty: QuantityRef::CostXPaid,
        } if where_x_expression.is_some() => {
            let expression = where_x_expression.expect("checked is_some above");
            parse_where_x_quantity_expression(expression)?
        }
        QuantityExpr::Ref {
            qty: QuantityRef::Variable { name },
        } if where_x_expression.is_some() && name.eq_ignore_ascii_case("X") => {
            let expression = where_x_expression.expect("checked is_some above");
            parse_where_x_quantity_expression(expression)?
        }
        // CR 107.3i: "search ... for up to X ..., where X is …" wraps the X
        // count in `UpTo`. Recurse into `max` so the defining clause rewrites
        // the inner `Variable("X")` (Oreskos Explorer's "up to X Plains cards"
        // must bind X to the where-clause population, not stay at 0). `up_to`
        // re-asserts the non-nesting invariant.
        QuantityExpr::UpTo { max } => {
            QuantityExpr::up_to(apply_where_x_quantity_expression(*max, where_x_expression)?)
        }
        QuantityExpr::Offset { inner, offset } => QuantityExpr::Offset {
            inner: Box::new(apply_where_x_quantity_expression(
                *inner,
                where_x_expression,
            )?),
            offset,
        },
        QuantityExpr::ClampMin { inner, minimum } => QuantityExpr::ClampMin {
            inner: Box::new(apply_where_x_quantity_expression(
                *inner,
                where_x_expression,
            )?),
            minimum,
        },
        QuantityExpr::Multiply { factor, inner } => QuantityExpr::Multiply {
            factor,
            inner: Box::new(apply_where_x_quantity_expression(
                *inner,
                where_x_expression,
            )?),
        },
        QuantityExpr::DivideRounded {
            inner,
            divisor,
            rounding,
        } => QuantityExpr::DivideRounded {
            inner: Box::new(apply_where_x_quantity_expression(
                *inner,
                where_x_expression,
            )?),
            divisor,
            rounding,
        },
        QuantityExpr::Sum { exprs } => QuantityExpr::Sum {
            exprs: exprs
                .into_iter()
                .map(|expr| apply_where_x_quantity_expression(expr, where_x_expression))
                .collect::<Option<Vec<_>>>()?,
        },
        QuantityExpr::Max { exprs } => QuantityExpr::Max {
            exprs: exprs
                .into_iter()
                .map(|expr| apply_where_x_quantity_expression(expr, where_x_expression))
                .collect::<Option<Vec<_>>>()?,
        },
        QuantityExpr::Difference { left, right } => QuantityExpr::Difference {
            left: Box::new(apply_where_x_quantity_expression(
                *left,
                where_x_expression,
            )?),
            right: Box::new(apply_where_x_quantity_expression(
                *right,
                where_x_expression,
            )?),
        },
        QuantityExpr::Power { base, exponent } => QuantityExpr::Power {
            base,
            exponent: Box::new(apply_where_x_quantity_expression(
                *exponent,
                where_x_expression,
            )?),
        },
        other => other,
    })
}

/// Bind an X-bearing quantity slot in place, recording an unrepresentable
/// where-X definition instead of fabricating one (CR 107.3c).
///
/// This is the single authority for the "rewrite a quantity slot under a
/// where-X clause" operation: every call site in the where-X rewriter family
/// routes through it so that a failed bind is reported exactly once, in one
/// way — as `unbound`, which the caller converts to `Effect::unimplemented`.
/// Callers must never inspect the binding themselves or supply a default.
fn bind_where_x_quantity(
    slot: &mut QuantityExpr,
    where_x_expression: Option<&str>,
    unbound: &mut Option<String>,
) {
    match apply_where_x_quantity_expression(slot.clone(), where_x_expression) {
        Some(bound) => *slot = bound,
        None => *unbound = where_x_expression.map(str::to_string),
    }
}

/// An absent slot has no X to bind, so `None` is left alone; a present one routes
/// through the single authority above.
fn bind_where_x_optional_quantity(
    slot: Option<&mut QuantityExpr>,
    where_x_expression: Option<&str>,
    unbound: &mut Option<String>,
) {
    if let Some(slot) = slot {
        bind_where_x_quantity(slot, where_x_expression, unbound);
    }
}

/// CR 122.1 + CR 107.3i: "enters with X +1/+1 counters on it, where X is …" — the counter
/// COUNT of an enters-with rider is an ordinary where-X quantity slot. Shared by every
/// carrier of the `(CounterType, QuantityExpr)` rider shape (`Token`, `ChangeZone`,
/// `ChangeZoneAll`) so the rider binds identically wherever it appears; G'raha Tia, Scion
/// Reborn ("create a 1/1 … Hero … and put X +1/+1 counters on it") rides this slot, and it
/// sits BESIDE the token's own `count`/`power`/`toughness` rather than inside them.
fn bind_where_x_enter_with_counters(
    entries: &mut [(CounterType, QuantityExpr)],
    where_x_expression: Option<&str>,
    unbound: &mut Option<String>,
) {
    for (_, count) in entries.iter_mut() {
        bind_where_x_quantity(count, where_x_expression, unbound);
    }
}

/// CR 613.4b: an absent P/T override has no X to bind. A present one routes through the
/// same `PtValue` rewriter the `Token`/`Pump` arms use, so a failed bind is reported as
/// `unbound` exactly as it is for a quantity slot.
fn bind_where_x_optional_pt(
    slot: &mut Option<PtValue>,
    where_x_expression: Option<&str>,
    unbound: &mut Option<String>,
) {
    let Some(value) = slot.as_ref() else {
        return;
    };
    match apply_where_x_expression(value.clone(), where_x_expression) {
        Some(bound) => *slot = Some(bound),
        None => *unbound = where_x_expression.map(str::to_string),
    }
}

pub(super) fn apply_where_x_effect_expression(
    effect: &mut Effect,
    where_x_expression: Option<&str>,
) {
    // CR 107.3c: set when the clause DEFINES X but the definition is not
    // representable. Recorded here and converted to a gap node after the match
    // (the arms hold a mutable borrow of `effect`'s fields).
    let mut unbound_where_x: Option<String> = None;
    match effect {
        Effect::DealDamage { amount, .. }
        | Effect::DamageAll { amount, .. }
        | Effect::DamageEachPlayer { amount, .. }
        | Effect::GainLife { amount, .. }
        | Effect::LoseLife { amount, .. }
        | Effect::ChangeSpeed { amount, .. }
        | Effect::Draw { count: amount, .. }
        | Effect::Mill { count: amount, .. }
        | Effect::PutCounter { count: amount, .. }
        | Effect::PutCounterAll { count: amount, .. }
        | Effect::ExileTop { count: amount, .. }
        | Effect::Discover {
            mana_value_limit: amount,
            ..
        }
        // CR 701.47a: "amass Orcs X, where X is …" (Fall of Cair Andros) — "put N
        // +1/+1 counters on that creature", so N is the bound quantity.
        | Effect::Amass { count: amount, .. }
        | Effect::Incubate { count: amount }
        // The rest of the single-quantity carriers. Each of these owns exactly one
        // `QuantityExpr` slot that a "where X is …" clause can define, and each was
        // previously falling through the `_ => {}` below — which did not bind, so the bare
        // `QuantityRef::Variable("X")` survived and resolved to 0 at runtime (amass 0 /
        // surveil 0 / discard 0 / monstrosity 0) while the face still rendered as fully
        // supported. The totality guard converted that fabrication into an honest red
        // (#5753); binding them here is what turns the representable ones back to green.
        | Effect::Adapt { count: amount, .. }
        | Effect::AddPendingETBCounters { count: amount, .. }
        | Effect::AdditionalPhase { count: amount, .. }
        | Effect::AssembleContraptions { count: amount, .. }
        | Effect::Bolster { count: amount, .. }
        | Effect::ChooseCounterAdjustment { count: amount, .. }
        | Effect::Cloak { count: amount, .. }
        | Effect::Connive { count: amount, .. }
        | Effect::CopyTokenOf { count: amount, .. }
        | Effect::Discard { count: amount, .. }
        | Effect::EachSourceDealsDamage { amount, .. }
        | Effect::Endure { amount, .. }
        | Effect::FlipCoins { count: amount, .. }
        | Effect::GainEnergy { amount, .. }
        | Effect::GivePlayerCounter { count: amount, .. }
        | Effect::GrantExtraLoyaltyActivations { amount, .. }
        | Effect::Intensify { amount, .. }
        | Effect::Manifest { count: amount, .. }
        | Effect::Monstrosity { count: amount, .. }
        | Effect::PutAtLibraryPosition { count: amount, .. }
        | Effect::PutChosenCounter { count: amount, .. }
        | Effect::RemoveCounter { count: amount, .. }
        | Effect::Renown { count: amount, .. }
        | Effect::RevealUntil { count: amount, .. }
        | Effect::RollDie { count: amount, .. }
        | Effect::Sacrifice { count: amount, .. }
        | Effect::SearchOutsideGame { count: amount, .. }
        | Effect::SetLifeTotal { amount, .. }
        | Effect::SkipNextStep { count: amount, .. }
        | Effect::SkipNextTurn { count: amount, .. }
        | Effect::Surveil { count: amount, .. } => {
            bind_where_x_quantity(amount, where_x_expression, &mut unbound_where_x);
        }
        // Multi-slot carriers: a where-X clause defines ONE X, and every slot that
        // references it must bind to the same expression (CR 107.3i: X has a single value
        // for the whole ability). Binding only the "main" count would leave the siblings
        // as bare placeholders resolving to 0.
        Effect::ChooseDrawnThisTurnPayOrTopdeck {
            count,
            life_payment,
            ..
        } => {
            bind_where_x_quantity(count, where_x_expression, &mut unbound_where_x);
            bind_where_x_quantity(life_payment, where_x_expression, &mut unbound_where_x);
        }
        Effect::CreateTokenCopyFromPool {
            mv_bound, count, ..
        } => {
            bind_where_x_quantity(mv_bound, where_x_expression, &mut unbound_where_x);
            bind_where_x_quantity(count, where_x_expression, &mut unbound_where_x);
        }
        Effect::PutSticker {
            count,
            max_ticket_cost,
            ..
        } => {
            bind_where_x_quantity(count, where_x_expression, &mut unbound_where_x);
            bind_where_x_optional_quantity(
                max_ticket_cost.as_mut(),
                where_x_expression,
                &mut unbound_where_x,
            );
        }
        // The optional-count carriers: same single where-X slot, but absent by default
        // (`RevealHand` with no count reveals the whole hand; `MoveCounters` with no count
        // moves all of them). `None` has no X to bind and is left alone.
        Effect::MoveCounters { count, .. }
        | Effect::CastCopyOfCard { count, .. }
        | Effect::RevealHand { count, .. } => {
            bind_where_x_optional_quantity(
                count.as_mut(),
                where_x_expression,
                &mut unbound_where_x,
            );
        }
        Effect::ChooseAndSacrificeRest {
            total_power_cap, ..
        } => {
            bind_where_x_optional_quantity(
                total_power_cap.as_mut(),
                where_x_expression,
                &mut unbound_where_x,
            );
        }
        // CR 122.1: the mass-move counterpart of `ChangeZone`'s enters-with rider.
        Effect::ChangeZoneAll {
            target,
            enter_with_counters,
            ..
        } => {
            bind_where_x_filter(target, where_x_expression, &mut unbound_where_x);
            bind_where_x_enter_with_counters(
                enter_with_counters,
                where_x_expression,
                &mut unbound_where_x,
            );
        }
        Effect::Token {
            count,
            power,
            toughness,
            enter_with_counters,
            ..
        } => {
            bind_where_x_quantity(count, where_x_expression, &mut unbound_where_x);
            // CR 122.1: the enters-with rider is a THIRD X site on a token, beside the
            // token count and its P/T. G'raha Tia, Scion Reborn creates a fixed 1/1 Hero
            // and puts X +1/+1 counters on it, so `count`/`power`/`toughness` are all
            // concrete and X lives only here — binding the other three left this slot a
            // bare placeholder and the Hero entered with 0 counters.
            bind_where_x_enter_with_counters(
                enter_with_counters,
                where_x_expression,
                &mut unbound_where_x,
            );
            match (
                apply_where_x_expression(power.clone(), where_x_expression),
                apply_where_x_expression(toughness.clone(), where_x_expression),
            ) {
                (Some(bound_power), Some(bound_toughness)) => {
                    *power = bound_power;
                    *toughness = bound_toughness;
                }
                _ => unbound_where_x = where_x_expression.map(str::to_string),
            }
        }
        // CR 613.4b (layer 7b): "becomes an X/X creature, where X is …" — an animated
        // permanent's base P/T is the same where-X quantity site as a token's, and
        // `Animate` is the fourth `PtValue` carrier alongside `Token`/`Pump`/`PumpAll`.
        Effect::Animate {
            power, toughness, ..
        } => {
            bind_where_x_optional_pt(power, where_x_expression, &mut unbound_where_x);
            bind_where_x_optional_pt(toughness, where_x_expression, &mut unbound_where_x);
        }
        // CR 107.3i + CR 109.4 + CR 109.5: "search/seek for up to X …, where X
        // is …" binds the search count (Oreskos Explorer). Eldritch Evolution
        // binds the filter's `Cmc` bound when X appears in the card filter.
        Effect::SearchLibrary { filter, count, .. } | Effect::Seek { filter, count, .. } => {
            bind_where_x_filter(filter, where_x_expression, &mut unbound_where_x);
            bind_where_x_quantity(count, where_x_expression, &mut unbound_where_x);
        }
        // CR 107.3i + CR 400.7: "return/put up to one target creature card with
        // mana value X or less ..., where X is <expression>" binds the
        // `ChangeZone` target filter's `Cmc` bound (Moseo, Vein's New Dean's
        // Infusion ability). Without this arm the filter's bound stayed an
        // unresolved bare `Variable("X")`, which resolves to 0 at runtime and
        // makes the reanimation target only mana value 0 or less — silently
        // breaking the trigger's intended behavior. Mirrors the
        // `SearchLibrary`/`Seek` filter rewrite above.
        Effect::ChangeZone {
            target,
            enter_with_counters,
            conditional_enter_with_counters,
            ..
        } => {
            bind_where_x_filter(target, where_x_expression, &mut unbound_where_x);
            // CR 122.1: same enters-with rider as `Token`/`ChangeZoneAll` — the moved
            // permanent's counter count is a where-X site of its own.
            bind_where_x_enter_with_counters(
                enter_with_counters,
                where_x_expression,
                &mut unbound_where_x,
            );
            for (_, _, count) in conditional_enter_with_counters.iter_mut() {
                bind_where_x_quantity(count, where_x_expression, &mut unbound_where_x);
            }
        }
        Effect::Destroy { target, .. } | Effect::Bounce { target, .. } => {
            bind_where_x_filter(target, where_x_expression, &mut unbound_where_x);
        }
        // `BounceAll` carries an optional count ("return X target creatures …") beside its
        // filter; the filter-only arm left that count a bare placeholder.
        Effect::BounceAll { target, count, .. } => {
            bind_where_x_filter(target, where_x_expression, &mut unbound_where_x);
            bind_where_x_optional_quantity(
                count.as_mut(),
                where_x_expression,
                &mut unbound_where_x,
            );
        }
        // CR 601.2e: a cast permission may be BOUNDED by X ("you may cast a spell with mana
        // value less than X from among them, where X is that spell's mana value" — Kiora,
        // Sovereign of the Deep). The bound lives in the permission constraint, not in the
        // target filter, so the filter-only arm never reached it and the constraint kept a
        // bare `Variable("X")` — permitting only mana value < 0, i.e. nothing at all.
        Effect::CastFromZone {
            target, constraint, ..
        } => {
            bind_where_x_filter(target, where_x_expression, &mut unbound_where_x);
            if let Some(CastPermissionConstraint::ManaValue { value, .. }) = constraint.as_mut() {
                bind_where_x_quantity(value, where_x_expression, &mut unbound_where_x);
            }
        }
        Effect::Dig {
            count,
            keep_count_expr,
            player,
            filter,
            ..
        } => {
            bind_where_x_quantity(count, where_x_expression, &mut unbound_where_x);
            // "look at the top N …, keep X of them" — the KEPT count is a second, distinct
            // quantity slot that the original arm did not bind.
            bind_where_x_optional_quantity(
                keep_count_expr.as_mut(),
                where_x_expression,
                &mut unbound_where_x,
            );
            bind_where_x_filter(player, where_x_expression, &mut unbound_where_x);
            bind_where_x_filter(filter, where_x_expression, &mut unbound_where_x);
        }
        Effect::Scry { count, .. } => {
            bind_where_x_quantity(count, where_x_expression, &mut unbound_where_x);
        }
        Effect::Pump {
            power, toughness, ..
        }
        | Effect::PumpAll {
            power, toughness, ..
        } => {
            match (
                apply_where_x_expression(power.clone(), where_x_expression),
                apply_where_x_expression(toughness.clone(), where_x_expression),
            ) {
                (Some(bound_power), Some(bound_toughness)) => {
                    *power = bound_power;
                    *toughness = bound_toughness;
                }
                _ => unbound_where_x = where_x_expression.map(str::to_string),
            }
        }
        Effect::PreventDamage {
            amount,
            amount_dynamic,
            ..
        } => {
            // CR 615.7: "prevent all …" must not inherit a sibling clause's
            // where-X binding (Arachnogenesis: token count uses where-X;
            // prevention is blanket).
            if let Some(expr) = where_x_expression {
                if !matches!(
                    amount,
                    crate::types::ability::PreventionAmount::All
                        | crate::types::ability::PreventionAmount::AllBut(_)
                ) {
                    *amount_dynamic = parse_where_x_quantity_expression(expr);
                }
            }
        }
        // CR 107.3i + CR 118.1: Resolution-time cost amounts (Life / Speed /
        // Energy / per-object scaled mana) reference the same X as the
        // surrounding ability. Tymna the Weaver's "you may pay X life, where X
        // is the number of opponents that were dealt combat damage this turn"
        // requires the PayCost amount to track the where-X binding alongside
        // the sub-ability's "draw X cards"; without this arm the cost amount
        // stayed as the bare `Variable("X")` and decoupled from the resolved
        // expression.
        Effect::PayCost { cost, scale, .. } => {
            // CR 118.1 + CR 118.5: per-object scaled mana (`scale`) tracks the
            // surrounding where-X binding before the cost amount itself.
            if let Some(times) = scale {
                bind_where_x_quantity(times, where_x_expression, &mut unbound_where_x);
            }
            apply_where_x_to_ability_cost(cost, where_x_expression, &mut unbound_where_x);
        }
        Effect::GenericEffect {
            static_abilities,
            target,
            ..
        } => {
            // CR 115.1: A `Some(target)` filter on the grant means the recipient
            // is announced as a target ("another target creature you control" —
            // Xenagos, God of Revels), so a "that creature" anaphor in the
            // where-clause is the chosen target, not a cost/trigger referent.
            let target_based = target.is_some();
            // CR 608.2c: "that creature's power"/"toughness" in the where-clause
            // of a *targeted* grant is the target anaphor. The shared quantity
            // grammar lowers the context-free phrase to `CostPaidObject` (its
            // triggered-ability sense); on a targeted grant it must instead read
            // the chosen recipient, so rebind that scope to `Target` here at the
            // lowering seam. Gating on the demonstrative anaphor keeps a genuine
            // participle cost referent ("the sacrificed creature's power",
            // `CostPaidObject`) untouched.
            let rebind_target_anaphor =
                target_based && where_x_is_demonstrative_target_creature_stat(where_x_expression);
            for static_def in static_abilities.iter_mut() {
                if let Some(condition) = static_def.condition.as_mut() {
                    apply_where_x_static_condition(
                        condition,
                        where_x_expression,
                        &mut unbound_where_x,
                    );
                }
                // CR 107.3i + CR 611.2c: A continuous "gets +X/+X … where X is
                // <expression>" grant lowers to dynamic P/T modifications whose
                // `value` defaults to `CostXPaid` (X paid as the spell/ability was
                // cast) when no binding clause has been applied yet. The
                // surrounding where-clause is the more specific binding and must
                // own every X reference, including those nested in the grant's
                // continuous modifications. Substitute it into each dynamic
                // modification so a triggered/targeted pump (Xenagos, God of
                // Revels: "where X is that creature's power") or a static grant
                // (Craterhoof Behemoth: "where X is the number of creatures you
                // control") tracks the bound quantity instead of the cost-X
                // fallback. Mirrors the `Pump`/`SearchLibrary` arms above.
                for modification in static_def.modifications.iter_mut() {
                    apply_where_x_continuous_modification(
                        modification,
                        where_x_expression,
                        &mut unbound_where_x,
                    );
                    if rebind_target_anaphor {
                        rebind_target_anaphor_continuous_modification(modification);
                    }
                }
            }
        }
        // Every remaining variant carries no `QuantityExpr` and no `PtValue`, so it has no
        // X slot to bind. The arms above now cover all 62 `QuantityExpr` carriers and all 4
        // `PtValue` carriers; this wildcard is reached only by variants with nothing to
        // rewrite. It is NOT an escape hatch — the totality guard below still asserts the
        // post-condition, so a FUTURE variant that adds a quantity slot without an arm here
        // reds honestly instead of silently fabricating.
        _ => {}
    }
    // CR 107.3c — TOTALITY GUARD. The match above rewrites the quantity slots of the
    // `Effect` variants it enumerates. Enumeration is necessary but not sufficient: a
    // variant can be enumerated and still leave an X unbound (a slot the arm forgets, or an
    // expression `parse_where_x_quantity_expression` cannot represent). Without this guard
    // the failure mode is a FABRICATION rather than a red — the effect keeps its bare
    // `Variable("X")`, which
    // resolves to 0 at runtime (amass 0 / surveil 0 / discard 0) while the face still
    // renders as fully supported. That lie is invisible BOTH to a red-count ledger
    // (there is no `Unimplemented` node to count) and to the zero-raw-text invariant
    // ("X" is the legitimate alias). So the pass asserts its own post-condition: if the
    // clause DEFINED X and an unbound X survived the rewrite, report the gap. A control
    // with an escape hatch is not a control.
    //
    // The guard is keyed on the EXPRESSION, never on tree-presence of `Variable("X")`.
    // Some expressions legitimately bind TO the placeholder, and for those a surviving
    // `Variable("X")` is the CORRECT binding, not a fabrication:
    //   - Join Forces (CR 107.3i): "where X is the total amount of mana paid this way"
    //     resolves through the `chosen_x` machinery.
    //   - Constraint tails (CR 608.2g): "where X is less than or equal to <bound>"
    //     BOUNDS the player's chosen X rather than defining it (Well of Lost Dreams).
    // A tree-presence check would flip both families to red.
    if let Some(expression) = where_x_expression.filter(|_| unbound_where_x.is_none()) {
        if !where_x_binds_to_placeholder(expression) && effect_retains_unbound_x(effect) {
            unbound_where_x = Some(expression.to_string());
        }
    }
    // CR 107.3c: the clause defines X, but we cannot represent that definition.
    // Report the gap instead of keeping a P/T placeholder that resolves to no
    // modification at all (a silent +0/+0 no-op that still reads as supported).
    if let Some(expression) = unbound_where_x {
        *effect = Effect::unimplemented("where_x_binding", format!("where X is {expression}"));
    }
}

/// CR 107.3i + CR 608.2g: does this where-X expression legitimately bind X to the
/// PLACEHOLDER itself, rather than to a concrete quantity?
///
/// `parse_where_x_quantity_expression` deliberately returns `Variable("X")` for two
/// families — Join Forces' "the total amount of mana paid this way" (resolved via
/// `chosen_x`) and the comparator-shaped constraint tails ("where X is less than or
/// equal to …", which bound rather than define X). For those, a residual
/// `Variable("X")` in the effect is the CORRECT lowering, so the totality guard must
/// not treat it as an unbound fabrication.
fn where_x_binds_to_placeholder(expression: &str) -> bool {
    matches!(
        parse_where_x_quantity_expression(expression),
        Some(QuantityExpr::Ref {
            qty: QuantityRef::Variable { ref name },
        }) if name.eq_ignore_ascii_case("X")
    )
}

/// Does an unbound `QuantityRef::Variable { name: "X" }` survive anywhere in `effect`?
///
/// Uses the key-anchored typed-evidence probe (`QUANTITY_KEYS`) rather than a
/// hand-rolled 64-variant visitor: a value reached through a quantity key IS a quantity
/// by construction, so no cross-enum name collision is reachable. `QuantityRef` must
/// never be probed unanchored — ten of its variant names are shared with other
/// internally-tagged enums (see `swallow_evidence`).
fn effect_retains_unbound_x(effect: &Effect) -> bool {
    crate::parser::swallow_evidence::UnitEvidence::of_effect(effect).any_quantity_ref(
        |qty| matches!(qty, QuantityRef::Variable { name } if name.eq_ignore_ascii_case("X")),
    )
}

/// CR 107.3i + CR 611.2c: Substitute a "where X is <expression>" binding into a
/// continuous modification's dynamic `QuantityExpr` value. Only the value-carrying
/// dynamic P/T and dynamic-keyword grants (the +X/+X / set-P/T / dynamic-keyword
/// variants) hold an X-bearing `QuantityExpr`; every other `ContinuousModification`
/// variant is a fixed/typed modification with no X to rebind (enumerated as
/// explicit no-ops below). `apply_where_x_quantity_expression` only rewrites a
/// `CostXPaid` / bare `Variable("X")` value, so a modification whose quantity is
/// already a concrete reference is left unchanged.
/// CR 613.4c + CR 702: a granted keyword's "where X is its `<P/T/mana value>`"
/// refers to the keyword's RECIPIENT (the creature that has the keyword), not the
/// grant's source object. The bare object-quantity parser defaults "its power" /
/// "its toughness" / "its mana value" to `Source` scope (the correct default in a
/// self-referential context); rebind those to `Recipient` for a granted dynamic
/// keyword so the continuous layer resolves the value against each affected
/// creature. Self-grants (subject `~`) are unaffected — recipient == source.
fn rebind_dynamic_keyword_value_to_recipient(
    value: crate::types::ability::QuantityExpr,
) -> crate::types::ability::QuantityExpr {
    use crate::types::ability::{ObjectScope, QuantityExpr, QuantityRef};
    let rebound = |scope: ObjectScope| match scope {
        ObjectScope::Source => ObjectScope::Recipient,
        other => other,
    };
    match value {
        QuantityExpr::Ref {
            qty: QuantityRef::Power { scope },
        } => QuantityExpr::Ref {
            qty: QuantityRef::Power {
                scope: rebound(scope),
            },
        },
        QuantityExpr::Ref {
            qty: QuantityRef::Toughness { scope },
        } => QuantityExpr::Ref {
            qty: QuantityRef::Toughness {
                scope: rebound(scope),
            },
        },
        QuantityExpr::Ref {
            qty: QuantityRef::ObjectManaValue { scope },
        } => QuantityExpr::Ref {
            qty: QuantityRef::ObjectManaValue {
                scope: rebound(scope),
            },
        },
        other => other,
    }
}

fn apply_where_x_continuous_modification(
    modification: &mut ContinuousModification,
    where_x_expression: Option<&str>,
    unbound: &mut Option<String>,
) {
    match modification {
        ContinuousModification::SetDynamicPower { value, .. }
        | ContinuousModification::SetDynamicToughness { value, .. }
        | ContinuousModification::SetPowerDynamic { value, .. }
        | ContinuousModification::SetToughnessDynamic { value, .. }
        | ContinuousModification::AddDynamicPower { value, .. }
        | ContinuousModification::AddDynamicToughness { value, .. } => {
            bind_where_x_quantity(value, where_x_expression, unbound);
        }
        ContinuousModification::AddDynamicKeyword { value, .. } => {
            bind_where_x_quantity(value, where_x_expression, unbound);
            // CR 613.4c + CR 702: a GRANTED keyword's "where X is its
            // power/toughness/mana value" refers to the keyword's RECIPIENT (the
            // creature that has the keyword), not the grant's source object. The
            // bare object-quantity parser defaults "its power" to `Source` scope;
            // rebind it to `Recipient` so the continuous layer resolves the count
            // against each affected creature (Infantry Shield: "Equipped creature
            // has … mobilize X, where X is its power"). Self-grants are unaffected
            // (recipient == source). This is the IR-lowering counterpart of the
            // same rebind on the direct grant path in `oracle_static/keyword_grant.rs`.
            *value = rebind_dynamic_keyword_value_to_recipient(value.clone());
        }
        // Resolution-time-consumed; where-X counter quantities are applied by
        // the counter/enter-with parser paths before this continuous grant pass.
        ContinuousModification::AddCounterOnEnter { .. }
        | ContinuousModification::SetStartingLoyalty { .. } => {}
        ContinuousModification::GrantTrigger { trigger } => {
            if let Some(execute) = trigger.execute.as_mut() {
                apply_where_x_ability_expression(execute, where_x_expression);
            }
        }
        // Non-dynamic modifications carry fixed integers, enum payloads, or
        // nested definitions that are already parsed/lowered independently.
        // Keep this wildcard-free so a future QuantityExpr-carrying variant
        // forces a deliberate where-X decision.
        ContinuousModification::CopyValues { .. }
        // CR 707.2c (Metamorphic Alteration): inert copy marker — no where-X carrier.
        | ContinuousModification::CopyChosen
        | ContinuousModification::SetName { .. }
        | ContinuousModification::SetTextName { .. }
        | ContinuousModification::AddPower { .. }
        | ContinuousModification::AddToughness { .. }
        | ContinuousModification::SetPower { .. }
        | ContinuousModification::SetToughness { .. }
        | ContinuousModification::AddKeyword { .. }
        | ContinuousModification::AddKeywordWithDerivedCost { .. }
        | ContinuousModification::RemoveKeyword { .. }
        | ContinuousModification::GrantAbility { .. }
        | ContinuousModification::GrantAllActivatedAbilitiesOf { .. }
        | ContinuousModification::GrantAllTriggeredAbilitiesOf { .. }
        | ContinuousModification::RemoveAllAbilities
        | ContinuousModification::AddType { .. }
        | ContinuousModification::RemoveType { .. }
        | ContinuousModification::AddSubtype { .. }
        | ContinuousModification::RemoveSubtype { .. }
        | ContinuousModification::SetCardTypes { .. }
        | ContinuousModification::RemoveAllSubtypes { .. }
        | ContinuousModification::AddAllCreatureTypes
        | ContinuousModification::AddAllBasicLandTypes
        | ContinuousModification::AddAllLandTypes
        | ContinuousModification::AddChosenSubtype { .. }
        | ContinuousModification::AddChosenColor { .. }
        | ContinuousModification::RemoveChosenKeyword
        | ContinuousModification::AddChosenKeyword
        | ContinuousModification::SetColor { .. }
        | ContinuousModification::AddColor { .. }
        | ContinuousModification::AddStaticMode { .. }
        | ContinuousModification::GrantStaticAbility { .. }
        // Granted object-hosted replacement: no where-X / anaphoric magnitude.
        | ContinuousModification::GrantReplacement { .. }
        | ContinuousModification::SwitchPowerToughness
        | ContinuousModification::AssignDamageFromToughness
        | ContinuousModification::AssignDamageAsThoughUnblocked
        | ContinuousModification::AssignNoCombatDamage
        | ContinuousModification::ChangeController
        | ContinuousModification::SetBasicLandType { .. }
        | ContinuousModification::SetChosenBasicLandType
        | ContinuousModification::SetChosenName
        | ContinuousModification::RetainPrintedTriggerFromSource { .. }
        | ContinuousModification::RetainPrintedAbilityFromSource { .. }
        | ContinuousModification::RetainAllOtherAbilitiesFromSource
        | ContinuousModification::AddSupertype { .. }
        | ContinuousModification::RemoveSupertype { .. }
        | ContinuousModification::RemoveManaCost => {}
    }
}

/// CR 608.2c: Does the where-clause definition read "that creature's power" /
/// "that creature's toughness" — the bare demonstrative anaphor to the grant's
/// chosen target? `parse_event_context_refs` lowers this context-free phrase to
/// `ObjectScope::CostPaidObject` (its triggered-ability sense); on a *targeted*
/// continuous grant the antecedent is instead the announced target, so the
/// caller rebinds that scope to `ObjectScope::Target`.
///
/// The participle cost referent ("the sacrificed creature's power", also
/// `CostPaidObject`) and every non-anaphoric where-X definition fail this gate
/// and are left untouched — only the bare demonstrative target anaphor matches.
fn where_x_is_demonstrative_target_creature_stat(where_x_expression: Option<&str>) -> bool {
    let Some(expression) = where_x_expression else {
        return false;
    };
    let expression = expression.trim().trim_end_matches('.').to_ascii_lowercase();
    // The `if` condition scopes the parser temporary so it drops at the end of
    // condition evaluation (before the owned `expression` string), avoiding the
    // tail-position borrow that an `is_ok()` return expression would create.
    if all_consuming(preceded(
        tag::<_, _, OracleError<'_>>("that creature's "),
        alt((tag("power"), tag("toughness"))),
    ))
    .parse(expression.as_str())
    .is_ok()
    {
        return true;
    }
    false
}

/// CR 608.2c: Rebind a `ObjectScope::CostPaidObject` power/toughness reference
/// inside a continuous modification's dynamic value to `ObjectScope::Target`.
/// Applied only on a targeted grant whose where-clause is the demonstrative
/// target anaphor (`where_x_is_demonstrative_target_creature_stat`), so the
/// "that creature's power"/"toughness" pump (Xenagos, God of Revels) reads the
/// announced recipient instead of the trigger/cost referent slot. Mirrors the
/// modification coverage of `apply_where_x_continuous_modification`.
fn rebind_target_anaphor_continuous_modification(modification: &mut ContinuousModification) {
    match modification {
        ContinuousModification::SetDynamicPower { value, .. }
        | ContinuousModification::SetDynamicToughness { value, .. }
        | ContinuousModification::SetPowerDynamic { value, .. }
        | ContinuousModification::SetToughnessDynamic { value, .. }
        | ContinuousModification::AddDynamicPower { value, .. }
        | ContinuousModification::AddDynamicToughness { value, .. }
        | ContinuousModification::AddDynamicKeyword { value, .. } => {
            rebind_cost_paid_object_pt_to_target(value);
        }
        ContinuousModification::AddCounterOnEnter { .. }
        | ContinuousModification::SetStartingLoyalty { .. } => {}
        ContinuousModification::CopyValues { .. }
        // CR 707.2c (Metamorphic Alteration): inert copy marker — no where-X carrier.
        | ContinuousModification::CopyChosen
        | ContinuousModification::SetName { .. }
        | ContinuousModification::SetTextName { .. }
        | ContinuousModification::AddPower { .. }
        | ContinuousModification::AddToughness { .. }
        | ContinuousModification::SetPower { .. }
        | ContinuousModification::SetToughness { .. }
        | ContinuousModification::AddKeyword { .. }
        | ContinuousModification::AddKeywordWithDerivedCost { .. }
        | ContinuousModification::RemoveKeyword { .. }
        | ContinuousModification::GrantAbility { .. }
        | ContinuousModification::GrantAllActivatedAbilitiesOf { .. }
        | ContinuousModification::GrantAllTriggeredAbilitiesOf { .. }
        | ContinuousModification::GrantTrigger { .. }
        | ContinuousModification::RemoveAllAbilities
        | ContinuousModification::AddType { .. }
        | ContinuousModification::RemoveType { .. }
        | ContinuousModification::AddSubtype { .. }
        | ContinuousModification::RemoveSubtype { .. }
        | ContinuousModification::SetCardTypes { .. }
        | ContinuousModification::RemoveAllSubtypes { .. }
        | ContinuousModification::AddAllCreatureTypes
        | ContinuousModification::AddAllBasicLandTypes
        | ContinuousModification::AddAllLandTypes
        | ContinuousModification::AddChosenSubtype { .. }
        | ContinuousModification::AddChosenColor { .. }
        | ContinuousModification::RemoveChosenKeyword
        | ContinuousModification::AddChosenKeyword
        | ContinuousModification::SetColor { .. }
        | ContinuousModification::AddColor { .. }
        | ContinuousModification::AddStaticMode { .. }
        | ContinuousModification::GrantStaticAbility { .. }
        // Granted object-hosted replacement: no where-X / anaphoric magnitude.
        | ContinuousModification::GrantReplacement { .. }
        | ContinuousModification::SwitchPowerToughness
        | ContinuousModification::AssignDamageFromToughness
        | ContinuousModification::AssignDamageAsThoughUnblocked
        | ContinuousModification::AssignNoCombatDamage
        | ContinuousModification::ChangeController
        | ContinuousModification::SetBasicLandType { .. }
        | ContinuousModification::SetChosenBasicLandType
        | ContinuousModification::SetChosenName
        | ContinuousModification::RetainPrintedTriggerFromSource { .. }
        | ContinuousModification::RetainPrintedAbilityFromSource { .. }
        | ContinuousModification::RetainAllOtherAbilitiesFromSource
        | ContinuousModification::AddSupertype { .. }
        | ContinuousModification::RemoveSupertype { .. }
        | ContinuousModification::RemoveManaCost => {}
    }
}

/// Retarget a `ObjectScope::CostPaidObject` power/toughness `QuantityRef` within
/// a `QuantityExpr` to `ObjectScope::Target`, recursing through every composite
/// arm. Only the per-object power/toughness refs are rewritten; every other
/// reference (object counts, mana value, non-`CostPaidObject` scopes) is left
/// as-is so unrelated where-X bindings are never disturbed.
fn rebind_cost_paid_object_pt_to_target(expr: &mut QuantityExpr) {
    match expr {
        QuantityExpr::Ref {
            qty: QuantityRef::Power { scope } | QuantityRef::Toughness { scope },
        } if *scope == ObjectScope::CostPaidObject => {
            *scope = ObjectScope::Target;
        }
        QuantityExpr::Ref { .. } | QuantityExpr::Fixed { .. } => {}
        QuantityExpr::DivideRounded { inner, .. }
        | QuantityExpr::Multiply { inner, .. }
        | QuantityExpr::ClampMin { inner, .. }
        | QuantityExpr::Offset { inner, .. }
        | QuantityExpr::UpTo { max: inner }
        | QuantityExpr::Power {
            exponent: inner, ..
        } => {
            rebind_cost_paid_object_pt_to_target(inner);
        }
        QuantityExpr::Sum { exprs } | QuantityExpr::Max { exprs } => {
            for inner in exprs {
                rebind_cost_paid_object_pt_to_target(inner);
            }
        }
        QuantityExpr::Difference { left, right } => {
            rebind_cost_paid_object_pt_to_target(left);
            rebind_cost_paid_object_pt_to_target(right);
        }
    }
}

/// CR 107.3i + CR 118.1: Propagate a "where X is <expression>" binding into the
/// `QuantityExpr` amounts of a resolution-time `AbilityCost`. Exhaustive over
/// `AbilityCost` (no wildcard) so a future variant carrying an X-amount — e.g. a
/// `Composite { …PayLife(X)… }` producer — forces a deliberate decision here
/// instead of silently skipping the rewrite. Recurses into the compositional
/// (`Composite`/`OneOf`), wrapping (`PerCounter`), and effect-nesting
/// (`EffectCost`) variants. The no-X variants
/// are enumerated as explicit no-ops: their amounts are either fixed integers
/// (`Loyalty`, `Mill`, `Blight`, counts on Sacrifice/Exile/TapCreatures/…) or a
/// static `ManaCost`/object filter that the where-X mana-value clause does not
/// bind (X-in-mana-cost is concretized at announcement, not by this rewrite).
fn apply_where_x_to_ability_cost(
    cost: &mut AbilityCost,
    where_x_expression: Option<&str>,
    unbound: &mut Option<String>,
) {
    match cost {
        AbilityCost::PayLife { amount }
        | AbilityCost::PaySpeed { amount }
        | AbilityCost::PayEnergy { amount }
        | AbilityCost::ManaDynamic { quantity: amount } => {
            bind_where_x_quantity(amount, where_x_expression, unbound);
        }
        // CR 701.9: "discard X cards, where X is …" — the discard count is a
        // `QuantityExpr` and must track the same where-X binding.
        AbilityCost::Discard { count, .. } => {
            bind_where_x_quantity(count, where_x_expression, unbound);
        }
        AbilityCost::Composite { costs } | AbilityCost::OneOf { costs } => {
            for sub in costs.iter_mut() {
                apply_where_x_to_ability_cost(sub, where_x_expression, unbound);
            }
        }
        AbilityCost::PerCounter { base, .. } => {
            apply_where_x_to_ability_cost(base, where_x_expression, unbound);
        }
        // CR 107.3i + CR 118.1: An effect performed as a cost nests an `Effect`
        // (e.g. `PutCounter { count: QuantityExpr }`), whose own quantity can
        // carry the surrounding where-X binding. Recurse through the shared
        // `apply_where_x_effect_expression` rewriter so a "where X is …" clause
        // flows into the nested effect's count exactly as it does for the
        // sub-ability's effects — never re-implement the per-effect quantity walk.
        AbilityCost::EffectCost { effect } => {
            apply_where_x_effect_expression(effect, where_x_expression);
        }
        // (the nested effect reports its own unrepresentable where-X binding by
        // rewriting itself to `Effect::unimplemented`, so no `unbound` plumbing
        // is needed here)
        // No X-bearing `QuantityExpr` amount to bind: fixed integer counts
        // (`Loyalty`, `Mill`, `Blight`, counts on Sacrifice/Exile/…) or a static
        // `ManaCost`/object filter that this where-X mana-value clause does not
        // bind (X-in-mana-cost is concretized at announcement, not by this
        // rewrite).
        AbilityCost::Mana { .. }
        | AbilityCost::Waterbend { .. }
        | AbilityCost::NinjutsuFamily { .. }
        | AbilityCost::Tap
        | AbilityCost::Untap
        | AbilityCost::Loyalty { .. }
        | AbilityCost::Sacrifice(_)
        | AbilityCost::Exile { .. }
        | AbilityCost::ExileMaterials { .. }
        | AbilityCost::CollectEvidence { .. }
        // CR 117.1: `ExileWithAggregate`'s threshold is a fixed `i32` and its
        // filter is static — no where-X `QuantityExpr` amount to bind.
        | AbilityCost::ExileWithAggregate { .. }
        | AbilityCost::TapCreatures { .. }
        | AbilityCost::RemoveCounter { .. }
        | AbilityCost::ReturnToHand { .. }
        | AbilityCost::Unattach
        | AbilityCost::UnattachFrom { .. }
        | AbilityCost::Mill { .. }
        | AbilityCost::Exert
        | AbilityCost::Blight { .. }
        | AbilityCost::Reveal { .. }
        | AbilityCost::Behold { .. }
        // CR 118.9: the borrowed keyword cost is read at runtime from the cast
        // spell's keyword — it carries no where-X `QuantityExpr` amount to bind.
        | AbilityCost::KeywordCostOfCastSpell { .. }
        // CR 702.21a: `count` is a fixed `u32`, not a `QuantityExpr` — no
        // where-X amount to bind.
        | AbilityCost::GetPlayerCounters { .. }
        | AbilityCost::Unimplemented { .. } => {}
    }
}

pub(super) fn apply_where_x_to_latest_def(
    defs: &mut [AbilityDefinition],
    where_x_expression: Option<&str>,
) {
    if let Some(def) = defs.last_mut() {
        apply_where_x_ability_expression(def, where_x_expression);
    }
}

/// Bind an X-bearing `TargetFilter` in place, recording an unrepresentable
/// where-X definition instead of fabricating one (CR 107.3c). Filter twin of
/// [`bind_where_x_quantity`].
fn bind_where_x_filter(
    slot: &mut TargetFilter,
    where_x_expression: Option<&str>,
    unbound: &mut Option<String>,
) {
    match apply_where_x_to_filter(slot.clone(), where_x_expression) {
        Some(bound) => *slot = bound,
        None => *unbound = where_x_expression.map(str::to_string),
    }
}

/// CR 202.3 + CR 107.3i: Substitute the literal `X` inside a `TargetFilter`'s
/// `FilterProp::Cmc` bounds with a trailing "where X is <expression>" defining
/// clause. A `Cmc` bound parsed as `QuantityRef::Variable("X")` carries no
/// defining expression until the where-clause is applied here — without this,
/// the mana-value bound is effectively unbounded (Birthing Ritual: "creature
/// card with mana value X or less ..., where X is 1 plus the sacrificed
/// creature's mana value").
///
/// Walks typed-filter property lists and target-filter compositions, recursing
/// through `AnyOf` nesting so composite "mana value N or M" bounds are
/// covered. Non-`Cmc` props and non-typed filters pass through unchanged.
///
/// Returns `None` when the where-X clause defines X but that definition has no
/// typed home (CR 107.3c) — the filter bound would otherwise carry a raw-text
/// `QuantityRef::Variable`, which resolves to 0 and silently narrows the filter
/// to "mana value 0 or less" while still reading as supported.
pub(crate) fn apply_where_x_to_filter(
    filter: TargetFilter,
    where_x_expression: Option<&str>,
) -> Option<TargetFilter> {
    if where_x_expression.is_none() {
        return Some(filter);
    }
    Some(match filter {
        TargetFilter::Typed(mut typed) => {
            typed.properties = typed
                .properties
                .into_iter()
                .map(|prop| apply_where_x_to_filter_prop(prop, where_x_expression))
                .collect::<Option<Vec<_>>>()?;
            TargetFilter::Typed(typed)
        }
        TargetFilter::And { filters } => TargetFilter::And {
            filters: filters
                .into_iter()
                .map(|filter| apply_where_x_to_filter(filter, where_x_expression))
                .collect::<Option<Vec<_>>>()?,
        },
        TargetFilter::Or { filters } => TargetFilter::Or {
            filters: filters
                .into_iter()
                .map(|filter| apply_where_x_to_filter(filter, where_x_expression))
                .collect::<Option<Vec<_>>>()?,
        },
        TargetFilter::Not { filter } => TargetFilter::Not {
            filter: Box::new(apply_where_x_to_filter(*filter, where_x_expression)?),
        },
        TargetFilter::TrackedSetFiltered {
            id,
            filter,
            caused_by,
        } => TargetFilter::TrackedSetFiltered {
            id,
            filter: Box::new(apply_where_x_to_filter(*filter, where_x_expression)?),
            caused_by,
        },
        other => other,
    })
}

/// CR 107.3i + CR 202.3: Substitute the X binding into a target-set constraint's
/// dynamic bound. Mirrors `apply_where_x_to_filter_prop`: maps the
/// `TotalManaValue.value` `QuantityExpr` through `apply_where_x_quantity_expression`
/// so `Variable("X")` + where-X `"the result"` becomes `EventContextAmount`.
/// Constraints without a quantity bound (`DifferentTargetPlayers`,
/// `DifferentObjectControllers`) are left unchanged.
fn apply_where_x_to_target_constraint(
    constraint: &mut TargetSelectionConstraint,
    where_x_expression: Option<&str>,
    unbound: &mut Option<String>,
) {
    if let TargetSelectionConstraint::TotalManaValue { value, .. } = constraint {
        bind_where_x_quantity(value, where_x_expression, unbound);
    }
}

fn apply_where_x_to_filter_prop(
    prop: FilterProp,
    where_x_expression: Option<&str>,
) -> Option<FilterProp> {
    Some(match prop {
        FilterProp::Cmc { comparator, value } => FilterProp::Cmc {
            comparator,
            value: apply_where_x_quantity_expression(value, where_x_expression)?,
        },
        FilterProp::Counters {
            counters,
            comparator,
            count,
        } => FilterProp::Counters {
            counters,
            comparator,
            count: apply_where_x_quantity_expression(count, where_x_expression)?,
        },
        FilterProp::PtComparison {
            stat,
            scope,
            comparator,
            value,
        } => FilterProp::PtComparison {
            stat,
            scope,
            comparator,
            value: apply_where_x_quantity_expression(value, where_x_expression)?,
        },
        FilterProp::CanEnchant { target } => FilterProp::CanEnchant {
            target: Box::new(apply_where_x_to_filter(*target, where_x_expression)?),
        },
        FilterProp::DifferentNameFrom { filter } => FilterProp::DifferentNameFrom {
            filter: Box::new(apply_where_x_to_filter(*filter, where_x_expression)?),
        },
        FilterProp::SharesQuality {
            quality,
            reference,
            relation,
        } => FilterProp::SharesQuality {
            quality,
            reference: match reference {
                Some(filter) => Some(Box::new(apply_where_x_to_filter(
                    *filter,
                    where_x_expression,
                )?)),
                None => None,
            },
            relation,
        },
        FilterProp::TargetsOnly { filter } => FilterProp::TargetsOnly {
            filter: Box::new(apply_where_x_to_filter(*filter, where_x_expression)?),
        },
        FilterProp::Targets { filter } => FilterProp::Targets {
            filter: Box::new(apply_where_x_to_filter(*filter, where_x_expression)?),
        },
        FilterProp::AnyOf { props } => FilterProp::AnyOf {
            props: props
                .into_iter()
                .map(|p| apply_where_x_to_filter_prop(p, where_x_expression))
                .collect::<Option<Vec<_>>>()?,
        },
        // CR 608.2c: Descend into the negated inner prop so X-substitution
        // reaches it (mirrors the AnyOf transform).
        FilterProp::Not { prop } => FilterProp::Not {
            prop: Box::new(apply_where_x_to_filter_prop(*prop, where_x_expression)?),
        },
        other => other,
    })
}

/// CR 120.10: the BARE demonstrative "that excess damage" — no subject, no
/// "dealt this way" tail to anchor it.
///
/// The shared quantity grammar deliberately declines this phrase, because out of
/// context its antecedent is ambiguous and the two readings resolve from DIFFERENT
/// state (see [`rebind_context_dependent_where_x`]). Only the def-level licence can
/// bind it, so it is recognised here rather than in the leaf combinators.
fn parse_bare_excess_demonstrative(input: &str) -> OracleResult<'_, ()> {
    all_consuming(value((), (tag("that "), tag("excess damage")))).parse(input)
}

/// CR 120.10 + CR 107.3c: licence a resolution-local demonstrative where-X tail
/// against the definition's OWN condition, normalizing it onto the explicit phrase
/// the shared quantity grammar already binds.
///
/// "…, where X is that excess damage" has two readings, and a context-free leaf
/// combinator cannot separate them:
///
///   - Contest of Claws — "IF EXCESS DAMAGE WAS DEALT THIS WAY, discover X, where X
///     is that excess damage." The sibling condition lowers to
///     `AbilityCondition::PreviousEffectAmount { channel: Excess }` on THIS def. That
///     condition is the LICENCE: it proves the antecedent is this resolution's own
///     excess tally, which `last_effect_excess_amount` is holding and which
///     `QuantityRef::PreviousEffectAmount { channel: Excess }` reads correctly.
///
///   - Fall of Cair Andros — "Whenever a creature an opponent controls is dealt
///     excess noncombat damage, amass Orcs X, where X is that excess damage." The
///     antecedent is the TRIGGERING EVENT. The triggered ability resolves as its own
///     top-level chain and the depth-0 prelude has already CLEARED
///     `last_effect_excess_amount`, so the resolution-local read would silently
///     amass 0 while rendering as supported. It carries no such condition, so it is
///     not licensed, and CR 107.3c keeps it an honest gap.
///
/// The disambiguator therefore lives exactly one layer above the leaf — on the def,
/// next to the effect — which is why this runs here and not in `oracle_nom`.
/// Rewriting onto the explicit phrase (rather than binding a second leaf) keeps ONE
/// grammar and ONE binding authority.
///
/// This mirrors the `rebind_target_anaphor` seam below, which likewise resolves a
/// demonstrative anaphor at the lowering layer against a disambiguator the leaf
/// cannot see.
fn rebind_context_dependent_where_x(
    def: &AbilityDefinition,
    where_x_expression: Option<&str>,
) -> Option<String> {
    let expression = where_x_expression?;
    let normalized = expression.trim().trim_end_matches('.').to_ascii_lowercase();
    parse_bare_excess_demonstrative(normalized.as_str()).ok()?;
    // The licence: this definition's own condition reads the resolution-local
    // EXCESS channel ("if excess damage was dealt this way").
    matches!(
        def.condition.as_ref(),
        Some(AbilityCondition::PreviousEffectAmount {
            channel: DamageChannel::Excess,
            ..
        })
    )
    .then(|| "the amount of excess damage dealt this way".to_string())
}

/// CR 601.2a-b + CR 602.2b: recognize the announce-time-lock qualifier that ends a
/// "where X is …" tail, and return the bare count expression with it removed.
///
/// "as you cast this spell" names a spell's announcement (CR 601.2a-b); "as you
/// activate this ability" names an activated ability's, which CR 602.2b makes
/// *identical* to 601.2b-i. They are the SAME moment, so one combinator recognizes
/// both through a single `alt()` over the shared tail rather than two bespoke arms.
///
/// The qualifier is load-bearing, not decoration. CR 107.3c makes a text-defined X a
/// LIVE value by default — "Note that the value of X may change while that spell or
/// ability is on the stack" — so binding the stripped expression into the ability's X
/// slots as an ordinary quantity would re-evaluate it at resolution, which is exactly
/// what the qualifier forbids. The only correct consumer is
/// `AbilityDefinition::announced_x`, which the engine evaluates once at announcement.
fn announce_locked_count(input: &str) -> OracleResult<'_, &str> {
    terminated(
        take_until(" as you "),
        terminated(
            preceded(
                tag(" as you "),
                alt((tag("cast this spell"), tag("activate this ability"))),
            ),
            eof,
        ),
    )
    .parse(input)
}

fn strip_announce_lock(expression: &str) -> Option<&str> {
    let bare = expression.trim().trim_end_matches('.');
    // `to_ascii_lowercase` is length-preserving, so a byte offset found in the
    // lowercase copy transfers to the original-case slice returned to the caller.
    let lower = bare.to_ascii_lowercase();
    let (_, prefix) = announce_locked_count(lower.as_str()).ok()?;
    Some(bare[..prefix.len()].trim_end())
}

pub(super) fn apply_where_x_ability_expression(
    def: &mut AbilityDefinition,
    where_x_expression: Option<&str>,
) {
    // CR 601.2b + CR 602.2b: an announce-time-locked "where X is …" clause defines X
    // as a count MEASURED AT ANNOUNCEMENT, overriding CR 107.3c's default that a
    // text-defined X "may change while that spell or ability is on the stack". Park
    // the count on the def and STOP: every `QuantityRef::Variable("X")` in this
    // ability is left intact and already reads the object's single X channel
    // (`chosen_x`, CR 107.3i), which the announcement step fills from `announced_x`.
    //
    // Binding the count into the X slots here instead — the tempting "just strip the
    // suffix" fix — would make each slot re-evaluate the board at whatever moment it
    // happens to be read (resolution, for a damage amount or a draw count), which is
    // precisely the behaviour the printed qualifier exists to forbid.
    if let Some(locked) = where_x_expression.and_then(strip_announce_lock) {
        match parse_where_x_quantity_expression(locked) {
            Some(expr) => {
                def.announced_x = Some(expr);
                return;
            }
            // CR 107.3c: the clause defines X but the count has no typed home. Report
            // the gap rather than keep a raw-text placeholder that resolves to 0 while
            // still reading as supported.
            None => {
                *def.effect =
                    Effect::unimplemented("where_x_binding", format!("where X is {locked}"));
                return;
            }
        }
    }

    // CR 120.10: a context-dependent demonstrative tail is licensed against this
    // def's condition and normalized onto the phrase the shared grammar binds,
    // BEFORE any of the rewrites below consume it. Unlicensed demonstratives fall
    // through unchanged and are reported as CR 107.3c gaps by the passes below.
    let licensed_where_x = rebind_context_dependent_where_x(def, where_x_expression);
    let where_x_expression = licensed_where_x.as_deref().or(where_x_expression);

    // CR 107.3i: All instances of X on an object share one value at any given
    // time. Substitute X in this AbilityDefinition's condition before walking
    // into effect/sub_ability/etc. The recursion below visits every chained
    // SequentialSibling node, so each node's own `condition` is reached here.
    // CR 107.3c: set when this ability's where-X clause DEFINES X but the
    // definition has no typed home. Converted to a gap node after the walk (the
    // rewrites below hold mutable borrows of `def`'s fields).
    let mut unbound_where_x: Option<String> = None;
    if let Some(cond) = def.condition.as_mut() {
        apply_where_x_ability_condition(cond, where_x_expression, &mut unbound_where_x);
    }
    if let Some(repeat_for) = def.repeat_for.take() {
        match apply_where_x_quantity_expression(repeat_for, where_x_expression) {
            Some(bound) => def.repeat_for = Some(bound),
            None => unbound_where_x = where_x_expression.map(str::to_string),
        }
    }
    if let Some(spec) = def.multi_target.as_mut() {
        // `map_quantities` is infallible, so bind each quantity through the
        // shared authority and record an unrepresentable definition out-of-band
        // rather than fabricating one.
        spec.map_quantities(|expr| {
            let mut slot = expr;
            bind_where_x_quantity(&mut slot, where_x_expression, &mut unbound_where_x);
            slot
        });
    }
    // CR 107.3i + CR 202.3: Rebind X in the target-set constraints (e.g. the
    // `TotalManaValue` cap on Ancient Brass Dragon, whose bound is the
    // `where X is the result` die value). Without this, the reflexive sub
    // inherits `Variable("X")` with no defining expression and the cap is
    // effectively unbounded.
    for constraint in def.target_constraints.iter_mut() {
        apply_where_x_to_target_constraint(constraint, where_x_expression, &mut unbound_where_x);
    }
    apply_where_x_effect_expression(def.effect.as_mut(), where_x_expression);
    // CR 107.3c: the clause defines X, but we cannot represent that definition.
    // Report the gap instead of keeping a raw-text placeholder that resolves to
    // 0 while still reading as a supported dynamic quantity.
    if let Some(expression) = unbound_where_x {
        *def.effect = Effect::unimplemented("where_x_binding", format!("where X is {expression}"));
    }
    if let Some(sub) = def.sub_ability.as_mut() {
        apply_where_x_ability_expression(sub, where_x_expression);
    }
    // CR 120.1 + CR 608.2c: `wrap_target_subject_damage` has already established
    // that this target-only picker supplies the damage source. A trailing
    // "where X is its power" binds after that wrapper is built, and the generic
    // where-X grammar correctly starts from its ordinary `Source` scope. Restore
    // the target-subject meaning after the binding so both direct damage legs
    // read the chosen creature's characteristics (Self-Destruct class).
    if where_x_expression.is_some() && matches!(def.effect.as_ref(), Effect::TargetOnly { .. }) {
        if let Some(sub) = def.sub_ability.as_deref_mut() {
            rebind_target_subject_damage_where_x(sub);
        }
    }
    if let Some(else_ability) = def.else_ability.as_mut() {
        apply_where_x_ability_expression(else_ability, where_x_expression);
    }
    for mode_ability in &mut def.mode_abilities {
        apply_where_x_ability_expression(mode_ability, where_x_expression);
    }
}

/// CR 120.1 + CR 608.2c: Walk the target-subject damage clause emitted beneath
/// `Effect::TargetOnly` and rebind only damage instructions whose source is the
/// chosen target. Other chained instructions are left alone.
fn rebind_target_subject_damage_where_x(def: &mut AbilityDefinition) {
    match def.effect.as_mut() {
        Effect::DealDamage {
            amount,
            damage_source: Some(DamageSource::Target),
            ..
        }
        | Effect::DamageAll {
            amount,
            damage_source: Some(DamageSource::Target),
            ..
        } => super::rebind_target_subject_object_scope(amount),
        _ => {}
    }
    if let Some(sub) = def.sub_ability.as_deref_mut() {
        rebind_target_subject_damage_where_x(sub);
    }
}

/// CR 107.3i: Substitute the X binding into every quantity expression nested
/// inside an `AbilityCondition`. Delegates leaf substitution to the existing
/// `apply_where_x_quantity_expression`; recurses through compound arms
/// (`And`/`Or`/`Not`/`ConditionInstead`). Leaf arms without quantity fields
/// fall through to the no-op `_` arm.
fn apply_where_x_ability_condition(
    cond: &mut AbilityCondition,
    where_x_expression: Option<&str>,
    unbound: &mut Option<String>,
) {
    match cond {
        AbilityCondition::QuantityCheck { lhs, rhs, .. } => {
            bind_where_x_quantity(lhs, where_x_expression, unbound);
            bind_where_x_quantity(rhs, where_x_expression, unbound);
        }
        AbilityCondition::And { conditions } | AbilityCondition::Or { conditions } => {
            for c in conditions.iter_mut() {
                apply_where_x_ability_condition(c, where_x_expression, unbound);
            }
        }
        AbilityCondition::Not { condition } => {
            apply_where_x_ability_condition(condition, where_x_expression, unbound);
        }
        AbilityCondition::ConditionInstead { inner } => {
            apply_where_x_ability_condition(inner, where_x_expression, unbound);
        }
        _ => {}
    }
}

fn apply_where_x_static_condition(
    condition: &mut StaticCondition,
    where_x_expression: Option<&str>,
    unbound: &mut Option<String>,
) {
    match condition {
        StaticCondition::QuantityComparison { lhs, rhs, .. } => {
            bind_where_x_quantity(lhs, where_x_expression, unbound);
            bind_where_x_quantity(rhs, where_x_expression, unbound);
        }
        StaticCondition::And { conditions } | StaticCondition::Or { conditions } => {
            for condition in conditions {
                apply_where_x_static_condition(condition, where_x_expression, unbound);
            }
        }
        StaticCondition::Not { condition } => {
            apply_where_x_static_condition(condition, where_x_expression, unbound);
        }
        _ => {}
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

    let (sign, body) = if let Ok((rest, _)) = tag::<_, _, OracleError<'_>>("+").parse(text) {
        (1, rest.trim())
    } else if let Ok((rest, _)) = tag::<_, _, OracleError<'_>>("-").parse(text) {
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

/// CR 122.6: Scan a remainder for a "with [N] [type] counter(s) on
/// it" suffix and lift the matched counter type + count into a
/// `Vec<(CounterType, QuantityExpr)>` slot for `Effect::ChangeZone.enter_with_counters`.
///
/// Matches the patterns:
///   * "with N <type> counter(s) on it" — fixed numeric (digits or English).
///   * "with a/an <type> counter on it" — singular article.
///   * Optional "additional " between count and type — purely a synonym in
///     this position; the counter is still added once during the move.
///   * Two or more of the above conjoined by `" and "` inside a single "with"
///     ("with a hexproof counter and an indestructible counter on it",
///     Perennation) — the returned `Vec` carries one entry per conjunct.
///
/// Returns an empty `Vec` when no clause is present, so the caller can stamp
/// it unconditionally.
///
/// Implemented as a `scan_preceded` over [`parse_enter_counters_clause_body`] —
/// the scanner advances at word boundaries, so the suffix can appear anywhere
/// after the destination phrase ("onto the battlefield tapped under your control
/// with two additional +1/+1 counters on it") without the caller having to
/// pre-trim.
pub(crate) fn parse_with_counters_suffix(lower: &str) -> Vec<(CounterType, QuantityExpr)> {
    parse_with_counters_suffix_spanned(lower).0
}

/// Like [`parse_with_counters_suffix`], but also reports the byte range in
/// `lower` that the matched `"with … counter(s) [on it]"` clause occupies —
/// `start` at the `"with "` token, `end` one past the last byte the clause
/// consumed. Returns `None` when no counter clause matched.
///
/// CR 122.1: counters are the marker this clause places.
///
/// The range is a full span, not a bare start offset, precisely so a caller can
/// excise ONLY the clause and keep whatever follows it. Counter clauses are
/// often not clause-final — "…with three +1/+1 counters on it and you become
/// the monarch" (Heart-Shaped Herb), "…with X +1/+1 counters on it and draw X
/// cards" (Cosima, God of the Voyage) — so a caller that truncates at `start`
/// silently discards a printed instruction. Exactly one call site still
/// truncates at `start` (`split_counterless_enter_counters`, mod.rs) and
/// documents in place why its tail is empty by construction.
///
/// The return-destination path does not use this function at all: it consumes
/// the counter clause as a leading entry rider via
/// [`parse_leading_enter_counters_clause`], which keeps its remainder a true
/// suffix rather than an excised span.
pub(crate) fn parse_with_counters_suffix_spanned(
    lower: &str,
) -> (
    Vec<(CounterType, QuantityExpr)>,
    Option<std::ops::Range<usize>>,
) {
    nom_primitives::scan_preceded(lower, parse_enter_counters_clause_body)
        .map(|(prefix, counters, rest)| {
            let span = prefix.len()..lower.len() - rest.len();
            (counters, Some(span))
        })
        .unwrap_or((Vec::new(), None))
}

/// CR 122.6: the body of a `"with …"` rider that gives counters as an object
/// enters the battlefield —
/// the `"with "` token followed by ONE OR MORE counter clauses conjoined by
/// `" and "`. The conjoined form is printed and load-bearing:
///   * "return target permanent card from your graveyard to the battlefield
///     with a hexproof counter and an indestructible counter on it"
///     (Perennation)
///   * "…return that card to the battlefield under its owner's control with a
///     vigilance counter and a lifelink counter on it" (Gilraen, Dúnedain
///     Protector)
///
/// Those two are the cards this combinator actually reaches, confirmed against
/// the CI parse diff. The self-referential "…enters with two +1/+1 counters and
/// a lifelink counter on it" shape (Dust Animus, Voidpouncer) prints the SAME
/// conjoined grammar but is parsed at a different seam that calls
/// `parse_counter_suffix_body_combinator` directly rather than through this
/// list, so it still lifts only the first conjunct. Routing that seam through
/// here is the follow-up; do not assume this function already covers it.
///
/// `separated_list1` is the right combinator rather than a hand-rolled loop
/// because nom backtracks the separator when the following element fails, which
/// is what stops the list from swallowing a non-counter conjunct: on "…counters
/// on it and you become the monarch" the `" and "` separator matches but
/// `parse_counter_suffix_body_combinator` rejects "you" at its leading
/// `parse_number`, so the list ends with the separator unconsumed and the
/// monarch instruction stays in the remainder. Same for "and draw X cards"
/// (Cosima) and "and with haste" (Voidpouncer) — every non-counter conjunct is
/// rejected by the element's mandatory leading count/article.
fn parse_enter_counters_clause_body(
    input: &str,
) -> OracleResult<'_, Vec<(CounterType, QuantityExpr)>> {
    preceded(
        tag("with "),
        separated_list1(tag(" and "), parse_counter_suffix_body_combinator),
    )
    .parse(input)
}

/// CR 122.6: the enter-with-counters rider in LEADING position, tolerating the
/// same optional `" and"` / `","` connector that [`parse_one_battlefield_rider`]
/// accepts — the two are sibling entry conditions printed in any order ("to the
/// battlefield tapped and with two stun counters on it"), so they must agree on
/// how conjuncts are joined.
///
/// Anchoring at the front (rather than scanning, as
/// [`parse_with_counters_suffix_spanned`] does) is what lets
/// `strip_return_destination_ext_with_remainder` CONSUME the clause — advancing
/// its entry offset past it — instead of cutting a span out of the middle of the
/// remainder. A consumed prefix leaves the remainder a genuine suffix slice, so
/// any instruction printed after the entry clauses stays reachable.
fn parse_leading_enter_counters_clause(
    input: &str,
) -> OracleResult<'_, Vec<(CounterType, QuantityExpr)>> {
    preceded(
        (opt(alt((tag(" and"), tag(",")))), tag(" ")),
        parse_enter_counters_clause_body,
    )
    .parse(input)
}

/// Combinator body for "[N|a|an] [additional ]<type> counter(s) on it". Used by
/// `parse_with_counters_suffix` AND by the exile-
/// anaphor counter clause in `oracle_replacement.rs` so both paths share the
/// same grammar.
///
/// Returns the parsed `(counter_type, count)` pair on success.
pub(crate) fn parse_counter_suffix_body_combinator(
    input: &str,
) -> nom::IResult<&str, (CounterType, QuantityExpr), OracleError<'_>> {
    // Count axis: dynamic "a number of … equal to <quantity>" FIRST, then the
    // fixed-number form. ORDER IS LOAD-BEARING: `parse_number` consumes the bare
    // article "a" as 1 (oracle_nom/primitives.rs:108/118), so the fixed path
    // would mis-parse "a number of …" by consuming "a" as count 1 and treating
    // "number of <type>" as the counter-type token. The dynamic arm gates on the
    // longer, more specific `tag("a number of ")`. A future `alt()` refactor
    // MUST keep dynamic before fixed for the same reason.
    match parse_dynamic_counter_suffix_body(input) {
        Ok((rest, body)) => return Ok((rest, body)),
        Err(err) => {
            if tag::<_, _, OracleError<'_>>("a number of ")
                .parse(input)
                .is_ok()
            {
                return Err(err);
            }
        }
    }

    // Count: digits, English word, or article ("a"/"an").
    let (rest, count) = nom_primitives::parse_number.parse(input)?;
    let (rest, _) = tag(" ").parse(rest)?;
    // "N fewer [type] counter(s)" — counter-relative-to-LKI pattern (Nine-Lives Familiar class).
    // CR 603.7c + CR 107.1b: The delayed trigger reads the source's pre-death counter count
    // via LKI and subtracts N, clamped to zero.
    if let Ok((fewer_rest, _)) = tag::<_, _, OracleError<'_>>("fewer ").parse(rest) {
        let (fewer_rest, type_token) = take_until(" counter").parse(fewer_rest)?;
        let counter_type = crate::types::counter::parse_counter_type(type_token);
        let (fewer_rest, _) = tag(" counter").parse(fewer_rest)?;
        let (fewer_rest, _) =
            nom::combinator::opt(tag::<_, _, OracleError<'_>>("s")).parse(fewer_rest)?;
        let (fewer_rest, _) =
            nom::combinator::opt(tag::<_, _, OracleError<'_>>(" on it")).parse(fewer_rest)?;
        return Ok((
            fewer_rest,
            (
                counter_type.clone(),
                QuantityExpr::ClampMin {
                    inner: Box::new(QuantityExpr::Offset {
                        inner: Box::new(QuantityExpr::Ref {
                            qty: QuantityRef::CountersOn {
                                scope: ObjectScope::Source,
                                counter_type: Some(counter_type),
                            },
                        }),
                        offset: -(count as i32),
                    }),
                    minimum: 0,
                },
            ),
        ));
    }
    // Optional "additional " — a synonym in this grammatical position.
    let (rest, _) =
        nom::combinator::opt(tag::<_, _, OracleError<'_>>("additional ")).parse(rest)?;

    // Counter type: parse the token up to " counter" / " counters". The body
    // accepts any non-whitespace name (including "+1/+1") followed by inline
    // tokens that don't terminate at " counter".
    let (rest, type_token) = take_until(" counter").parse(rest)?;
    let counter_type = crate::types::counter::parse_counter_type(type_token);
    let (rest, _) = tag(" counter").parse(rest)?;
    // Optional plural "s".
    let (rest, _) = nom::combinator::opt(tag::<_, _, OracleError<'_>>("s")).parse(rest)?;
    // "on it" is grammatical filler with no rules content — BOTH spellings are
    // printed, so the terminator is `opt` rather than required. Filler PRESENT:
    // "…with two stun counters on it." (Unstoppable Slasher), "…with a +1/+1
    // counter on it." Filler ABSENT: "…with a vigilance counter and a lifelink
    // counter on it." (Gilraen, Dúnedain Protector — the filler closes the
    // conjoined list, so the non-final element carries none), "…with two +1/+1
    // counters and a lifelink counter on it." (Dust Animus). This is an
    // observation about printed wording, not a rule; no CR section governs it,
    // which is why none is cited here. When a caller gives counters as an object
    // enters the battlefield, CR 122.6 describes that effect; this shared
    // grammar does not determine whether its caller is an entry clause.
    let (rest, _) = nom::combinator::opt(tag::<_, _, OracleError<'_>>(" on it")).parse(rest)?;

    Ok((
        rest,
        (
            counter_type,
            QuantityExpr::Fixed {
                value: count as i32,
            },
        ),
    ))
}

/// Parses "a number of <type> counter(s) on it equal to <quantity>" dynamic
/// counts for entry-counter clauses (e.g. The Eleventh Doctor) and post-token
/// counter effects (Oversimplify, Fractal Anomaly class). Delegates the quantity
/// to the shared `parse_cda_quantity` building block so any "<verb> a number of
/// X counters … equal to …" card parses composed dynamic quantities
/// (twice/half/aggregate/difference), not just bare refs.
pub(crate) fn parse_dynamic_counter_suffix_body(
    input: &str,
) -> nom::IResult<&str, (CounterType, QuantityExpr), OracleError<'_>> {
    let (rest, _) = tag("a number of ").parse(input)?;
    let (rest, type_token) = take_until(" counter").parse(rest)?;
    let counter_type = crate::types::counter::parse_counter_type(type_token);
    let (rest, _) = tag(" counter").parse(rest)?;
    let (rest, _) = nom::combinator::opt(tag::<_, _, OracleError<'_>>("s")).parse(rest)?;
    let (rest, _) = tag(" on it equal to ").parse(rest)?;
    // Quantity: delegate to the full CDA quantity grammar so composed forms
    // (twice/half/aggregate/difference/sum) parse in enter-with-counters slots.
    let qty_text = rest.trim_end_matches('.').trim();
    let Some(qty) = parse_cda_quantity(qty_text) else {
        return Err(nom::Err::Failure(OracleError::new(
            rest,
            nom::error::ErrorKind::Fail,
        )));
    };
    Ok(("", (counter_type, qty)))
}

#[cfg(test)]
mod tests {
    use super::{
        match_create_of_those_tokens, nest_whenever_this_turn_token_cleanup_delayed_trigger,
        parse_enter_counters_clause_body, parse_where_x_quantity_expression,
        patch_choose_from_zone_counter_continuation_target, relink_gated_token_referent_consumers,
        strip_redundant_flip_win_quantifier, strip_return_destination_ext_with_remainder,
        strip_temporal_prefix, strip_temporal_suffix, strip_trailing_duration,
        strip_trailing_where_x, value_quantity_clause_owns_this_turn_suffix,
        ControlClausePossessor,
    };
    use crate::parser::oracle_util::TextPair;
    use crate::types::ability::{
        AbilityCondition, AbilityDefinition, AbilityKind, AggregateFunction,
        ContinuousModification, DelayedTriggerCondition, Duration, Effect, ModalChoice,
        ObjectProperty, ObjectScope, PtValue, QuantityExpr, QuantityRef, SubAbilityLink,
        TargetFilter, TriggerDefinition,
    };
    use crate::types::counter::CounterType;
    use crate::types::keywords::KeywordKind;
    use crate::types::phase::Phase;
    use crate::types::triggers::{PlaneswalkRole, TriggerMode};
    use crate::types::zones::Zone;

    #[test]
    fn strip_redundant_flip_win_quantifier_accepts_number_and_tense_variants() {
        for prefix in [
            "For each flip you won, ",
            "For each flips you won, ",
            "For each flip you win, ",
            "For each flips you win, ",
        ] {
            assert_eq!(
                strip_redundant_flip_win_quantifier(&format!("{prefix}draw a card.")),
                Some("draw a card.".to_string()),
                "must strip {prefix:?}"
            );
        }
    }

    fn gated_token_creator_for_relink() -> AbilityDefinition {
        let mut creator = AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::Token {
                name: "Soldier".to_string(),
                power: PtValue::Fixed(1),
                toughness: PtValue::Fixed(1),
                types: vec!["Creature".to_string(), "Soldier".to_string()],
                colors: vec![],
                keywords: vec![],
                tapped: false,
                count: QuantityExpr::Fixed { value: 1 },
                owner: TargetFilter::Controller,
                attach_to: None,
                enters_attacking: false,
                supertypes: vec![],
                static_abilities: vec![],
                enter_with_counters: vec![],
            },
        );
        creator.condition = Some(AbilityCondition::WhenYouDo);
        creator
    }

    fn last_created_consumer_for_relink(target: TargetFilter) -> AbilityDefinition {
        let mut consumer = AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::PutCounter {
                counter_type: CounterType::Plus1Plus1,
                count: QuantityExpr::Fixed { value: 1 },
                target,
            },
        );
        consumer.sub_link = SubAbilityLink::SequentialSibling;
        consumer
    }

    /// CR 603.12 + CR 608.2c: a token referent hidden by any `TargetFilter`
    /// wrapper still makes the following clause dependent on the gated token
    /// creator, so the false branch cannot bind an earlier resolution's token.
    #[test]
    fn relink_follows_last_created_through_target_filter_wrappers() {
        let wrapped_filters = [
            TargetFilter::Not {
                filter: Box::new(TargetFilter::LastCreated),
            },
            TargetFilter::TrackedSetFiltered {
                id: crate::types::identifiers::TrackedSetId(0),
                filter: Box::new(TargetFilter::LastCreated),
                caused_by: None,
            },
            TargetFilter::ChosenDamageSource {
                filter: Some(Box::new(TargetFilter::LastCreated)),
            },
        ];

        for filter in wrapped_filters {
            let mut defs = vec![
                gated_token_creator_for_relink(),
                last_created_consumer_for_relink(filter),
            ];
            relink_gated_token_referent_consumers(&mut defs);
            assert_eq!(
                defs[1].sub_link,
                SubAbilityLink::ContinuationStep,
                "a wrapped LastCreated reader must stay on the gated creator's continuation path"
            );
        }
    }

    /// CR 700.2 + CR 603.12: modal mode bodies are part of the containing
    /// definition for the re-link decision. A `LastCreated` reader in a chosen
    /// mode must not remain a standalone sibling of its gated token creator.
    #[test]
    fn relink_follows_last_created_through_modal_modes() {
        let modal = ModalChoice {
            min_choices: 1,
            max_choices: 1,
            mode_count: 1,
            ..Default::default()
        };
        let consumer = AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Controller,
            },
        )
        .with_modal(
            modal,
            vec![last_created_consumer_for_relink(TargetFilter::LastCreated)],
        );
        let mut defs = vec![gated_token_creator_for_relink(), consumer];
        defs[1].sub_link = SubAbilityLink::SequentialSibling;

        relink_gated_token_referent_consumers(&mut defs);

        assert_eq!(
            defs[1].sub_link,
            SubAbilityLink::ContinuationStep,
            "a modal LastCreated reader must keep its wrapper on the gated continuation path"
        );
    }

    /// CR 608.2c: a `ChooseFromZone` head with a `RemoveCounter`/`PutCounter`
    /// `sub_ability` whose `target` is the `SelfRef` "it" anaphor (Amy Pond's
    /// "choose a suspended card you own and remove that many time counters from
    /// it") must rebind that target to `ParentTarget` so the counters land on the
    /// CHOSEN card, not the ability source.
    #[test]
    fn patch_binds_choose_from_zone_counter_continuation_to_chosen_card() {
        use crate::types::ability::{CardSelectionMode, Chooser, QuantityRef, ZoneOwner};

        let mut def = AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::ChooseFromZone {
                count: 1,
                zone: Zone::Exile,
                additional_zones: vec![],
                zone_owner: ZoneOwner::Controller,
                filter: None,
                chooser: Chooser::Controller,
                up_to: false,
                selection: CardSelectionMode::Chosen,
                constraint: None,
            },
        );
        def.sub_ability = Some(Box::new(AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::RemoveCounter {
                counter_type: Some(CounterType::Time),
                count: QuantityExpr::Ref {
                    qty: QuantityRef::EventContextAmount,
                },
                target: TargetFilter::SelfRef,
            },
        )));

        patch_choose_from_zone_counter_continuation_target(&mut def);

        let sub = def.sub_ability.as_ref().expect("sub_ability preserved");
        assert!(
            matches!(
                &*sub.effect,
                Effect::RemoveCounter {
                    target: TargetFilter::ParentTarget,
                    ..
                }
            ),
            "the counter continuation's SelfRef must be rebound to ParentTarget, got {:?}",
            sub.effect
        );
    }

    /// Negative guard: a `RemoveCounter` head with NO `ChooseFromZone` parent keeps
    /// its `SelfRef` (the rebind is scoped to the choose-a-card anaphor only).
    #[test]
    fn patch_leaves_non_choose_from_zone_self_ref_counter_untouched() {
        let mut def = AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::RemoveCounter {
                counter_type: Some(CounterType::Time),
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::SelfRef,
            },
        );
        patch_choose_from_zone_counter_continuation_target(&mut def);
        assert!(matches!(
            &*def.effect,
            Effect::RemoveCounter {
                target: TargetFilter::SelfRef,
                ..
            }
        ));
    }

    /// CR 707.10f + CR 702.10a + CR 603.1: Choreographed Sparks' mode-2 "Copy
    /// target creature spell you control. The copy gains haste and \"At the
    /// beginning of the end step, sacrifice ~.\"" must fold the grant into the
    /// `CopySpell.additional_modifications` (AddKeyword(Haste) + GrantTrigger),
    /// leaving no residual `Unimplemented` sub-ability.
    #[test]
    fn choreographed_sparks_folds_copy_gains_haste_and_sac_into_additional_mods() {
        const TEXT: &str = "This spell can't be copied.\nChoose one or both —\n• Copy target instant or sorcery spell you control. You may choose new targets for the copy.\n• Copy target creature spell you control. The copy gains haste and \"At the beginning of the end step, sacrifice this token.\"";
        let parsed = crate::parser::oracle::parse_oracle_text(
            TEXT,
            "Choreographed Sparks",
            &[],
            &["Instant".to_string()],
            &[],
        );
        let mods = parsed
            .abilities
            .iter()
            .filter_map(find_copy_spell_additional_mods)
            .find(|mods| !mods.is_empty())
            .expect("mode-2 CopySpell must carry additional_modifications after the fold");
        assert_copy_gains_haste_and_sac(&mods);
        assert!(
            !parsed
                .abilities
                .iter()
                .any(ability_chain_has_unimplemented_the),
            "the 'the copy gains...' clause must no longer be Unimplemented"
        );
    }

    /// CR 707.10f + CR 603.1: Nalfeshnee's grant lives in a TRIGGER-execute chain
    /// ("Whenever you cast a spell from exile, copy it. ... the copy gains haste
    /// and \"...\""), which bypasses the `parse_effect_chain` wrappers. Folding at
    /// the `lower_effect_chain_ir` chokepoint (not the wrappers) is what makes
    /// this byte-identical grant flip supported too — the ≥2-class proof.
    #[test]
    fn nalfeshnee_trigger_folds_copy_gains_haste_and_sac_into_additional_mods() {
        const TEXT: &str = "Flying\nWhenever you cast a spell from exile, copy it. You may choose new targets for the copy. If it's a permanent spell, the copy gains haste and \"At the beginning of the end step, sacrifice this permanent.\" (A copy of a permanent spell becomes a token.)";
        let parsed = crate::parser::oracle::parse_oracle_text(
            TEXT,
            "Nalfeshnee",
            &[],
            &["Creature".to_string()],
            &["Beast".to_string(), "Demon".to_string()],
        );
        let mods = parsed
            .triggers
            .iter()
            .filter_map(|t| t.execute.as_deref())
            .filter_map(find_copy_spell_additional_mods)
            .find(|mods| !mods.is_empty())
            .expect("Nalfeshnee's trigger-execute CopySpell must carry additional_modifications");
        assert_copy_gains_haste_and_sac(&mods);
        assert!(
            !parsed
                .triggers
                .iter()
                .filter_map(|t| t.execute.as_deref())
                .any(ability_chain_has_unimplemented_the),
            "the 'the copy gains...' clause must no longer be Unimplemented"
        );
    }

    /// Walk a def chain for a `CopySpell` and return its `additional_modifications`.
    fn find_copy_spell_additional_mods(
        def: &AbilityDefinition,
    ) -> Option<Vec<ContinuousModification>> {
        let mut cur = Some(def);
        while let Some(d) = cur {
            if let Effect::CopySpell {
                additional_modifications,
                ..
            } = d.effect.as_ref()
            {
                return Some(additional_modifications.clone());
            }
            cur = d.sub_ability.as_deref();
        }
        None
    }

    fn ability_chain_has_unimplemented_the(def: &AbilityDefinition) -> bool {
        let mut cur = Some(def);
        while let Some(d) = cur {
            if matches!(d.effect.as_ref(), Effect::Unimplemented { name, .. } if name == "the") {
                return true;
            }
            cur = d.sub_ability.as_deref();
        }
        false
    }

    fn assert_copy_gains_haste_and_sac(mods: &[ContinuousModification]) {
        use crate::types::keywords::Keyword;
        assert!(
            mods.iter().any(|m| matches!(
                m,
                ContinuousModification::AddKeyword {
                    keyword: Keyword::Haste
                }
            )),
            "expected AddKeyword(Haste); got {mods:?}"
        );
        assert!(
            mods.iter().any(|m| matches!(
                m,
                ContinuousModification::GrantTrigger { trigger }
                    if matches!(trigger.execute.as_deref().map(|e| e.effect.as_ref()),
                        Some(Effect::Sacrifice { .. }))
            )),
            "expected GrantTrigger wrapping an end-step Sacrifice; got {mods:?}"
        );
    }

    /// CR 702.62b + CR 122.1 + CR 608.2c: Amy Pond's combat-damage trigger effect
    /// must lower to `ChooseFromZone { Exile }` whose NESTED `sub_ability` is
    /// `RemoveCounter { Time, EventContextAmount, ParentTarget }` — not two flat
    /// sibling clauses. The §C chain split, the §B choose recognizer, the
    /// `EventContextAmount` "that many" amount, and the §D anaphor rebind all land
    /// in one pass.
    #[test]
    fn amy_pond_trigger_effect_nests_remove_counter_under_choose_from_zone() {
        use crate::types::ability::QuantityRef;

        // Mimic the trigger's self-ref subject so "it" lowers to SelfRef pre-patch.
        let mut ctx = crate::parser::oracle_ir::context::ParseContext {
            subject: Some(TargetFilter::SelfRef),
            ..Default::default()
        };
        let def = crate::parser::oracle_effect::parse_effect_chain_with_context(
            "choose a suspended card you own and remove that many time counters from it",
            AbilityKind::Spell,
            &mut ctx,
        );

        assert!(
            matches!(
                &*def.effect,
                Effect::ChooseFromZone {
                    zone: Zone::Exile,
                    ..
                }
            ),
            "head must be ChooseFromZone {{ Exile }}, got {:?}",
            def.effect
        );
        let sub = def
            .sub_ability
            .as_ref()
            .expect("RemoveCounter must be NESTED as ChooseFromZone.sub_ability");
        match &*sub.effect {
            Effect::RemoveCounter {
                counter_type,
                count,
                target,
            } => {
                assert_eq!(*counter_type, Some(CounterType::Time));
                assert!(
                    matches!(
                        count,
                        QuantityExpr::Ref {
                            qty: QuantityRef::EventContextAmount
                        }
                    ),
                    "\"that many\" must be EventContextAmount, got {count:?}"
                );
                assert_eq!(
                    *target,
                    TargetFilter::ParentTarget,
                    "\"it\" must rebind to the chosen card (ParentTarget)"
                );
            }
            other => panic!("expected nested RemoveCounter, got {other:?}"),
        }
    }

    /// CR 107.3c: the "create N of those tokens" anaphor binds its count to a
    /// trailing ", where X is <expr>" clause when present (Adipose Offspring and
    /// The Final Days), and otherwise keeps the spell's announced {X}
    /// (Starnheim Unleashed / Conqueror's Pledge).
    #[test]
    fn match_create_of_those_tokens_binds_trailing_where_x_clause() {
        // CR 107.3c: cost-paid-object possessive → Toughness { CostPaidObject }.
        let adipose = Effect::unimplemented(
            "create",
            "create x of those tokens, where x is the sacrificed creature's toughness",
        );
        assert_eq!(
            match_create_of_those_tokens(&adipose),
            Some(QuantityExpr::Ref {
                qty: QuantityRef::Toughness {
                    scope: ObjectScope::CostPaidObject,
                },
            }),
        );

        // Boy-scout: The Final Days' graveyard-creature-count where-clause.
        let final_days = Effect::unimplemented(
            "create",
            "create x of those tokens, where x is the number of creature cards in your \
             graveyard",
        );
        assert!(
            matches!(
                match_create_of_those_tokens(&final_days),
                Some(QuantityExpr::Ref {
                    qty: QuantityRef::ZoneCardCount { .. }
                })
            ),
            "graveyard creature-count where-clause must bind, got {:?}",
            match_create_of_those_tokens(&final_days)
        );

        // No where-clause → the count stays the spell's announced {X}.
        let bare = Effect::unimplemented("create", "create x of those tokens");
        assert_eq!(
            match_create_of_those_tokens(&bare),
            Some(QuantityExpr::Ref {
                qty: QuantityRef::Variable {
                    name: "X".to_string(),
                },
            }),
        );
    }

    // CR 101.4 + CR 608.2c: the comma-prefixed per-player imperative scope ("for
    // each player, <imperative> ... that player controls") strips to PlayerFilter::All
    // plus the bare imperative residual. Building block for The Curse of Fenric I.
    #[test]
    fn for_each_player_comma_prefix_strips_to_all_scope() {
        use crate::types::ability::PlayerFilter;
        let (scope, residual) = super::strip_each_player_subject(
            "for each player, destroy up to one target creature that player controls",
        );
        assert_eq!(scope, Some(PlayerFilter::All));
        assert_eq!(
            residual, "destroy up to one target creature that player controls",
            "residual must be the bare imperative"
        );
    }

    // CR 608.2c + CR 109.4 + CR 608.2h: "each player other than its controller
    // <verb>" strips to `PlayerFilter::AllExcept { ParentObjectTargetController }`
    // and leaves the deconjugated imperative residual. The "each player other
    // than " arm must beat the bare "each player " arm. Building block for
    // Fractured Identity and the "each player other than ⟨ref⟩ does X" class.
    #[test]
    fn each_player_other_than_its_controller_strips_to_all_except_scope() {
        use crate::types::ability::PlayerFilter;
        let (scope, residual) = super::strip_each_player_subject(
            "each player other than its controller creates a token that's a copy of it.",
        );
        assert_eq!(
            scope,
            Some(PlayerFilter::AllExcept {
                exclude: Box::new(PlayerFilter::ParentObjectTargetController),
            }),
        );
        assert_eq!(
            residual, "create a token that's a copy of it.",
            "residual must be the deconjugated imperative with the exclusion stripped"
        );
    }

    // CR 122.1 + CR 402.1: "each opponent who has N or more poison counters"
    // and sibling hand-size / cards-drawn attr clauses share the quantity-path
    // attribute grammar via `parse_player_attribute_attr_clause`.
    #[test]
    fn each_opponent_who_has_poison_counters_strips_player_attribute_scope() {
        use crate::types::ability::{
            Comparator, CountScope, PlayerFilter, PlayerRelation, QuantityExpr, QuantityRef,
        };
        use crate::types::player::PlayerCounterKind;
        let (scope, residual) = super::strip_each_player_subject(
            "each opponent who has three or more poison counters exiles the top card of their library",
        );
        assert_eq!(
            scope,
            Some(PlayerFilter::PlayerAttribute {
                relation: PlayerRelation::Opponent,
                attr: Box::new(QuantityRef::PlayerCounter {
                    kind: PlayerCounterKind::Poison,
                    scope: CountScope::ScopedPlayer,
                }),
                comparator: Comparator::GE,
                value: Box::new(QuantityExpr::Fixed { value: 3 }),
            })
        );
        assert_eq!(
            residual, "exile the top card of their library",
            "residual must be the deconjugated imperative after the attr clause"
        );
    }

    #[test]
    fn each_opponent_with_cards_in_hand_strips_hand_size_attribute_scope() {
        use crate::types::ability::{
            Comparator, PlayerFilter, PlayerRelation, PlayerScope, QuantityExpr, QuantityRef,
        };
        let (scope, residual) = super::strip_each_player_subject(
            "each opponent with two or more cards in hand discards a card",
        );
        assert_eq!(
            scope,
            Some(PlayerFilter::PlayerAttribute {
                relation: PlayerRelation::Opponent,
                attr: Box::new(QuantityRef::HandSize {
                    player: PlayerScope::ScopedPlayer,
                }),
                comparator: Comparator::GE,
                value: Box::new(QuantityExpr::Fixed { value: 2 }),
            })
        );
        assert_eq!(residual, "discard a card");
    }

    // CR 406.2 + CR 610.3: "the owner of each card exiled with ~ " strips to the
    // OwnersOfCardsExiledBySource player scope. Building block for Trial of a Time
    // Lord IV (and unblocks the Possibility Storm owner-of-exiled sibling).
    #[test]
    fn owner_of_each_card_exiled_with_source_strips_scope() {
        use crate::types::ability::PlayerFilter;
        let (scope, residual) = super::strip_player_scope_subject(
            "the owner of each card exiled with ~ puts that card on the bottom of their library",
        );
        assert_eq!(scope, Some(PlayerFilter::OwnersOfCardsExiledBySource));
        assert_eq!(
            residual, "put that card on the bottom of their library",
            "residual must be the deconjugated imperative"
        );
    }

    // CR 406.2 + CR 610.3: end-to-end — the owner-of-exiled return clause lowers
    // to PutAtLibraryPosition with target ExiledBySource and Bottom position (the
    // "that card" anaphor rebinds to the source-linked exile pool).
    #[test]
    fn owner_of_each_card_exiled_lowers_to_bottom_of_library() {
        use crate::types::ability::{LibraryPosition, TargetFilter};
        let def = super::super::parse_effect_chain(
            "the owner of each card exiled with ~ puts that card on the bottom of their library",
            AbilityKind::Spell,
        );
        match *def.effect {
            Effect::PutAtLibraryPosition {
                ref target,
                position: LibraryPosition::Bottom,
                ..
            } => assert!(
                matches!(target, TargetFilter::ExiledBySource),
                "expected ExiledBySource target, got {target:?}"
            ),
            ref other => panic!("expected PutAtLibraryPosition(Bottom), got {other:?}"),
        }
    }

    #[test]
    fn extract_optional_target_multi_target_recovers_tap_up_to_four() {
        use crate::types::ability::MultiTargetSpec;
        let spec = super::extract_optional_target_multi_target("tap up to four target permanents")
            .expect("Elder Deep-Fiend cast trigger shape");
        assert_eq!(
            spec,
            MultiTargetSpec::up_to(QuantityExpr::Fixed { value: 4 })
        );
    }

    #[test]
    fn extract_verb_up_to_multi_target_recovers_untap_lands() {
        use crate::types::ability::MultiTargetSpec;
        let spec = super::extract_verb_up_to_multi_target("untap up to five lands")
            .expect("Peregrine Drake ETB shape");
        assert_eq!(
            spec,
            MultiTargetSpec::up_to(QuantityExpr::Fixed { value: 5 })
        );
    }

    #[test]
    fn distribute_damage_power_equal_pattern() {
        // Gap 1: "damage equal to its power" — Pattern B where qty follows "damage equal to"
        use crate::types::game_state::DistributionUnit;
        let text = "deal damage equal to its power divided as you choose among any number of target creatures and/or players";
        let lower = text.to_lowercase();
        let clause = super::try_parse_distribute_damage(&lower, text).expect("Gap 1 should parse");
        assert!(matches!(clause.distribute, Some(DistributionUnit::Damage)));
        assert!(
            clause.multi_target.is_some(),
            "must have multi_target for distribute"
        );
        assert!(matches!(clause.effect, Effect::DealDamage { .. }));
    }

    #[test]
    fn distribute_counters_third_person_predicate() {
        // Gap 2: "distributes" (3rd-person) verb after subject stripping
        use crate::types::game_state::DistributionUnit;
        let text = "distributes 3 +1/+1 counters among any number of target creatures you control";
        let lower = text.to_lowercase();
        let clause =
            super::try_parse_distribute_counters(&lower, text).expect("Gap 2 should parse");
        // Counter distribution uses DistributionUnit::Counters, NOT Damage
        assert!(matches!(
            clause.distribute,
            Some(DistributionUnit::Counters(_))
        ));
        assert!(clause.multi_target.is_some());
    }

    #[test]
    fn distribute_prevent_damage_fixed() {
        // Gap 3: fixed-N prevent-divide
        use crate::types::ability::PreventionAmount;
        use crate::types::game_state::DistributionUnit;
        let text =
            "prevent the next 5 damage divided as you choose among any number of target creatures";
        let clause = super::try_parse_prevent_distribute(text).expect("Gap 3 should parse");
        assert!(matches!(clause.distribute, Some(DistributionUnit::Damage)));
        assert!(clause.multi_target.is_some());
        assert!(matches!(
            clause.effect,
            Effect::PreventDamage {
                amount: PreventionAmount::Next(5),
                ..
            }
        ));
    }

    /// CR 615 (issue #1094): the bidirectional interceptor must NOT claim a
    /// "distributed among" clause — it lacks the "dealt to and dealt by"
    /// marker, so `try_parse_bidirectional_prevent` returns `None` and the
    /// distribute path (tried next in the family arm) still owns it. Regression
    /// guard on the step-9 ordering.
    #[test]
    fn bidirectional_prevent_ignores_distribute_clause() {
        let text =
            "prevent the next 5 damage divided as you choose among any number of target creatures";
        assert!(
            super::try_parse_bidirectional_prevent(text, true).is_none(),
            "distribute clause must not be claimed by the bidirectional interceptor"
        );
        // And the distribute path still parses it (ordering safety).
        assert!(super::try_parse_prevent_distribute(text).is_some());
    }

    /// CR 615 + CR 608.2c (issue #1094): the bidirectional split with the gate
    /// enabled produces a recipient ("to") node bound to `ParentTarget` and a
    /// source-only ("by") SequentialSibling with `damage_source_filter ==
    /// Some(ParentTarget)`. Driven at the interceptor level so the two-node
    /// structure is asserted directly.
    #[test]
    fn bidirectional_prevent_splits_into_to_and_by_nodes() {
        use crate::types::ability::{PreventionScope, SubAbilityLink, TargetFilter};
        let text =
            "prevent all combat damage that would be dealt to and dealt by that creature this turn";
        let clause = super::try_parse_bidirectional_prevent(text, true)
            .expect("bidirectional split with gate enabled");
        match &clause.effect {
            Effect::PreventDamage {
                target,
                damage_source_filter,
                scope,
                ..
            } => {
                assert_eq!(*target, TargetFilter::ParentTarget);
                assert!(damage_source_filter.is_none());
                assert_eq!(*scope, PreventionScope::CombatDamage);
            }
            other => panic!("expected 'to' PreventDamage, got {other:?}"),
        }
        let by = clause.sub_ability.as_deref().expect("'by' sub_ability");
        assert_eq!(by.sub_link, SubAbilityLink::SequentialSibling);
        match &*by.effect {
            Effect::PreventDamage {
                target,
                damage_source_filter,
                ..
            } => {
                assert_eq!(*target, TargetFilter::Any);
                assert_eq!(
                    damage_source_filter.as_ref(),
                    Some(&TargetFilter::ParentTarget)
                );
            }
            other => panic!("expected 'by' PreventDamage, got {other:?}"),
        }
        // Gate off ⇒ no split.
        assert!(
            super::try_parse_bidirectional_prevent(text, false).is_none(),
            "gate false ⇒ interceptor is a no-op"
        );
    }

    /// CR 400.7 + CR 700.4: A per-turn VALUE quantity's " this turn" suffix must
    /// not be claimed as an outer effect duration. Both the value-ownership
    /// predicate and the mid-clause ", or " duration stripper must defer to the
    /// quantity grammar so a binary-choice alternative branch is never amputated.
    #[test]
    fn value_quantity_owns_died_this_turn_suffix() {
        assert!(value_quantity_clause_owns_this_turn_suffix(
            "each of your opponents loses life equal to the total power of daleks that died this turn"
        ));
        // The mid-clause ", or …" stripper must leave the whole choice intact.
        let (rest, dur) = strip_trailing_duration(
            "Destroy all Dalek creatures and each of your opponents loses life equal to the total power of Daleks that died this turn, or destroy all non-Dalek creatures",
        );
        assert_eq!(
            rest,
            "Destroy all Dalek creatures and each of your opponents loses life equal to the total power of Daleks that died this turn, or destroy all non-Dalek creatures"
        );
        assert_eq!(dur, None);
    }

    /// A genuine "this turn" duration before ", or " that is NOT a per-turn
    /// quantity must still strip — the guard is scoped to value quantities only.
    #[test]
    fn genuine_this_turn_before_or_still_strips() {
        let (rest, dur) =
            strip_trailing_duration("creatures you control get +2/+2 this turn, or +0/+0");
        assert_eq!(dur, Some(Duration::UntilEndOfTurn));
        assert_eq!(rest, "creatures you control get +2/+2");
    }

    /// CR 119.3: A plain "lose 2 life this turn" with no dynamic quantity does
    /// NOT trigger value-ownership; the suffix is a real duration boundary.
    #[test]
    fn plain_this_turn_not_owned_by_value_quantity() {
        assert!(!value_quantity_clause_owns_this_turn_suffix(
            "creatures you control get +1/+1 this turn"
        ));
    }

    /// The shared dynamic counter grammar accepts composed quantities.
    #[test]
    fn dynamic_counter_suffix_parses_aggregate_equal_to() {
        use super::parse_dynamic_counter_suffix_body;
        let (_, (counter_type, count)) = parse_dynamic_counter_suffix_body(
            "a number of +1/+1 counters on it equal to the greatest mana value among cards in exile",
        )
        .unwrap();
        assert_eq!(counter_type, CounterType::Plus1Plus1);
        assert!(matches!(
            count,
            QuantityExpr::Ref {
                qty: QuantityRef::Aggregate {
                    function: AggregateFunction::Max,
                    property: ObjectProperty::ManaValue,
                    ..
                }
            }
        ));
    }

    /// CR 122.6 (:1208) + CR 110.2a (:618) + issue #1498: a counter clause with
    /// no `" on it"` filler must lift its counters onto `enter_with_counters`,
    /// and — the discriminating half — whatever is printed AFTER it must survive
    /// and reach the normal entry-clause path rather than being truncated away.
    /// Here the trailing "under its owner's control" must land on
    /// `dest.control`; under the old start-offset truncation it was discarded
    /// outright, so `control` came back `None` and this test fails if that
    /// behavior returns.
    ///
    /// SYNTHETIC INPUT — not a printed card. This text was once attributed to
    /// Unstoppable Slasher; that attribution was fabricated. The real card reads
    /// "…return it to the battlefield tapped under its owner's control with two
    /// stun counters on it." (filler present, clause-final). The filler-less
    /// shape this test pins IS real, but only clause-finally ("…with a vigilance
    /// counter and a lifelink counter on it", where the non-final element
    /// carries no filler); the trailing controller clause is an artificial
    /// stress case for the consumption path, kept because no printed card
    /// exercises an entry clause after the counters.
    #[test]
    fn return_to_battlefield_lifts_stun_counters_without_on_it_filler() {
        let (target, dest, remainder) = strip_return_destination_ext_with_remainder(
            "it to the battlefield tapped and with two stun counters under its owner's control",
        );
        assert_eq!(target, "it");
        let dest = dest.expect("expected a battlefield return destination");
        assert_eq!(dest.zone, Zone::Battlefield);
        assert!(dest.enter_tapped);
        assert_eq!(
            dest.enter_with_counters,
            vec![(CounterType::Stun, QuantityExpr::Fixed { value: 2 })]
        );
        // DISCRIMINATING: the clause printed after the counters is consumed as
        // an entry clause, not dropped. Truncating at the counter clause's start
        // offset (the previous behavior) left this `None`.
        assert_eq!(
            dest.control,
            Some(ControlClausePossessor::Owner),
            "the control clause printed after the counters must survive, got {:?}",
            dest.control
        );
        assert_eq!(
            remainder, "",
            "every entry clause is consumed, so nothing dangles, got {remainder:?}"
        );
    }

    /// CR 725.1 (:6240) + CR 608.2c (:2795) + CR 122.1 (:1178): Heart-Shaped
    /// Herb. An instruction printed after the counter clause is NOT part of the
    /// destination and must be handed back as the remainder for normal clause
    /// processing. This is the unit-level discriminator for the bug the PR
    /// fixes: the old start-offset truncation returned "" here, so the monarch
    /// instruction never reached a dispatcher.
    ///
    /// The full-sentence form is split upstream by `starts_bare_and_clause`
    /// (sequence.rs) before this function sees it; this test pins the seam
    /// itself so the destination parser stops depending on that split for
    /// correctness.
    #[test]
    fn return_to_battlefield_keeps_instruction_printed_after_counters() {
        let (target, dest, remainder) = strip_return_destination_ext_with_remainder(
            "that card to the battlefield under its owner's control with three +1/+1 counters on it and you become the monarch",
        );
        assert_eq!(target, "that card");
        let dest = dest.expect("expected a battlefield return destination");
        assert_eq!(dest.zone, Zone::Battlefield);
        assert_eq!(dest.control, Some(ControlClausePossessor::Owner));
        assert_eq!(
            dest.enter_with_counters,
            vec![(CounterType::Plus1Plus1, QuantityExpr::Fixed { value: 3 })]
        );
        // DISCRIMINATING: `" and you become the monarch"` is not a counter, a
        // rider or a control clause, so it must come back untouched.
        assert_eq!(
            remainder, " and you become the monarch",
            "the trailing instruction must survive the counter-clause consumption, got {remainder:?}"
        );
    }

    /// CR 122.1 (:1178): conjoined counter clauses inside ONE "with …" rider.
    /// Verbatim Oracle text of Perennation; Gilraen, Dúnedain Protector prints
    /// the same shape after a control clause. Parsing only the first conjunct
    /// (the previous behavior) silently dropped the second counter.
    #[test]
    fn return_to_battlefield_lifts_conjoined_counter_clauses() {
        let (_, dest, remainder) = strip_return_destination_ext_with_remainder(
            "target permanent card from your graveyard to the battlefield with a hexproof counter and an indestructible counter on it",
        );
        let dest = dest.expect("expected a battlefield return destination");
        assert_eq!(
            dest.enter_with_counters,
            vec![
                (
                    CounterType::Keyword(KeywordKind::Hexproof),
                    QuantityExpr::Fixed { value: 1 }
                ),
                (
                    CounterType::Keyword(KeywordKind::Indestructible),
                    QuantityExpr::Fixed { value: 1 }
                ),
            ],
            "both conjuncts of the counter rider must be lifted"
        );
        assert_eq!(remainder, "");
    }

    /// The conjoined-counter list must NOT swallow a non-counter conjunct: nom
    /// backtracks the `" and "` separator when the element parser rejects the
    /// text after it, because every element must open with a count or article.
    /// Pins the boundary against the conjunct shapes printed after a counter
    /// rider — a subject+predicate ("and you become the monarch",
    /// Heart-Shaped Herb), an imperative verb ("and draw two cards", the shape
    /// Cosima, God of the Voyage prints with an X count) and a second `"with"`
    /// rider ("and with haste", Voidpouncer).
    ///
    /// Cosima's literal `"with X +1/+1 counters"` is deliberately NOT used here:
    /// `parse_counter_suffix_body_combinator` opens on
    /// `nom_primitives::parse_number`, which accepts digits and English number
    /// words but not `"x"`, so an X-counted rider never reaches this list at
    /// all. That is a pre-existing gap in the count axis, unrelated to the
    /// conjunct boundary this test pins.
    #[test]
    fn counter_clause_list_stops_at_non_counter_conjunct() {
        for (input, expected_rest) in [
            (
                "with three +1/+1 counters on it and you become the monarch",
                " and you become the monarch",
            ),
            (
                "with two +1/+1 counters on it and draw two cards",
                " and draw two cards",
            ),
            // Voidpouncer: a second "with" rider, not a second counter.
            (
                "with two +1/+1 counters and a trample counter on it and with haste",
                " and with haste",
            ),
        ] {
            let (rest, counters) =
                parse_enter_counters_clause_body(input).expect("counter rider must parse");
            assert_eq!(rest, expected_rest, "wrong stop point for {input:?}");
            assert!(
                !counters.is_empty(),
                "reach guard: the rider itself must have parsed for {input:?}"
            );
        }
    }

    fn variable_x() -> QuantityExpr {
        QuantityExpr::Ref {
            qty: QuantityRef::Variable {
                name: "X".to_string(),
            },
        }
    }

    #[test]
    fn strip_trailing_where_x_stops_at_next_sentence() {
        let text = "put x +1/+1 counters on another target creature you control, where x is halana and alena's power. that creature gains haste until end of turn.";
        let lower = text.to_ascii_lowercase();
        let expr = strip_trailing_where_x(TextPair::new(text, &lower))
            .1
            .expect("where-x");
        assert_eq!(expr, "halana and alena's power");
    }

    #[test]
    fn strip_trailing_where_x_stops_at_non_enumerated_comma_continuation() {
        let text = "draw x cards, where x is the number of creatures you control, draw a card.";
        let lower = text.to_ascii_lowercase();
        let (without_where_x, expr) = strip_trailing_where_x(TextPair::new(text, &lower));

        assert_eq!(without_where_x.original, "draw x cards");
        assert_eq!(expr.as_deref(), Some("the number of creatures you control"));
    }

    #[test]
    fn where_x_comparator_bounds_preserve_variable_x() {
        for expression in [
            "less than or equal to the amount of life you gained",
            "less than the amount of life you gained",
            "greater than the number of creatures you control",
            "greater than or equal to the number of cards in your hand",
            "equal to the number of opponents",
        ] {
            assert_eq!(
                parse_where_x_quantity_expression(expression),
                Some(variable_x()),
                "{expression}"
            );
        }
    }

    #[test]
    fn token_cleanup_nesting_splits_only_cleanup_node_from_sibling_chain() {
        let token_creator = AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::Token {
                name: "Warrior".to_string(),
                power: PtValue::Fixed(1),
                toughness: PtValue::Fixed(1),
                types: vec!["Creature".to_string(), "Warrior".to_string()],
                colors: vec![],
                keywords: vec![],
                tapped: true,
                count: QuantityExpr::Fixed { value: 2 },
                owner: TargetFilter::Controller,
                attach_to: None,
                enters_attacking: true,
                supertypes: vec![],
                static_abilities: vec![],
                enter_with_counters: vec![],
            },
        );
        let mut cleanup = AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::CreateDelayedTrigger {
                condition: DelayedTriggerCondition::AtNextPhase { phase: Phase::End },
                effect: Box::new(AbilityDefinition::new(
                    AbilityKind::Spell,
                    Effect::Sacrifice {
                        target: TargetFilter::ParentTarget,
                        count: QuantityExpr::Fixed { value: 2 },
                        min_count: 0,
                    },
                )),
                uses_tracked_set: false,
            },
        );
        let mut following_sibling = AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Controller,
            },
        );
        following_sibling.sub_link = crate::types::ability::SubAbilityLink::SequentialSibling;
        cleanup.sub_ability = Some(Box::new(following_sibling));
        let mut outer = AbilityDefinition::new(
            AbilityKind::Activated,
            Effect::CreateDelayedTrigger {
                condition: DelayedTriggerCondition::WheneverEvent {
                    trigger: Box::new(TriggerDefinition::new(TriggerMode::YouAttack)),
                    expiry: crate::types::ability::WheneverEventExpiry::EndOfTurn,
                },
                effect: Box::new(token_creator),
                uses_tracked_set: false,
            },
        );
        outer.sub_ability = Some(Box::new(cleanup));

        nest_whenever_this_turn_token_cleanup_delayed_trigger(&mut outer);

        let Effect::CreateDelayedTrigger { effect: inner, .. } = outer.effect.as_ref() else {
            panic!("expected outer delayed trigger");
        };
        let nested_cleanup = inner
            .sub_ability
            .as_deref()
            .expect("cleanup node must move under token creator");
        let Effect::CreateDelayedTrigger {
            effect: cleanup_effect,
            ..
        } = nested_cleanup.effect.as_ref()
        else {
            panic!("expected nested cleanup delayed trigger");
        };
        assert!(
            nested_cleanup.sub_ability.is_none(),
            "only the cleanup node should move under the token creator"
        );
        assert!(
            matches!(
                cleanup_effect.effect.as_ref(),
                Effect::Sacrifice {
                    target: TargetFilter::LastCreated,
                    ..
                }
            ),
            "nested cleanup target must be rewritten to LastCreated"
        );
        assert!(
            matches!(
                outer
                    .sub_ability
                    .as_deref()
                    .map(|ability| ability.effect.as_ref()),
                Some(Effect::Draw { .. })
            ),
            "sibling effects after the cleanup must remain on the outer ability"
        );
    }

    /// CR 603.7a + CR 104.3e: the anaphoric "at the beginning of that turn's end
    /// step" (extra-turn-with-a-cost cards) is recognized by both temporal
    /// recognizers, mapping to the controller's next end step — identical to the
    /// existing "your next end step" arm.
    #[test]
    fn that_turns_end_step_temporal_resolves_to_controller_next_end_step() {
        let expected = DelayedTriggerCondition::AtNextPhaseForPlayer {
            phase: Phase::End,
            player: crate::types::player::PlayerId(0),
            gate: crate::types::ability::TurnGate::None,
        };

        let (rest, cond) =
            strip_temporal_prefix("at the beginning of that turn's end step, you lose the game");
        assert_eq!(rest, "you lose the game");
        assert_eq!(cond, Some(expected.clone()));

        let (rest, cond) =
            strip_temporal_suffix("you lose the game at the beginning of that turn's end step");
        assert_eq!(rest, "you lose the game");
        assert_eq!(cond, Some(expected));
    }

    /// CR 511.2 + CR 603.7a: "At this turn's next end of combat, …" prefix-form
    /// delayed trigger fires at the end-of-combat step of the current turn.
    /// Covers Triton Tactics, Glyph of Doom.
    #[test]
    fn strip_temporal_prefix_at_this_turns_next_end_of_combat() {
        let (text, cond) =
            strip_temporal_prefix("at this turn's next end of combat, untap that creature");
        assert_eq!(text, "untap that creature");
        assert_eq!(
            cond,
            Some(DelayedTriggerCondition::AtNextPhase {
                phase: Phase::EndCombat,
            })
        );
    }

    /// CR 603.7a + CR 701.31: the inline "When a player planeswalks, …" delayed
    /// trigger prefix strips to its body and yields a `WhenNextEvent` condition
    /// keyed to `Planeswalked { role: Any }`, no `or_trigger`, `Persistent` lifetime
    /// (CR 603.7b — no stated duration). The Doctor's Childhood Barn's delayed
    /// phase-in.
    #[test]
    fn strip_temporal_prefix_when_a_player_planeswalks() {
        let (body, cond) =
            strip_temporal_prefix("when a player planeswalks, those permanents phase in");
        assert_eq!(body, "those permanents phase in");
        assert_eq!(
            cond,
            Some(DelayedTriggerCondition::WhenNextEvent {
                trigger: Box::new(TriggerDefinition::new(TriggerMode::Planeswalked {
                    role: PlaneswalkRole::Any,
                })),
                or_trigger: None,
                lifetime: crate::types::ability::DelayedTriggerLifetime::Persistent,
            })
        );
    }

    /// Build-the-class: the extra-turn-with-a-cost family parses to BOTH an
    /// `ExtraTurn` effect AND a delayed `LoseTheGame` trigger fired at the extra
    /// turn's end step (CR 603.7a). Previously the second sentence was dropped as
    /// an `Effect:at` gap, so these cards became a downside-free extra turn.
    #[test]
    fn extra_turn_then_lose_parses_delayed_lose_the_game() {
        use crate::parser::oracle_effect::parse_effect_chain;

        // Recursively collect every effect in the def + sub_ability chain,
        // descending into CreateDelayedTrigger's inner effect.
        fn collect<'a>(def: &'a AbilityDefinition, out: &mut Vec<&'a Effect>) {
            out.push(&def.effect);
            if let Effect::CreateDelayedTrigger { effect, .. } = &*def.effect {
                collect(effect, out);
            }
            if let Some(sub) = def.sub_ability.as_deref() {
                collect(sub, out);
            }
        }

        for text in [
            "Take an extra turn after this one. At the beginning of that turn's end step, you lose the game.",
            "Creatures you control gain indestructible. Take an extra turn after this one. At the beginning of that turn's end step, you lose the game.",
        ] {
            let def = parse_effect_chain(text, AbilityKind::Spell);
            let mut effects = Vec::new();
            collect(&def, &mut effects);

            assert!(
                effects
                    .iter()
                    .any(|e| matches!(e, Effect::ExtraTurn { .. })),
                "expected an ExtraTurn effect in {text:?}, got {effects:?}"
            );
            let delayed_lose = effects.iter().any(|e| {
                matches!(
                    e,
                    Effect::CreateDelayedTrigger {
                        condition: DelayedTriggerCondition::AtNextPhaseForPlayer {
                            phase: Phase::End,
                            ..
                        },
                        effect,
                        ..
                    } if matches!(&*effect.effect, Effect::LoseTheGame { .. })
                )
            });
            assert!(
                delayed_lose,
                "expected a delayed LoseTheGame at the extra turn's end step in {text:?}, got {effects:?}"
            );
        }
    }

    /// Issue #528: Nine-Lives Familiar — "return it to the battlefield with one
    /// fewer revival counter on it" must produce a ClampMin(Offset(CountersOn))
    /// quantity, not a bogus counter type "fewer revival".
    #[test]
    fn return_to_battlefield_with_one_fewer_counter_produces_offset_quantity() {
        let (target, dest, remainder) = strip_return_destination_ext_with_remainder(
            "it to the battlefield with one fewer revival counter on it",
        );
        assert_eq!(target, "it");
        let dest = dest.expect("expected a battlefield return destination");
        assert_eq!(dest.zone, Zone::Battlefield);
        assert_eq!(dest.enter_with_counters.len(), 1);
        let (ct, qty) = &dest.enter_with_counters[0];
        assert_eq!(*ct, CounterType::Generic("revival".to_string()));
        // ClampMin { Offset { Ref { CountersOn { Source, revival } }, -1 }, 0 }
        match qty {
            QuantityExpr::ClampMin { inner, minimum } => {
                assert_eq!(*minimum, 0);
                match inner.as_ref() {
                    QuantityExpr::Offset { inner, offset } => {
                        assert_eq!(*offset, -1);
                        match inner.as_ref() {
                            QuantityExpr::Ref {
                                qty:
                                    QuantityRef::CountersOn {
                                        scope,
                                        counter_type,
                                    },
                            } => {
                                assert_eq!(*scope, ObjectScope::Source);
                                assert_eq!(
                                    *counter_type,
                                    Some(CounterType::Generic("revival".to_string()))
                                );
                            }
                            other => panic!("expected CountersOn ref, got {other:?}"),
                        }
                    }
                    other => panic!("expected Offset, got {other:?}"),
                }
            }
            other => panic!("expected ClampMin, got {other:?}"),
        }
        assert_eq!(remainder, "");
    }
}
#[cfg(test)]
mod where_x_tests {
    use super::parse_where_x_quantity_expression;
    use crate::types::ability::{
        AbilityDefinition, AbilityKind, Comparator, ContinuousModification, ControllerRef,
        DigSource, Duration, Effect, FilterProp, ObjectScope, PlayerScope, PtValue, QuantityExpr,
        QuantityRef, StaticDefinition, TargetFilter, TriggerDefinition, TypeFilter, TypedFilter,
    };
    use crate::types::triggers::TriggerMode;
    use crate::types::zones::Zone;

    /// CR 706.2 + CR 706.4: "where X is the result" (of a die roll / coin flip)
    /// binds X to the rolled value via `EventContextAmount` — the same channel
    /// the inline "equal to the result" class uses. Building-block guard for
    /// Ancient Bronze Dragon's reflexive "put X +1/+1 counters … where X is the
    /// result" (issue #1602, Deliverable 1).
    #[test]
    fn where_x_is_the_result_binds_event_context_amount() {
        assert_eq!(
            parse_where_x_quantity_expression("the result"),
            Some(QuantityExpr::Ref {
                qty: QuantityRef::EventContextAmount,
            })
        );
    }

    #[test]
    fn where_x_tokens_created_this_turn_binds_typed_quantity() {
        use crate::types::ability::{FilterProp, PlayerScope, TargetFilter, TypedFilter};

        assert_eq!(
            parse_where_x_quantity_expression("the number of tokens you created this turn"),
            Some(QuantityExpr::Ref {
                qty: QuantityRef::TokensCreatedThisTurn {
                    player: PlayerScope::Controller,
                    filter: TargetFilter::Typed(TypedFilter {
                        type_filters: vec![],
                        controller: None,
                        properties: vec![FilterProp::Token],
                    }),
                },
            })
        );
    }

    #[test]
    fn where_x_life_lost_this_turn_binds_typed_quantity() {
        assert_eq!(
            parse_where_x_quantity_expression("the life you've lost this turn"),
            Some(QuantityExpr::Ref {
                qty: QuantityRef::LifeLostThisTurn {
                    player: PlayerScope::Controller
                },
            })
        );
    }

    /// Issue #1993: Halana and Alena, Partners — "where X is [name]'s power".
    #[test]
    fn where_x_printed_name_possessive_power_is_source() {
        assert_eq!(
            parse_where_x_quantity_expression("Halana and Alena's power"),
            Some(QuantityExpr::Ref {
                qty: QuantityRef::Power {
                    scope: ObjectScope::Source,
                },
            })
        );
    }

    #[test]
    fn strip_trailing_duration_preserves_tokens_created_this_turn_phrase() {
        use super::strip_trailing_duration;

        let text = "create X 1/1 white Spirit creature tokens with flying, where X is the number of tokens you created this turn.";
        let (stripped, duration) = strip_trailing_duration(text);
        assert!(
            duration.is_none(),
            "quantity tracker must not become a duration"
        );
        assert_eq!(stripped, text);
    }

    #[test]
    fn strip_trailing_duration_preserves_where_x_life_lost_this_turn_phrase() {
        use super::strip_trailing_duration;

        let text = "draw X cards, where X is the life you've lost this turn.";
        let (stripped, duration) = strip_trailing_duration(text);
        assert!(
            duration.is_none(),
            "quantity tracker must not become a duration"
        );
        assert_eq!(stripped, text);
    }

    #[test]
    fn strip_trailing_duration_preserves_life_lost_this_turn_phrase() {
        use super::strip_trailing_duration;

        let text = "draw a card for each opponent who lost life this turn.";
        let (stripped, duration) = strip_trailing_duration(text);
        assert!(duration.is_none());
        assert_eq!(stripped, text);
    }

    #[test]
    fn strip_trailing_duration_still_strips_outer_duration_after_where_x_clause() {
        use super::strip_trailing_duration;

        let text = "draw X cards, where X is the life you've lost this turn, then target creature gets +1/+1 this turn.";
        let (stripped, duration) = strip_trailing_duration(text);
        assert_eq!(
            duration,
            Some(Duration::UntilEndOfTurn),
            "outer duration must still be recognized"
        );
        assert_eq!(
            stripped,
            "draw X cards, where X is the life you've lost this turn, then target creature gets +1/+1"
        );
    }

    /// Issue #4735 — Admiral Beckett Brass end-to-end: the end-step steal trigger
    /// must lower to `Effect::GainControl` with NO duration (a control-change with
    /// no stated duration is permanent, CR 611.2a) — the "this turn" from the
    /// look-back target clause must not leak onto the effect as `UntilEndOfTurn`.
    #[test]
    fn beckett_brass_gain_control_has_no_duration() {
        use crate::types::ability::Effect;
        let parsed = crate::parser::oracle::parse_oracle_text(
            "Other Pirates you control get +1/+1.\nAt the beginning of your end step, gain control of target nonland permanent controlled by a player who was dealt combat damage by three or more Pirates this turn.",
            "Admiral Beckett Brass",
            &[],
            &["Creature".to_string()],
            &["Human".to_string(), "Pirate".to_string()],
        );
        let steal = parsed
            .triggers
            .iter()
            .find(|t| {
                matches!(
                    &*t.execute.as_ref().unwrap().effect,
                    Effect::GainControl { .. }
                )
            })
            .expect("Beckett's end-step trigger must lower to GainControl");
        let execute = steal.execute.as_ref().unwrap();
        assert_eq!(
            execute.duration, None,
            "the steal is permanent (CR 611.2a) — no phantom UntilEndOfTurn from the look-back clause, got {:?}",
            execute.duration
        );
    }

    /// Issue #4735 — Admiral Beckett Brass: the end-step steal target must now
    /// carry the CONTROLLER PREDICATE `FilterProp::ControllerMatches` wrapping
    /// `OpponentDealtDamage { CombatOnly, Some(Typed(Pirate)) }` (the object-side
    /// bridge into the PlayerFilter enum), and the whole card must lower with ZERO
    /// `Effect::Unimplemented` (positive reach-guard: the target now parses
    /// semantically, not just structurally for duration purposes). The numeric
    /// "three or more" is a documented DEFERRED gap — consumed, not enforced.
    #[test]
    fn beckett_brass_gain_control_target_carries_controller_matches() {
        use crate::types::ability::{
            DamageKindFilter, Effect, FilterProp, PlayerFilter, TargetFilter, TypeFilter,
        };
        let parsed = crate::parser::oracle::parse_oracle_text(
            "Other Pirates you control get +1/+1.\nAt the beginning of your end step, gain control of target nonland permanent controlled by a player who was dealt combat damage by three or more Pirates this turn.",
            "Admiral Beckett Brass",
            &[],
            &["Creature".to_string()],
            &["Human".to_string(), "Pirate".to_string()],
        );

        // Positive reach-guard: nothing on this card lowered to Unimplemented.
        assert!(
            !parsed.triggers.iter().any(|t| {
                t.execute
                    .as_ref()
                    .is_some_and(|e| matches!(&*e.effect, Effect::Unimplemented { .. }))
            }),
            "Beckett must not produce any Unimplemented effect once the look-back target parses"
        );

        let steal = parsed
            .triggers
            .iter()
            .find_map(|t| match &*t.execute.as_ref()?.effect {
                Effect::GainControl { target } => Some(target.clone()),
                _ => None,
            })
            .expect("Beckett's end-step trigger must lower to GainControl");

        let TargetFilter::Typed(typed) = &steal else {
            panic!("expected a Typed GainControl target, got {steal:?}");
        };
        let has_predicate = typed.properties.iter().any(|p| {
            matches!(
                p,
                FilterProp::ControllerMatches { player }
                    if matches!(
                        &**player,
                        PlayerFilter::OpponentDealtDamage {
                            kind: DamageKindFilter::CombatOnly,
                            source: Some(src),
                            // "three or more Pirates" → the count threshold is
                            // enforced (not dropped), so min_sources must be 3.
                            min_sources: 3,
                        } if matches!(
                            &**src,
                            TargetFilter::Typed(t)
                                if t.type_filters
                                    .contains(&TypeFilter::Subtype("Pirate".to_string()))
                        )
                    )
            )
        });
        assert!(
            has_predicate,
            "GainControl target must carry ControllerMatches{{OpponentDealtDamage{{CombatOnly, Some(Pirate), min_sources: 3}}}}, got {:?}",
            typed.properties
        );
    }

    #[test]
    fn strip_trailing_duration_still_strips_genuine_this_turn_duration() {
        use super::strip_trailing_duration;

        let (stripped, duration) = strip_trailing_duration("that creature gains haste this turn.");
        assert_eq!(duration, Some(Duration::UntilEndOfTurn));
        assert_eq!(stripped, "that creature gains haste");
    }

    /// Issue #4735 — Admiral Beckett Brass: the "this turn" belongs to the
    /// "controlled by a player who was dealt combat damage … this turn" look-back
    /// relative clause on the target, NOT to the control-change's duration (which
    /// is permanent, CR 611.2a). The `who`-introduced player look-back guard must
    /// preserve the whole clause and yield no duration.
    #[test]
    fn strip_trailing_duration_preserves_who_dealt_combat_damage_lookback() {
        use super::strip_trailing_duration;

        let text = "gain control of target nonland permanent controlled by a player who was dealt combat damage by three or more pirates this turn.";
        let (stripped, duration) = strip_trailing_duration(text);
        assert!(
            duration.is_none(),
            "the player look-back 'this turn' must not become an effect duration, got {duration:?}"
        );
        // The guard preserves the full clause (trailing period retained, as for
        // the other `preserves_*` guard tests).
        assert_eq!(stripped, text.trim());
    }

    /// Hostile fixture (review requirement): a `who`-introduced look-back
    /// relative clause followed by a GENUINE outer "until end of turn" duration —
    /// the outer duration must still strip, the guard must NOT over-suppress it.
    #[test]
    fn strip_trailing_duration_who_lookback_plus_genuine_outer_duration() {
        use super::strip_trailing_duration;

        let text = "target creature controlled by a player who lost life this turn gains haste until end of turn.";
        let (stripped, duration) = strip_trailing_duration(text);
        assert_eq!(
            duration,
            Some(Duration::UntilEndOfTurn),
            "the genuine outer 'until end of turn' must still be recognized"
        );
        assert_eq!(
            stripped,
            "target creature controlled by a player who lost life this turn gains haste"
        );
    }

    /// The new delegation must NOT shadow `parse_cda_quantity`: "the number of
    /// …" expressions still route through the CDA-quantity path (the event-
    /// context combinator returns `None` for them).
    #[test]
    fn cda_quantity_returns_none_for_the_result() {
        // Precondition for the "CDA first, event-context fallback" ordering:
        // `parse_cda_quantity` does not classify the bare die-result phrase, so
        // the event-context delegation can safely catch it without shadowing any
        // CDA-handled where-X binding.
        assert_eq!(
            crate::parser::oracle_quantity::parse_cda_quantity("the result"),
            None
        );
    }

    #[test]
    fn where_x_number_of_phrase_not_shadowed_by_event_context() {
        // "the number of creatures you control" is a CDA-quantity object count,
        // not an event-context amount — must not resolve to EventContextAmount.
        let parsed = parse_where_x_quantity_expression("the number of creatures you control");
        assert_ne!(
            parsed,
            Some(QuantityExpr::Ref {
                qty: QuantityRef::EventContextAmount,
            }),
            "the number-of phrase must route through parse_cda_quantity, not the \
             event-context delegation"
        );
    }

    /// CR 107.3i + CR 115.1: a where-X count may depend on objects controlled
    /// by a target player. The shared where-X parser owns that count grammar;
    /// effect-specific parsers only surface the companion target slot.
    #[test]
    fn where_x_number_of_target_player_controlled_type_binds_target_player_count() {
        let parsed =
            parse_where_x_quantity_expression("the number of Islands target opponent controls");
        let Some(QuantityExpr::Ref {
            qty: QuantityRef::ObjectCount { filter },
        }) = parsed
        else {
            panic!("expected target-player object count, got {parsed:?}");
        };
        let TargetFilter::Typed(typed) = filter else {
            panic!("expected typed object count filter, got {filter:?}");
        };
        // CR 109.4: "target opponent controls" now lowers to the opponent-constrained
        // ControllerRef::TargetOpponent (was the looser TargetPlayer).
        assert_eq!(typed.controller, Some(ControllerRef::TargetOpponent));
        assert!(
            typed
                .type_filters
                .contains(&TypeFilter::Subtype("Island".to_string())),
            "expected Island subtype in object-count filter, got {:?}",
            typed.type_filters
        );
    }

    /// CR 107.3i + CR 202.3: the where-X traversal rebinds a `TotalManaValue`
    /// target constraint's `Variable("X")` cap to the die-result
    /// `EventContextAmount` (Ancient Brass Dragon's "where X is the result").
    /// CR 601.2a-b + CR 602.2b: the announce-lock recognizer is a BUILDING BLOCK, so it is
    /// tested across its input range, not on one card. Both wordings name the same
    /// announcement step (602.2b makes activating follow 601.2b-i identically), so one
    /// `alt()` must accept both; anything else must fall through untouched, because a
    /// false positive here would FREEZE a value CR 107.3c says may change while on the
    /// stack.
    #[test]
    fn strip_announce_lock_accepts_both_wordings_and_nothing_else() {
        // SPELL surface (CR 601.2a-b)
        assert_eq!(
            super::strip_announce_lock(
                "the number of Mountains you control as you cast this spell"
            ),
            Some("the number of Mountains you control")
        );
        // ACTIVATED-ABILITY surface (CR 602.2b) — same channel, same combinator
        assert_eq!(
            super::strip_announce_lock(
                "the number of Bobbleheads you control as you activate this ability"
            ),
            Some("the number of Bobbleheads you control")
        );
        // Trailing period + mixed case: the returned slice keeps ORIGINAL case.
        assert_eq!(
            super::strip_announce_lock(
                "the greatest power among creatures you control As You Cast This Spell."
            ),
            Some("the greatest power among creatures you control")
        );

        // NEGATIVE: an UNLOCKED text-defined X (CR 107.3c) must not match — it is a live
        // value and freezing it would be rules-wrong.
        assert_eq!(
            super::strip_announce_lock("the number of creatures you control"),
            None
        );
        // NEGATIVE: a lock-shaped phrase that is not the announce qualifier.
        assert_eq!(
            super::strip_announce_lock("the number of cards you drew as you cast your last spell"),
            None
        );
        // NEGATIVE: the qualifier must TERMINATE the tail (eof), not sit mid-phrase.
        assert_eq!(
            super::strip_announce_lock("X as you cast this spell plus the number of Islands"),
            None
        );
    }

    #[test]
    fn apply_where_x_to_target_constraint_binds_total_mana_value_cap() {
        use crate::types::ability::Comparator;
        use crate::types::game_state::TargetSelectionConstraint;

        let mut constraint = TargetSelectionConstraint::TotalManaValue {
            comparator: Comparator::LE,
            value: QuantityExpr::Ref {
                qty: QuantityRef::Variable { name: "X".into() },
            },
        };
        let mut unbound = None;
        super::apply_where_x_to_target_constraint(
            &mut constraint,
            Some("the result"),
            &mut unbound,
        );
        assert_eq!(
            unbound, None,
            "\"the result\" is representable, so no gap is recorded"
        );
        assert_eq!(
            constraint,
            TargetSelectionConstraint::TotalManaValue {
                comparator: Comparator::LE,
                value: QuantityExpr::Ref {
                    qty: QuantityRef::EventContextAmount,
                },
            }
        );
    }

    #[test]
    fn parse_total_mana_value_target_constraint_preserves_fixed_cap() {
        use crate::types::ability::Comparator;
        use crate::types::game_state::TargetSelectionConstraint;

        assert_eq!(
            super::parse_total_mana_value_target_constraint(
                "target creature cards with total mana value 6 or less from graveyards"
            ),
            Some(TargetSelectionConstraint::TotalManaValue {
                comparator: Comparator::LE,
                value: QuantityExpr::Fixed { value: 6 },
            })
        );
    }

    #[test]
    fn strip_trailing_where_x_stops_at_following_sentence() {
        let (_, expr) = super::strip_trailing_where_x(crate::parser::oracle_util::TextPair::new(
            "creature card with mana value X or less, where X is 2 plus the sacrificed creature's mana value. Put that card onto the battlefield",
            "creature card with mana value x or less, where x is 2 plus the sacrificed creature's mana value. put that card onto the battlefield",
        ));
        assert_eq!(
            expr.as_deref(),
            Some("2 plus the sacrificed creature's mana value")
        );
    }

    /// Constraints without a quantity bound are left untouched.
    #[test]
    fn apply_where_x_to_target_constraint_leaves_non_quantity_unchanged() {
        use crate::types::game_state::TargetSelectionConstraint;

        let mut constraint = TargetSelectionConstraint::DifferentObjectControllers;
        let mut unbound = None;
        super::apply_where_x_to_target_constraint(
            &mut constraint,
            Some("the result"),
            &mut unbound,
        );
        assert_eq!(
            constraint,
            TargetSelectionConstraint::DifferentObjectControllers
        );
    }

    #[test]
    fn apply_where_x_quantity_expression_recurses_sum_max_difference_power() {
        fn x_ref() -> QuantityExpr {
            QuantityExpr::Ref {
                qty: QuantityRef::Variable { name: "X".into() },
            }
        }

        let expression = QuantityExpr::Sum {
            exprs: vec![
                x_ref(),
                QuantityExpr::Max {
                    exprs: vec![
                        x_ref(),
                        QuantityExpr::Difference {
                            left: Box::new(x_ref()),
                            right: Box::new(QuantityExpr::Power {
                                base: 2,
                                exponent: Box::new(x_ref()),
                            }),
                        },
                    ],
                },
            ],
        };

        let rewritten = super::apply_where_x_quantity_expression(expression, Some("the result"))
            .expect("\"the result\" is representable, so the bind must succeed");
        let QuantityExpr::Sum { exprs } = rewritten else {
            panic!("expected Sum");
        };
        assert!(
            exprs.iter().all(|expr| !expr.contains_x()),
            "all nested X refs must be rewritten, got {exprs:?}"
        );
        fn has_event_context_amount(expr: &QuantityExpr) -> bool {
            match expr {
                QuantityExpr::Ref {
                    qty: QuantityRef::EventContextAmount,
                } => true,
                QuantityExpr::Offset { inner, .. }
                | QuantityExpr::ClampMin { inner, .. }
                | QuantityExpr::Multiply { inner, .. }
                | QuantityExpr::DivideRounded { inner, .. }
                | QuantityExpr::UpTo { max: inner }
                | QuantityExpr::Power {
                    exponent: inner, ..
                } => has_event_context_amount(inner),
                QuantityExpr::Sum { exprs } | QuantityExpr::Max { exprs } => {
                    exprs.iter().any(has_event_context_amount)
                }
                QuantityExpr::Difference { left, right } => {
                    has_event_context_amount(left) || has_event_context_amount(right)
                }
                QuantityExpr::Fixed { .. } | QuantityExpr::Ref { .. } => false,
            }
        }

        assert!(
            exprs.iter().all(has_event_context_amount),
            "rewritten expression should contain the where-X event amount in every branch: {exprs:?}"
        );
    }

    #[test]
    fn apply_where_x_effect_expression_rewrites_token_count_and_pt() {
        let mut effect = Effect::Token {
            name: "Ooze".to_string(),
            power: PtValue::Variable("X".to_string()),
            toughness: PtValue::Variable("X".to_string()),
            types: vec!["Creature".to_string(), "Ooze".to_string()],
            colors: vec![],
            keywords: vec![],
            tapped: false,
            count: QuantityExpr::Ref {
                qty: QuantityRef::Variable {
                    name: "X".to_string(),
                },
            },
            owner: TargetFilter::Controller,
            attach_to: None,
            enters_attacking: false,
            supertypes: vec![],
            static_abilities: vec![],
            enter_with_counters: vec![],
        };

        super::apply_where_x_effect_expression(&mut effect, Some("that spell's mana value"));

        let expected = QuantityExpr::Ref {
            qty: QuantityRef::ObjectManaValue {
                scope: ObjectScope::EventSource,
            },
        };
        let Effect::Token {
            count,
            power,
            toughness,
            ..
        } = effect
        else {
            panic!("expected Token");
        };
        assert_eq!(count, expected.clone(), "token count must bind where-X");
        assert_eq!(
            power,
            PtValue::Quantity(expected.clone()),
            "token power must bind where-X"
        );
        assert_eq!(
            toughness,
            PtValue::Quantity(expected),
            "token toughness must bind where-X"
        );
    }

    #[test]
    fn where_x_rewrites_grant_trigger_execute_for_emergent_woodwurm() {
        fn x_ref() -> QuantityExpr {
            QuantityExpr::Ref {
                qty: QuantityRef::Variable { name: "X".into() },
            }
        }

        let mut effect = Effect::GenericEffect {
            static_abilities: vec![StaticDefinition::continuous().modifications(vec![
                ContinuousModification::GrantTrigger {
                    trigger: Box::new(TriggerDefinition::new(TriggerMode::Attacks).execute(
                        AbilityDefinition::new(
                            AbilityKind::Spell,
                            Effect::Dig {
                                player: TargetFilter::Controller,
                                count: x_ref(),
                                destination: Some(Zone::Battlefield),
                                keep_count: Some(1),
                                keep_count_expr: None,
                                up_to: true,
                                filter: TargetFilter::Typed(
                                    TypedFilter::new(TypeFilter::Permanent).properties(vec![
                                        FilterProp::Cmc {
                                            comparator: Comparator::LE,
                                            value: x_ref(),
                                        },
                                    ]),
                                ),
                                rest_destination: Some(Zone::Library),
                                rest_order: crate::types::ability::DigRestOrder::Preserve,
                                reveal: true,
                                enter_tapped: false,
                                enters_attacking: false,
                                source: DigSource::Library,
                            },
                        ),
                    )),
                },
            ])],
            duration: Some(Duration::UntilEndOfTurn),
            target: None,
            end_cost: None,
        };

        super::apply_where_x_effect_expression(&mut effect, Some("its power"));

        let Effect::GenericEffect {
            static_abilities, ..
        } = effect
        else {
            panic!("expected GenericEffect");
        };
        let [static_def] = static_abilities.as_slice() else {
            panic!("expected one static definition, got {static_abilities:?}");
        };
        let [ContinuousModification::GrantTrigger { trigger }] =
            static_def.modifications.as_slice()
        else {
            panic!(
                "expected one GrantTrigger, got {:?}",
                static_def.modifications
            );
        };
        let execute = trigger.execute.as_ref().expect("grant trigger execute");
        let Effect::Dig { count, filter, .. } = execute.effect.as_ref() else {
            panic!("expected Dig execute, got {:?}", execute.effect);
        };
        assert!(
            !count.contains_x(),
            "Dig count must bind where-X: {count:?}"
        );
        let TargetFilter::Typed(typed) = filter else {
            panic!("expected typed Dig filter, got {filter:?}");
        };
        let Some(FilterProp::Cmc { value, .. }) = typed
            .properties
            .iter()
            .find(|prop| matches!(prop, FilterProp::Cmc { .. }))
        else {
            panic!("expected Cmc filter property, got {:?}", typed.properties);
        };
        assert!(
            !value.contains_x(),
            "Dig filter Cmc must bind where-X: {value:?}"
        );
    }

    /// Issue #1375 — CR 608.2c + CR 115.10a + CR 202.3: "where X is that card's
    /// mana value" is an anaphoric reference to a card revealed by an earlier
    /// instruction in the same ability (Twilight Prophet, Erratic Mutation, …),
    /// NOT a target (CR 115.10a — no "target" word). It must bind to
    /// `ObjectScope::Demonstrative` (resolved via `effect_context_object`), not
    /// `Target` (which reads the empty target slot and yields 0). Reverting the
    /// guard makes this bind `Target` — the failing assertion below.
    #[test]
    fn where_x_that_cards_mana_value_binds_demonstrative() {
        assert_eq!(
            parse_where_x_quantity_expression("that card's mana value"),
            Some(QuantityExpr::Ref {
                qty: QuantityRef::ObjectManaValue {
                    scope: ObjectScope::Demonstrative,
                },
            })
        );
        // CR 202.3 synonym: "converted mana cost" routes identically.
        assert_eq!(
            parse_where_x_quantity_expression("that card's converted mana cost"),
            Some(QuantityExpr::Ref {
                qty: QuantityRef::ObjectManaValue {
                    scope: ObjectScope::Demonstrative,
                },
            })
        );
        // Trailing sentence punctuation must resolve identically — the guard
        // matches the trimmed phrase, so the demonstrative binding must be built
        // from the trimmed text, not the raw "that card's mana value." input.
        assert_eq!(
            parse_where_x_quantity_expression("that card's mana value."),
            Some(QuantityExpr::Ref {
                qty: QuantityRef::ObjectManaValue {
                    scope: ObjectScope::Demonstrative,
                },
            })
        );
    }

    /// G2 no-regression — "that spell's mana value" in the SAME where-X path
    /// must stay on its current `EventSource` binding (Draining Whelk / Spell
    /// Swindle class). The "that card's MV" guard matches only the literal
    /// `card` possessive, never `spell`, so `parse_event_context_quantity`
    /// (which would emit `Demonstrative` for "that spell's") is never consulted
    /// for spells.
    #[test]
    fn where_x_that_spells_mana_value_stays_event_source() {
        assert_eq!(
            parse_where_x_quantity_expression("that spell's mana value"),
            Some(QuantityExpr::Ref {
                qty: QuantityRef::ObjectManaValue {
                    scope: ObjectScope::EventSource,
                },
            })
        );
    }

    /// G1 safety proof — "that creature's mana value" (targeted where-X cards
    /// like Feeding Grounds / Living Armor) must stay `Target`. The guard
    /// deliberately excludes `creature`/`permanent`/`planeswalker` because those
    /// are correctly the targeted object in these bindings; flipping them to
    /// Demonstrative would regress those cards to 0.
    #[test]
    fn where_x_that_creatures_mana_value_stays_target() {
        assert_eq!(
            parse_where_x_quantity_expression("that creature's mana value"),
            Some(QuantityExpr::Ref {
                qty: QuantityRef::ObjectManaValue {
                    scope: ObjectScope::Target,
                },
            })
        );
        assert_eq!(
            parse_where_x_quantity_expression("that permanent's mana value"),
            Some(QuantityExpr::Ref {
                qty: QuantityRef::ObjectManaValue {
                    scope: ObjectScope::Target,
                },
            })
        );
    }

    /// Issue #1375 full-card — Twilight Prophet's real Oracle text. BOTH the
    /// upkeep trigger's `LoseLife.amount` and `GainLife.amount` must bind
    /// `ObjectManaValue { scope: Demonstrative }` (was `Target` → 0/0 drain).
    #[test]
    fn twilight_prophet_upkeep_drains_bind_demonstrative_mana_value() {
        use crate::types::ability::Effect;

        let parsed = crate::parser::oracle::parse_oracle_text(
            "Ascend (If you control ten or more permanents, you get the city's blessing for the rest of the game.)\nAt the beginning of your upkeep, if you have the city's blessing, reveal the top card of your library and put it into your hand. Each opponent loses X life and you gain X life, where X is that card's mana value.",
            "Twilight Prophet",
            &["Ascend".to_string()],
            &["Creature".to_string()],
            &["Vampire".to_string(), "Cleric".to_string()],
        );

        // Walk the upkeep trigger's execute chain, collecting every LoseLife /
        // GainLife amount. (test-only tree walk over parsed AbilityDefinitions —
        // not parser dispatch.)
        fn collect_life_amounts(
            def: &crate::types::ability::AbilityDefinition,
            lose: &mut Vec<QuantityExpr>,
            gain: &mut Vec<QuantityExpr>,
        ) {
            match &*def.effect {
                Effect::LoseLife { amount, .. } => lose.push(amount.clone()),
                Effect::GainLife { amount, .. } => gain.push(amount.clone()),
                _ => {}
            }
            if let Some(sub) = def.sub_ability.as_ref() {
                collect_life_amounts(sub, lose, gain);
            }
        }

        let mut lose = Vec::new();
        let mut gain = Vec::new();
        for trigger in &parsed.triggers {
            if let Some(exec) = trigger.execute.as_ref() {
                collect_life_amounts(exec, &mut lose, &mut gain);
            }
        }

        let demonstrative_mv = QuantityExpr::Ref {
            qty: QuantityRef::ObjectManaValue {
                scope: ObjectScope::Demonstrative,
            },
        };
        assert!(
            lose.contains(&demonstrative_mv),
            "each-opponent LoseLife.amount must bind Demonstrative mana value, got {lose:?}"
        );
        assert!(
            gain.contains(&demonstrative_mv),
            "you-gain GainLife.amount must bind Demonstrative mana value, got {gain:?}"
        );
    }
}

#[cfg(test)]
mod token_anaphor_rewrite_tests {
    use super::*;

    /// CR 608.2c + CR 611.2c: Token-anaphor lowering must rebind the outer
    /// `GenericEffect.target` even when the granted static's own `affected`
    /// filter intentionally names a different object set. This is the
    /// quoted-static shape: the token receives the static ability, and that
    /// static ability affects another class of objects.
    #[test]
    fn generic_effect_rewrites_outer_target_without_inner_affected_rewrite() {
        let mut effect = Effect::GenericEffect {
            static_abilities: vec![StaticDefinition::continuous()
                .affected(TargetFilter::Typed(TypedFilter::creature()))
                .modifications(vec![ContinuousModification::GrantStaticAbility {
                    definition: Box::new(
                        StaticDefinition::continuous()
                            .affected(TargetFilter::Typed(TypedFilter::creature()))
                            .modifications(vec![ContinuousModification::AddPower { value: 1 }]),
                    ),
                }])],
            duration: None,
            target: Some(TargetFilter::ParentTarget),
            end_cost: None,
        };

        rewrite_parent_target_to_last_created(&mut effect, false);

        match effect {
            Effect::GenericEffect {
                static_abilities,
                duration,
                target,
                end_cost: _,
            } => {
                assert_eq!(target, Some(TargetFilter::LastCreated));
                assert_eq!(duration, Some(Duration::Permanent));
                assert!(
                    matches!(static_abilities[0].affected, Some(TargetFilter::Typed(_))),
                    "inner affected filter must stay on the granted static's object set"
                );
            }
            other => panic!("expected GenericEffect, got {other:?}"),
        }
    }
}

#[cfg(test)]
mod strip_optional_effect_prefix_tests {
    use super::strip_optional_effect_prefix;

    #[test]
    fn choose_new_targets_is_not_generic_optional() {
        let text = "you may choose new targets for target spell or ability";
        let (is_optional, _, _, rest) = strip_optional_effect_prefix(text);
        assert!(
            !is_optional,
            "retarget clauses must keep the full surface form"
        );
        assert_eq!(rest, text);
    }

    #[test]
    fn generic_you_may_still_strips() {
        let (is_optional, _, _, rest) = strip_optional_effect_prefix("you may draw a card");
        assert!(is_optional);
        assert_eq!(rest, "draw a card");
    }

    #[test]
    fn beseech_style_you_may_cast_still_strips() {
        let (is_optional, _, _, rest) = strip_optional_effect_prefix(
            "you may cast the exiled card without paying its mana cost",
        );
        assert!(is_optional);
        assert_eq!(rest, "cast the exiled card without paying its mana cost");
    }
}

/// DynQty subgroup D — "[once] for each ⟨player-set⟩" lift for fieldless Investigate.
/// Building-block tests for the shared split refactor (byte-identity), the player-set
/// lift helper, and the wrapper-vs-`_ref` non-domination guard.
#[cfg(test)]
mod dq_d_player_set_lift_tests {
    use super::{for_each_repeatable_repeat_for, strip_for_each_repeat_suffix};
    use crate::parser::oracle_nom::quantity::parse_for_each_clause_ref;
    use crate::types::ability::{MultiTargetSpec, PlayerFilter, QuantityExpr, QuantityRef};

    // Matrix #3 — the shared `split_for_each_suffix` refactor is byte-identical:
    // each input yields the SAME `(Option<QuantityExpr>, String)` as pre-refactor.
    // Reverting to a byte-changing split (or admitting `PlayerCount` into the gate)
    // flips one of these assertions.
    #[test]
    fn strip_for_each_repeat_suffix_byte_identity_corpus() {
        // (a) CommanderCast "for each" lift is preserved.
        let (qty, base) = strip_for_each_repeat_suffix(
            "copy it for each time you've cast your commander from the command zone this game",
        );
        assert!(
            matches!(
                qty,
                Some(QuantityExpr::Ref {
                    qty: QuantityRef::CommanderCastFromCommandZoneCount
                })
            ),
            "CommanderCast lift must survive the refactor: {qty:?}"
        );
        assert_eq!(base, "copy it");

        // (b) Thousand-Year Storm's trigger-bound history is lifted by the
        // CopySpell seam and keeps the triggering-spell boundary typed.
        let (qty, base) = strip_for_each_repeat_suffix(
            "copy it for each other instant and sorcery spell you've cast before it this turn",
        );
        assert!(
            matches!(
                qty,
                Some(QuantityExpr::Ref {
                    qty: QuantityRef::SpellsCastBeforeTriggeringSpell { .. }
                })
            ),
            "trigger-bound spell history must be lifted for CopySpell: {qty:?}"
        );
        assert_eq!(base, "copy it");

        // (c) a player-set for-each is REJECTED by this gate (routes through the
        // fieldless-Investigate seam instead) → `(None, <full text>)`.
        let input = "investigate for each opponent who lost life this turn";
        let (qty, base) = strip_for_each_repeat_suffix(input);
        assert!(
            qty.is_none(),
            "PlayerCount must not be lifted here: {qty:?}"
        );
        assert_eq!(base, input);

        // (d) the Zada distinct-copy ObjectCount lift is preserved: strip lifts
        // "other creature you control that the spell could target" to an
        // `ObjectCount{CouldBeTargetedByTriggeringSpell}` and returns the base "copy
        // that spell". Byte-identical to the pre-refactor `_ref + eof` path, and proves
        // the new player-set routing did not disturb the CopySpell/Zada gate.
        let (qty, base) = strip_for_each_repeat_suffix(
            "copy that spell for each other creature you control that the spell could target",
        );
        assert!(
            matches!(
                qty,
                Some(QuantityExpr::Ref {
                    qty: QuantityRef::ObjectCount { .. }
                })
            ),
            "Zada ObjectCount lift must survive the refactor: {qty:?}"
        );
        assert_eq!(base, "copy that spell");

        // (e) no "for each" suffix at all → unchanged passthrough.
        let (qty, base) = strip_for_each_repeat_suffix("draw a card");
        assert!(qty.is_none());
        assert_eq!(base, "draw a card");
    }

    // Matrix #4 — the repeatable member-count lift helper (widened: player-set OR object-set).
    #[test]
    fn for_each_repeatable_repeat_for_lifts_any_repeatable_count() {
        // Teysa: OpponentLostLife → PlayerCount.
        let teysa =
            for_each_repeatable_repeat_for("investigate for each opponent who lost life this turn");
        assert!(
            matches!(
                teysa,
                Some(QuantityExpr::Ref {
                    qty: QuantityRef::PlayerCount {
                        filter: PlayerFilter::OpponentLostLife
                    }
                })
            ),
            "Teysa must lift OpponentLostLife: {teysa:?}"
        );

        // Wojek: PlayerAttribute (comparative hand size). REVERT PROBE: switching the
        // helper body from the `parse_for_each_clause` wrapper to `parse_for_each_clause_ref`
        // makes THIS case return `None` (the `_ref` alt has no PlayerAttribute arm) —
        // that is the wrapper-vs-`_ref` guard.
        let wojek = for_each_repeatable_repeat_for(
            "investigate once for each opponent who has more cards in hand than you",
        );
        assert!(
            matches!(
                wojek,
                Some(QuantityExpr::Ref {
                    qty: QuantityRef::PlayerCount {
                        filter: PlayerFilter::PlayerAttribute { .. }
                    }
                })
            ),
            "Wojek must lift PlayerAttribute via the wrapper: {wojek:?}"
        );

        // Object for-each now DOES lift (parameterized gate-widen). "attacking creature
        // you control" is an already-supported typed filter (needs no Gap A / FilterProp::
        // Goaded), so the widened helper lifts it to `ObjectCount`. REVERT PROBE: narrowing
        // the gate back to `PlayerCount`-only flips this assertion to None.
        let object_lift =
            for_each_repeatable_repeat_for("investigate for each attacking creature you control");
        assert!(
            matches!(
                object_lift,
                Some(QuantityExpr::Ref {
                    qty: QuantityRef::ObjectCount { .. }
                })
            ),
            "object for-each must now lift to ObjectCount: {object_lift:?}"
        );

        // Amount-ref for-each is NOT lifted (fail-closed member-count restriction).
        // Tamiyo Meets the Story Circle's "investigate twice for each card discarded this
        // way" parses the tail to a contextual `FilteredTrackedSetSize`, NOT a member
        // count. Lifting it would silently drop the leading "twice" Fixed multiplier
        // (N Clues instead of 2×N — CR 701.16a). REVERT PROBE: broadening the body back to
        // `parse_for_each_clause(&tail).map(...)` makes this return `Some` and FAILS.
        assert!(
            for_each_repeatable_repeat_for("investigate twice for each card discarded this way")
                .is_none(),
            "a contextual amount-ref (FilteredTrackedSetSize) must NOT be lifted"
        );

        // No "for each" suffix → None.
        assert!(for_each_repeatable_repeat_for("investigate").is_none());
    }

    // Matrix #2 — non-domination: the bare `_ref` combinator does NOT consume Wojek's
    // comparative tail. This is why the helper MUST use the wrapper (which reaches the
    // `oracle_quantity` PlayerAttribute producer). If `_ref` DID consume this to empty,
    // matrix #4/#6's discriminator would be vacuous.
    #[test]
    fn parse_for_each_clause_ref_does_not_dominate_comparative_hand_size() {
        let tail = "opponent who has more cards in hand than you";
        match parse_for_each_clause_ref(tail) {
            Err(_) => {} // rejected outright — non-dominating
            Ok((rest, _)) => assert!(
                !rest.is_empty(),
                "_ref must NOT consume the comparative tail to empty (would make the \
                 wrapper's `rest.is_empty()` gate fire): rest={rest:?}"
            ),
        }
    }

    // ─────────────────────────────────────────────────────────────────────
    // CR 601.2c + CR 115.4 — the "to each of ⟨cardinality⟩ ⟨noun⟩" matrix.
    // ─────────────────────────────────────────────────────────────────────

    fn x_expr() -> QuantityExpr {
        QuantityExpr::Ref {
            qty: QuantityRef::Variable {
                name: "X".to_string(),
            },
        }
    }

    fn fixed(n: i32) -> QuantityExpr {
        QuantityExpr::Fixed { value: n }
    }

    /// Claim 1 — all six cells of the cardinality × noun matrix parse, plus the
    /// X-count and larger-literal leaves.
    ///
    /// CR 601.2c (announced count) × CR 115.4 (bare-plural damage target class)
    /// / CR 115.1a (typed target).
    #[test]
    fn parse_each_of_target_distribution_covers_the_full_cardinality_noun_matrix() {
        use super::{parse_each_of_target_distribution, EachOfTargetNoun};

        // Cell 1 — bounded × bare plural. REGRESSION: Prismari Charm, Storm of Steel.
        // ORDERING GUARD (load-bearing): this must be `fixed(1, 2)` with an EMPTY
        // remainder, NOT `exact(1)` with remainder "or two targets". If the exact
        // arm ever wins here, "one or two targets" silently becomes a one-target
        // spell.
        assert_eq!(
            parse_each_of_target_distribution("one or two targets"),
            Some((
                MultiTargetSpec::fixed(1, 2),
                EachOfTargetNoun::AnyTargets,
                ""
            )),
            "bounded × bare-plural must win over the exact arm's leading \"one\""
        );

        // Cell 2 — bounded × typed (new leaf). The noun is NOT consumed.
        assert_eq!(
            parse_each_of_target_distribution("one or two target creatures"),
            Some((
                MultiTargetSpec::fixed(1, 2),
                EachOfTargetNoun::Typed,
                "target creatures"
            ))
        );

        // Cells 1b / 2b — the SECOND `BOUNDED_TARGET_CARDINALITIES` member.
        // `parse_bounded_target_cardinality` composes its noun off that shared
        // const, so both entries must be exercised or the second can regress (or
        // be shadowed by the exact arm) without a matrix failure. Same ordering
        // guard as cell 1: `fixed(1, 3)`, never `exact(1)` with a stranded
        // remainder.
        assert_eq!(
            parse_each_of_target_distribution("one, two, or three targets"),
            Some((
                MultiTargetSpec::fixed(1, 3),
                EachOfTargetNoun::AnyTargets,
                ""
            )),
            "the three-target bounded stem must win over the exact arm's leading \"one\""
        );
        assert_eq!(
            parse_each_of_target_distribution("one, two, or three target creatures"),
            Some((
                MultiTargetSpec::fixed(1, 3),
                EachOfTargetNoun::Typed,
                "target creatures"
            ))
        );

        // Cell 3 — optional × bare plural (new leaf). NEW: Shower of Coals,
        // Jaya's Immolating Inferno, Myojin of Roaring Blades.
        assert_eq!(
            parse_each_of_target_distribution("up to three targets"),
            Some((
                MultiTargetSpec::up_to(fixed(3)),
                EachOfTargetNoun::AnyTargets,
                ""
            ))
        );

        // Cell 4 — optional × typed. REGRESSION: Dual Shot, Wrap in Flames.
        assert_eq!(
            parse_each_of_target_distribution("up to two target creatures"),
            Some((
                MultiTargetSpec::up_to(fixed(2)),
                EachOfTargetNoun::Typed,
                "target creatures"
            ))
        );

        // Cell 5 — exact × bare plural (new leaf). NEW: Furious Reprisal,
        // Pinnacle of Rage.
        assert_eq!(
            parse_each_of_target_distribution("two targets"),
            Some((
                MultiTargetSpec::exact(fixed(2)),
                EachOfTargetNoun::AnyTargets,
                ""
            ))
        );

        // Cell 5b — exact × bare plural, X count. NEW: Firestorm, Meteor Blast.
        assert_eq!(
            parse_each_of_target_distribution("x targets"),
            Some((
                MultiTargetSpec::exact(x_expr()),
                EachOfTargetNoun::AnyTargets,
                ""
            ))
        );

        // Cell 6 — exact × typed (new leaf). NEW: Jagged Lightning, Swelter,
        // Twinstrike.
        assert_eq!(
            parse_each_of_target_distribution("two target creatures"),
            Some((
                MultiTargetSpec::exact(fixed(2)),
                EachOfTargetNoun::Typed,
                "target creatures"
            ))
        );

        // Cell 6b — optional × bare plural, X count. Batroc the Leaper.
        assert_eq!(
            parse_each_of_target_distribution("up to x targets"),
            Some((
                MultiTargetSpec::up_to(x_expr()),
                EachOfTargetNoun::AnyTargets,
                ""
            ))
        );

        // Cell 6c — Chandra, the Firebrand's [−6].
        assert_eq!(
            parse_each_of_target_distribution("up to six targets"),
            Some((
                MultiTargetSpec::up_to(fixed(6)),
                EachOfTargetNoun::AnyTargets,
                ""
            ))
        );

        // Cell 6d — Fall of the Titans, Chandra Hope's Beacon.
        assert_eq!(
            parse_each_of_target_distribution("up to two targets"),
            Some((
                MultiTargetSpec::up_to(fixed(2)),
                EachOfTargetNoun::AnyTargets,
                ""
            ))
        );

        // Grandfathered lexical `other` on the TYPED arm only (CR 115.3
        // distinctness is applied downstream by `parse_target_with_ctx`, not by
        // the cardinality head).
        assert_eq!(
            parse_each_of_target_distribution("up to two other target creatures"),
            Some((
                MultiTargetSpec::up_to(fixed(2)),
                EachOfTargetNoun::Typed,
                "other target creatures"
            ))
        );

        // FENCE GUARD — `targets` must not match inside a longer word. This pins
        // `not(satisfy(char::is_alphanumeric))`; dropping the fence makes this
        // return `Some(exact(2), AnyTargets, "omething")`.
        assert_eq!(
            parse_each_of_target_distribution("two targetsomething"),
            None,
            "the bare-plural arm must be fenced at a word boundary"
        );

        // Original casing is preserved in the remainder handed to
        // `parse_target_with_ctx`.
        assert_eq!(
            parse_each_of_target_distribution("two target Goblins"),
            Some((
                MultiTargetSpec::exact(fixed(2)),
                EachOfTargetNoun::Typed,
                "target Goblins"
            ))
        );
    }

    /// Claim 2 — hostile negatives, each paired with a positive reach-guard in
    /// the SAME test so no `None` assertion is vacuous.
    #[test]
    fn parse_each_of_target_distribution_rejects_non_cardinality_heads() {
        use super::{parse_each_of_target_distribution, EachOfTargetNoun};

        // REACH-GUARD: the combinator is live in this test.
        assert_eq!(
            parse_each_of_target_distribution("two targets"),
            Some((
                MultiTargetSpec::exact(fixed(2)),
                EachOfTargetNoun::AnyTargets,
                ""
            )),
            "reach-guard: the positive path must still parse, so the None \
             assertions below are not vacuous"
        );

        // "~ deals N damage to each of your opponents" — must fall through to
        // `parse_damage_each_player_scope` (DamageEachPlayer), not this seam.
        assert_eq!(
            parse_each_of_target_distribution("your opponents"),
            None,
            "player-scope heads belong to parse_damage_each_player_scope"
        );

        // No "of": "each creature" never reaches this combinator, but the head
        // itself must not parse either.
        assert_eq!(parse_each_of_target_distribution("each creature"), None);

        // VERBATIM Shower of Coals Threshold anaphor. It must NOT parse — the
        // second sentence stays dropped (an anaphor to the already-chosen
        // targets), which is what keeps that card an honest PARTIAL fix.
        assert_eq!(
            parse_each_of_target_distribution("those permanents and/or players instead"),
            None,
            "the Threshold anaphor must not be mistaken for a cardinality head"
        );

        assert_eq!(parse_each_of_target_distribution("them"), None);

        // A bare count with no noun at all.
        assert_eq!(parse_each_of_target_distribution("two"), None);

        // ASYMMETRY (deliberate, matches pre-existing behaviour): the
        // bare-plural arm does NOT accept "other targets". Drakuseth's
        // "up to two other targets" clause is dropped upstream by the
        // compound-damage splitter, so this returns None exactly as before.
        assert_eq!(
            parse_each_of_target_distribution("up to two other targets"),
            None,
            "bare-plural `other targets` is intentionally NOT accepted"
        );
    }
}
