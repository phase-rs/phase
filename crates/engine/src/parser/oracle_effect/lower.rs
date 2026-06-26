use nom::branch::alt;
use nom::bytes::complete::{tag, take_till1, take_until};
use nom::character::complete::{multispace0, multispace1};
use nom::combinator::{all_consuming, eof, map, not, opt, peek, rest, value, verify};
use nom::sequence::{preceded, terminated};
use nom::Parser;

use super::super::oracle_nom::bridge::{nom_on_lower, nom_parse_lower, split_once_on_lower};
use super::super::oracle_nom::duration::{parse_duration, parse_for_as_long_as_condition};
use super::super::oracle_nom::error::{OracleError, OracleResult};
use super::super::oracle_nom::primitives as nom_primitives;
use super::super::oracle_nom::quantity as nom_quantity;
use super::super::oracle_quantity::{
    parse_cda_quantity, parse_cda_quantity_with_context, parse_for_each_clause,
    parse_for_each_clause_expr, parse_for_each_clause_expr_with_context, parse_quantity_ref,
};
use super::super::oracle_target::{
    parse_target, parse_target_with_ctx, parse_that_clause_suffix, parse_type_phrase_with_ctx,
};
use super::super::oracle_util::{parse_comparator_prefix, parse_count_expr, strip_after, TextPair};
use crate::parser::oracle_ir::ast::*;
use crate::parser::oracle_ir::context::ParseContext;
use crate::parser::oracle_ir::diagnostic::OracleDiagnostic;
use crate::parser::oracle_ir::effect_chain::{ClauseIr, EffectChainIr, SpecialClause};
use crate::types::ability::{
    AbilityCondition, AbilityCost, AbilityDefinition, AbilityKind, AttackScope, AttackSubject,
    CastFromZoneDriver, CastingPermission, Comparator, ContinuousModification, ControllerRef,
    DamageSource, DelayedTriggerCondition, Duration, Effect, EffectScope, FilterProp,
    GameRestriction, LibraryPosition, ManaSpendPermission, MultiTargetSpec, ObjectScope,
    PlayerFilter, PreventionAmount, PreventionScope, PtValue, QuantityExpr, QuantityRef,
    RestrictionPlayerScope, RoundingMode, StaticCondition, StaticDefinition, SubAbilityLink,
    TargetChoiceTiming, TargetFilter, TypeFilter, TypedFilter,
};
use crate::types::counter::CounterType;
use crate::types::game_state::{DistributionUnit, TargetSelectionConstraint};
use crate::types::mana::ManaCost;
use crate::types::phase::Phase;
use crate::types::statics::StaticMode;
use crate::types::zones::{EtbTapState, Zone};

// Parse-phase functions from the parent module (oracle_effect/mod.rs).
// These are private to oracle_effect but accessible here as a descendant module.
use super::conditions::ability_condition_to_static_condition;
use super::sequence::apply_clause_continuation;
use super::subject;
use super::{
    append_to_deepest_sub_ability, apply_player_scope_rewrites,
    attach_alt_cost_to_prior_cast_from_zone, attach_mana_retention_to_prior_mana,
    attach_repeat_process_keywords, attach_same_is_true_keywords,
    collapse_ephemeral_color_choice_mana, contains_explicit_tracked_set_pronoun,
    contains_implicit_tracked_set_pronoun, each_target_filter_mut, fold_cast_copy_of_card_defs,
    has_explicit_player_target, mark_uses_tracked_set, parse_effect_clause,
    parse_event_context_ref_with_ctx, parse_for_each_object_copy_parts,
    publishes_tracked_set_from_resolution, refine_damage_target_remainder,
    replace_player_anaphor_with_parent_target, retarget_counter_additional_cost_to_target,
    rewrite_parent_targets_to_tracked_set, rewrite_rounding_mode, rewrite_that_type_mana_instead,
    scan_contains_phrase, stamp_delayed_returns, target_filter_controller_ref,
    try_fold_token_repeat_into_count, wire_optional_cast_decline_fallback,
};

fn rewrite_player_anaphor_targets_in_definition(def: &mut AbilityDefinition) {
    replace_player_anaphor_with_parent_target(def.effect.as_mut());
    if let Some(sub) = def.sub_ability.as_deref_mut() {
        rewrite_player_anaphor_targets_in_definition(sub);
    }
    if let Some(else_ability) = def.else_ability.as_deref_mut() {
        rewrite_player_anaphor_targets_in_definition(else_ability);
    }
}

