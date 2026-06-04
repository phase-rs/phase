use nom::branch::alt;
use nom::bytes::complete::{tag, take_till1, take_until};
use nom::character::complete::{multispace0, multispace1};
use nom::combinator::{eof, opt, rest, value};
use nom::sequence::{preceded, terminated};
use nom::Parser;

use super::super::oracle_nom::bridge::{nom_on_lower, split_once_on_lower};
use super::super::oracle_nom::condition as nom_condition;
use super::super::oracle_nom::error::{OracleError, OracleResult};
use super::super::oracle_nom::primitives as nom_primitives;
use super::super::oracle_nom::quantity as nom_quantity;
use super::super::oracle_quantity::{
    parse_cda_quantity, parse_cda_quantity_with_context, parse_for_each_clause,
    parse_for_each_clause_expr,
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
    AbilityCondition, AbilityDefinition, AbilityKind, Comparator, ControllerRef, DamageSource,
    DelayedTriggerCondition, Duration, Effect, FilterProp, MultiTargetSpec, PaymentCost,
    PlayerFilter, PlayerScope, PtValue, QuantityExpr, QuantityRef, RoundingMode, StaticCondition,
    StaticDefinition, SubAbilityLink, TargetChoiceTiming, TargetFilter, TypeFilter, TypedFilter,
};
use crate::types::counter::CounterType;
use crate::types::game_state::{DistributionUnit, TargetSelectionConstraint};
use crate::types::phase::Phase;
use crate::types::zones::Zone;

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
    contains_implicit_tracked_set_pronoun, fold_cast_copy_of_card_defs, has_explicit_player_target,
    mark_uses_tracked_set, parse_effect_clause, parse_event_context_ref_with_ctx,
    parse_for_each_object_copy_parts, publishes_tracked_set_from_resolution,
    refine_damage_target_remainder, replace_player_anaphor_with_parent_target,
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
                        for d in defs.iter_mut().rev() {
                            if d.condition.is_some() {
                                d.else_ability = Some(else_def.clone());
                                break;
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
                                        *enter_tapped = true;
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
                                            *enter_tapped = false;
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

        // Non-absorbed, non-special followup continuation — apply it to the
        // previous defs before building this clause's def.
        if let Some(ref continuation) = clause_ir.followup_continuation {
            apply_clause_continuation(&mut defs, continuation.clone(), kind);
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
        let clause_sub = if is_target_only {
            def.sub_ability = clause_ir.parsed.sub_ability.clone();
            None
        } else {
            clause_ir.parsed.sub_ability.clone()
        };

        if clause_ir.is_optional
            && !matches!(&clause_ir.parsed.effect, Effect::SearchOutsideGame { .. })
            && !matches!(
                &clause_ir.parsed.effect,
                Effect::GrantCastingPermission { .. }
            )
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
            // Find the previous non-special, non-absorbed clause
            let prev_effect = defs.last().map(|d| &*d.effect);
            if let Some(prev_eff) = prev_effect {
                if publishes_tracked_set_from_resolution(prev_eff) {
                    let has_tracked_ref = contains_explicit_tracked_set_pronoun(&source_text_lower)
                        || contains_implicit_tracked_set_pronoun(&source_text_lower);
                    if has_tracked_ref {
                        for current in &mut current_defs {
                            mark_uses_tracked_set(current);
                            rewrite_parent_targets_to_tracked_set(&mut current.effect);
                        }
                    }
                }

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

    // CR 701.20a vs CR 701.16a: Demote reveal-Dig back to RevealTop when no DigFromAmong
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
                player: TargetFilter::Controller,
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
            if prev.optional
                && matches!(*prev.effect, Effect::CastFromZone { .. })
                && matches!(
                    *chain.effect,
                    Effect::PutAtLibraryPosition {
                        target: TargetFilter::ExiledBySource,
                        ..
                    }
                )
            {
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
    rewire_result_anchored_subchain(&mut result);
    wire_optional_cast_decline_fallback(&mut result);
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
    if matches!(
        clause_ir.parsed.effect,
        Effect::Tap { .. } | Effect::Untap { .. }
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

/// CR 608.2k + CR 603.7c + CR 701.36a: Rewrite token anaphors following a
/// token-creating effect.
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
        rewrite_populated_anaphor_in_effect(&mut defs[i].effect);
    }
}

pub(super) fn is_token_creating_effect(effect: &Effect) -> bool {
    matches!(
        effect,
        Effect::Populate | Effect::Token { .. } | Effect::CopyTokenOf { .. }
    )
}

/// Walk an effect, rewriting the populated-token anaphor at whichever level
/// it appears. Recurses into `CreateDelayedTrigger.effect` so the "sacrifice
/// it" pattern inside a delayed trigger also rewrites.
fn rewrite_populated_anaphor_in_effect(effect: &mut Effect) {
    // Case 1: bare Unimplemented anaphor at the top level (e.g., "the token
    // created this way gains haste").
    if let Some(new_effect) = rewrite_token_created_this_way_unimplemented(effect) {
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
}

/// If `effect` is `Unimplemented { description: "<anaphor> <verb-phrase>" }`,
/// try to parse the verb phrase as a continuous modification set and return
/// a replacement `GenericEffect`. Returns `None` when the shape doesn't
/// match so the caller leaves the effect untouched.
///
/// CR 608.2k + CR 603.7c: Recognized anaphor prefixes resolve to the
/// just-created token via `TargetFilter::LastCreated`. The longer
/// populate-specific phrases ("the token(s) created this way ") MUST be
/// tried before the plain "the token " prefix to avoid the latter
/// shadowing the qualified forms when both could match.
pub(crate) fn rewrite_token_created_this_way_unimplemented(effect: &Effect) -> Option<Effect> {
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
        tag("the token "),
    ));
    let (rest, _matched) = anaphor.parse(lower.as_str()).ok()?;
    let mods = crate::parser::oracle_static::parse_continuous_modifications(rest);
    if mods.is_empty() {
        return None;
    }
    let static_def = StaticDefinition::continuous()
        .affected(TargetFilter::LastCreated)
        .modifications(mods)
        .description(text.to_string());
    Some(Effect::GenericEffect {
        static_abilities: vec![static_def],
        duration: Some(Duration::UntilEndOfTurn),
        target: Some(TargetFilter::LastCreated),
    })
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
        | Effect::Tap { target, .. }
        | Effect::Untap { target, .. }
        | Effect::Pump { target, .. }
        | Effect::ChangeZone { target, .. } => {
            if matches!(target, TargetFilter::ParentTarget) {
                *target = TargetFilter::LastCreated;
            }
        }
        _ => {}
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
        // CR 705: Consolidate coin flip branches
        if matches!(
            &*defs[i].effect,
            Effect::FlipCoin {
                win_effect: None,
                lose_effect: None,
            }
        ) {
            let mut win = None;
            let mut lose = None;
            let mut j = i + 1;
            while j < defs.len() && (win.is_none() || lose.is_none()) {
                match &*defs[j].effect {
                    Effect::FlipCoin {
                        win_effect: Some(w),
                        lose_effect: None,
                    } if win.is_none() => {
                        win = Some(w.clone());
                        j += 1;
                    }
                    Effect::FlipCoin {
                        win_effect: None,
                        lose_effect: Some(l),
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
        } = &*defs[i].effect
        {
            if i + 1 < defs.len() {
                let count = count.clone();
                let next = defs.remove(i + 1);
                *defs[i].effect = Effect::FlipCoins {
                    count,
                    win_effect: Some(Box::new(next)),
                    lose_effect: None,
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
    let lower = text.to_lowercase();
    // CR 608.2d: "each opponent may" — per-opponent optional effect.
    // "any opponent may" — first-accept-wins opponent-choice optional effect.
    // "you may" — standard optional effect prefix.
    if let Some(((scope, player_scope), rest)) = nom_on_lower(text, &lower, |input| {
        alt((
            value(
                (None, Some(PlayerFilter::Opponent)),
                tag("each opponent may "),
            ),
            value(
                (
                    Some(crate::types::ability::OpponentMayScope::AnyOpponent),
                    None,
                ),
                tag("any opponent may "),
            ),
            value((None, None), tag("you may ")),
            // CR 608.2c: "the first player may" — Oath of Mages and analogous
            // cross-clause patterns where the chooser of a prior sentence
            // (= TriggeringPlayer for upkeep/event triggers) is invited to
            // take an optional action in a later sentence. Marks the chunk
            // optional and scopes the actor to the triggering player.
            value(
                (None, Some(PlayerFilter::TriggeringPlayer)),
                tag("the first player may "),
            ),
        ))
        .parse(input)
    }) {
        (true, scope, player_scope, rest.to_string())
    } else {
        (false, None, None, text.to_string())
    }
}

/// CR 609.3: Detect and strip a trailing "a number of times equal to the
/// difference" repeat suffix. On success returns the suffix-free head; the
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

/// CR 609.3: Strip "for each [X], " prefix from effect text.
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

pub(super) fn strip_for_each_prefix(text: &str) -> (Option<QuantityExpr>, String) {
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

/// CR 609.3: Strip trailing "for each [quantity]" repeat suffixes whose base
/// action should be repeated rather than have an embedded amount replaced.
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

/// CR 609.3: Strip "twice" / "three times" / "N times" suffix to produce a
/// `repeat_for` count. Unified with `strip_for_each_prefix` at the chain level
/// so the base action is parsed normally and the resolver loops it N times.
pub(super) fn strip_repeat_count_suffix(text: &str) -> (Option<QuantityExpr>, String) {
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
pub(super) fn strip_player_scope_subject(text: &str) -> (Option<PlayerFilter>, String) {
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
        ))
        .parse(i)
    });
    let Some((scope, rest)) = scope_rest else {
        return (None, text.to_string());
    };

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
    // source-specific `OpponentAttackedBySourceThisTurn`, which excludes the
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
            Some(PlayerFilter::OpponentAttackedBySourceThisTurn),
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
    rest.chars()
        .all(|c| c.is_ascii_whitespace() || c.is_ascii_punctuation())
        .then_some(filter)
}

pub(super) fn strip_leading_duration(text: &str) -> Option<(Duration, &str)> {
    let lower = text.to_lowercase();
    if let Some((duration, rest)) = nom_on_lower(text, &lower, |i| {
        alt((
            value(Duration::UntilEndOfTurn, tag("until end of turn, ")),
            // CR 514.2: "until the end of your next turn" persists through
            // that turn's cleanup step.
            value(
                Duration::UntilEndOfNextTurnOf {
                    player: PlayerScope::Controller,
                },
                tag("until the end of your next turn, "),
            ),
            value(
                Duration::UntilNextTurnOf {
                    player: PlayerScope::Controller,
                },
                tag("until your next turn, "),
            ),
            // CR 513.1 + CR 611.2a: Rocco, Street Chef and the floating
            // play-permission class — "Until your next end step, ...".
            value(
                Duration::UntilNextStepOf {
                    step: Phase::End,
                    player: PlayerScope::Controller,
                },
                tag("until your next end step, "),
            ),
        ))
        .parse(i)
    }) {
        return Some((duration, rest.trim()));
    }

    // CR 611.2b: "For as long as [condition], [effect]" — leading duration prefix.
    if let Ok((rest, _)) = tag::<_, _, OracleError<'_>>("for as long as ").parse(lower.as_str()) {
        // Split "condition, effect_body" on the first ", " delimiter.
        if let Ok((effect_body, condition_text)) =
            terminated(take_until(", "), tag::<_, _, OracleError<'_>>(", ")).parse(rest)
        {
            if let Some(dur) = parse_for_as_long_as_condition(condition_text) {
                let prefix_len = "for as long as ".len() + condition_text.len() + ", ".len();
                return Some((dur, text[prefix_len..].trim()));
            }
            let _ = effect_body; // consumed by combinator; unused here
        }
    }

    None
}

pub(crate) fn strip_trailing_duration(text: &str) -> (&str, Option<Duration>) {
    let lower = text.to_lowercase();
    if target_relative_clause_owns_suffix(lower.as_str()) {
        return (text, None);
    }
    for (suffix, duration) in [
        (" this turn", Duration::UntilEndOfTurn),
        (" until end of turn", Duration::UntilEndOfTurn),
        (
            // CR 514.2: cleanup-pruned next-turn duration.
            " until the end of your next turn",
            Duration::UntilEndOfNextTurnOf {
                player: PlayerScope::Controller,
            },
        ),
        (" until end of combat", Duration::UntilEndOfCombat),
        // CR 514.2 + CR 611.2a + CR 108.3: Third-person "their next turn"
        // appears in grants whose grantee is not the ability's controller
        // (Suspend Aggression: "its owner may play it"; Expedited Inheritance:
        // "its controller may ... They may play those cards"). Theme D's
        // `granted_to` binds to the grantee, so `UntilNextTurnOf { Controller }`
        // is semantically "until the end of the grantee's next turn" at prune time.
        (
            " until the end of their next turn",
            Duration::UntilEndOfNextTurnOf {
                player: PlayerScope::Controller,
            },
        ),
        (
            " until their next turn",
            Duration::UntilNextTurnOf {
                player: PlayerScope::Controller,
            },
        ),
        (
            " until your next turn",
            Duration::UntilNextTurnOf {
                player: PlayerScope::Controller,
            },
        ),
        (
            " until ~ leaves the battlefield",
            Duration::UntilHostLeavesPlay,
        ),
        (
            " until this creature leaves the battlefield",
            Duration::UntilHostLeavesPlay,
        ),
    ] {
        if lower.ends_with(suffix) {
            let end = text.len() - suffix.len();
            return (text[..end].trim_end_matches(',').trim(), Some(duration));
        }
    }

    // CR 611.2b: "for as long as [condition]" — extract condition from trailing phrase.
    if let Some(pos) = lower.rfind(" for as long as ") {
        let condition_text = &lower[pos + " for as long as ".len()..];
        if let Some(dur) = parse_for_as_long_as_condition(condition_text) {
            let stripped = text[..pos].trim_end_matches(',').trim();
            return (stripped, Some(dur));
        }
    }

    (text, None)
}

fn target_relative_clause_owns_suffix(input: &str) -> bool {
    let Ok((relative_clause, _)) = take_until::<_, _, OracleError<'_>>(" that ").parse(input)
    else {
        return false;
    };
    let Some((_, consumed)) = parse_that_clause_suffix(relative_clause) else {
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

/// CR 611.2b: Parse the condition text after "for as long as" into a Duration variant.
/// Maps known condition phrases to typed Duration/StaticCondition variants:
/// - "~ remains tapped" / "it remains tapped" → ForAsLongAs { SourceIsTapped }
/// - "you control ~" → UntilHostLeavesPlay (existing variant)
/// - "~ remains on the battlefield" → UntilHostLeavesPlay
/// - "it has a {type} counter on it" → ForAsLongAs { HasCounters }
/// - Compound: "you control ~ and it remains tapped" → ForAsLongAs { And [...] }
/// - Unknown → ForAsLongAs { Unrecognized }
fn parse_for_as_long_as_condition(condition: &str) -> Option<Duration> {
    let condition = condition.trim().trim_end_matches('.');

    // Compound: "you control ~ and it remains tapped"
    if let Ok((right, left)) =
        terminated(take_until(" and "), tag::<_, _, OracleError<'_>>(" and ")).parse(condition)
    {
        let left = left.trim();
        let right = right.trim();
        let left_dur = parse_for_as_long_as_condition(left)?;
        let right_dur = parse_for_as_long_as_condition(right)?;
        let left_cond = duration_to_condition(left_dur);
        let right_cond = duration_to_condition(right_dur);
        return Some(Duration::ForAsLongAs {
            condition: StaticCondition::And {
                conditions: vec![left_cond, right_cond],
            },
        });
    }

    // "~ remains tapped" / "it remains tapped" / "this creature remains tapped"
    if scan_contains_phrase(condition, "remains tapped") {
        return Some(Duration::ForAsLongAs {
            condition: StaticCondition::SourceIsTapped,
        });
    }

    // "you control ~" / "you control this creature"
    if tag::<_, _, OracleError<'_>>("you control ")
        .parse(condition)
        .is_ok()
    {
        return Some(Duration::UntilHostLeavesPlay);
    }

    // "~ remains on the battlefield" / "it remains on the battlefield"
    if scan_contains_phrase(condition, "remains on the battlefield") {
        return Some(Duration::UntilHostLeavesPlay);
    }

    // CR 122.1: "<subject> has <quantity> [type] counter[s] on it" — delegate to
    // the shared nom combinator so the typed/bare/quantity grammar lives in a
    // single authority (`parse_inner_condition` family) rather than being
    // duplicated here as bespoke string matching.
    if let Ok((rest, condition)) = nom_condition::parse_source_has_counters(condition) {
        if rest.trim().is_empty() {
            return Some(Duration::ForAsLongAs { condition });
        }
    }

    // Fallback: unrecognized condition text
    Some(Duration::ForAsLongAs {
        condition: StaticCondition::Unrecognized {
            text: condition.to_string(),
        },
    })
}

/// Convert a Duration back into a StaticCondition for compound "and" clauses.
/// UntilHostLeavesPlay maps to IsPresent { filter: None } (source must be on battlefield).
fn duration_to_condition(dur: Duration) -> StaticCondition {
    match dur {
        Duration::ForAsLongAs { condition } => condition,
        Duration::UntilHostLeavesPlay => StaticCondition::IsPresent { filter: None },
        _ => StaticCondition::None,
    }
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

fn parse_controlled_by_different_players_target_constraint(text: &str) -> bool {
    let lower = text.to_lowercase();
    let mut parser = preceded(
        take_until::<_, _, OracleError<'_>>(" controlled by different players"),
        tag(" controlled by different players"),
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
    for (phrase, zone, transformed, enters_under_you, enter_tapped, enters_attacking) in patterns {
        if let Some(pos) = lower.rfind(phrase) {
            let after_destination = &lower[pos + phrase.len()..];
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
                    let trimmed = text[pos + phrase.len()..pos + phrase.len() + off].trim_end();
                    trimmed
                        // allow-noncombinator: structural cleanup of a trailing " and" connector on an already-sliced remainder, not parsing dispatch
                        .strip_suffix(" and")
                        .map(|s| s.trim_end())
                        .unwrap_or(trimmed)
                }
                None => &text[pos + phrase.len()..],
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
        if tag::<_, _, OracleError<'_>>("damage").parse(rem).is_ok() {
            let skip = after_tp.lower.len() - rem.len() + "damage".len();
            let (_, rest) = after_tp.split_at(skip);
            (qty, rest)
        } else {
            return None;
        }
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

    // CR 115.1d: Detect "any number of" quantifier before the target phrase.
    let target_lower = target_text.to_lowercase();
    let (stripped_target_text, multi_target) = if let Ok((rest, _)) =
        tag::<_, _, OracleError<'_>>("any number of ").parse(target_lower.as_str())
    {
        let skip = target_lower.len() - rest.len();
        (
            &target_text[skip..],
            Some(multi_target_for_distribute_among(&amount)),
        )
    } else {
        (target_text, None)
    };
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
    // "distribute " is 11 bytes; Oracle text is ASCII so byte == char offsets.
    let (after_lower, _) = tag::<_, _, OracleError<'_>>("distribute ")
        .parse(lower)
        .ok()?;
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
    let target_text_lower = &lower[target_offset..];
    let (stripped_target, multi_target) = if let Ok((rest, _)) =
        tag::<_, _, OracleError<'_>>("any number of ").parse(target_text_lower)
    {
        let skip = target_text_lower.len() - rest.len();
        (
            &target_text[skip..],
            Some(multi_target_for_distribute_among(&count_expr)),
        )
    } else {
        strip_optional_target_prefix(target_text)
    };
    let (target, _) = parse_target(stripped_target);

    // Verify the "among" comes after the counter word (sanity guard against false matches).
    let expected_min =
        "distribute ".len() + (after_lower.len() - rest_lower.len()) + type_end + counter_word_len;
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
    } else if let Ok((rem, _)) = tag::<_, _, OracleError<'_>>("that much damage").parse(after_lower)
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
            // Parse amount using existing helpers
            let qty = crate::parser::oracle_quantity::parse_event_context_quantity(amount_phrase)
                .or_else(|| {
                    if references_defending_player {
                        ctx.with_player_scope(ControllerRef::DefendingPlayer, |amount_ctx| {
                            parse_cda_quantity_with_context(amount_phrase, amount_ctx)
                        })
                    } else {
                        parse_cda_quantity_with_context(amount_phrase, ctx)
                    }
                });
            if let Some(qty) = qty {
                // Route based on target phrase
                if target_phrase == "itself" {
                    // CR 608.2k: When target is "itself", an anaphoric "its power"
                    // means the target's power. Only `Anaphoric` is remapped — an
                    // explicit possessive ("the sacrificed creature's power",
                    // `CostPaidObject` per CR 608.2k) keeps its fixed referent.
                    let qty = match &qty {
                        QuantityExpr::Ref {
                            qty:
                                QuantityRef::Power {
                                    scope: crate::types::ability::ObjectScope::Anaphoric,
                                },
                        } => QuantityExpr::Ref {
                            qty: QuantityRef::Power {
                                scope: crate::types::ability::ObjectScope::Target,
                            },
                        },
                        other => other.clone(),
                    };
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
        // CR 120.1: The amount of a "deals damage equal to <qty>" clause may be a
        // dynamic count ("the number of creatures you control" — Ajani, Nacatl
        // Avenger). Mirror the sibling "damage to <target> equal to <amount>"
        // branch: try the event-context refs first, then fall back to the general
        // CDA quantity parser (`the number of … you control`, `your life total`,
        // …). Without this fallback the phrase degrades to a raw `Variable`, which
        // resolves to 0 at runtime — the damage silently no-ops.
        let qty = crate::parser::oracle_quantity::parse_event_context_quantity(qty_text)
            .or_else(|| {
                crate::parser::oracle_quantity::parse_cda_quantity_with_context(qty_text, ctx)
            })
            .unwrap_or_else(|| QuantityExpr::Ref {
                qty: QuantityRef::Variable {
                    name: qty_text.to_string(),
                },
            });
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

pub(crate) fn parse_pump_clause(predicate: &str) -> Option<(PtValue, PtValue, Option<Duration>)> {
    let predicate_lower = predicate.to_lowercase();
    let predicate_tp = TextPair::new(predicate, &predicate_lower);
    let (without_where, where_x_expression) = strip_trailing_where_x(predicate_tp);
    // Strip "for each [clause]" suffix before duration extraction.
    let (without_for_each, for_each_qty) =
        strip_trailing_for_each_clause_expr(without_where.original);
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
fn strip_trailing_for_each_clause_expr(text: &str) -> (&str, Option<QuantityExpr>) {
    let lower = text.to_lowercase();
    if let Some(pos) = lower.rfind(" for each ") {
        let clause_text = lower[pos + " for each ".len()..].trim_end_matches('.');
        if let Some(quantity) = parse_for_each_clause_expr(clause_text) {
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
            let expression = after
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

pub(super) fn strip_leading_sequence_connector(text: &str) -> &str {
    let trimmed = text.trim_start();

    if trimmed.eq_ignore_ascii_case("then") {
        return "";
    }

    // Try to strip a leading sequence connector using nom alt().
    // Mixed case requires explicit variants since nom tag() is exact-match.
    match alt((
        tag::<_, _, OracleError<'_>>("Then, "),
        tag("Then "),
        tag("then, "),
        tag("then "),
        tag("and "),
        tag("And "),
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
            return Some(QuantityExpr::Offset {
                inner: Box::new(inner),
                offset: n as i32,
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
        // CR 107.3i + CR 109.4 + CR 109.5: "search/seek for up to X …, where X
        // is …" binds the search count to the defining clause (Oreskos
        // Explorer: "search your library for up to X Plains cards, where X is
        // the number of players who control more lands than you"). The count is
        // an `UpTo` wrapper whose inner `Variable("X")` is rewritten by the
        // recursion arm; the where-clause population is a control comparison
        // (CR 109.4) relative to "you" (CR 109.5).
        | Effect::SearchLibrary { count: amount, .. }
        | Effect::Seek { count: amount, .. }
        | Effect::ExileTop { count: amount, .. }
        | Effect::Discover {
            mana_value_limit: amount,
        }
        | Effect::Incubate { count: amount } => {
            *amount = apply_where_x_quantity_expression(amount.clone(), where_x_expression);
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
        Effect::PreventDamage { amount_dynamic, .. } => {
            if let Some(expr) = where_x_expression {
                *amount_dynamic = parse_where_x_quantity_expression(expr);
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
        Effect::PayCost { cost, .. } => match cost {
            PaymentCost::Life { amount }
            | PaymentCost::Speed { amount }
            | PaymentCost::Energy { amount } => {
                *amount = apply_where_x_quantity_expression(amount.clone(), where_x_expression);
            }
            PaymentCost::ScaledMana { times, .. } => {
                *times = apply_where_x_quantity_expression(times.clone(), where_x_expression);
            }
            PaymentCost::Mana { .. } | PaymentCost::AbilityCost { .. } => {}
        },
        _ => {}
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
    if let Ok((rest, body)) = parse_dynamic_counter_suffix_body(input) {
        return Ok((rest, body));
    }

    // Count: digits, English word, or article ("a"/"an").
    let (rest, count) = nom_primitives::parse_number.parse(input)?;
    let (rest, _) = tag(" ").parse(rest)?;

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
/// `parse_quantity_ref` building block so any "<verb> a number of X
/// counters … equal to …" card parses through the same combinator. CR 614.1c
/// is the authorizing rule for "enters with counters" replacement effects.
pub(crate) fn parse_dynamic_counter_suffix_body(
    input: &str,
) -> nom::IResult<&str, (CounterType, QuantityExpr), OracleError<'_>> {
    let (rest, _) = tag("a number of ").parse(input)?;
    let (rest, type_token) = take_until(" counter").parse(rest)?;
    let counter_type = crate::types::counter::parse_counter_type(type_token);
    let (rest, _) = tag(" counter").parse(rest)?;
    let (rest, _) = nom::combinator::opt(tag::<_, _, OracleError<'_>>("s")).parse(rest)?;
    let (rest, _) = tag(" on it equal to ").parse(rest)?;
    // Quantity: delegate to the shared quantity-ref combinator. Consume the
    // full clause (including any trailing period) so callers see it consumed.
    let qty_text = rest.trim_end_matches('.').trim_end();
    let qty = crate::parser::oracle_quantity::parse_quantity_ref(qty_text).ok_or(
        nom::Err::Error(OracleError::new(rest, nom::error::ErrorKind::Verify)),
    )?;
    Ok(("", (counter_type, QuantityExpr::Ref { qty })))
}

#[cfg(test)]
mod tests {
    use super::{parse_where_x_quantity_expression, strip_return_destination_ext_with_remainder};
    use crate::types::ability::{QuantityExpr, QuantityRef};
    use crate::types::counter::CounterType;
    use crate::types::zones::Zone;

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
}

#[cfg(test)]
mod where_x_tests {
    use super::parse_where_x_quantity_expression;
    use crate::types::ability::{
        ControllerRef, QuantityExpr, QuantityRef, TargetFilter, TypeFilter,
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