/// CR 608.2c: True when an ability's primary effect acts on the ability's own
/// source permanent (`TargetFilter::SelfRef`). Self-targeting "If <self status>,
/// A on it. Otherwise, B it." abilities (Repeat Offender) lower the "if" body's
/// "it" to `SelfRef`, which the runtime resolves to `source_id`; the "otherwise"
/// body's "it" is the SAME anaphor (the source), so — applying the rules of
/// English to the whole text (CR 608.2c) — it must resolve the same way.
fn definition_targets_self_source(def: &AbilityDefinition) -> bool {
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
fn is_stable_branch_amount(expr: &QuantityExpr) -> bool {
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
fn rewrite_else_event_context_to_stable(def: &mut AbilityDefinition, stable: &QuantityExpr) {
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
fn rewrite_else_parent_target_to_self_ref(def: &mut AbilityDefinition) {
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

fn is_spend_mana_as_any_color_rider(clause: &ClauseIr) -> bool {
    let Effect::GenericEffect {
        static_abilities, ..
    } = &clause.parsed.effect
    else {
        return false;
    };
    if static_abilities.len() != 1
        || static_abilities[0].mode != (StaticMode::SpendManaAsAnyColor { spell_filter: None })
    {
        return false;
    }

    let lower = clause.source_text.to_ascii_lowercase();
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

fn attach_any_color_mana_rider_to_previous_play_from_exile(defs: &mut [AbilityDefinition]) -> bool {
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

/// CR 601.2f: Detect the "each spell cast this way costs {N} more to cast"
/// rider sentence (Lightstall Inquisitor) and return the cost increase. This is
/// a cost-raise scoped to spells cast via the immediately-preceding
/// `PlayFromExile` grant ("this way" = the just-granted exile play), not a
/// global static cost increase — so it folds into the grant's `cast_cost_raise`
/// rather than emitting a standalone `StaticMode::ModifyCost`. Generic over the
/// printed increase (`{1}`, `{2}`, …); the mana symbols are case-insensitive
/// digits in the common generic case.
fn cast_cost_raise_rider(clause: &ClauseIr) -> Option<ManaCost> {
    let lower = clause.source_text.to_ascii_lowercase();
    nom_on_lower(clause.source_text.trim(), lower.trim(), |i| {
        let (i, _) = tag("each spell cast this way costs ").parse(i)?;
        let (i, cost) = nom_primitives::parse_mana_cost(i)?;
        let (i, _) = tag(" more to cast").parse(i)?;
        let (i, _) = opt(tag(".")).parse(i)?;
        eof(i)?;
        Ok((i, cost))
    })
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
fn is_land_enters_tapped_rider(clause: &ClauseIr) -> bool {
    let lower = clause.source_text.to_ascii_lowercase();
    let trimmed = lower.trim().trim_end_matches('.').trim();
    parses_land_enters_tapped_rider(trimmed)
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
fn attach_cast_cost_raise_to_previous_play_from_exile(
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
fn attach_land_enters_tapped_to_previous_play_from_exile(defs: &mut [AbilityDefinition]) -> bool {
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

pub(crate) fn lower_effect_chain_ir(ir: &EffectChainIr) -> AbilityDefinition {
    let kind = ir.kind;

    // ── Phase 1: ClauseIr → AbilityDefinition ──────────────────────────
    let mut defs: Vec<AbilityDefinition> = Vec::new();
    // CR 608.2c: Boundary that followed the previous normal-path clause. Used to
    // stamp each clause's `sub_link` — a `Sentence` boundary before this clause
    // makes it a `SequentialSibling` (independent following instruction); a
    // `Comma`/`Then`/no boundary makes it a within-clause `ContinuationStep`.
    let mut prev_boundary: Option<ClauseBoundary> = None;
    for clause_ir in &ir.clauses {
        // CR 608.2c: Handle absorbed clauses and special (rider) clauses that
        // modify previous defs rather than emitting new sibling defs. Each path
        // evaluates to `true`; the boundary advance below then runs uniformly so
        // a following normal clause stamps `sub_link` from the correct boundary.
        let handled_as_special: bool = {
            if clause_ir.absorbed_by_followup {
                // Apply the followup continuation to the defs built so far.
                if let Some(ref continuation) = clause_ir.followup_continuation {
                    apply_clause_continuation(&mut defs, continuation.clone(), kind);
                    apply_where_x_to_latest_def(&mut defs, clause_ir.where_x_expression.as_deref());
                }
                true
            } else if let Some(ref special) = clause_ir.special {
                match special {
                    SpecialClause::AltCostRider(cost) => {
                        attach_alt_cost_to_prior_cast_from_zone(&mut defs, cost.clone());
                        true
                    }
                    SpecialClause::Otherwise(else_def) => {
                        // Walk defs backward to find the most recent conditional
                        let mut attached = false;
                        for d in defs.iter_mut().rev() {
                            if d.condition.is_some() {
                                let mut else_def = else_def.clone();
                                // CR 608.2c: when the gated clause acts on the
                                // source (`SelfRef`), the else clause's "it" anaphor
                                // is the same source — rebind its `ParentTarget`
                                // default to `SelfRef` so a self-targeting ability's
                                // else branch is not a no-op against an empty target
                                // list (Repeat Offender's "Otherwise, suspect it").
                                if definition_targets_self_source(d) {
                                    rewrite_else_parent_target_to_self_ref(&mut else_def);
                                }
                                // CR 608.2c: bind the else branch's "that much"
                                // anaphor (`EventContextAmount`) to the if branch's
                                // stable magnitude. The if branch's amount is the
                                // same printed quantity "that much" refers to; on
                                // the else branch the antecedent instruction was
                                // skipped, so the per-instruction `EventContextAmount`
                                // channel reads 0 (Caustic Bronco: "each opponent
                                // loses that much life"). Only a stable antecedent
                                // amount (object-/fixed-bound, not itself
                                // `EventContextAmount`) is propagated.
                                if let Some(stable) = d
                                    .effect
                                    .count_expr()
                                    .filter(|e| is_stable_branch_amount(e))
                                    .cloned()
                                {
                                    rewrite_else_event_context_to_stable(&mut else_def, &stable);
                                }
                                d.else_ability = Some(else_def);
                                attached = true;
                                break;
                            }
                        }
                        // CR 608.2d + CR 101.4: standalone "If no one does, X" on
                        // an "any opponent/player may" head (Browbeat, Book
                        // Burning). The head has no `condition` — it is made
                        // conditional by `optional_for`. The reward is the
                        // no-one-accepted branch. Synthesize a
                        // `Not(OptionalEffectPerformed)`-gated sub carrying the
                        // reward: on accept the head's chain skips it (signal
                        // true → negated false); on all-decline the runtime
                        // decline path fires it (see `handle_opponent_may_choice`).
                        if !attached {
                            for d in defs.iter_mut().rev() {
                                if d.optional_for.is_some() && d.sub_ability.is_none() {
                                    let mut reward = (**else_def).clone();
                                    reward.condition = Some(AbilityCondition::Not {
                                        condition: Box::new(AbilityCondition::effect_performed()),
                                    });
                                    d.sub_ability = Some(Box::new(reward));
                                    break;
                                }
                            }
                        }
                        true
                    }
                    SpecialClause::OtherwiseFallback(else_def) => {
                        defs.push(AbilityDefinition::new(
                            kind,
                            Effect::Unimplemented {
                                name: "otherwise".to_string(),
                                description: Some("Otherwise".to_string()),
                            },
                        ));
                        defs.push(*else_def.clone());
                        true
                    }
                    SpecialClause::DieExileRider(rider_def) => {
                        if let Some(last_def) = defs.last_mut() {
                            // CR 614.1a + CR 608.2c: Append the rider as the
                            // tail of the existing sub_ability chain instead of
                            // overwriting it. Multi-target damage spells
                            // (Serpentine Spike) populate the sub_ability chain
                            // with continuation damage events; the rider must
                            // attach AFTER the chain so all continuation events
                            // resolve before the replacement attaches.
                            append_to_deepest_sub_ability(last_def, Some(rider_def.clone()));
                        }
                        true
                    }
                    SpecialClause::CantBeRegeneratedRider(rider_def) => {
                        // CR 608.2c + CR 701.19c: Attach the "<noun> dealt damage
                        // this way can't be regenerated" rider as the tail of the
                        // preceding damage clause's sub_ability chain. The rider's
                        // `GenericEffect{target: TrackedSet}` then trips
                        // `next_sub_needs_tracked_set` on that damage clause, so it
                        // publishes the damaged object ids the rider's
                        // CantBeRegenerated static binds to.
                        if let Some(last_def) = defs.last_mut() {
                            append_to_deepest_sub_ability(last_def, Some(rider_def.clone()));
                        }
                        true
                    }
                    SpecialClause::DigInsteadAlt(alt_def) => {
                        if let Some(last_def) = defs.pop() {
                            let mut new_def = *alt_def.clone();
                            apply_where_x_ability_expression(
                                &mut new_def,
                                clause_ir.where_x_expression.as_deref(),
                            );
                            new_def.else_ability = Some(Box::new(last_def));
                            defs.push(new_def);
                        }
                        true
                    }
                    SpecialClause::InsteadClause(instead_def) => {
                        // CR 614.1a + CR 608.2c: assemble a multi-clause base + an
                        // "instead" override so the runtime can produce both
                        // branches. Clause 1 becomes the root and is the Cow-swap
                        // target — when the override's `ConditionInstead` fires,
                        // `effects/mod.rs` swaps the root's effect with the
                        // override's at parent resolution, and the override branch
                        // returns terminally (see the `ConditionInstead` arm at
                        // ~line 2713 in `effects/mod.rs`). To make the tail clauses
                        // (2..N) conditional on the override NOT firing, we stash
                        // them in the override's `else_ability`: the runtime only
                        // walks `else_ability` when the swap did not happen. Net:
                        // condition true → only the override's effect runs (clause
                        // 1 swapped away, tail bypassed); condition false → clause
                        // 1 runs as printed, then the tail runs from
                        // `else_ability`. Single-clause bases collapse to the
                        // prior shape (empty tail → no `else_ability`).
                        if !defs.is_empty() {
                            let mut chain_defs = std::mem::take(&mut defs);
                            let mut root = chain_defs.remove(0);
                            for next in chain_defs {
                                append_to_deepest_sub_ability(&mut root, Some(Box::new(next)));
                            }
                            let mut instead = *instead_def.clone();
                            // CR 702.33d + CR 707.10: Resolve "create N of those
                            // tokens" anaphor against the root (the antecedent
                            // for a multi-clause base is the first printed clause).
                            rewrite_those_tokens_from_antecedent(&mut instead.effect, &root.effect);
                            if rewrite_counter_instead_target_from_antecedent(
                                &mut instead.effect,
                                &root.effect,
                            ) {
                                instead.target_choice_timing = root.target_choice_timing;
                            }
                            if has_explicit_player_target(root.effect.as_ref()) {
                                rewrite_player_anaphor_targets_in_definition(&mut instead);
                            }
                            instead.else_ability = root.sub_ability.take();
                            root.sub_ability = Some(Box::new(instead));
                            defs.push(root);
                        }
                        true
                    }
                    SpecialClause::EntersTappedAttacking => {
                        // CR 508.4 / CR 614.1: Conditional enters-tapped-attacking modifier
                        if let Some(prev) = defs.last() {
                            let can_patch = matches!(
                                &*prev.effect,
                                Effect::CopyTokenOf { .. }
                                    | Effect::Token { .. }
                                    | Effect::ChangeZone { .. }
                            );
                            if can_patch {
                                let mut patched = defs.pop().unwrap();
                                match &mut *patched.effect {
                                    Effect::CopyTokenOf {
                                        enters_attacking,
                                        tapped,
                                        ..
                                    } => {
                                        *enters_attacking = true;
                                        *tapped = true;
                                    }
                                    Effect::Token {
                                        enters_attacking,
                                        tapped,
                                        ..
                                    } => {
                                        *enters_attacking = true;
                                        *tapped = true;
                                    }
                                    Effect::ChangeZone {
                                        enters_attacking,
                                        enter_tapped,
                                        ..
                                    } => {
                                        *enters_attacking = true;
                                        *enter_tapped = crate::types::zones::EtbTapState::Tapped;
                                    }
                                    _ => {}
                                }
                                let original = {
                                    let mut orig = patched.clone();
                                    match &mut *orig.effect {
                                        Effect::CopyTokenOf {
                                            enters_attacking,
                                            tapped,
                                            ..
                                        } => {
                                            *enters_attacking = false;
                                            *tapped = false;
                                        }
                                        Effect::Token {
                                            enters_attacking,
                                            tapped,
                                            ..
                                        } => {
                                            *enters_attacking = false;
                                            *tapped = false;
                                        }
                                        Effect::ChangeZone {
                                            enters_attacking,
                                            enter_tapped,
                                            ..
                                        } => {
                                            *enters_attacking = false;
                                            *enter_tapped =
                                                crate::types::zones::EtbTapState::Unspecified;
                                        }
                                        _ => {}
                                    }
                                    orig
                                };
                                patched.condition = clause_ir.condition.clone();
                                patched.else_ability = Some(Box::new(original));
                                defs.push(patched);
                            }
                        }
                        true
                    }
                    SpecialClause::KeywordInsteadOverride => {
                        // Build the def for this clause and attach to previous as sub_ability
                        let mut def = AbilityDefinition::new(kind, clause_ir.parsed.effect.clone());
                        let effective_cond = clause_ir
                            .condition
                            .as_ref()
                            .or(clause_ir.parsed.condition.as_ref());
                        if let Some(cond) = effective_cond {
                            def = def.condition(cond.clone());
                        }
                        if let Some(prev) = defs.last_mut() {
                            prev.sub_ability = Some(Box::new(def));
                        }
                        true
                    }
                    SpecialClause::AdditionalCostInsteadSearch => {
                        // Build this clause's def and fold else_ability from the trailing clause.
                        // The trailing ChangeZone was produced by the previous SearchLibrary's
                        // intrinsic continuation (SearchDestination).
                        let mut def = AbilityDefinition::new(kind, clause_ir.parsed.effect.clone());
                        let effective_cond = clause_ir
                            .condition
                            .as_ref()
                            .or(clause_ir.parsed.condition.as_ref());
                        if let Some(cond) = effective_cond {
                            def = def.condition(cond.clone());
                        }
                        // Pop trailing search-destination ChangeZone and attach as else_ability
                        if defs.len() >= 2 {
                            let trailing_is_search_destination = matches!(
                                &*defs.last().unwrap().effect,
                                Effect::ChangeZone {
                                    origin: Some(Zone::Library),
                                    destination: Zone::Hand,
                                    ..
                                }
                            );
                            if trailing_is_search_destination {
                                def.else_ability = Some(Box::new(defs.pop().unwrap()));
                            }
                        }
                        defs.push(def);
                        // Apply intrinsic continuation for THIS SearchLibrary (e.g., reveal flag, ChangeZone).
                        if let Some(ref continuation) = clause_ir.intrinsic_continuation {
                            apply_clause_continuation(&mut defs, continuation.clone(), kind);
                        }
                        true
                    }
                    SpecialClause::DrawnThisTurnPayOrTopdeck { life_payment } => {
                        if let Some(last_def) = defs.last_mut() {
                            if let Effect::ChooseDrawnThisTurnPayOrTopdeck {
                                life_payment: current,
                                ..
                            } = &mut *last_def.effect
                            {
                                *current = life_payment.clone();
                            }
                        }
                        true
                    }
                    SpecialClause::ManaRetention(expiry) => {
                        attach_mana_retention_to_prior_mana(&mut defs, *expiry);
                        true
                    }
                    SpecialClause::SameIsTrueFor(keywords) => {
                        attach_same_is_true_keywords(&mut defs, keywords);
                        true
                    }
                    SpecialClause::RepeatProcessForKeywords(keywords) => {
                        attach_repeat_process_keywords(&mut defs, keywords);
                        true
                    }
                }
            } else {
                false
            }
        };

        // CR 608.2c: A special/absorbed clause emits no sibling def, but it
        // still occupies a slot in the chunk sequence and carries its own
        // trailing `boundary` (`ClauseIr.boundary` is populated from
        // `ClauseChunk.boundary_after`). Advance `prev_boundary` so a following
        // normal clause stamps its `sub_link` from the boundary AFTER this
        // clause, not the stale boundary that preceded it.
        if handled_as_special {
            prev_boundary = clause_ir.boundary;
            continue;
        }

        // CR 609.4b + CR 608.2c: Brainstealer/Daxos-class any-color mana
        // riders may be split into their own sentence or comma sibling after a
        // `PlayFromExile` grant. They scope the existing exile-play
        // permission, so fold the rider into the prior grant instead of
        // emitting a broad standalone `SpendManaAsAnyColor` effect.
        if is_spend_mana_as_any_color_rider(clause_ir)
            && attach_any_color_mana_rider_to_previous_play_from_exile(&mut defs)
        {
            prev_boundary = clause_ir.boundary;
            continue;
        }

        // CR 601.2f + CR 614.1c: Lightstall Inquisitor's "Each spell cast this
        // way costs {1} more to cast." / "Each land played this way enters
        // tapped." rider sentences scope to the preceding `PlayFromExile`
        // grant. Fold each into the grant (`cast_cost_raise` /
        // `land_enter_tapped`) instead of emitting a standalone cost-modify
        // static or board-wide ETB-tapped replacement — "this way" binds them
        // to the exile-play permission, not to all spells/lands.
        if let Some(cost) = cast_cost_raise_rider(clause_ir) {
            if attach_cast_cost_raise_to_previous_play_from_exile(&mut defs, cost) {
                prev_boundary = clause_ir.boundary;
                continue;
            }
        }
        if is_land_enters_tapped_rider(clause_ir)
            && attach_land_enters_tapped_to_previous_play_from_exile(&mut defs)
        {
            prev_boundary = clause_ir.boundary;
            continue;
        }

        // Non-absorbed, non-special followup continuation — apply it to the
        // previous defs before building this clause's def.
        if let Some(ref continuation) = clause_ir.followup_continuation {
            apply_clause_continuation(&mut defs, continuation.clone(), kind);
            apply_where_x_to_latest_def(&mut defs, clause_ir.where_x_expression.as_deref());
        }

        // ── Build AbilityDefinition from ClauseIr ──
        let is_target_only = matches!(clause_ir.parsed.effect, Effect::TargetOnly { .. });
        let mut def = AbilityDefinition::new(kind, clause_ir.parsed.effect.clone());
        // CR 608.2c: This clause's link to its parent = the boundary that
        // SEPARATED the previous clause from this one. A `Sentence` boundary
        // marks a `SequentialSibling` (next printed instruction, resolves even
        // when an optional parent is declined); `Comma`/`Then`/none marks a
        // within-clause `ContinuationStep` (part of the parent's action).
        def.sub_link = match prev_boundary {
            Some(ClauseBoundary::Sentence) => SubAbilityLink::SequentialSibling,
            Some(ClauseBoundary::Then) | Some(ClauseBoundary::Comma) | None => {
                SubAbilityLink::ContinuationStep
            }
        };
        def.target_choice_timing = target_choice_timing_for_clause(clause_ir);
        // CR 115.1 + CR 701.9b: copy the per-clause selection mode captured by
        // `parse_target_with_ctx` during chunk parse. `Random` flips the engine
        // off the controller-choice path at target-selection time.
        def.target_selection_mode = clause_ir.target_selection_mode;
        // CR 601.2c + CR 603.3d: copy the per-clause target chooser captured by
        // `parse_target_with_ctx` during chunk parse, so a targeted "of their
        // choice" routes target selection to the scoped (upkeep) player.
        def.target_chooser = clause_ir.target_chooser.clone();
        let clause_sub = if is_target_only {
            def.sub_ability = clause_ir.parsed.sub_ability.clone();
            None
        } else {
            clause_ir.parsed.sub_ability.clone()
        };

        // CR 118.9 + CR 608.2g: A *standing-duration* `CastFromZone` lingering
        // grant (no DuringResolution driver, no alternative cost, no
        // eligibility constraint, and an explicit duration — Discover/Nashi/
        // Urza-class "until end of turn, you may play that card without
        // paying its mana cost") is unconditional, like
        // `GrantCastingPermission`: the "may" describes the later cast
        // decision, not the grant itself. Gating the grant behind an
        // immediate accept/decline drops the permission entirely when
        // declined (issue #720 follow-up: Urza, Lord High Artificer).
        // `constraint`/`alt_ability_cost` are EXCLUDED from this carve-out:
        // Beseech the Mirror's "...if that spell's mana value is 4 or less"
        // (constraint) and Infamous Cruelclaw's "...by discarding a card
        // rather than paying its mana cost" (alt_ability_cost) both need the
        // immediate accept/decline because declining branches into a
        // fallback action (hand fallback) or an alternative payment the
        // engine must resolve right now, not later. A missing `duration` is
        // ALSO excluded: Memory Plunder's "you may cast target instant or
        // sorcery card... without paying its mana cost" carries no standing
        // duration at all, so its "may" is the immediate resolution-time
        // decision the existing `OptionalEffectChoice` prompt correctly
        // drives (issue #2884) — only an explicit duration marks the grant
        // as deferred to a later priority window.
        let is_lingering_cast_from_zone = matches!(
            &clause_ir.parsed.effect,
            Effect::CastFromZone {
                driver: CastFromZoneDriver::LingeringPermission,
                constraint: None,
                alt_ability_cost: None,
                duration: Some(_),
                ..
            }
        );
        // CR 107.1b/c + CR 117.1d: Join Forces' "each player may pay any
        // amount of mana" is NOT an OptionalEffectChoice — the "may" only
        // means each player may pay zero. PayAmountChoice (min=0) handles
        // that; flagging the PayCost as optional would let a decline skip the
        // mill/draw body.
        let is_join_forces_pay_any_amount_mana_cost = clause_ir.player_scope
            == Some(PlayerFilter::All)
            && clause_ir.starting_with == Some(ControllerRef::You)
            && matches!(
                &clause_ir.parsed.effect,
                Effect::PayCost {
                    cost: AbilityCost::Mana { cost },
                    scale: None,
                    ..
                } if crate::game::casting_costs::cost_has_x(cost)
            );
        let is_pay_to_end_effect_termination =
            crate::parser::clause_shell::is_you_may_pay_to_end_effect_phrase(
                &clause_ir.source_text.to_ascii_lowercase(),
            );
        if clause_ir.is_optional
            && !matches!(&clause_ir.parsed.effect, Effect::SearchOutsideGame { .. })
            && !matches!(
                &clause_ir.parsed.effect,
                Effect::GrantCastingPermission { .. }
            )
            && !is_lingering_cast_from_zone
            && !is_join_forces_pay_any_amount_mana_cost
            && !is_pay_to_end_effect_termination
        {
            def.optional = true;
            def.optional_for = clause_ir.opponent_may_scope;
        }
        // CR 117.3a + CR 608.2c: Propagate subject-phrase "may" modal.
        if clause_ir.parsed.optional
            && !matches!(
                &clause_ir.parsed.effect,
                Effect::GrantCastingPermission { .. }
            )
            && !is_lingering_cast_from_zone
            && !is_join_forces_pay_any_amount_mana_cost
            && !is_pay_to_end_effect_termination
        {
            def.optional = true;
        }
        if matches!(&clause_ir.parsed.effect, Effect::SearchOutsideGame { .. }) {
            def.optional = false;
            def.optional_for = None;
        }
        if let Some(ref qty) = clause_ir.repeat_for {
            if matches!(*def.effect, Effect::TargetOnly { .. }) {
                if let Some(sub) = def.sub_ability.as_mut() {
                    sub.repeat_for = Some(qty.clone());
                } else {
                    def.repeat_for = Some(qty.clone());
                }
            } else if ir.clauses.len() == 1
                && clause_ir.parsed.sub_ability.is_none()
                && try_fold_token_repeat_into_count(def.effect.as_mut(), qty)
            {
                // CR 111.1 + CR 616.1: bare "for each X, create a token" folded
                // into one batched CreateToken event — no loop. Conservatively
                // restricted to a single-clause ability: a trailing sibling may
                // reference the created tokens (a tracked set or "those tokens"
                // anaphor — e.g. Ezuri's Predation's fight pairing depends on the
                // per-iteration creation), and we do not yet distinguish such
                // token-referencing siblings from independent ones (e.g. Moogles'
                // Valor's "creatures you control gain indestructible"), so we keep
                // the loop for all multi-clause cases. The chained-body guard
                // reads `clause_ir.parsed.sub_ability` (the non-TargetOnly path
                // attaches it after this point via `clause_sub`, so
                // `def.sub_ability` is not yet populated here).
            } else {
                def.repeat_for = Some(qty.clone());
            }
        }
        if let Some(scope) = clause_ir.player_scope.clone() {
            def.player_scope = Some(scope);
        }
        // CR 101.4 + CR 800.4: Stamp the turn-order override from the chunk's
        // "Starting with you, " prefix (Join Forces). The iteration site reads
        // this via `players::apnap_order_from(state, starting_with, controller)`
        // so the controller is prompted first regardless of the active player.
        if let Some(ref who) = clause_ir.starting_with {
            def.starting_with = Some(who.clone());
        }
        if let Some(ref duration) = clause_ir.parsed.duration {
            def = def.duration(duration.clone());
        }
        // CR 608.2c: Apply condition — chain-level takes priority over clause-level.
        let effective_condition = clause_ir
            .condition
            .as_ref()
            .or(clause_ir.parsed.condition.as_ref());
        // CR 608.2c + CR 109.2: When a "if it's a [type], it ..." card-type gate
        // sits on a clause whose effect acts on the parent target (the anaphoric
        // "it" resolved to the previously-targeted object — e.g. Azure Beastbinder's
        // "If it's a creature, it also has base power and toughness 2/2"), the
        // type description refers to that *permanent*, not a revealed card. The
        // chunk parser emits `RevealedHasCardType` for every "if it's a [type]"
        // head, but that variant evaluates against the last revealed/zone-changed
        // card and would be ALWAYS-FALSE here (no reveal context), silently
        // dropping the rider. Convert it to `TargetMatchesFilter` (the same
        // conversion the Disintegrate/Carbonize damage-rider path performs via
        // `card_type_condition_as_target_match`) so the gate evaluates against the
        // bound parent target.
        //
        // Two guards keep genuine reveal-context gates (Goblin Guide:
        // "defending player reveals the top card of their library. If it's a
        // land card, that player puts it into their hand."; Delver-class)
        // untouched:
        //   1. The gated effect must target `ParentTarget` (the "it" anaphor).
        //   2. No prior clause in the chain may publish a revealed/zone-changed
        //      subject — that is exactly the source `RevealedHasCardType` reads
        //      at resolution (`last_revealed_ids` / `last_zone_changed_ids`), so
        //      when such a publisher exists the "it" really is the revealed card
        //      and the original variant is correct.
        let chain_has_revealed_subject = defs
            .iter()
            .any(|d| effect_publishes_revealed_subject(&d.effect));
        let converted_condition = effective_condition.and_then(|cond| {
            (!chain_has_revealed_subject
                && matches!(def.effect.target_filter(), Some(TargetFilter::ParentTarget)))
            .then(|| super::conditions::card_type_condition_as_target_match(cond))
            .flatten()
        });
        let effective_condition = converted_condition.as_ref().or(effective_condition);
        if let Some(cond) = effective_condition {
            // CR 603.4 + CR 608.2h: An in-effect `if` on a continuous
            // keyword-grant clause (Odric, Lunarch Marshal) must gate each
            // `StaticDefinition` individually, NOT the whole ability — the
            // "the same is true for" continuation later swaps the gated
            // keyword per arm. Push the condition down onto every
            // `StaticDefinition` (as a `StaticCondition`, where `effect.rs`
            // evaluates it once at resolution) instead of onto
            // `AbilityDefinition.condition`. Falls back to the ability-level
            // condition when the effect is not a `GenericEffect` or the
            // condition is not invertible to a `StaticCondition`.
            let pushed_down = if let Effect::GenericEffect {
                static_abilities, ..
            } = &mut *def.effect
            {
                ability_condition_to_static_condition(cond).map(|static_cond| {
                    for static_def in static_abilities.iter_mut() {
                        static_def.condition = Some(static_cond.clone());
                    }
                })
            } else {
                None
            };
            if pushed_down.is_none() {
                def = def.condition(cond.clone());
            }
        }
        // CR 115.1d: Apply multi-target spec — prefer explicit choose-count text,
        // then strip result, then clause-level propagation.
        if let Some(spec) = extract_exact_target_multi_target(&clause_ir.source_text) {
            def = def.multi_target(spec);
        } else if let Some(spec) = extract_bounded_target_multi_target(&clause_ir.source_text) {
            def = def.multi_target(spec);
        } else if let Some(spec) = extract_optional_target_multi_target(&clause_ir.source_text) {
            def = def.multi_target(spec);
        } else if let Some(spec) = extract_verb_up_to_multi_target(&clause_ir.source_text) {
            def = def.multi_target(spec);
        } else if let Some(ref spec) = clause_ir.multi_target {
            def = def.multi_target(spec.clone());
        } else if let Some(ref spec) = clause_ir.parsed.multi_target {
            def = def.multi_target(spec.clone());
        }
        if parse_controlled_by_different_players_target_constraint(&clause_ir.source_text) {
            def = def.target_constraint(TargetSelectionConstraint::DifferentObjectControllers);
        }
        if let Some(constraint) = parse_total_mana_value_target_constraint(&clause_ir.source_text) {
            def = def.target_constraint(constraint);
        }
        // CR 601.2d: Propagate distribute flag.
        if let Some(ref unit) = clause_ir.parsed.distribute {
            def = def.distribute(unit.clone());
        }
        if let Some(ref modifier) = clause_ir.unless_pay {
            def = def.unless_pay(modifier.clone());
        }

        let mut current_defs = vec![def];
        if let Some(ref sub) = clause_sub {
            current_defs.push(*sub.clone());
        }
        for current in &mut current_defs {
            apply_where_x_ability_expression(current, clause_ir.where_x_expression.as_deref());
        }

        // CR 603.7: Wrap in CreateDelayedTrigger if temporal suffix was found.
        if let Some(ref delayed_cond) = clause_ir.delayed_condition {
            for current in &mut current_defs {
                let inner = std::mem::replace(
                    current,
                    AbilityDefinition::new(
                        kind,
                        Effect::Unimplemented {
                            name: "placeholder".to_string(),
                            description: None,
                        },
                    ),
                );
                // CR 608.2c: Lift condition/optional/repeat/player_scope to outer wrapper.
                let lifted_condition = inner.condition.clone();
                let lifted_optional = inner.optional;
                let lifted_optional_for = inner.optional_for;
                let lifted_repeat_for = inner.repeat_for.clone();
                let lifted_player_scope = inner.player_scope.clone();
                *current = AbilityDefinition::new(
                    kind,
                    Effect::CreateDelayedTrigger {
                        condition: delayed_cond.clone(),
                        effect: Box::new(inner),
                        uses_tracked_set: false,
                    },
                );
                current.condition = lifted_condition;
                current.optional = lifted_optional;
                current.optional_for = lifted_optional_for;
                current.repeat_for = lifted_repeat_for;
                current.player_scope = lifted_player_scope;
            }
        }

        // CR 603.7: Cross-clause pronoun → mark uses_tracked_set on delayed trigger
        // and bind direct follow-up ParentTarget references to the affected set.
        if !current_defs.is_empty() {
            let source_text_lower = clause_ir.source_text.to_lowercase();
            // CR 603.7: Scan ALL prior clauses for a tracked-set publisher — an
            // intermediate non-publishing clause (e.g. Investigate) must not
            // shadow an earlier exile clause. Example: Disorder in the Court
            // (exile → investigate → return the exiled cards).
            let any_prior_publishes = defs
                .iter()
                .any(|d| publishes_tracked_set_from_resolution(&d.effect));
            if any_prior_publishes {
                let has_tracked_ref = contains_explicit_tracked_set_pronoun(&source_text_lower)
                    || contains_implicit_tracked_set_pronoun(&source_text_lower);
                if has_tracked_ref {
                    for current in &mut current_defs {
                        mark_uses_tracked_set(current);
                        rewrite_parent_targets_to_tracked_set(&mut current.effect);
                    }
                }
            }

            // Find the previous non-special, non-absorbed clause
            let prev_effect = defs.last().map(|d| &*d.effect);
            if let Some(prev_eff) = prev_effect {
                // CR 603.7c: Stamp the prior clause's zone destination as the
                // expected origin of any delayed `ParentTarget` return, so the
                // resolver's CR 400.7 `origin` guard suppresses the return when the
                // snapshotted referent has left that zone. Sibling of (not nested
                // in) the tracked-set rewrite above — this must fire for the
                // non-anaphor "that card" phrasing too.
                //
                // CR 406.1: `ExileTop` always moves cards to `Zone::Exile`. Without
                // this arm the Necropotence / Bomat Courier class's delayed return
                // ("put that card into your hand at the beginning of your next end
                // step") would not have its `origin: Exile` stamped, so the
                // resolver's referent-zone guard would erroneously suppress the
                // recall even when the card is still in exile.
                let prev_zone: Option<Zone> = match prev_eff {
                    Effect::ChangeZone { destination, .. }
                    | Effect::ChangeZoneAll { destination, .. } => Some(*destination),
                    Effect::ExileTop { .. } => Some(Zone::Exile),
                    _ => None,
                };
                if let Some(zone) = prev_zone {
                    for current in &mut current_defs {
                        stamp_delayed_returns(&mut current.effect, zone);
                    }
                }
            }
        }

        defs.extend(current_defs);

        // Apply intrinsic continuation after extending defs with current clause's defs.
        if let Some(ref continuation) = clause_ir.intrinsic_continuation {
            apply_clause_continuation(&mut defs, continuation.clone(), kind);
            apply_where_x_to_latest_def(&mut defs, clause_ir.where_x_expression.as_deref());
        }

        // CR 608.2c: Advance the separating boundary for the next normal-path
        // clause. Special/absorbed clauses also advance `prev_boundary` (via the
        // `handled_as_special` branch above) — although they emit no sibling
        // def, they occupy a chunk slot and carry their own trailing boundary,
        // so a following normal clause must stamp `sub_link` from the boundary
        // AFTER the special clause, not the stale one that preceded it.
        prev_boundary = clause_ir.boundary;
    }

    // ── Phase 2: Post-loop assembly (unchanged) ────────────────────────
    let kind = ir.kind;
    let chain_rounding = ir.chain_rounding;

    // CR 701.20a / CR 701.20e: Demote reveal-Dig back to RevealTop when no DigFromAmong
    // continuation patched it. An unpatched Dig { reveal: true, keep_count: None, filter: Any }
    // is a simple "reveal the top N" with no player selection — it must resolve synchronously
    // (via RevealTop) so that sub_ability chains like RevealedHasCardType evaluate inline.
    for def in &mut defs {
        if let Effect::Dig {
            count,
            keep_count: None,
            filter: TargetFilter::Any,
            reveal: true,
            destination,
            rest_destination,
            player,
            ..
        } = &*def.effect
        {
            if destination == &Some(Zone::Library) && rest_destination == &Some(Zone::Library) {
                continue;
            }
            let count_val = match count {
                QuantityExpr::Fixed { value } => *value as u32,
                _ => 1,
            };
            *def.effect = Effect::RevealTop {
                player: player.clone(),
                count: count_val,
            };
        }
    }

    // CR 701.20a + CR 608.2c: A bare private "look at the top N cards" instruction
    // is only a look; it does not move a chosen card to hand. Continuations that
    // actually choose cards from among them patch destination/keep_count before this
    // pass. Anything still in the raw private-Dig shape is a pure peek: skip
    // DigChoice and only populate last_revealed_ids for downstream conditions.
    for def in &mut defs {
        if let Effect::Dig {
            reveal: false,
            keep_count: None,
            filter: TargetFilter::Any,
            destination: None,
            rest_destination: None,
            ..
        } = &*def.effect
        {
            if let Effect::Dig { keep_count, .. } = &mut *def.effect {
                *keep_count = Some(0);
            }
        }
    }

    // CR 702.33d + CR 608.2e: Resolve "create [N] of those tokens [instead]"
    // anaphoric subs — the sub-ability parses as `Unimplemented` because the
    // noun "those tokens" refers back to the previous clause's token-creation
    // effect. Rewrite those subs by cloning the previous effect with an
    // updated count (Rite of Replication / Saproling Migration / Krothuss).
    resolve_those_tokens_anaphors(&mut defs);

    // CR 701.36a + CR 603.7c: Resolve "the token created this way …" and the
    // "sacrifice it" anaphors that follow a token-creating effect (Populate,
    // CopyTokenOf, Token). The antecedent is the populated / created token;
    // `TargetFilter::LastCreated` at runtime resolves against
    // `state.last_created_token_ids` (snapshotted at delayed-trigger
    // creation for the Sacrifice case — CR 603.7c).
    resolve_populated_token_anaphors(&mut defs);

    // CR 707.12: "Copy [a card]. You may cast the copy ..." is not a stack
    // copy (CR 707.10). It creates a card copy in the source zone, then casts
    // that copy during resolution. Fold the two parsed imperative clauses into
    // the dedicated engine primitive before generic chain assembly.
    fold_cast_copy_of_card_defs(&mut defs);

    // CR 706 + CR 705: Consolidate die result table lines into their parent RollDie,
    // and coin flip conditional branches into their parent FlipCoin.
    consolidate_die_and_coin_defs(&mut defs, kind);

    // Chain: last has no sub_ability, each earlier one chains to next.
    // When a def already has a sub_ability (e.g., TargetOnly with attached Explore),
    // append to the deepest sub rather than overwriting.
    let mut result = if defs.len() > 1 {
        let last = defs.pop().unwrap();
        let mut chain = last;
        while let Some(mut prev) = defs.pop() {
            if prev.condition == Some(AbilityCondition::AdditionalCostPaidInstead) {
                if let Some(base_chain) = prev.else_ability.as_mut() {
                    if matches!(
                        (&*base_chain.effect, &*chain.effect),
                        (
                            Effect::ChangeZone {
                                origin: Some(Zone::Library),
                                destination: Zone::Hand,
                                ..
                            },
                            Effect::ChangeZone {
                                origin: Some(Zone::Library),
                                destination: Zone::Hand,
                                ..
                            }
                        )
                    ) {
                        append_to_deepest_sub_ability(base_chain, chain.sub_ability.clone());
                    }
                }
            }
            // A node attached as a `sub_ability` is a resolution continuation
            // of its parent, not an independently activatable ability.
            // Normalize its kind to `Spell` (the "resolves alongside parent"
            // kind) before linking. This matches the convention used by
            // dedicated clause builders that construct sub-abilities directly
            // (e.g., `try_parse_pump_with_damage_sub` at line 3220).
            chain.kind = AbilityKind::Spell;
            if prev.optional && is_linked_exile_cast_bottom_cleanup(&prev.effect, &chain.effect) {
                normalize_linked_exile_cast_bottom_cleanup(&mut chain.effect);
                prev.else_ability = Some(Box::new(chain.clone()));
            }
            if prev.sub_ability.is_some() {
                // Walk to the deepest sub_ability and append there
                let mut cursor = &mut prev;
                while cursor.sub_ability.is_some() {
                    cursor = cursor.sub_ability.as_mut().unwrap();
                }
                cursor.sub_ability = Some(Box::new(chain));
            } else {
                prev.sub_ability = Some(Box::new(chain));
            }
            chain = prev;
        }
        chain
    } else {
        defs.pop().unwrap_or_else(|| {
            AbilityDefinition::new(
                kind,
                Effect::Unimplemented {
                    name: "empty".to_string(),
                    description: None,
                },
            )
        })
    };

    // CR 608.2 + CR 107.2: Wherever an ability in the chain carries
    // `player_scope` (outermost OR a nested sub-ability), rewrite target-scoped
    // refs ("their life", "their hand") to their per-iterating-player
    // equivalents. Walks the whole tree so a scoped clause buried under earlier
    // non-scoped clauses (Betor, Kin to All) is still rewritten.
    apply_player_scope_rewrites(&mut result);

    // CR 107.1a: Apply the chain-level rounding annotation (captured above)
    // to every DivideRounded in the built tree. No-op when the sentence was
    // absent (chain_rounding == None).
    if let Some(mode) = chain_rounding {
        rewrite_rounding_mode(&mut result, mode);
    }

    collapse_ephemeral_color_choice_mana(&mut result);
    rewrite_that_type_mana_instead(&mut result);

    // CR 303.4f + CR 301.5b + CR 603.7d: Wire `forward_result: true` on a
    // parent zone-change to Battlefield when the chained sub-ability is an
    // `Attach` gated by `ZoneChangedThisWay`. Without this, the runtime
    // resolves the sub-ability with `source_id` = the original ability source
    // (the trigger source / Saga / activated permanent), so the Attach tries
    // to equip *that* object to the chosen creature — wrong for Armored
    // Skyhunter (Skyhunter cannot equip itself), wrong for Vault 101: Birthday
    // Party (a Saga is not Equipment), wrong for Quest for the Holy Relic and
    // Stonehewer Giant (the searcher is not the moved Equipment).
    //
    // CR 608.2c: The same flag also wires sub-chains whose own clauses
    // anchor on the just-moved card via the bare-"it" anaphor
    // (`TargetFilter::SelfRef`) — Emperor of Bones' "[…] put a creature
    // card exiled with this creature onto the battlefield […]. It gains
    // haste. Sacrifice it at the beginning of the next end step." The
    // trailing GenericEffect/Pump and CreateDelayedTrigger subs target
    // `SelfRef` so the runtime's `source_id` rewrite resolves them to the
    // moved card instead of Emperor itself.
    //
    // The `forward_result` flag makes the runtime forward the just-moved
    // card's id as the sub-ability's `source_id` (see `effects/mod.rs`
    // forward_result branch), so `Attach::resolve` operates on the correct
    // attaching object.
    rewire_cross_sentence_token_counter_attach(&mut result);
    rewire_token_attach_sibling(&mut result);
    fold_token_it_has_grants_into_token_statics(&mut result);
    nest_whenever_this_turn_token_cleanup_delayed_trigger(&mut result);
    rewire_result_anchored_subchain(&mut result);
    wire_optional_cast_decline_fallback(&mut result);
    retarget_counter_additional_cost_to_target(&mut result);
    if matches!(&*result.effect, Effect::SearchOutsideGame { .. }) {
        result.optional = false;
        result.optional_for = None;
    }

    // CR 608.2c + CR 107.1c: A trailing "repeat this process" directive sets a
    // chain-level loop predicate; apply it to the assembled root ability so the
    // resolver re-follows the whole chain.
    if let Some(ref continuation) = ir.repeat_until {
        result.repeat_until = Some(continuation.clone());
    }

    result
}

fn target_choice_timing_for_clause(clause_ir: &ClauseIr) -> TargetChoiceTiming {
    if let Effect::PutCounter { target, .. } = &clause_ir.parsed.effect {
        let lower = clause_ir.source_text.to_ascii_lowercase();
        if !nom_primitives::scan_contains(&lower, "target ")
            && target.contains_source_attachment_host()
        {
            return TargetChoiceTiming::Resolution;
        }
    }
    if matches!(clause_ir.parsed.effect, Effect::MultiplyCounter { .. }) {
        let lower = clause_ir.source_text.to_ascii_lowercase();
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
        let lower = clause_ir.source_text.to_ascii_lowercase();
        if !nom_primitives::scan_contains(&lower, "target ") {
            return TargetChoiceTiming::Resolution;
        }
    }

    let Effect::ChangeZone {
        origin: Some(origin),
        ..
    } = &clause_ir.parsed.effect
    else {
        return TargetChoiceTiming::Stack;
    };
    if *origin == Zone::Battlefield {
        return TargetChoiceTiming::Stack;
    }

    let lower = clause_ir.source_text.to_ascii_lowercase();
    if nom_primitives::scan_contains(&lower, "target ") {
        TargetChoiceTiming::Stack
    } else {
        TargetChoiceTiming::Resolution
    }
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
fn rewire_result_anchored_subchain(def: &mut AbilityDefinition) {
    if let Some(sub) = def.sub_ability.as_ref() {
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
            }
        );
        if parent_moves_to_battlefield
            && (sub_is_attach_with_zone_changed_cond || sub_targets_moved_card(sub))
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

/// CR 702.33d + CR 608.2e: Resolve "create [N] of those tokens [instead]"
/// anaphoric clauses. The clause refers back to the previous def's token
/// creation effect (either `Token` or `CopyTokenOf`) and reproduces it with
/// a new count. We walk `defs` looking for an `Unimplemented` clause whose
/// description matches the anaphor, and rewrite its effect as a clone of the
/// previous def's effect with the parsed count.
fn resolve_those_tokens_anaphors(defs: &mut [AbilityDefinition]) {
    for i in 1..defs.len() {
        let (prev_rest, cur_rest) = defs.split_at_mut(i);
        let prev = &prev_rest[i - 1];
        let cur = &mut cur_rest[0];
        rewrite_those_tokens_from_antecedent(&mut cur.effect, &prev.effect);
    }
}

/// CR 702.33d + CR 707.10: If `cur` is an `Unimplemented` "create N of those
/// tokens" anaphor, rewrite it as a clone of the `antecedent` token-creation
/// effect with count set to N. No-op when the shapes don't match.
fn rewrite_those_tokens_from_antecedent(cur: &mut Effect, antecedent: &Effect) {
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

fn rewrite_counter_instead_target_from_antecedent(cur: &mut Effect, antecedent: &Effect) -> bool {
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
    let Effect::PutCounter {
        target: antecedent_target,
        ..
    } = antecedent
    else {
        return false;
    };
    if antecedent_target.contains_source_attachment_host() {
        *current_target = antecedent_target.clone();
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
    // Accept end, or a comma/whitespace-prefixed modifier.
    if tail.is_empty() || matches!(tail.chars().next(), Some(' ' | ',' | '.')) {
        Some(count)
    } else {
        None
    }
}

/// CR 611.2c + CR 603.7c + CR 111.2 + CR 707.2 + CR 701.36a: Rewrite token
/// anaphors following a token-creating effect.
///
/// Two rewrites, both scoped to defs whose chain contains a prior token
/// creator (`Populate`, `CopyTokenOf`, `Token`):
///
/// 1. `Effect::Unimplemented { description: "<anaphor> <mod>" }`
///    → `GenericEffect { target: Some(LastCreated), static_abilities: [...],
///    duration: Some(UntilEndOfTurn) }` where the modifications are parsed
///    from the verb phrase ("gains haste" / "gets +1/+1" / …).
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
fn resolve_populated_token_anaphors(defs: &mut [AbilityDefinition]) {
    for i in 0..defs.len() {
        if !defs[..i]
            .iter()
            .any(|d| is_token_creating_effect(&d.effect))
        {
            continue;
        }
        rewrite_populated_anaphor_in_def(&mut defs[i]);
    }
}

pub(super) fn is_token_creating_effect(effect: &Effect) -> bool {
    matches!(
        effect,
        Effect::Populate | Effect::Token { .. } | Effect::CopyTokenOf { .. }
    )
}

/// Rebind a `GenericEffect` grant's `SelfRef` recipient(s) to `LastCreated`.
/// No-op for any other effect, and for grants that already name a concrete
/// recipient — only the source-defaulted `SelfRef` is the misbound "it".
fn rebind_self_ref_grant_to_last_created(effect: &mut Effect) {
    let Effect::GenericEffect {
        static_abilities,
        target,
        ..
    } = effect
    else {
        return;
    };
    let mut rebound = false;
    for static_def in static_abilities.iter_mut() {
        if matches!(static_def.affected, Some(TargetFilter::SelfRef)) {
            static_def.affected = Some(TargetFilter::LastCreated);
            rebound = true;
        }
    }
    if rebound && matches!(target, None | Some(TargetFilter::SelfRef)) {
        *target = Some(TargetFilter::LastCreated);
    }
}

/// Walk an ability definition, rewriting the populated-token anaphor at
/// whichever level it appears. Recurses into `CreateDelayedTrigger.effect` so
/// the "sacrifice it" pattern inside a delayed trigger also rewrites.
fn rewrite_populated_anaphor_in_def(def: &mut AbilityDefinition) {
    if let Some(new_effect) =
        rewrite_token_created_this_way_unimplemented(&def.effect, def.duration.clone())
    {
        *def.effect = new_effect;
        def.duration = None;
        return;
    }

    rewrite_populated_anaphor_in_effect(&mut def.effect);
    // CR 608.2c + CR 701.36a: recurse into sub_ability chains so anaphoric
    // rewrites apply to sibling followups (Fractal Harness PutCounter/Attach).
    if let Some(sub) = def.sub_ability.as_mut() {
        rewrite_populated_anaphor_in_def(sub);
    }
}

/// CR 608.2c + CR 122.6 + CR 614.1c: Fractal Harness class — sentence
/// splitting lowers token creation and a sibling `PutCounter` targeting
/// `SelfRef` (the ETB source). Preserve `Token -> PutCounter -> Attach` as
/// separate instructions; rebind anaphoric targets to `LastCreated`.
fn rewire_cross_sentence_token_counter_attach(def: &mut AbilityDefinition) {
    if !matches!(&*def.effect, Effect::Token { .. }) {
        return;
    }
    let Some(put_box) = def.sub_ability.take() else {
        return;
    };
    let mut put_sub = *put_box;
    if put_sub.sub_link != SubAbilityLink::SequentialSibling {
        def.sub_ability = Some(Box::new(put_sub));
        return;
    }
    let Effect::PutCounter { target, .. } = put_sub.effect.as_ref() else {
        def.sub_ability = Some(Box::new(put_sub));
        return;
    };
    if !matches!(target, TargetFilter::SelfRef | TargetFilter::ParentTarget) {
        def.sub_ability = Some(Box::new(put_sub));
        return;
    }
    let attach_sub = match put_sub.sub_ability.take() {
        Some(sub) if matches!(&*sub.effect, Effect::Attach { .. }) => sub,
        other => {
            put_sub.sub_ability = other;
            def.sub_ability = Some(Box::new(put_sub));
            return;
        }
    };

    if let Effect::PutCounter { target, .. } = &mut *put_sub.effect {
        *target = TargetFilter::LastCreated;
    }
    put_sub.sub_link = SubAbilityLink::ContinuationStep;

    let mut attach_sub = *attach_sub;
    attach_sub.sub_link = SubAbilityLink::ContinuationStep;
    rewrite_parent_target_to_last_created(&mut attach_sub.effect);

    put_sub.sub_ability = Some(Box::new(attach_sub));
    def.sub_ability = Some(Box::new(put_sub));
}

/// CR 608.2c + CR 301.5b: Token creation followed by a sibling `Attach`
/// ("create a Kor Soldier token. You may attach an Equipment you control to
/// it") — the bare-"it" host anaphor must target `LastCreated`, not
/// `ParentTarget` (the token-creating effect has no parent target slot).
fn rewire_token_attach_sibling(def: &mut AbilityDefinition) {
    // Walk the whole sub-ability chain: the token + bare-Attach pair is not
    // always at the root. Field-Tested Frying Pan ("create a Food token, then
    // create a 1/1 white Halfling creature token and attach this Equipment to
    // it") nests the Attach under the *second* token, so a root-only check would
    // miss it and leave "it" bound to ParentTarget/TriggeringSource (which has no
    // referent here) instead of the just-created token.
    let mut node: Option<&mut AbilityDefinition> = Some(def);
    while let Some(current) = node {
        if matches!(&*current.effect, Effect::Token { .. }) {
            if let Some(sub) = current.sub_ability.as_mut() {
                // Token → PutCounter → Attach is owned by
                // `rewire_cross_sentence_token_counter_attach`.
                if !matches!(&*sub.effect, Effect::PutCounter { .. }) {
                    if let Effect::Attach { target, .. } = sub.effect.as_mut() {
                        if matches!(
                            target,
                            TargetFilter::ParentTarget | TargetFilter::TriggeringSource
                        ) {
                            *target = TargetFilter::LastCreated;
                        }
                    }
                }
            }
        }
        node = current.sub_ability.as_deref_mut();
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

fn fold_token_it_has_grants_into_token_statics(def: &mut AbilityDefinition) {
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
    let Effect::GenericEffect {
        static_abilities,
        duration,
        target,
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
fn rewrite_populated_anaphor_in_effect(effect: &mut Effect) {
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
        rewrite_parent_target_to_last_created(&mut inner.effect);
        rewrite_populated_anaphor_in_effect(&mut inner.effect);
    }

    // Case 3: a bare "it gains/gets X" grant that parsed to a `GenericEffect`
    // targeting `SelfRef` (the imperative parser's default for the bare pronoun
    // "it") — directly after a token-creating effect, "it" is the created token
    // (God-Pharaoh's Gift: "create a token … It gains haste"). Rebind to the
    // just-created token.
    rebind_self_ref_grant_to_last_created(effect);

    // Case 4 (CR 301.5b + CR 122.6a): imperative followups like Fractal Harness's
    // "attach this Equipment to it" parse "it" as ParentTarget (Self-ETB trigger
    // subject). After a token creator in the same chain, rewrite to LastCreated.
    rewrite_parent_target_to_last_created(effect);
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
        duration: duration
            .or(clause_duration)
            .or(Some(Duration::UntilEndOfTurn)),
        target: Some(TargetFilter::LastCreated),
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
fn effect_publishes_revealed_subject(effect: &Effect) -> bool {
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
pub(super) fn rewrite_parent_target_to_last_created(effect: &mut Effect) {
    match effect {
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
        | Effect::Attach { target, .. }
        | Effect::ChangeZone { target, .. } => {
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
        _ => {}
    }
}

/// CR 603.7c: Sentence splitting can leave a WheneverEvent delayed trigger's
/// token-creating inner effect and its end-step cleanup delayed trigger as
/// sibling `sub_ability` links on the activated ability. Rewire the cleanup
/// under the token creator so it registers when the WheneverEvent fires, not
/// at activation time (Dalkovan Encampment, Encore sacrifice riders).
fn nest_whenever_this_turn_token_cleanup_delayed_trigger(def: &mut AbilityDefinition) {
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
        rewrite_parent_target_to_last_created(&mut cleanup_effect.effect);
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

        // CR 705: Consolidate FlipCoinUntilLose with its following effect clause.
        // The next def becomes the win_effect that is executed per win.
        if matches!(&*defs[i].effect, Effect::FlipCoinUntilLose { .. }) && i + 1 < defs.len() {
            let next = defs.remove(i + 1);
            *defs[i].effect = Effect::FlipCoinUntilLose {
                win_effect: Box::new(next),
            };
        }

        // CR 705: Consolidate FlipCoins with its following effect clause — the
        // "for each heads …" / "skips their next X turns where X is the number of
        // coins that came up heads" sentence. The next def is attached as the
        // win_effect (runs once per heads). Only consolidates when the parent
        // `FlipCoins` has no branches already set (i.e., came straight from the
        // imperative lowering, not from a prior consolidation pass).
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
                let next = defs.remove(i + 1);
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
    let lower = text.to_lowercase();
    if let Some(((), rest)) = nom_on_lower(text, &lower, |i| value((), tag("for each ")).parse(i)) {
        let rest_lower = &lower[text.len() - rest.len()..];
        if let Ok((remainder, clause)) =
            terminated(take_until(", "), tag::<_, _, OracleError<'_>>(", ")).parse(rest_lower)
        {
            if let Some(qty) = parse_for_each_clause(clause) {
                // CR 106.1: "for each color among [X], add one mana of that color"
                // must NOT be split into (repeat_for, "add one mana of that color").
                // The "that color" anaphors the per-iteration color, not the
                // source's `ChosenAttribute::Color`. Let the full text flow
                // through to `try_parse_for_each_color_mana_public` which emits
                // a single `DistinctColorsAmongPermanents` mana ability.
                if matches!(qty, QuantityRef::DistinctColorsAmongPermanents { .. })
                    && remainder
                        .trim_end_matches('.')
                        .trim()
                        .eq_ignore_ascii_case("add one mana of that color")
                {
                    return (None, text.to_string());
                }
                if parse_for_each_object_copy_parts(text, &lower).is_some() {
                    return (None, text.to_string());
                }
                // CR 606.3: The Chain Veil's "For each planeswalker you control,
                // you may activate one of its loyalty abilities once this turn..."
                // is parsed as a single Effect::GrantExtraLoyaltyActivations —
                // the "for each planeswalker" preamble names the beneficiaries
                // (every planeswalker the controller controls gets +1 cap), not
                // a repeat count. Bailing out keeps the residual text intact so
                // the imperative dispatch can recognize the full pattern.
                if is_chain_veil_for_each_grant(&lower) {
                    return (None, text.to_string());
                }
                let offset = text.len() - remainder.len();
                return (Some(QuantityExpr::Ref { qty }), text[offset..].to_string());
            }
        }
    }
    (None, text.to_string())
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
            && target_filter_is_single_object_target(filter)
    })
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

fn per_opponent_target_fanout_min(text: &str) -> usize {
    let lower = text.to_ascii_lowercase();
    let Some((_, rest)) = nom_on_lower(text, &lower, |input| {
        value((), tag("gain control of ")).parse(input)
    }) else {
        return 1;
    };
    let (_, spec) = strip_optional_target_prefix(rest);
    if spec.is_some_and(|spec| spec.min_is_fixed_zero()) {
        0
    } else {
        1
    }
}

/// CR 107.1: Strip trailing "for each [quantity]" repeat suffixes whose base
/// action should be repeated rather than have an embedded amount replaced. The
/// repeat count is an integer per-each quantity (count templating), not the
/// CR 609.3 "do as much as possible" rule.
pub(super) fn strip_for_each_repeat_suffix(text: &str) -> (Option<QuantityExpr>, String) {
    let lower = text.to_lowercase();
    let parsed = nom_on_lower(text, &lower, |input| {
        let (rest, base) = take_until::<_, _, OracleError<'_>>(" for each ").parse(input)?;
        let (rest, _) = tag(" for each ").parse(rest)?;
        let (rest, qty) = nom_quantity::parse_for_each_clause_ref(rest)?;
        let (rest, _) = nom::combinator::opt(tag(".")).parse(rest)?;
        let (rest, _) = nom::combinator::eof::<_, OracleError<'_>>(rest)?;
        Ok((rest, (base.len(), qty)))
    });
    if let Some(((base_len, qty), _)) = parsed {
        if matches!(qty, QuantityRef::CommanderCastFromCommandZoneCount) {
            return (
                Some(QuantityExpr::Ref { qty }),
                text[..base_len].trim_end().to_string(),
            );
        }
    }
    (None, text.to_string())
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

pub(super) fn strip_each_player_subject(text: &str) -> (Option<PlayerFilter>, String) {
    let lower = text.to_lowercase();
    let scope_rest = nom_on_lower(text, &lower, |i| {
        alt((
            value(
                PlayerFilter::HighestSpeed,
                tag("each player with the highest speed among players "),
            ),
            value(PlayerFilter::Opponent, tag("each other player ")),
            value(PlayerFilter::Opponent, tag("each opponent ")),
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

    // CR 700.2 + CR 701.21a + CR 608.2c: "for each player, you choose …" (Tragic
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

    // CR 608.2c: A leading "also" after a resolved player-scope subject
    // ("each opponent also discards a card") is a continuation adverb with no
    // semantic weight — the same additive connector handled for self-ref
    // subjects in `parse_effect_clause_inner`. Strip it via `tag()` so the
    // residual ("discards a card") deconjugates and dispatches normally.
    let rest = nom_on_lower(rest, &rest_condition_lower, |i| {
        value((), tag("also ")).parse(i)
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
    if target_relative_clause_owns_suffix(lower.as_str()) {
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
            },
        ),
        (
            " at the beginning of your next upkeep",
            DelayedTriggerCondition::AtNextPhaseForPlayer {
                phase: Phase::Upkeep,
                player: crate::types::player::PlayerId(0),
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
pub(super) fn strip_temporal_prefix(text: &str) -> (&str, Option<DelayedTriggerCondition>) {
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
                },
                tag("at the beginning of your next upkeep, "),
            ),
            value(
                DelayedTriggerCondition::AtNextPhaseForPlayer {
                    phase: Phase::End,
                    player: crate::types::player::PlayerId(0),
                },
                tag("at the beginning of your next end step, "),
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

fn parse_controlled_by_different_players_target_constraint(text: &str) -> bool {
    let lower = text.to_lowercase();
    let mut parser = preceded(
        take_until::<_, _, OracleError<'_>>(CONTROLLED_BY_DIFFERENT_PLAYERS),
        tag(CONTROLLED_BY_DIFFERENT_PLAYERS),
    );
    parser.parse(lower.as_str()).is_ok()
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
fn parse_total_mana_value_target_constraint(text: &str) -> Option<TargetSelectionConstraint> {
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
    let (_, target_text) = preceded(
        tag::<_, _, OracleError<'_>>("double the number of each kind of counter on "),
        rest,
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

fn parse_each_of_up_to_damage_target<'a>(
    target_phrase: &'a str,
    ctx: &mut ParseContext,
) -> Option<(TargetFilter, &'a str)> {
    let lower = target_phrase.to_lowercase();
    let (after_each_of_lower, _) = tag::<_, _, OracleError<'_>>("each of ")
        .parse(lower.as_str())
        .ok()?;
    let consumed = lower.len() - after_each_of_lower.len();
    let after_each_of = &target_phrase[consumed..];
    if let Some((remainder, _)) = strip_bounded_targets_placeholder(after_each_of) {
        if remainder.is_empty() {
            return Some((TargetFilter::Any, ""));
        }
    }
    let (target_text, multi_target) = strip_optional_target_prefix(after_each_of);
    multi_target.as_ref()?;
    let (target, remainder) = parse_target_with_ctx(target_text, ctx);
    Some(refine_damage_target_remainder(target, remainder))
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
    "exile", "tap", "untap", "goad", "return", "destroy", "choose",
];

pub(super) const BOUNDED_TARGET_PHRASES: &[(&str, usize, usize)] = &[
    ("one or two targets", 1, 2),
    ("one, two, or three targets", 1, 3),
];

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
    for &(phrase, min, max) in BOUNDED_TARGET_PHRASES {
        if let Ok((rest, _)) = tag::<_, _, OracleError<'_>>(phrase).parse(lower.as_str()) {
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
    for (prefix, min, max) in [
        ("one or two target ", 1usize, 2usize),
        ("one, two, or three target ", 1, 3),
    ] {
        if let Ok((rest, _)) = tag::<_, _, OracleError<'_>>(prefix).parse(lower.as_str()) {
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

/// CR 115.1d: Strip optional target-count prefixes before a targeted phrase.
/// "up to one target creature" → ("target creature", Some { min: 0, max: Some(1) })
/// "up to one other target creature or spell" → ("other target creature or spell", Some { ... })
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

    if let Some((_, rest_orig)) = nom_on_lower(after_verb_tp.original, after_verb_tp.lower, |i| {
        value((), tag("any number of ")).parse(i)
    }) {
        let rebuilt = format!("{}{}", verb_tp.original, rest_orig);
        return (rebuilt, Some(MultiTargetSpec::unlimited(0)));
    }
    if let Some((_, after_up_to_orig)) =
        nom_on_lower(after_verb_tp.original, after_verb_tp.lower, |i| {
            value((), tag("up to ")).parse(i)
        })
    {
        let after_up_to_lower =
            &after_verb_tp.lower[after_verb_tp.lower.len() - after_up_to_orig.len()..];
        let after_up_to = TextPair::new(after_up_to_orig, after_up_to_lower);
        if let Ok((remainder, max)) = parse_multi_target_count_expr(after_up_to.lower) {
            let consumed_len = after_up_to.lower.len() - remainder.len();
            let (_, rest) = after_up_to.split_at(consumed_len);
            let rebuilt = format!("{}{}", verb_tp.original, rest.original.trim_start());
            return (rebuilt, Some(MultiTargetSpec::up_to(max)));
        }
    }
    (text.to_string(), None)
}

/// Strip "to the battlefield [under X's control]" and similar destination phrases.
/// Returns the remaining target text and the destination zone (if battlefield).
/// Result of parsing a "return ... to <zone>" destination phrase.
pub(super) struct ReturnDestination {
    pub(super) zone: Zone,
    pub(super) transformed: bool,
    // CR 110.2a: Controller override on ETB. `Some(ref)` routes the object to
    // the player resolved from `ref`; `None` leaves the object under its
    // owner's control. Downstream IR/Effect construction passes it through
    // unchanged into `Effect::ChangeZone.enters_under`.
    pub(super) enters_under: Option<ControllerRef>,
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

/// Detect "return ... to <zone>" destination phrase, including "transformed" flag.
pub(super) fn strip_return_destination_ext(text: &str) -> (&str, Option<ReturnDestination>) {
    let (target, dest, _) = strip_return_destination_ext_with_remainder(text);
    (target, dest)
}

pub(super) fn strip_return_destination_ext_with_remainder(
    text: &str,
) -> (&str, Option<ReturnDestination>, &str) {
    let lower = text.to_lowercase();
    // Ordered longest-first to avoid partial matches.
    // "transformed" variants must come before their non-transformed counterparts.
    // Tuples: (phrase, zone, transformed, enters_under_you, enter_tapped, enters_attacking)
    // The `enters_under_you` bool is the parser-table carrier for the
    // controller-override flag; it maps to `Some(ControllerRef::You)` / `None`
    // at the `ReturnDestination` construction site below (CR 110.2a).
    // Ordered longest-first; compound patterns must precede their shorter substrings.
    let patterns: &[(&str, Zone, bool, bool, bool, bool)] = &[
        // Tapped + transformed + owner's control (compound, longest)
        (
            " to the battlefield tapped and transformed under its owner's control",
            Zone::Battlefield,
            true,
            false,
            true,
            false,
        ),
        // Transformed + your control
        (
            " to the battlefield transformed under your control",
            Zone::Battlefield,
            true,
            true,
            false,
            false,
        ),
        // Transformed + owner's control variants
        (
            " to the battlefield transformed under their owners' control",
            Zone::Battlefield,
            true,
            false,
            false,
            false,
        ),
        (
            " to the battlefield transformed under its owner's control",
            Zone::Battlefield,
            true,
            false,
            false,
            false,
        ),
        (
            " to the battlefield transformed under his owner's control",
            Zone::Battlefield,
            true,
            false,
            false,
            false,
        ),
        (
            " to the battlefield transformed under her owner's control",
            Zone::Battlefield,
            true,
            false,
            false,
            false,
        ),
        (
            " to the battlefield transformed",
            Zone::Battlefield,
            true,
            false,
            false,
            false,
        ),
        // CR 508.4: Tapped and attacking (must precede shorter "tapped" variants)
        (
            " to the battlefield tapped and attacking",
            Zone::Battlefield,
            false,
            false,
            true,
            true,
        ),
        (
            " onto the battlefield tapped and attacking",
            Zone::Battlefield,
            false,
            false,
            true,
            true,
        ),
        // Tapped + control variants (must precede shorter "tapped" and "under X control")
        (
            " to the battlefield tapped under their owners' control",
            Zone::Battlefield,
            false,
            false,
            true,
            false,
        ),
        (
            " to the battlefield tapped under its owner's control",
            Zone::Battlefield,
            false,
            false,
            true,
            false,
        ),
        (
            " to the battlefield tapped under your control",
            Zone::Battlefield,
            false,
            true,
            true,
            false,
        ),
        // Simple control variants
        (
            " to the battlefield under their owners' control",
            Zone::Battlefield,
            false,
            false,
            false,
            false,
        ),
        (
            " to the battlefield under its owner's control",
            Zone::Battlefield,
            false,
            false,
            false,
            false,
        ),
        // CR 110.2: "under your control" — controller override.
        (
            " to the battlefield under your control",
            Zone::Battlefield,
            false,
            true,
            false,
            false,
        ),
        // CR 614.1: "tapped" — enters tapped.
        (
            " to the battlefield tapped",
            Zone::Battlefield,
            false,
            false,
            true,
            false,
        ),
        (
            " to the battlefield",
            Zone::Battlefield,
            false,
            false,
            false,
            false,
        ),
        // "onto" variants
        (
            " onto the battlefield under your control",
            Zone::Battlefield,
            false,
            true,
            false,
            false,
        ),
        (
            " onto the battlefield tapped",
            Zone::Battlefield,
            false,
            false,
            true,
            false,
        ),
        (
            " onto the battlefield",
            Zone::Battlefield,
            false,
            false,
            false,
            false,
        ),
        // Hand destinations
        (
            " to its owner's hand",
            Zone::Hand,
            false,
            false,
            false,
            false,
        ),
        (
            " to their owner's hand",
            Zone::Hand,
            false,
            false,
            false,
            false,
        ),
        (
            " to their owners' hands",
            Zone::Hand,
            false,
            false,
            false,
            false,
        ),
        (" to their hand", Zone::Hand, false, false, false, false),
        (" to your hand", Zone::Hand, false, false, false, false),
        // Graveyard destinations
        (
            " to its owner's graveyard",
            Zone::Graveyard,
            false,
            false,
            false,
            false,
        ),
        (
            " to their owner's graveyard",
            Zone::Graveyard,
            false,
            false,
            false,
            false,
        ),
        (
            " to their owners' graveyards",
            Zone::Graveyard,
            false,
            false,
            false,
            false,
        ),
        (
            " to your graveyard",
            Zone::Graveyard,
            false,
            false,
            false,
            false,
        ),
        // Command-zone destinations
        (
            " to the command zone",
            Zone::Command,
            false,
            false,
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
    for (phrase, zone, transformed, enters_under_you, enter_tapped, enters_attacking) in patterns {
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
            let after_destination = &lower[pos + phrase_len..];
            let (enter_with_counters, counters_offset) =
                parse_with_counters_suffix_spanned(after_destination);
            // CR 614.1c: when the "with N <type> counter(s)" clause is lifted
            // onto `enter_with_counters`, excise it (and any leading " and"
            // connector) from the returned remainder so the caller does not
            // re-parse "and with two stun counters …" into a dangling
            // Unimplemented follow-up clause (Unstoppable Slasher).
            let original_after_destination = match counters_offset {
                Some(off) => {
                    // CR 614.1c: strip a trailing " and" connector left after
                    // excising the consumed counter clause. Space-anchored
                    // `strip_suffix(" and")` (not `trim_end_matches("and")`,
                    // which is not word-anchored and would corrupt a remainder
                    // ending in "brand"/"island"); mirrors the leading
                    // `strip_leading_sequence_connector` analogue.
                    let trimmed = text[pos + phrase_len..pos + phrase_len + off].trim_end();
                    trimmed
                        // allow-noncombinator: structural cleanup of a trailing " and" connector on an already-sliced remainder, not parsing dispatch
                        .strip_suffix(" and")
                        .map(|s| s.trim_end())
                        .unwrap_or(trimmed)
                }
                None => &text[pos + phrase_len..],
            };
            return (
                text[..pos].trim(),
                Some(ReturnDestination {
                    zone: *zone,
                    transformed: *transformed,
                    enters_under: enters_under_you.then_some(ControllerRef::You),
                    enter_tapped: *enter_tapped,
                    enters_attacking: *enters_attacking,
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
    // CR 110.2a: parse the controller-override clause (or its absence) directly
    // into `Option<ControllerRef>`. Only `"under your control"` produces a
    // controller override; "owner's control" variants leave the object under
    // its owner's control (no override).
    let (input, enters_under) = alt((
        value(
            Some(ControllerRef::You),
            tag::<_, _, OracleError<'_>>(" under your control"),
        ),
        value(None, tag(" under their owners' control")),
        value(None, tag(" under its owner's control")),
        value(None, tag("")),
    ))
    .parse(input)?;
    let (input, _) = tag(" ").parse(input)?;
    Ok((
        input,
        ReturnDestination {
            zone: Zone::Battlefield,
            transformed: modifier.0,
            enters_under,
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
            enters_under: None,
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
            enters_under: None,
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
            enters_under: None,
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
/// least one). Fixed positive distributions still require at least one target;
/// "up to" and variable amounts can legally resolve to an empty pool.
fn multi_target_for_distribute_among(distribution_amount: &QuantityExpr) -> MultiTargetSpec {
    let (inner, is_up_to) = distribution_amount.peel_up_to();
    let min = if is_up_to {
        QuantityExpr::Fixed { value: 0 }
    } else {
        match inner {
            QuantityExpr::Fixed { value } if *value > 0 => QuantityExpr::Fixed { value: 1 },
            _ => QuantityExpr::Fixed { value: 0 },
        }
    };
    MultiTargetSpec::bounded_expr(min, inner.clone())
}

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
        let qty_end = [
            "divided as you choose among",
            "distributed among",
            "divided evenly",
        ]
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
                        },
                        "",
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
        // Team-Up. (Graceful Takedown's compound source set is deferred — see
        // `is_compound_source_each_power_damage`.)
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
                },
                rem,
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
            },
            "",
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
            },
            "",
        ));
    }

    let (target, rem) = parse_target_with_ctx(after_to, ctx);
    let (target, rem) = refine_damage_target_remainder(target, rem);
    let rem = trim_dangling_target_word(rem);
    Some((
        Effect::DealDamage {
            amount,
            target,
            damage_source: None,
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
    let power = apply_where_x_expression(power, where_x_expression.as_deref());
    let toughness = apply_where_x_expression(toughness, where_x_expression.as_deref());

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

fn apply_where_x_expression(value: PtValue, where_x_expression: Option<&str>) -> PtValue {
    match (value, where_x_expression) {
        (PtValue::Variable(alias), Some(expression)) if alias.eq_ignore_ascii_case("X") => {
            parse_where_x_quantity_expression(expression)
                .map(PtValue::Quantity)
                .unwrap_or_else(|| PtValue::Variable(expression.to_string()))
        }
        (PtValue::Variable(alias), Some(expression)) if alias.eq_ignore_ascii_case("-X") => {
            parse_where_x_quantity_expression(expression)
                .map(|inner| {
                    PtValue::Quantity(QuantityExpr::Multiply {
                        factor: -1,
                        inner: Box::new(inner),
                    })
                })
                .unwrap_or_else(|| PtValue::Variable(format!("-({expression})")))
        }
        (value, _) => value,
    }
}

pub(crate) fn parse_where_x_quantity_expression(where_x_expression: &str) -> Option<QuantityExpr> {
    let expression = where_x_expression.trim().trim_end_matches('.');
    let expression_lower = expression.to_ascii_lowercase();
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
    if tag::<_, _, OracleError<'_>>("the total amount of mana paid this way")
        .parse(expression_lower.as_str())
        .is_ok_and(|(rest, _)| rest.is_empty())
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
    let lower = expression.to_ascii_lowercase();
    if tag::<_, _, OracleError<'_>>("the number of times ")
        .parse(lower.as_str())
        .is_ok()
    {
        return None;
    }
    // CDA-quantity classification takes precedence: it is the more specific
    // where-X interpreter (object counts, "that spell's mana value",
    // "the number of age counters on this enchantment", etc.).
    if let Some(expr) = parse_cda_quantity(where_x_expression) {
        return Some(expr);
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
    let (rest, _) = tag::<_, _, OracleError<'_>>("the number of times ")
        .parse(lower.as_str())
        .ok()?;
    let rest = alt((
        preceded(
            take_until::<_, _, OracleError<'_>>(" was kicked"),
            tag(" was kicked"),
        ),
        preceded(
            take_until::<_, _, OracleError<'_>>(" kicked"),
            tag(" kicked"),
        ),
    ))
    .parse(rest)
    .ok()?
    .0;
    rest.is_empty().then_some(QuantityExpr::Ref {
        qty: QuantityRef::KickerCount,
    })
}

pub(super) fn apply_where_x_quantity_expression(
    value: QuantityExpr,
    where_x_expression: Option<&str>,
) -> QuantityExpr {
    match value {
        // CR 107.3i: Generic "X is N or more" condition parsing defaults to
        // CostXPaid for X-cost spells, but a surrounding "where X is ..." clause
        // is the more specific binding and must own every X reference in the
        // ability, including later-sentence rider conditions.
        QuantityExpr::Ref {
            qty: QuantityRef::CostXPaid,
        } if where_x_expression.is_some() => {
            let expression = where_x_expression.expect("checked is_some above");
            parse_where_x_quantity_expression(expression).unwrap_or_else(|| QuantityExpr::Ref {
                qty: QuantityRef::Variable {
                    name: expression.to_string(),
                },
            })
        }
        QuantityExpr::Ref {
            qty: QuantityRef::Variable { name },
        } if where_x_expression.is_some() && name.eq_ignore_ascii_case("X") => {
            let expression = where_x_expression.expect("checked is_some above");
            parse_where_x_quantity_expression(expression).unwrap_or_else(|| QuantityExpr::Ref {
                qty: QuantityRef::Variable {
                    name: expression.to_string(),
                },
            })
        }
        // CR 107.3i: "search ... for up to X ..., where X is …" wraps the X
        // count in `UpTo`. Recurse into `max` so the defining clause rewrites
        // the inner `Variable("X")` (Oreskos Explorer's "up to X Plains cards"
        // must bind X to the where-clause population, not stay at 0). `up_to`
        // re-asserts the non-nesting invariant.
        QuantityExpr::UpTo { max } => {
            QuantityExpr::up_to(apply_where_x_quantity_expression(*max, where_x_expression))
        }
        QuantityExpr::Offset { inner, offset } => QuantityExpr::Offset {
            inner: Box::new(apply_where_x_quantity_expression(
                *inner,
                where_x_expression,
            )),
            offset,
        },
        QuantityExpr::ClampMin { inner, minimum } => QuantityExpr::ClampMin {
            inner: Box::new(apply_where_x_quantity_expression(
                *inner,
                where_x_expression,
            )),
            minimum,
        },
        QuantityExpr::Multiply { factor, inner } => QuantityExpr::Multiply {
            factor,
            inner: Box::new(apply_where_x_quantity_expression(
                *inner,
                where_x_expression,
            )),
        },
        QuantityExpr::DivideRounded {
            inner,
            divisor,
            rounding,
        } => QuantityExpr::DivideRounded {
            inner: Box::new(apply_where_x_quantity_expression(
                *inner,
                where_x_expression,
            )),
            divisor,
            rounding,
        },
        other => other,
    }
}

pub(super) fn apply_where_x_effect_expression(
    effect: &mut Effect,
    where_x_expression: Option<&str>,
) {
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
        | Effect::Token { count: amount, .. }
        | Effect::Dig { count: amount, .. }
        | Effect::ExileTop { count: amount, .. }
        | Effect::Discover {
            mana_value_limit: amount,
            ..
        }
        | Effect::Incubate { count: amount } => {
            *amount = apply_where_x_quantity_expression(amount.clone(), where_x_expression);
        }
        // CR 107.3i + CR 109.4 + CR 109.5: "search/seek for up to X …, where X
        // is …" binds the search count (Oreskos Explorer). Eldritch Evolution
        // binds the filter's `Cmc` bound when X appears in the card filter.
        Effect::SearchLibrary { filter, count, .. } | Effect::Seek { filter, count, .. } => {
            *filter = apply_where_x_to_filter(filter.clone(), where_x_expression);
            *count = apply_where_x_quantity_expression(count.clone(), where_x_expression);
        }
        // CR 107.3i + CR 400.7: "return/put up to one target creature card with
        // mana value X or less ..., where X is <expression>" binds the
        // `ChangeZone` target filter's `Cmc` bound (Moseo, Vein's New Dean's
        // Infusion ability). Without this arm the filter's bound stayed an
        // unresolved bare `Variable("X")`, which resolves to 0 at runtime and
        // makes the reanimation target only mana value 0 or less — silently
        // breaking the trigger's intended behavior. Mirrors the
        // `SearchLibrary`/`Seek` filter rewrite above.
        Effect::ChangeZone { target, .. } => {
            *target = apply_where_x_to_filter(target.clone(), where_x_expression);
        }
        Effect::Scry { count, .. } => {
            *count = apply_where_x_quantity_expression(count.clone(), where_x_expression);
        }
        Effect::Pump {
            power, toughness, ..
        }
        | Effect::PumpAll {
            power, toughness, ..
        } => {
            *power = apply_where_x_expression(power.clone(), where_x_expression);
            *toughness = apply_where_x_expression(toughness.clone(), where_x_expression);
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
                *times = apply_where_x_quantity_expression(times.clone(), where_x_expression);
            }
            apply_where_x_to_ability_cost(cost, where_x_expression);
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
                    apply_where_x_static_condition(condition, where_x_expression);
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
                    apply_where_x_continuous_modification(modification, where_x_expression);
                    if rebind_target_anaphor {
                        rebind_target_anaphor_continuous_modification(modification);
                    }
                }
            }
        }
        _ => {}
    }
}

/// CR 107.3i + CR 611.2c: Substitute a "where X is <expression>" binding into a
/// continuous modification's dynamic `QuantityExpr` value. Only the value-carrying
/// dynamic P/T and dynamic-keyword grants (the +X/+X / set-P/T / dynamic-keyword
/// variants) hold an X-bearing `QuantityExpr`; every other `ContinuousModification`
/// variant is a fixed/typed modification with no X to rebind (enumerated as
/// explicit no-ops below). `apply_where_x_quantity_expression` only rewrites a
/// `CostXPaid` / bare `Variable("X")` value, so a modification whose quantity is
/// already a concrete reference is left unchanged.
fn apply_where_x_continuous_modification(
    modification: &mut ContinuousModification,
    where_x_expression: Option<&str>,
) {
    match modification {
        ContinuousModification::SetDynamicPower { value, .. }
        | ContinuousModification::SetDynamicToughness { value, .. }
        | ContinuousModification::SetPowerDynamic { value, .. }
        | ContinuousModification::SetToughnessDynamic { value, .. }
        | ContinuousModification::AddDynamicPower { value, .. }
        | ContinuousModification::AddDynamicToughness { value, .. }
        | ContinuousModification::AddDynamicKeyword { value, .. } => {
            *value = apply_where_x_quantity_expression(value.clone(), where_x_expression);
        }
        // Resolution-time-consumed; where-X counter quantities are applied by
        // the counter/enter-with parser paths before this continuous grant pass.
        ContinuousModification::AddCounterOnEnter { .. }
        | ContinuousModification::SetStartingLoyalty { .. } => {}
        // Non-dynamic modifications carry fixed integers, enum payloads, or
        // nested definitions that are already parsed/lowered independently.
        // Keep this wildcard-free so a future QuantityExpr-carrying variant
        // forces a deliberate where-X decision.
        ContinuousModification::CopyValues { .. }
        | ContinuousModification::SetName { .. }
        | ContinuousModification::AddPower { .. }
        | ContinuousModification::AddToughness { .. }
        | ContinuousModification::SetPower { .. }
        | ContinuousModification::SetToughness { .. }
        | ContinuousModification::AddKeyword { .. }
        | ContinuousModification::RemoveKeyword { .. }
        | ContinuousModification::GrantAbility { .. }
        | ContinuousModification::GrantAllActivatedAbilitiesOf { .. }
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
        | ContinuousModification::AddChosenColor
        | ContinuousModification::RemoveChosenKeyword
        | ContinuousModification::AddChosenKeyword
        | ContinuousModification::SetColor { .. }
        | ContinuousModification::AddColor { .. }
        | ContinuousModification::AddStaticMode { .. }
        | ContinuousModification::GrantStaticAbility { .. }
        | ContinuousModification::SwitchPowerToughness
        | ContinuousModification::AssignDamageFromToughness
        | ContinuousModification::AssignDamageAsThoughUnblocked
        | ContinuousModification::AssignNoCombatDamage
        | ContinuousModification::ChangeController
        | ContinuousModification::SetBasicLandType { .. }
        | ContinuousModification::SetChosenBasicLandType
        | ContinuousModification::RetainPrintedTriggerFromSource { .. }
        | ContinuousModification::RetainPrintedAbilityFromSource { .. }
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
        | ContinuousModification::SetName { .. }
        | ContinuousModification::AddPower { .. }
        | ContinuousModification::AddToughness { .. }
        | ContinuousModification::SetPower { .. }
        | ContinuousModification::SetToughness { .. }
        | ContinuousModification::AddKeyword { .. }
        | ContinuousModification::RemoveKeyword { .. }
        | ContinuousModification::GrantAbility { .. }
        | ContinuousModification::GrantAllActivatedAbilitiesOf { .. }
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
        | ContinuousModification::AddChosenColor
        | ContinuousModification::RemoveChosenKeyword
        | ContinuousModification::AddChosenKeyword
        | ContinuousModification::SetColor { .. }
        | ContinuousModification::AddColor { .. }
        | ContinuousModification::AddStaticMode { .. }
        | ContinuousModification::GrantStaticAbility { .. }
        | ContinuousModification::SwitchPowerToughness
        | ContinuousModification::AssignDamageFromToughness
        | ContinuousModification::AssignDamageAsThoughUnblocked
        | ContinuousModification::AssignNoCombatDamage
        | ContinuousModification::ChangeController
        | ContinuousModification::SetBasicLandType { .. }
        | ContinuousModification::SetChosenBasicLandType
        | ContinuousModification::RetainPrintedTriggerFromSource { .. }
        | ContinuousModification::RetainPrintedAbilityFromSource { .. }
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
fn apply_where_x_to_ability_cost(cost: &mut AbilityCost, where_x_expression: Option<&str>) {
    match cost {
        AbilityCost::PayLife { amount }
        | AbilityCost::PaySpeed { amount }
        | AbilityCost::PayEnergy { amount }
        | AbilityCost::ManaDynamic { quantity: amount } => {
            *amount = apply_where_x_quantity_expression(amount.clone(), where_x_expression);
        }
        // CR 701.9: "discard X cards, where X is …" — the discard count is a
        // `QuantityExpr` and must track the same where-X binding.
        AbilityCost::Discard { count, .. } => {
            *count = apply_where_x_quantity_expression(count.clone(), where_x_expression);
        }
        AbilityCost::Composite { costs } | AbilityCost::OneOf { costs } => {
            for sub in costs.iter_mut() {
                apply_where_x_to_ability_cost(sub, where_x_expression);
            }
        }
        AbilityCost::PerCounter { base, .. } => {
            apply_where_x_to_ability_cost(base, where_x_expression);
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
        | AbilityCost::TapCreatures { .. }
        | AbilityCost::RemoveCounter { .. }
        | AbilityCost::ReturnToHand { .. }
        | AbilityCost::Unattach
        | AbilityCost::Mill { .. }
        | AbilityCost::Exert
        | AbilityCost::Blight { .. }
        | AbilityCost::Reveal { .. }
        | AbilityCost::Behold { .. }
        | AbilityCost::Unimplemented { .. } => {}
    }
}

fn apply_where_x_to_latest_def(defs: &mut [AbilityDefinition], where_x_expression: Option<&str>) {
    if let Some(def) = defs.last_mut() {
        apply_where_x_ability_expression(def, where_x_expression);
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
pub(crate) fn apply_where_x_to_filter(
    filter: TargetFilter,
    where_x_expression: Option<&str>,
) -> TargetFilter {
    if where_x_expression.is_none() {
        return filter;
    }
    match filter {
        TargetFilter::Typed(mut typed) => {
            typed.properties = typed
                .properties
                .into_iter()
                .map(|prop| apply_where_x_to_filter_prop(prop, where_x_expression))
                .collect();
            TargetFilter::Typed(typed)
        }
        TargetFilter::And { filters } => TargetFilter::And {
            filters: filters
                .into_iter()
                .map(|filter| apply_where_x_to_filter(filter, where_x_expression))
                .collect(),
        },
        TargetFilter::Or { filters } => TargetFilter::Or {
            filters: filters
                .into_iter()
                .map(|filter| apply_where_x_to_filter(filter, where_x_expression))
                .collect(),
        },
        TargetFilter::Not { filter } => TargetFilter::Not {
            filter: Box::new(apply_where_x_to_filter(*filter, where_x_expression)),
        },
        other => other,
    }
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
) {
    if let TargetSelectionConstraint::TotalManaValue { value, .. } = constraint {
        *value = apply_where_x_quantity_expression(value.clone(), where_x_expression);
    }
}

fn apply_where_x_to_filter_prop(prop: FilterProp, where_x_expression: Option<&str>) -> FilterProp {
    match prop {
        FilterProp::Cmc { comparator, value } => FilterProp::Cmc {
            comparator,
            value: apply_where_x_quantity_expression(value, where_x_expression),
        },
        FilterProp::AnyOf { props } => FilterProp::AnyOf {
            props: props
                .into_iter()
                .map(|p| apply_where_x_to_filter_prop(p, where_x_expression))
                .collect(),
        },
        // CR 608.2c: Descend into the negated inner prop so X-substitution
        // reaches it (mirrors the AnyOf transform).
        FilterProp::Not { prop } => FilterProp::Not {
            prop: Box::new(apply_where_x_to_filter_prop(*prop, where_x_expression)),
        },
        other => other,
    }
}

pub(super) fn apply_where_x_ability_expression(
    def: &mut AbilityDefinition,
    where_x_expression: Option<&str>,
) {
    // CR 107.3i: All instances of X on an object share one value at any given
    // time. Substitute X in this AbilityDefinition's condition before walking
    // into effect/sub_ability/etc. The recursion below visits every chained
    // SequentialSibling node, so each node's own `condition` is reached here.
    if let Some(cond) = def.condition.as_mut() {
        apply_where_x_ability_condition(cond, where_x_expression);
    }
    if let Some(repeat_for) = def.repeat_for.take() {
        def.repeat_for = Some(apply_where_x_quantity_expression(
            repeat_for,
            where_x_expression,
        ));
    }
    if let Some(spec) = def.multi_target.as_mut() {
        spec.map_quantities(|expr| apply_where_x_quantity_expression(expr, where_x_expression));
    }
    // CR 107.3i + CR 202.3: Rebind X in the target-set constraints (e.g. the
    // `TotalManaValue` cap on Ancient Brass Dragon, whose bound is the
    // `where X is the result` die value). Without this, the reflexive sub
    // inherits `Variable("X")` with no defining expression and the cap is
    // effectively unbounded.
    for constraint in def.target_constraints.iter_mut() {
        apply_where_x_to_target_constraint(constraint, where_x_expression);
    }
    apply_where_x_effect_expression(def.effect.as_mut(), where_x_expression);
    if let Some(sub) = def.sub_ability.as_mut() {
        apply_where_x_ability_expression(sub, where_x_expression);
    }
    if let Some(else_ability) = def.else_ability.as_mut() {
        apply_where_x_ability_expression(else_ability, where_x_expression);
    }
    for mode_ability in &mut def.mode_abilities {
        apply_where_x_ability_expression(mode_ability, where_x_expression);
    }
}

/// CR 107.3i: Substitute the X binding into every quantity expression nested
/// inside an `AbilityCondition`. Delegates leaf substitution to the existing
/// `apply_where_x_quantity_expression`; recurses through compound arms
/// (`And`/`Or`/`Not`/`ConditionInstead`). Leaf arms without quantity fields
/// fall through to the no-op `_` arm.
fn apply_where_x_ability_condition(cond: &mut AbilityCondition, where_x_expression: Option<&str>) {
    match cond {
        AbilityCondition::QuantityCheck { lhs, rhs, .. } => {
            *lhs = apply_where_x_quantity_expression(lhs.clone(), where_x_expression);
            *rhs = apply_where_x_quantity_expression(rhs.clone(), where_x_expression);
        }
        AbilityCondition::And { conditions } | AbilityCondition::Or { conditions } => {
            for c in conditions.iter_mut() {
                apply_where_x_ability_condition(c, where_x_expression);
            }
        }
        AbilityCondition::Not { condition } => {
            apply_where_x_ability_condition(condition, where_x_expression);
        }
        AbilityCondition::ConditionInstead { inner } => {
            apply_where_x_ability_condition(inner, where_x_expression);
        }
        _ => {}
    }
}

fn apply_where_x_static_condition(
    condition: &mut StaticCondition,
    where_x_expression: Option<&str>,
) {
    match condition {
        StaticCondition::QuantityComparison { lhs, rhs, .. } => {
            *lhs = apply_where_x_quantity_expression(lhs.clone(), where_x_expression);
            *rhs = apply_where_x_quantity_expression(rhs.clone(), where_x_expression);
        }
        StaticCondition::And { conditions } | StaticCondition::Or { conditions } => {
            for condition in conditions {
                apply_where_x_static_condition(condition, where_x_expression);
            }
        }
        StaticCondition::Not { condition } => {
            apply_where_x_static_condition(condition, where_x_expression);
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

/// CR 122.1 + CR 614.1c: Scan a remainder for a "with [N] [type] counter(s) on
/// it" suffix and lift the matched counter type + count into a
/// `Vec<(CounterType, QuantityExpr)>` slot for `Effect::ChangeZone.enter_with_counters`.
///
/// Matches the patterns:
///   * "with N <type> counter(s) on it" — fixed numeric (digits or English).
///   * "with a/an <type> counter on it" — singular article.
///   * Optional "additional " between count and type — purely a synonym in
///     this position; the counter is still added once during the move.
///
/// Returns an empty `Vec` when no clause is present, so the caller can stamp
/// it unconditionally.
///
/// Implemented as a `scan_preceded` over the body combinator — the scanner
/// advances at word boundaries, so the suffix can appear anywhere after the
/// destination phrase ("onto the battlefield tapped under your control with
/// two additional +1/+1 counters on it") without the caller having to
/// pre-trim. The body combinator gates on `tag("with ")` then dispatches to
/// `parse_counter_suffix_body`.
pub(crate) fn parse_with_counters_suffix(lower: &str) -> Vec<(CounterType, QuantityExpr)> {
    parse_with_counters_suffix_spanned(lower).0
}

/// Like [`parse_with_counters_suffix`], but also reports the byte offset in
/// `lower` at which the matched `"with N <type> counter(s) [on it]"` clause
/// begins (the start of the `"with "` token). Callers that need to excise the
/// consumed counter clause from a larger remainder — e.g.
/// `strip_return_destination_ext_with_remainder`, so "return it to the
/// battlefield tapped and with two stun counters under its owner's control"
/// does not leave a dangling "and with two stun counters …" clause once the
/// counters are lifted onto `enter_with_counters` (Unstoppable Slasher) — use
/// this offset to truncate. Returns `None` for the offset when no counter
/// clause matched.
pub(crate) fn parse_with_counters_suffix_spanned(
    lower: &str,
) -> (Vec<(CounterType, QuantityExpr)>, Option<usize>) {
    nom_primitives::scan_preceded(lower, |i| {
        let (i, _) = tag::<_, _, OracleError<'_>>("with ").parse(i)?;
        parse_counter_suffix_body_combinator(i)
    })
    .map(|(prefix, val, _)| (vec![val], Some(prefix.len())))
    .unwrap_or((Vec::new(), None))
}

/// CR 122.1 + CR 614.1c: Combinator body for "[N|a|an] [additional ]<type>
/// counter(s) on it". Used by `parse_with_counters_suffix` AND by the exile-
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
    // CR 614.1c: "on it" is grammatical filler — present in "return it to the
    // battlefield with two +1/+1 counters on it" but absent when a controller
    // clause follows ("return it to the battlefield tapped and with two stun
    // counters under its owner's control", Unstoppable Slasher). Optional so
    // both shapes lift the counters onto `enter_with_counters`.
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

/// CR 122.1 + CR 614.1c: "a number of <type> counter(s) on it equal to
/// <quantity>" — dynamic counter count for "enters with counters" clauses
/// (e.g. The Eleventh Doctor: "with a number of time counters on it equal to
/// its mana value") AND for the post-token "create a … token and put[s] a
/// number of <type> counters on it equal to <quantity>" form (Oversimplify,
/// Fractal Anomaly class). Delegates the quantity to the shared
/// `parse_cda_quantity` building block so any "<verb> a number of X
/// counters … equal to …" card parses composed dynamic quantities
/// (twice/half/aggregate/difference), not just bare refs. CR 614.1c is the
/// authorizing rule for "enters with counters" replacement effects.
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
        nest_whenever_this_turn_token_cleanup_delayed_trigger, parse_where_x_quantity_expression,
        strip_return_destination_ext_with_remainder, strip_temporal_prefix, strip_temporal_suffix,
        strip_trailing_duration, strip_trailing_where_x,
        value_quantity_clause_owns_this_turn_suffix,
    };
    use crate::parser::oracle_util::TextPair;
    use crate::types::ability::{
        AbilityDefinition, AbilityKind, AggregateFunction, DelayedTriggerCondition, Duration,
        Effect, ObjectProperty, ObjectScope, PtValue, QuantityExpr, QuantityRef, TargetFilter,
        TriggerDefinition,
    };
    use crate::types::counter::CounterType;
    use crate::types::phase::Phase;
    use crate::types::triggers::TriggerMode;
    use crate::types::zones::Zone;

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

    /// CR 614.1c: dynamic enter-with-counters suffix accepts composed quantities.
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

    /// CR 614.1c + issue #1498: "return it to the battlefield tapped and with
    /// two stun counters under its owner's control" (Unstoppable Slasher) must
    /// lift the stun counters onto `enter_with_counters` and excise the counter
    /// clause from the returned remainder so no dangling follow-up clause is
    /// re-parsed. The `" on it"` filler is absent here (a controller clause
    /// follows the counters), which the optional terminator now tolerates.
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
        // The counter clause (and its leading " and" connector) is excised, so
        // nothing dangling remains to be re-parsed as a follow-up clause.
        assert_eq!(
            remainder, "",
            "the counter clause must be excised from the remainder, got {remainder:?}"
        );
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
                effects.iter().any(|e| matches!(e, Effect::ExtraTurn { .. })),
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
        ControllerRef, Duration, ObjectScope, PlayerScope, QuantityExpr, QuantityRef, TargetFilter,
        TypeFilter,
    };

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

    #[test]
    fn strip_trailing_duration_still_strips_genuine_this_turn_duration() {
        use super::strip_trailing_duration;

        let (stripped, duration) = strip_trailing_duration("that creature gains haste this turn.");
        assert_eq!(duration, Some(Duration::UntilEndOfTurn));
        assert_eq!(stripped, "that creature gains haste");
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
        assert_eq!(typed.controller, Some(ControllerRef::TargetPlayer));
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
        super::apply_where_x_to_target_constraint(&mut constraint, Some("the result"));
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
        super::apply_where_x_to_target_constraint(&mut constraint, Some("the result"));
        assert_eq!(
            constraint,
            TargetSelectionConstraint::DifferentObjectControllers
        );
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
