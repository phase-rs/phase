use std::collections::HashMap;

use engine::ai_support::{
    is_targeted_exchange_root, targeted_exchange_verdict, AiDecisionContext, CandidateAction,
    TargetedExchangeVerdict,
};
use engine::game::combat::AttackTarget;
use engine::types::ability::{
    AbilityCondition, ActivationRestriction, CostCategory, Effect, PtValue, TargetFilter, TargetRef,
};
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::game_state::{GameState, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::keywords::Keyword;
use engine::types::mana::ManaType;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;

use crate::combat_ai::is_lethal_attack_available;
use crate::config::AiConfig;
use crate::context::AiContext;
use crate::planner::PreparedCandidate;
use crate::policies::context::{collect_ability_effects, PolicyContext};
use crate::policies::effect_classify::{
    effect_polarity, extract_target_filter, targets_creatures_only, EffectPolarity,
};
use crate::policies::stack_awareness::{has_pending_removal, will_target_die_from_stack};
use crate::policies::strategy_helpers::can_pay_ward_cost;
use crate::search::ability_is_temporary_combat_modifier;
#[cfg(test)]
use engine::types::game_state::CastPaymentMode;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum GateDecision {
    Reject,
    Allow,
    AllowWithPenalty(f64),
}

#[derive(Debug, Clone)]
pub struct GatedCandidate {
    pub source_index: usize,
    pub candidate: CandidateAction,
    pub penalty: f64,
}

/// Layering rule: `tactical_gate` owns rule-derived legality and futility
/// decisions that are provably never useful, such as impossible counters,
/// destroy-vs-indestructible targets, redundant removal on already-dying
/// creatures, and pump with no live combat window. Judgment-weighted
/// preferences stay in `policies/`; the same predicate must not be scored in
/// both layers.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TacticalWindow {
    OwnPreCombatMain,
    OwnPostCombatMain,
    OpponentMain,
    CombatBeforeBlocks,
    CombatAfterBlocks,
    CombatDamage,
    StackResponse,
    EndStep,
    Other,
}

#[derive(Debug, Clone, Copy)]
struct TacticalFacts {
    window: TacticalWindow,
    live_stack_response: bool,
    pass_preserves_stronger_window: bool,
}

impl TacticalFacts {
    fn derive(state: &GameState, ai_player: PlayerId) -> Self {
        let live_stack_response = !state.stack.is_empty();
        let own_turn = engine::game::turn_control::turn_decision_maker(state) == ai_player;
        let window = if live_stack_response {
            TacticalWindow::StackResponse
        } else {
            match state.phase {
                Phase::PreCombatMain if own_turn => TacticalWindow::OwnPreCombatMain,
                Phase::PostCombatMain if own_turn => TacticalWindow::OwnPostCombatMain,
                Phase::PreCombatMain | Phase::PostCombatMain => TacticalWindow::OpponentMain,
                Phase::BeginCombat | Phase::DeclareAttackers => TacticalWindow::CombatBeforeBlocks,
                Phase::DeclareBlockers | Phase::EndCombat => TacticalWindow::CombatAfterBlocks,
                Phase::CombatDamage => TacticalWindow::CombatDamage,
                Phase::End | Phase::Cleanup => TacticalWindow::EndStep,
                _ => TacticalWindow::Other,
            }
        };
        let pass_preserves_stronger_window = own_turn
            && state.stack.is_empty()
            && matches!(
                state.phase,
                Phase::PreCombatMain | Phase::BeginCombat | Phase::DeclareAttackers
            );

        Self {
            window,
            live_stack_response,
            pass_preserves_stronger_window,
        }
    }
}

pub fn gate_candidates(
    state: &GameState,
    decision: &AiDecisionContext,
    candidates: Vec<CandidateAction>,
    ai_player: PlayerId,
    config: &AiConfig,
    context: &AiContext,
) -> Vec<GatedCandidate> {
    let prepared = candidates
        .into_iter()
        .enumerate()
        .map(|(source_index, candidate)| PreparedCandidate {
            source_index,
            candidate,
            payment_successor: None,
        })
        .collect();
    gate_prepared_candidates(state, decision, prepared, ai_player, config, context)
}

pub(crate) fn gate_prepared_candidates(
    state: &GameState,
    decision: &AiDecisionContext,
    candidates: Vec<PreparedCandidate>,
    ai_player: PlayerId,
    config: &AiConfig,
    context: &AiContext,
) -> Vec<GatedCandidate> {
    candidates
        .into_iter()
        .filter_map(|prepared| {
            let source_index = prepared.source_index;
            let candidate = prepared.candidate;
            let decision_result = {
                let policy_ctx = PolicyContext {
                    state,
                    decision,
                    candidate: &candidate,
                    ai_player,
                    config,
                    context,
                    cast_facts: None,
                    search_depth: crate::policies::context::SearchDepth::Root,
                };
                assess_candidate(&policy_ctx)
            };
            match decision_result {
                GateDecision::Reject => None,
                GateDecision::Allow => Some(GatedCandidate {
                    source_index,
                    candidate,
                    penalty: 0.0,
                }),
                GateDecision::AllowWithPenalty(penalty) => Some(GatedCandidate {
                    source_index,
                    candidate,
                    penalty,
                }),
            }
        })
        .collect()
}

fn assess_candidate(ctx: &PolicyContext<'_>) -> GateDecision {
    match &ctx.candidate.action {
        GameAction::CastSpell { .. } | GameAction::ActivateAbility { .. } => assess_pre_cast(ctx),
        GameAction::ChooseTarget {
            target: Some(target),
        } => {
            if let Some(rejection) = reject_futile_target(ctx, target) {
                return rejection;
            }
            let penalty = target_choice_penalty(ctx, target);
            if penalty < 0.0 {
                GateDecision::AllowWithPenalty(penalty)
            } else {
                GateDecision::Allow
            }
        }
        GameAction::ChooseTarget { target: None } => GateDecision::Allow,
        // CR 702.51a (Convoke) / CR 702.126a (Improvise) / Waterbend: a
        // dual-purpose permanent's Colorless convoke-family marker must not be
        // taken while a sibling candidate for the SAME object could still pay
        // an outstanding colored pip via its native mana ability — spending
        // the Colorless marker first permanently strands that pip and
        // dead-ends `ManaPayment` (the Metallic Rebuke bug:
        // `search::fallback_action`'s "can_cast_object_now has a gap" panic).
        // Zero-cost dominance, not a scoring preference: the native ability
        // can always still cover the trailing generic slot afterward.
        GameAction::TapForConvoke {
            object_id,
            mana_type: ManaType::Colorless,
        } => {
            if matches!(ctx.decision.waiting_for, WaitingFor::ManaPayment { .. })
                && crate::mana_colors::convoke_native_tap_still_demanded(
                    ctx.state,
                    &ctx.decision.candidates,
                    *object_id,
                )
            {
                GateDecision::Reject
            } else {
                GateDecision::Allow
            }
        }
        GameAction::SelectTargets { targets } => {
            for target in targets {
                if let Some(rejection) = reject_futile_target(ctx, target) {
                    return rejection;
                }
            }
            let penalty = targets
                .iter()
                .map(|target| target_choice_penalty(ctx, target))
                .sum::<f64>();
            if penalty < 0.0 {
                GateDecision::AllowWithPenalty(penalty)
            } else {
                GateDecision::Allow
            }
        }
        // CR 601.2: Announcing a spell commits the caster — the rules provide
        // no strategic rewind. CancelCast exists only as a mechanical escape
        // when the cast cannot be completed (no legal targets after a
        // replacement effect, unaffordable cost after a cost-increase static).
        // Removing it from the strategic pool prevents regret-based cast/cancel
        // loops — once a ChooseTarget or pay-cost option exists, the AI must
        // pick one. The genuine-escape cases fall through to
        // `search::fallback_action`, which emits CancelCast when the scored
        // pool is empty.
        GameAction::CancelCast => GateDecision::Reject,
        _ => GateDecision::Allow,
    }
}

fn assess_pre_cast(ctx: &PolicyContext<'_>) -> GateDecision {
    // CR 601.2c + CR 608.2c: Target-sourced self-damage and fight exchanges are
    // evaluated from reducer-issued, fully-bound target paths before scoring.
    // `Indeterminate` stays fail-open: this is a proof-backed veto only.
    if is_targeted_exchange_root(&ctx.candidate.action)
        && matches!(
            targeted_exchange_verdict(ctx.state, ctx.candidate),
            TargetedExchangeVerdict::Reject
        )
    {
        return GateDecision::Reject;
    }

    // CR 608.2c: Reject abilities whose source-type condition is known to fail.
    // E.g. Figure of Fable's "{1}{G/W}{G/W}: If this creature is a Scout, ..." when
    // the source is not currently a Scout. The ability is legal to activate but wastes mana.
    if let GameAction::ActivateAbility {
        source_id,
        ability_index,
    } = &ctx.candidate.action
    {
        if let Some(object) = ctx.state.objects.get(source_id) {
            if let Some(ability_def) = object.abilities.get(*ability_index) {
                if let Some(AbilityCondition::SourceMatchesFilter { ref filter }) =
                    ability_def.condition
                {
                    if !engine::game::filter::matches_target_filter(
                        ctx.state,
                        *source_id,
                        filter,
                        &engine::game::filter::FilterContext::from_source(ctx.state, *source_id),
                    ) {
                        return GateDecision::Reject;
                    }
                }
            }
        }
    }

    // When a lethal attack is available (opponent has no untapped blockers and AI has
    // enough power to kill them), reject pre-combat main spells. Attacking first is
    // almost always correct — spending mana dorks or convoke creatures before attacking
    // removes them from the attack and misses the lethal window.
    if matches!(
        TacticalFacts::derive(ctx.state, ctx.ai_player).window,
        TacticalWindow::OwnPreCombatMain
    ) && is_lethal_attack_available(ctx.state, ctx.ai_player)
    {
        // Carve-out: direct-damage spells that can target players may themselves be
        // lethal or supplement the attack — allow with a mild penalty.
        let effects = ctx.effects();
        let is_direct_damage = effects.iter().any(|e| {
            matches!(
                e,
                Effect::DealDamage {
                    target: TargetFilter::Any | TargetFilter::Player,
                    ..
                }
            )
        });
        if is_direct_damage {
            return GateDecision::AllowWithPenalty(-5.0);
        }
        return GateDecision::Reject;
    }

    let effects = ctx.effects();
    if effects.is_empty() {
        return GateDecision::Allow;
    }

    if effects
        .iter()
        .any(|effect| matches!(effect, Effect::Counter { .. }))
        && (ctx.state.stack.is_empty()
            || ctx
                .state
                .stack
                .iter()
                .all(|entry| entry.controller == ctx.ai_player))
    {
        return GateDecision::Reject;
    }

    if is_redundant_creature_only_removal(ctx, &effects) {
        return GateDecision::Reject;
    }

    if let Some((power_bonus, toughness_bonus)) = pure_fixed_pump_bonus(&effects) {
        let source_is_spell = ctx.source_object().is_some_and(|source| {
            source.card_types.core_types.contains(&CoreType::Instant)
                || source.card_types.core_types.contains(&CoreType::Sorcery)
        });
        if source_is_spell {
            let facts = TacticalFacts::derive(ctx.state, ctx.ai_player);
            if should_reject_pump_window(ctx, &facts, power_bonus, toughness_bonus) {
                return GateDecision::Reject;
            }
            if facts.pass_preserves_stronger_window && !facts.live_stack_response {
                return GateDecision::AllowWithPenalty(-1.0);
            }
        } else if let Some(source_id) = activated_ueot_pump_source(ctx) {
            // CR 514.2: an "until end of turn" pump ends at cleanup, so an
            // activation outside a live combat/stack window buys nothing —
            // the same waste the spell branch above rejects. Complementary to
            // `search::empty_stack_activation_is_low_value`, which only lets
            // the priority fast path skip searching when EVERY candidate is
            // low value; this is the per-candidate bound.
            let facts = TacticalFacts::derive(ctx.state, ctx.ai_player);
            // A repeatable mana-sink pump on our own unblocked attacker after
            // blockers are declared is a value judgment (how much face damage
            // is this mana worth?), not a provable waste — leave it to policy
            // scoring rather than the categorical gate.
            let mana_sink_into_open_board = power_bonus > 0
                && matches!(
                    facts.window,
                    TacticalWindow::CombatAfterBlocks | TacticalWindow::CombatDamage
                )
                && self_pump_on_unblocked_attacker(ctx.state, ctx.ai_player, source_id, &effects);
            if !mana_sink_into_open_board {
                if should_reject_pump_window(ctx, &facts, power_bonus, toughness_bonus) {
                    return GateDecision::Reject;
                }
                if facts.pass_preserves_stronger_window && !facts.live_stack_response {
                    return GateDecision::AllowWithPenalty(-1.0);
                }
            }
        }
    }

    GateDecision::Allow
}

/// Hard-reject targets that are provably futile (e.g., destroy vs indestructible).
/// Called before `target_choice_penalty` so these never reach scoring.
fn reject_futile_target(ctx: &PolicyContext<'_>, target: &TargetRef) -> Option<GateDecision> {
    let TargetRef::Object(object_id) = target else {
        return None;
    };
    let object = ctx.state.objects.get(object_id)?;
    let effects = ctx.effects();

    // CR 701.8 + CR 702.12b: destroy-based removal can't destroy an
    // indestructible permanent.
    let is_destroy = effects.iter().any(|e| matches!(e, Effect::Destroy { .. }));
    if is_destroy && object.has_keyword(&Keyword::Indestructible) {
        return Some(GateDecision::Reject);
    }

    // CR 702.12b: an indestructible creature ignores the lethal-damage SBA
    // (CR 704.5g), so a damage-only spell can NEVER kill it regardless of the
    // amount — provably futile. Shrink effects are exempt: reducing toughness to
    // 0 kills via CR 704.5f (which indestructible does not prevent), and two
    // shrink spells can combine, so those stay in the judgment layer.
    if object.has_keyword(&Keyword::Indestructible)
        && deals_damage(&effects)
        && !has_toughness_shrink(&effects)
    {
        return Some(GateDecision::Reject);
    }

    // CR 702.21a: targeting a warded permanent triggers ward; if the AI can't
    // pay the cost, the spell is simply countered — a strict card-down. Never
    // choose such a target.
    for keyword in &object.keywords {
        if let Keyword::Ward(ward) = keyword {
            if !can_pay_ward_cost(ctx, ward, object) {
                return Some(GateDecision::Reject);
            }
            break;
        }
    }

    None
}

/// Whether any effect deals damage (fixed or variable).
fn deals_damage(effects: &[&Effect]) -> bool {
    effects
        .iter()
        .any(|e| matches!(e, Effect::DealDamage { .. }))
}

/// Whether any effect reduces toughness via a negative `Pump` or a negative P/T
/// counter. Variable pump toughness is treated as possible shrink (conservative:
/// never hard-reject a line that might reduce toughness to 0).
fn has_toughness_shrink(effects: &[&Effect]) -> bool {
    effects.iter().any(|e| match e {
        Effect::Pump { toughness, .. } => match toughness {
            PtValue::Fixed(v) => *v < 0,
            PtValue::Variable(_) | PtValue::Quantity(_) => true,
        },
        Effect::PutCounter { counter_type, .. } => counter_type
            .power_toughness_delta()
            .is_some_and(|(_, t)| t < 0),
        _ => false,
    })
}

fn target_choice_penalty(ctx: &PolicyContext<'_>, target: &TargetRef) -> f64 {
    let TargetRef::Object(object_id) = target else {
        return 0.0;
    };

    let effects = ctx.effects();

    // Pumping a tapped creature not participating in combat deals no combat benefit.
    // CR 508.1d / CR 509.1a: Both attackers and blockers can benefit from pump.
    let is_pump = effects.iter().any(|e| matches!(e, Effect::Pump { .. }));
    if is_pump {
        if let Some(object) = ctx.state.objects.get(object_id) {
            if object.tapped {
                let in_combat = ctx.state.combat.as_ref().is_some_and(|c| {
                    c.attackers.iter().any(|a| a.object_id == *object_id)
                        || c.blocker_to_attacker.contains_key(object_id)
                });
                if !in_combat {
                    return -8.0;
                }
            }
        }
    }

    let harmful = effects
        .iter()
        .any(|effect| matches!(effect_polarity(effect), EffectPolarity::Harmful));
    if harmful
        && has_pending_removal(ctx.state, *object_id)
        && will_target_die_from_stack(ctx.state, *object_id)
    {
        -10.0
    } else {
        0.0
    }
}

fn is_redundant_creature_only_removal(ctx: &PolicyContext<'_>, effects: &[&Effect]) -> bool {
    // The source supplies the targeting quality (color/type) the engine needs
    // to evaluate Protection / HexproofFrom; without it, fail open.
    let Some(source) = ctx.source_object() else {
        return false;
    };

    // A MIXED spell carrying a useful MASS wipe is never "redundant creature-only
    // removal": the wipe's NON-targeted population (CR 115.10a) is an independent
    // line that can clear creatures hexproof/protected FROM TARGETING (CR 702.11b)
    // — so a creature-only half with no live opponent TARGET must not suppress the
    // cast. Consult the resolver-mirroring mass seam (`ctx.has_opposing_mass_population`)
    // before declaring redundancy.
    if ctx.has_opposing_mass_population() {
        return false;
    }

    let mut saw_creature_only_harm = false;
    for effect in effects {
        if !(matches!(effect_polarity(effect), EffectPolarity::Harmful)
            && targets_creatures_only(effect))
        {
            continue;
        }
        saw_creature_only_harm = true;
        let Some(filter) = extract_target_filter(effect) else {
            // Can't analyze the filter — not provably redundant.
            return false;
        };
        // CR 702.11/702.16/702.18 + CR 608.2b: defer targeting legality to the
        // engine (Shroud, Hexproof-vs-opponents, "Hexproof from [quality]",
        // Protection, ignore-hexproof) instead of re-checking keywords here.
        let has_live_opponent_target =
            ctx.has_legal_opponent_creature_target(filter, source.id, |id| {
                // A target already dying to a stack effect is not a reason to
                // keep this redundant removal.
                !will_target_die_from_stack(ctx.state, id)
            });
        if has_live_opponent_target {
            return false;
        }
    }

    saw_creature_only_harm
}

fn pure_fixed_pump_bonus(effects: &[&Effect]) -> Option<(i32, i32)> {
    if effects.is_empty()
        || !effects
            .iter()
            .all(|effect| matches!(effect, Effect::Pump { .. }))
    {
        return None;
    }

    let mut power_bonus = 0;
    let mut toughness_bonus = 0;
    for effect in effects {
        let Effect::Pump {
            power, toughness, ..
        } = effect
        else {
            return None;
        };
        let PtValue::Fixed(power) = power else {
            return None;
        };
        let PtValue::Fixed(toughness) = toughness else {
            return None;
        };
        power_bonus += *power;
        toughness_bonus += *toughness;
    }
    Some((power_bonus, toughness_bonus))
}

/// The activated ability behind an in-class temporary-pump candidate, or
/// `None` when the candidate is not one. Returns the ability's source object
/// id (the permanent whose ability is being activated).
///
/// `ability_is_temporary_combat_modifier` reads the DEFINITION — `ctx.effects()`
/// flattens the chain and drops `duration`, so a permanent pump would be
/// indistinguishable from an "until end of turn" one there.
///
/// Every check here is card-local and runs BEFORE `TacticalFacts::derive`, so a
/// candidate outside the class costs one map lookup and a cost-category walk.
/// The exclusions:
/// - sacrifice / loyalty / discard costs: the activation's payoff is the cost
///   itself (a sacrifice outlet, a planeswalker's loyalty economy, a discard
///   enabler), priced by `free_outlet_activation` / `self_cost_value`, so the
///   pump window is the wrong lens.
/// - `AsSorcery`: there is no later, stronger window to save the ability for.
///
/// PayLife stays in class — Desolation Prowler is the reported card, and its
/// life cost is priced separately by the self-cost policies.
fn activated_ueot_pump_source(ctx: &PolicyContext<'_>) -> Option<ObjectId> {
    let GameAction::ActivateAbility {
        source_id,
        ability_index,
    } = &ctx.candidate.action
    else {
        return None;
    };
    let ability = ctx
        .state
        .objects
        .get(source_id)?
        .abilities
        .get(*ability_index)?;
    if !ability_is_temporary_combat_modifier(ability) {
        return None;
    }
    let cost_is_its_own_payoff = ability.cost.as_ref().is_some_and(|cost| {
        cost.categories().iter().any(|category| {
            matches!(
                category,
                CostCategory::SacrificesPermanent
                    | CostCategory::PaysLoyalty
                    | CostCategory::Discards
            )
        })
    });
    if cost_is_its_own_payoff
        || ability
            .activation_restrictions
            .contains(&ActivationRestriction::AsSorcery)
    {
        return None;
    }
    Some(*source_id)
}

/// Whether the pump lands on the ability's own source (`SelfRef`) and that
/// permanent is one of the AI's unblocked attackers — CR 509.1h: an attacking
/// creature no blocker was assigned to remains unblocked.
///
/// CR 510.3: once combat damage has been dealt the active player gets priority
/// again while still inside the combat phase, and `Phase::EndCombat` maps to
/// `CombatAfterBlocks` too. Extra power at that point buys nothing, so
/// `regular_damage_done` closes the stand-down.
fn self_pump_on_unblocked_attacker(
    state: &GameState,
    ai_player: PlayerId,
    source_id: ObjectId,
    effects: &[&Effect],
) -> bool {
    if !effects.iter().all(|effect| {
        matches!(
            effect,
            Effect::Pump {
                target: TargetFilter::SelfRef,
                ..
            }
        )
    }) {
        return false;
    }
    if state
        .objects
        .get(&source_id)
        .is_none_or(|source| source.controller != ai_player)
    {
        return false;
    }
    let Some(combat) = &state.combat else {
        return false;
    };
    if combat.regular_damage_done {
        return false;
    }
    combat.attackers.iter().any(|attacker| {
        attacker.object_id == source_id
            && !attacker.blocked
            && combat
                .blocker_assignments
                .get(&source_id)
                .is_none_or(|blockers| blockers.is_empty())
    })
}

fn should_reject_pump_window(
    ctx: &PolicyContext<'_>,
    facts: &TacticalFacts,
    power_bonus: i32,
    toughness_bonus: i32,
) -> bool {
    if facts.live_stack_response
        && pump_can_save_from_hostile_stack(ctx.state, ctx.ai_player, toughness_bonus)
    {
        return false;
    }

    match facts.window {
        TacticalWindow::OwnPostCombatMain
        | TacticalWindow::OpponentMain
        | TacticalWindow::EndStep => {
            return true;
        }
        TacticalWindow::OwnPreCombatMain | TacticalWindow::CombatBeforeBlocks => {
            return facts.pass_preserves_stronger_window;
        }
        TacticalWindow::Other => return true,
        TacticalWindow::CombatAfterBlocks
        | TacticalWindow::CombatDamage
        | TacticalWindow::StackResponse => {}
    }

    !pump_changes_combat_outcome(ctx.state, ctx.ai_player, power_bonus, toughness_bonus)
}

/// Check if pumping can actually save a creature from hostile stack effects.
/// Destroy/Exile/Counter/Bounce kill regardless of stats — pump doesn't help.
/// Only damage-based removal can be survived with a toughness boost.
fn pump_can_save_from_hostile_stack(
    state: &GameState,
    ai_player: PlayerId,
    toughness_bonus: i32,
) -> bool {
    use engine::types::ability::QuantityExpr;

    state.stack.iter().any(|entry| {
        let Some(ability) = entry.ability() else {
            return false;
        };
        ability.targets.iter().any(|target| {
            let TargetRef::Object(object_id) = target else {
                return false;
            };
            let Some(object) = state.objects.get(object_id) else {
                return false;
            };
            if object.controller != ai_player
                || !object.card_types.core_types.contains(&CoreType::Creature)
            {
                return false;
            }

            let effects = collect_ability_effects(ability);
            for effect in &effects {
                match effect {
                    // Destroy/Exile/Counter/Bounce — pump doesn't save
                    Effect::Destroy { .. } | Effect::Counter { .. } | Effect::Bounce { .. } => {
                        return false
                    }
                    Effect::ChangeZone { .. } => return false,
                    // Damage — pump saves if toughness + bonus > damage
                    Effect::DealDamage {
                        amount: QuantityExpr::Fixed { value },
                        ..
                    } => {
                        let toughness = object.toughness.unwrap_or(0);
                        let remaining = toughness - object.damage_marked as i32;
                        if remaining + toughness_bonus > *value {
                            return true;
                        }
                    }
                    _ => {}
                }
            }
            false
        })
    })
}

fn pump_changes_combat_outcome(
    state: &GameState,
    ai_player: PlayerId,
    power_bonus: i32,
    toughness_bonus: i32,
) -> bool {
    let Some(combat) = &state.combat else {
        return false;
    };

    // CR 509.1: Aggregate AI's unblocked damage per defending player. A 4-player
    // pod where AI attacks player A for 5 and player B for 5 must NOT report
    // 10 unblocked damage when checking if pumping makes A's lethal threshold —
    // only attackers heading to A count toward A's life threat.
    let mut unblocked_per_defender: HashMap<PlayerId, i32> = HashMap::new();
    for attacker in &combat.attackers {
        let Some(attacker_obj) = state.objects.get(&attacker.object_id) else {
            continue;
        };
        if attacker_obj.controller != ai_player {
            continue;
        }
        let blocked = attacker.blocked
            || combat
                .blocker_assignments
                .get(&attacker.object_id)
                .is_some_and(|blockers| !blockers.is_empty());
        if !blocked {
            if let AttackTarget::Player(defending) = attacker.attack_target {
                *unblocked_per_defender.entry(defending).or_insert(0) +=
                    attacker_obj.power.unwrap_or(0);
            }
        }
    }

    for attacker in &combat.attackers {
        let Some(attacker_obj) = state.objects.get(&attacker.object_id) else {
            continue;
        };
        let blockers = combat
            .blocker_assignments
            .get(&attacker.object_id)
            .cloned()
            .unwrap_or_default();

        if attacker_obj.controller == ai_player {
            if blockers.is_empty() {
                let total_for_defender = match attacker.attack_target {
                    AttackTarget::Player(pid) => {
                        unblocked_per_defender.get(&pid).copied().unwrap_or(0)
                    }
                    _ => 0,
                };
                if unblocked_attack_becomes_lethal(state, attacker, total_for_defender, power_bonus)
                {
                    return true;
                }
                continue;
            }

            if blockers.len() == 1
                && combat_trade_improves(
                    state,
                    attacker.object_id,
                    blockers[0],
                    power_bonus,
                    toughness_bonus,
                )
            {
                return true;
            }
        } else {
            for blocker_id in
                combat
                    .blocker_to_attacker
                    .iter()
                    .filter_map(|(blocker_id, attacker_ids)| {
                        attacker_ids
                            .contains(&attacker.object_id)
                            .then_some(*blocker_id)
                    })
            {
                let Some(blocker_obj) = state.objects.get(&blocker_id) else {
                    continue;
                };
                if blocker_obj.controller == ai_player
                    && combat_trade_improves(
                        state,
                        blocker_id,
                        attacker.object_id,
                        power_bonus,
                        toughness_bonus,
                    )
                {
                    return true;
                }
            }
        }
    }

    false
}

fn combat_trade_improves(
    state: &GameState,
    my_creature_id: ObjectId,
    opposing_creature_id: ObjectId,
    power_bonus: i32,
    toughness_bonus: i32,
) -> bool {
    let Some(my_creature) = state.objects.get(&my_creature_id) else {
        return false;
    };
    let Some(opposing_creature) = state.objects.get(&opposing_creature_id) else {
        return false;
    };

    let my_power = my_creature.power.unwrap_or(0);
    let my_toughness = my_creature.toughness.unwrap_or(0) - my_creature.damage_marked as i32;
    let opposing_power = opposing_creature.power.unwrap_or(0);
    let opposing_toughness =
        opposing_creature.toughness.unwrap_or(0) - opposing_creature.damage_marked as i32;

    let dies_without_pump = my_toughness <= opposing_power;
    let survives_with_pump = my_toughness + toughness_bonus > opposing_power;
    if dies_without_pump && survives_with_pump {
        return true;
    }

    let fails_to_kill_without_pump = my_power < opposing_toughness;
    let kills_with_pump = my_power + power_bonus >= opposing_toughness;
    fails_to_kill_without_pump && kills_with_pump
}

fn unblocked_attack_becomes_lethal(
    state: &GameState,
    attacker: &engine::game::combat::AttackerInfo,
    total_unblocked_damage: i32,
    power_bonus: i32,
) -> bool {
    let AttackTarget::Player(defending_player) = attacker.attack_target else {
        return false;
    };
    let life = state.players[defending_player.0 as usize].life;
    total_unblocked_damage < life && total_unblocked_damage + power_bonus >= life
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{create_config, AiDifficulty, Platform};
    use engine::ai_support::{ActionMetadata, TacticalClass};
    use engine::game::combat::{AttackerInfo, CombatState};
    use engine::game::scenario::{GameScenario, P0, P1};
    use engine::types::ability::{BounceSelection, EffectKind, ResolvedAbility, TargetFilter};
    use engine::types::ability::{
        QuantityModification, ReplacementDefinition, ReplacementPlayerScope,
    };
    use engine::types::game_state::{
        PendingCast, StackEntry, StackEntryKind, TargetEffectDetail, TargetSelectionProgress,
        TargetSelectionSlot, WaitingFor,
    };
    use engine::types::identifiers::CardId;
    use engine::types::keywords::WardCost;
    use engine::types::mana::ManaCost;
    use engine::types::replacements::ReplacementEvent;
    use std::sync::Arc;

    #[test]
    fn rejects_pump_after_combat_without_live_threat() {
        let mut scenario = GameScenario::new();
        scenario.add_creature(P0, "Bear", 2, 2);
        let growth = scenario
            .add_spell_to_hand_from_oracle(
                P0,
                "Giant Growth",
                true,
                "Target creature gets +3/+3 until end of turn.",
            )
            .id();

        let mut runner = scenario.build();
        let state = runner.state_mut();
        state.phase = Phase::PostCombatMain;
        state.active_player = P1;
        state.priority_player = P0;
        state.waiting_for = WaitingFor::Priority { player: P0 };

        let config = create_config(AiDifficulty::VeryHard, Platform::Wasm);
        let decision = AiDecisionContext {
            waiting_for: state.waiting_for.clone(),
            candidates: Vec::new(),
        };
        let candidate = CandidateAction {
            action: GameAction::CastSpell {
                object_id: growth,
                card_id: state.objects.get(&growth).unwrap().card_id,
                targets: Vec::new(),

                payment_mode: CastPaymentMode::Auto,
            },
            metadata: ActionMetadata::for_actor(Some(P0), TacticalClass::Spell),
        };
        let ctx = PolicyContext {
            state,
            decision: &decision,
            candidate: &candidate,
            ai_player: P0,
            config: &config,
            context: &AiContext::empty(&config.weights),
            cast_facts: None,
            search_depth: crate::policies::context::SearchDepth::Root,
        };

        assert_eq!(assess_candidate(&ctx), GateDecision::Reject);
    }

    #[test]
    fn allows_pump_that_wins_combat() {
        let mut scenario = GameScenario::new();
        let attacker = scenario.add_creature(P0, "Attacker", 2, 2).id();
        let blocker = scenario.add_creature(P1, "Blocker", 4, 4).id();
        let growth = scenario
            .add_spell_to_hand_from_oracle(
                P0,
                "Giant Growth",
                true,
                "Target creature gets +3/+3 until end of turn.",
            )
            .id();

        let mut runner = scenario.build();
        let state = runner.state_mut();
        state.phase = Phase::DeclareBlockers;
        state.active_player = P0;
        state.priority_player = P0;
        state.waiting_for = WaitingFor::Priority { player: P0 };
        state.combat = Some(CombatState {
            attackers: vec![AttackerInfo::attacking_player(attacker, P1)],
            blocker_assignments: [(attacker, vec![blocker])].into_iter().collect(),
            blocker_to_attacker: [(blocker, vec![attacker])].into_iter().collect(),
            ..Default::default()
        });

        let config = create_config(AiDifficulty::VeryHard, Platform::Wasm);
        let decision = AiDecisionContext {
            waiting_for: state.waiting_for.clone(),
            candidates: Vec::new(),
        };
        let candidate = CandidateAction {
            action: GameAction::CastSpell {
                object_id: growth,
                card_id: state.objects.get(&growth).unwrap().card_id,
                targets: Vec::new(),

                payment_mode: CastPaymentMode::Auto,
            },
            metadata: ActionMetadata::for_actor(Some(P0), TacticalClass::Spell),
        };
        let ctx = PolicyContext {
            state,
            decision: &decision,
            candidate: &candidate,
            ai_player: P0,
            config: &config,
            context: &AiContext::empty(&config.weights),
            cast_facts: None,
            search_depth: crate::policies::context::SearchDepth::Root,
        };

        assert_ne!(assess_candidate(&ctx), GateDecision::Reject);
    }

    #[test]
    fn penalizes_targeting_already_dead_creature() {
        let mut scenario = GameScenario::new();
        let creature = scenario.add_creature(P1, "Target", 2, 2).id();
        let mut runner = scenario.build();
        let state = runner.state_mut();
        state.stack.push_back(StackEntry {
            id: ObjectId(200),
            source_id: ObjectId(201),
            controller: P0,
            kind: StackEntryKind::Spell {
                ability: Some(Box::new(ResolvedAbility::new(
                    Effect::Destroy {
                        target: TargetFilter::Any,
                        cant_regenerate: false,
                    },
                    vec![TargetRef::Object(creature)],
                    ObjectId(201),
                    P0,
                ))),
                card_id: CardId(201),
                casting_variant: Default::default(),
                actual_mana_spent: 0,
            },
        });
        let decision = AiDecisionContext {
            waiting_for: WaitingFor::TargetSelection {
                player: P0,
                pending_cast: Box::new(PendingCast::new(
                    ObjectId(202),
                    CardId(202),
                    ResolvedAbility::new(
                        Effect::Destroy {
                            target: TargetFilter::Any,
                            cant_regenerate: false,
                        },
                        Vec::new(),
                        ObjectId(202),
                        P0,
                    ),
                    ManaCost::zero(),
                )),
                target_slots: vec![TargetSelectionSlot {
                    legal_targets: vec![TargetRef::Object(creature)],
                    optional: false,
                    chooser: None,
                    effect_kind: EffectKind::NoOp,
                    effect_detail: TargetEffectDetail::None,
                }],
                mode_labels: Vec::new(),
                selection: TargetSelectionProgress::default(),
            },
            candidates: Vec::new(),
        };
        let candidate = CandidateAction {
            action: GameAction::ChooseTarget {
                target: Some(TargetRef::Object(creature)),
            },
            metadata: ActionMetadata::for_actor(Some(P0), TacticalClass::Target),
        };
        let config = create_config(AiDifficulty::VeryHard, Platform::Wasm);
        let ctx = PolicyContext {
            state,
            decision: &decision,
            candidate: &candidate,
            ai_player: P0,
            config: &config,
            context: &AiContext::empty(&config.weights),
            cast_facts: None,
            search_depth: crate::policies::context::SearchDepth::Root,
        };

        assert_eq!(
            assess_candidate(&ctx),
            GateDecision::AllowWithPenalty(-10.0)
        );
    }

    /// CR 601.2: A spell is cast the moment it's announced — the rules provide
    /// no strategic rewind. The AI's strategic pool must reject CancelCast so
    /// that pre-cast commitment stays coherent with targeting and payment.
    /// (The fallback_action escape in `search.rs` still supplies CancelCast
    /// when the scored pool is empty, covering genuine "can't complete cast"
    /// cases like unaffordable post-cost-increase mana or all targets gone.)
    #[test]
    fn rejects_cancel_cast_as_strategic_candidate() {
        let mut scenario = GameScenario::new();
        let creature = scenario.add_creature(P1, "Elvish Mystic", 1, 1).id();
        let unsummon = scenario
            .add_spell_to_hand_from_oracle(
                P0,
                "Unsummon",
                true,
                "Return target creature to its owner's hand.",
            )
            .id();
        let mut runner = scenario.build();
        let state = runner.state_mut();
        state.waiting_for = WaitingFor::TargetSelection {
            player: P0,
            pending_cast: Box::new(PendingCast::new(
                unsummon,
                state.objects.get(&unsummon).unwrap().card_id,
                ResolvedAbility::new(
                    Effect::Bounce {
                        target: TargetFilter::Any,
                        destination: None,
                        selection: BounceSelection::Targeted,
                    },
                    Vec::new(),
                    unsummon,
                    P0,
                ),
                ManaCost::zero(),
            )),
            target_slots: vec![TargetSelectionSlot {
                legal_targets: vec![TargetRef::Object(creature)],
                optional: false,
                chooser: None,
                effect_kind: EffectKind::NoOp,
                effect_detail: TargetEffectDetail::None,
            }],
            mode_labels: Vec::new(),
            selection: TargetSelectionProgress::default(),
        };

        let config = create_config(AiDifficulty::VeryHard, Platform::Wasm);
        let decision = AiDecisionContext {
            waiting_for: state.waiting_for.clone(),
            candidates: Vec::new(),
        };
        let candidate = CandidateAction {
            action: GameAction::CancelCast,
            metadata: ActionMetadata::for_actor(Some(P0), TacticalClass::Pass),
        };
        let ctx = PolicyContext {
            state,
            decision: &decision,
            candidate: &candidate,
            ai_player: P0,
            config: &config,
            context: &AiContext::empty(&config.weights),
            cast_facts: None,
            search_depth: crate::policies::context::SearchDepth::Root,
        };

        assert_eq!(assess_candidate(&ctx), GateDecision::Reject);
    }

    /// Build a `ChooseTarget` decision for a damage spell aimed at `creature`.
    fn damage_target_decision(creature: ObjectId, damage: i32) -> AiDecisionContext {
        AiDecisionContext {
            waiting_for: WaitingFor::TargetSelection {
                player: P0,
                pending_cast: Box::new(PendingCast::new(
                    ObjectId(900),
                    CardId(900),
                    ResolvedAbility::new(
                        Effect::DealDamage {
                            amount: engine::types::ability::QuantityExpr::Fixed { value: damage },
                            target: TargetFilter::Any,
                            damage_source: None,
                            excess: None,
                        },
                        Vec::new(),
                        ObjectId(900),
                        P0,
                    ),
                    ManaCost::zero(),
                )),
                target_slots: vec![TargetSelectionSlot {
                    legal_targets: vec![TargetRef::Object(creature)],
                    optional: false,
                    chooser: None,
                    effect_kind: EffectKind::NoOp,
                    effect_detail: TargetEffectDetail::None,
                }],
                mode_labels: Vec::new(),
                selection: TargetSelectionProgress::default(),
            },
            candidates: Vec::new(),
        }
    }

    fn choose_target_candidate(creature: ObjectId) -> CandidateAction {
        CandidateAction {
            action: GameAction::ChooseTarget {
                target: Some(TargetRef::Object(creature)),
            },
            metadata: ActionMetadata::for_actor(Some(P0), TacticalClass::Target),
        }
    }

    /// CR 702.12b: a damage-only spell can never kill an indestructible creature.
    #[test]
    fn rejects_damage_targeting_indestructible_creature() {
        let mut scenario = GameScenario::new();
        let creature = scenario
            .add_creature(P1, "Darksteel Wall", 0, 4)
            .with_keyword(Keyword::Indestructible)
            .id();
        let mut runner = scenario.build();
        let state = runner.state_mut();
        let decision = damage_target_decision(creature, 3);
        let candidate = choose_target_candidate(creature);
        let config = create_config(AiDifficulty::VeryHard, Platform::Wasm);
        let ctx = PolicyContext {
            state,
            decision: &decision,
            candidate: &candidate,
            ai_player: P0,
            config: &config,
            context: &AiContext::empty(&config.weights),
            cast_facts: None,
            search_depth: crate::policies::context::SearchDepth::Root,
        };
        assert_eq!(assess_candidate(&ctx), GateDecision::Reject);
    }

    /// A normal creature targeted by damage is NOT gate-rejected — non-lethal
    /// burn is a judgment-layer preference, not a provable futility.
    #[test]
    fn allows_damage_targeting_normal_creature() {
        let mut scenario = GameScenario::new();
        let creature = scenario.add_creature(P1, "Wall", 0, 4).id();
        let mut runner = scenario.build();
        let state = runner.state_mut();
        let decision = damage_target_decision(creature, 3);
        let candidate = choose_target_candidate(creature);
        let config = create_config(AiDifficulty::VeryHard, Platform::Wasm);
        let ctx = PolicyContext {
            state,
            decision: &decision,
            candidate: &candidate,
            ai_player: P0,
            config: &config,
            context: &AiContext::empty(&config.weights),
            cast_facts: None,
            search_depth: crate::policies::context::SearchDepth::Root,
        };
        assert_ne!(assess_candidate(&ctx), GateDecision::Reject);
    }

    /// CR 702.21a: never target a warded creature whose ward cost the AI can't
    /// pay — the spell would just be countered.
    #[test]
    fn rejects_targeting_unpayable_ward() {
        let mut scenario = GameScenario::new();
        let creature = scenario
            .add_creature(P1, "Warded", 2, 2)
            .with_keyword(Keyword::Ward(WardCost::PayLife(100)))
            .id();
        let mut runner = scenario.build();
        let state = runner.state_mut();
        // P0 starts at 20 life — paying 100 is impossible.
        let decision = damage_target_decision(creature, 3);
        let candidate = choose_target_candidate(creature);
        let config = create_config(AiDifficulty::VeryHard, Platform::Wasm);
        let ctx = PolicyContext {
            state,
            decision: &decision,
            candidate: &candidate,
            ai_player: P0,
            config: &config,
            context: &AiContext::empty(&config.weights),
            cast_facts: None,
            search_depth: crate::policies::context::SearchDepth::Root,
        };
        assert_eq!(assess_candidate(&ctx), GateDecision::Reject);
    }

    /// A payable ward does not gate the target out — it's only priced (in the
    /// judgment layer), not vetoed.
    #[test]
    fn allows_targeting_payable_ward() {
        let mut scenario = GameScenario::new();
        let creature = scenario
            .add_creature(P1, "Warded", 2, 2)
            .with_keyword(Keyword::Ward(WardCost::PayLife(2)))
            .id();
        let mut runner = scenario.build();
        let state = runner.state_mut();
        let decision = damage_target_decision(creature, 3);
        let candidate = choose_target_candidate(creature);
        let config = create_config(AiDifficulty::VeryHard, Platform::Wasm);
        let ctx = PolicyContext {
            state,
            decision: &decision,
            candidate: &candidate,
            ai_player: P0,
            config: &config,
            context: &AiContext::empty(&config.weights),
            cast_facts: None,
            search_depth: crate::policies::context::SearchDepth::Root,
        };
        assert_ne!(assess_candidate(&ctx), GateDecision::Reject);
    }

    /// CR 702.21a + CR 119.4: Phyrexian Fleshgorger's Ward uses its current
    /// power as the life payment, so targeting is futile at or below that
    /// power and remains available when the AI can pay without losing.
    #[test]
    fn dynamic_life_ward_uses_the_warded_creatures_current_power() {
        const FLESHGORGER: &str =
            "Menace, lifelink\nWard—Pay life equal to Phyrexian Fleshgorger's power.";

        let gate_for = |life, current_power| {
            let mut scenario = GameScenario::new();
            scenario.with_life(P0, life);
            let creature = scenario
                .add_creature_from_oracle(P1, "Phyrexian Fleshgorger", 7, 5, FLESHGORGER)
                .id();
            let mut runner = scenario.build();
            let state = runner.state_mut();
            let fleshgorger = state.objects.get_mut(&creature).unwrap();
            fleshgorger.base_power = Some(current_power);
            fleshgorger.power = Some(current_power);

            let decision = damage_target_decision(creature, 3);
            let candidate = choose_target_candidate(creature);
            let config = create_config(AiDifficulty::VeryHard, Platform::Wasm);
            let ctx = PolicyContext {
                state,
                decision: &decision,
                candidate: &candidate,
                ai_player: P0,
                config: &config,
                context: &AiContext::empty(&config.weights),
                cast_facts: None,
                search_depth: crate::policies::context::SearchDepth::Root,
            };
            assess_candidate(&ctx)
        };

        assert_eq!(gate_for(6, 7), GateDecision::Reject);
        assert_eq!(gate_for(7, 7), GateDecision::Reject);
        assert_ne!(gate_for(8, 7), GateDecision::Reject);
        assert_ne!(gate_for(4, 3), GateDecision::Reject);
    }

    /// CR 702.21a + CR 104.3d: a Ward payment that would push the AI to ten
    /// or more poison counters must reject the target — the AI must never
    /// treat ending its own game as an ordinary payable ward cost.
    #[test]
    fn rejects_targeting_ward_that_would_be_lethal_poison() {
        let mut scenario = GameScenario::new();
        let creature = scenario
            .add_creature(P1, "Warded", 2, 2)
            .with_keyword(Keyword::Ward(WardCost::GetPlayerCounters {
                counter_kind: engine::types::player::PlayerCounterKind::Poison,
                count: 5,
            }))
            .id();
        let mut runner = scenario.build();
        let state = runner.state_mut();
        state.players[P0.0 as usize].poison_counters = 5;
        let decision = damage_target_decision(creature, 3);
        let candidate = choose_target_candidate(creature);
        let config = create_config(AiDifficulty::VeryHard, Platform::Wasm);
        let ctx = PolicyContext {
            state,
            decision: &decision,
            candidate: &candidate,
            ai_player: P0,
            config: &config,
            context: &AiContext::empty(&config.weights),
            cast_facts: None,
            search_depth: crate::policies::context::SearchDepth::Root,
        };
        assert_eq!(assess_candidate(&ctx), GateDecision::Reject);
    }

    /// A poison Ward payment that stays below the ten-poison threshold does
    /// not gate the target out — mirrors `allows_targeting_payable_ward`.
    #[test]
    fn allows_targeting_ward_with_nonlethal_poison_payment() {
        let mut scenario = GameScenario::new();
        let creature = scenario
            .add_creature(P1, "Warded", 2, 2)
            .with_keyword(Keyword::Ward(WardCost::GetPlayerCounters {
                counter_kind: engine::types::player::PlayerCounterKind::Poison,
                count: 5,
            }))
            .id();
        let mut runner = scenario.build();
        let state = runner.state_mut();
        // P0 starts at 0 poison — 0 + 5 = 5, well below the 10-poison SBA.
        let decision = damage_target_decision(creature, 3);
        let candidate = choose_target_candidate(creature);
        let config = create_config(AiDifficulty::VeryHard, Platform::Wasm);
        let ctx = PolicyContext {
            state,
            decision: &decision,
            candidate: &candidate,
            ai_player: P0,
            config: &config,
            context: &AiContext::empty(&config.weights),
            cast_facts: None,
            search_depth: crate::policies::context::SearchDepth::Root,
        };
        assert_ne!(assess_candidate(&ctx), GateDecision::Reject);
    }

    /// CR 702.21a + CR 104.3d: two individually-nonlethal poison sub-costs in
    /// a `Compound` Ward can be jointly lethal — the aggregate across every
    /// sub-cost must be checked, not each sub-cost against the same
    /// unchanged starting total.
    #[test]
    fn rejects_targeting_ward_with_jointly_lethal_compound_poison() {
        let mut scenario = GameScenario::new();
        let creature = scenario
            .add_creature(P1, "Warded", 2, 2)
            .with_keyword(Keyword::Ward(WardCost::Compound(vec![
                WardCost::GetPlayerCounters {
                    counter_kind: engine::types::player::PlayerCounterKind::Poison,
                    count: 3,
                },
                WardCost::GetPlayerCounters {
                    counter_kind: engine::types::player::PlayerCounterKind::Poison,
                    count: 3,
                },
            ])))
            .id();
        let mut runner = scenario.build();
        let state = runner.state_mut();
        // 4 existing + 3 + 3 = 10 (lethal), but 4 + 3 = 7 alone is not — a
        // per-sub-cost check against the same starting total would wrongly
        // allow this.
        state.players[P0.0 as usize].poison_counters = 4;
        let decision = damage_target_decision(creature, 3);
        let candidate = choose_target_candidate(creature);
        let config = create_config(AiDifficulty::VeryHard, Platform::Wasm);
        let ctx = PolicyContext {
            state,
            decision: &decision,
            candidate: &candidate,
            ai_player: P0,
            config: &config,
            context: &AiContext::empty(&config.weights),
            cast_facts: None,
            search_depth: crate::policies::context::SearchDepth::Root,
        };
        assert_eq!(assess_candidate(&ctx), GateDecision::Reject);
    }

    /// CR 702.21a: the ward-affordability gate applies to any targetable
    /// permanent, not just creatures — a lethal poison Ward on a noncreature
    /// permanent must be rejected identically.
    #[test]
    fn rejects_targeting_noncreature_ward_that_would_be_lethal_poison() {
        let mut scenario = GameScenario::new();
        let artifact = scenario
            .add_creature(P1, "Warded Artifact", 0, 0)
            .as_artifact()
            .with_keyword(Keyword::Ward(WardCost::GetPlayerCounters {
                counter_kind: engine::types::player::PlayerCounterKind::Poison,
                count: 5,
            }))
            .id();
        let mut runner = scenario.build();
        let state = runner.state_mut();
        state.players[P0.0 as usize].poison_counters = 5;
        let decision = damage_target_decision(artifact, 3);
        let candidate = choose_target_candidate(artifact);
        let config = create_config(AiDifficulty::VeryHard, Platform::Wasm);
        let ctx = PolicyContext {
            state,
            decision: &decision,
            candidate: &candidate,
            ai_player: P0,
            config: &config,
            context: &AiContext::empty(&config.weights),
            cast_facts: None,
            search_depth: crate::policies::context::SearchDepth::Root,
        };
        assert_eq!(assess_candidate(&ctx), GateDecision::Reject);
    }

    /// CR 104.3d + CR 614.1a: a doubler on the poison counters the AI itself
    /// would receive can make an individually-nonlethal PRINTED count
    /// actually lethal once replacement-adjusted. The AI must project the
    /// real, replacement-adjusted result (`preview_player_counter_addition`)
    /// rather than trusting the printed count — the naive printed-count math
    /// (4 existing + 3 printed = 7) would wrongly call this safe, but the
    /// doubled result (4 + 6 = 10) is lethal.
    #[test]
    fn rejects_targeting_ward_with_lethal_poison_after_doubling_replacement() {
        let mut scenario = GameScenario::new();
        // A permanent the AI (P0) controls that doubles poison counters P0
        // would receive. `valid_player: Some(You)` + the default recipient
        // scope means this applies whenever P0 is the one gaining counters,
        // mirroring how `player_counter.rs`'s own Solemnity test constructs a
        // global player-counter replacement, parameterized to double instead
        // of prevent.
        let doubler_id = scenario.add_creature(P0, "Poison Doubler", 0, 0).id();
        let mut doubler_def = ReplacementDefinition::new(ReplacementEvent::AddCounter)
            .quantity_modification(QuantityModification::DOUBLE);
        doubler_def.valid_player = Some(ReplacementPlayerScope::You);
        let creature = scenario
            .add_creature(P1, "Warded", 2, 2)
            .with_keyword(Keyword::Ward(WardCost::GetPlayerCounters {
                counter_kind: engine::types::player::PlayerCounterKind::Poison,
                count: 3,
            }))
            .id();
        let mut runner = scenario.build();
        let state = runner.state_mut();
        state
            .objects
            .get_mut(&doubler_id)
            .unwrap()
            .replacement_definitions = vec![doubler_def].into();
        state.players[P0.0 as usize].poison_counters = 4;
        let decision = damage_target_decision(creature, 3);
        let candidate = choose_target_candidate(creature);
        let config = create_config(AiDifficulty::VeryHard, Platform::Wasm);
        let ctx = PolicyContext {
            state,
            decision: &decision,
            candidate: &candidate,
            ai_player: P0,
            config: &config,
            context: &AiContext::empty(&config.weights),
            cast_facts: None,
            search_depth: crate::policies::context::SearchDepth::Root,
        };
        assert_eq!(assess_candidate(&ctx), GateDecision::Reject);
    }

    /// CR 702.21a: a "players can't get counters" replacement (Solemnity) means
    /// the AI's Ward payment will actually FAIL — `costs.rs`'s
    /// `AbilityCost::GetPlayerCounters` treats `Prevented` as a failed payment,
    /// not a zero-cost one — so the AI must not target into this believing the
    /// Ward is safely (and freely) payable.
    #[test]
    fn rejects_targeting_ward_with_prevented_player_counter_payment() {
        let mut scenario = GameScenario::new();
        let solemnity_id = scenario.add_creature(P0, "Solemnity", 0, 0).id();
        let mut prevent_def = ReplacementDefinition::new(ReplacementEvent::AddCounter)
            .quantity_modification(QuantityModification::Prevent);
        prevent_def.valid_player = Some(ReplacementPlayerScope::AnyPlayer);
        let creature = scenario
            .add_creature(P1, "Warded", 2, 2)
            .with_keyword(Keyword::Ward(WardCost::GetPlayerCounters {
                counter_kind: engine::types::player::PlayerCounterKind::Poison,
                count: 3,
            }))
            .id();
        let mut runner = scenario.build();
        let state = runner.state_mut();
        state
            .objects
            .get_mut(&solemnity_id)
            .unwrap()
            .replacement_definitions = vec![prevent_def].into();
        let decision = damage_target_decision(creature, 3);
        let candidate = choose_target_candidate(creature);
        let config = create_config(AiDifficulty::VeryHard, Platform::Wasm);
        let ctx = PolicyContext {
            state,
            decision: &decision,
            candidate: &candidate,
            ai_player: P0,
            config: &config,
            context: &AiContext::empty(&config.weights),
            cast_facts: None,
            search_depth: crate::policies::context::SearchDepth::Root,
        };
        assert_eq!(assess_candidate(&ctx), GateDecision::Reject);
    }

    /// CR 702.21a: a `Compound` Ward's sub-costs are conjoined — ALL must be
    /// payable, so a prevented `GetPlayerCounters` sub-cost must reject the
    /// whole cost even when its sibling sub-cost (here, a small life payment)
    /// is perfectly payable on its own. Proves the recursion through
    /// `Compound`'s `.all(|cost| can_pay_ward_cost(...))`, not just the direct
    /// leaf case covered by `rejects_targeting_ward_with_prevented_player_counter_payment`.
    #[test]
    fn rejects_compound_ward_with_prevented_player_counter_leaf() {
        let mut scenario = GameScenario::new();
        let solemnity_id = scenario.add_creature(P0, "Solemnity", 0, 0).id();
        let mut prevent_def = ReplacementDefinition::new(ReplacementEvent::AddCounter)
            .quantity_modification(QuantityModification::Prevent);
        prevent_def.valid_player = Some(ReplacementPlayerScope::AnyPlayer);
        let creature = scenario
            .add_creature(P1, "Warded", 2, 2)
            .with_keyword(Keyword::Ward(WardCost::Compound(vec![
                WardCost::PayLife(2), // trivially payable on its own (P0 starts at 20 life)
                WardCost::GetPlayerCounters {
                    counter_kind: engine::types::player::PlayerCounterKind::Poison,
                    count: 3,
                },
            ])))
            .id();
        let mut runner = scenario.build();
        let state = runner.state_mut();
        state
            .objects
            .get_mut(&solemnity_id)
            .unwrap()
            .replacement_definitions = vec![prevent_def].into();
        let decision = damage_target_decision(creature, 3);
        let candidate = choose_target_candidate(creature);
        let config = create_config(AiDifficulty::VeryHard, Platform::Wasm);
        let ctx = PolicyContext {
            state,
            decision: &decision,
            candidate: &candidate,
            ai_player: P0,
            config: &config,
            context: &AiContext::empty(&config.weights),
            cast_facts: None,
            search_depth: crate::policies::context::SearchDepth::Root,
        };
        assert_eq!(assess_candidate(&ctx), GateDecision::Reject);
    }
    /// Build an Improvise `ManaPayment` decision context: a `TapForConvoke`
    /// Colorless candidate for `object_id`, plus whatever sibling candidates
    /// the caller supplies for that same dual-purpose permanent.
    fn improvise_mana_payment_decision(
        sibling_candidates: Vec<CandidateAction>,
        object_id: ObjectId,
    ) -> AiDecisionContext {
        let mut candidates = vec![CandidateAction {
            action: GameAction::TapForConvoke {
                object_id,
                mana_type: ManaType::Colorless,
            },
            metadata: ActionMetadata::for_actor(Some(P0), TacticalClass::Mana),
        }];
        candidates.extend(sibling_candidates);
        AiDecisionContext {
            waiting_for: WaitingFor::ManaPayment {
                player: P0,
                convoke_mode: Some(engine::types::game_state::ConvokeMode::Improvise),
            },
            candidates,
        }
    }

    fn native_blue_tap_candidate(object_id: ObjectId) -> CandidateAction {
        CandidateAction {
            action: GameAction::TapLandForMana {
                selection: engine::types::mana::ManaSourceSelection {
                    source: engine::types::identifiers::ObjectIncarnationRef {
                        object_id,
                        incarnation: 0,
                    },
                    ability_index: None,
                    mana_type: ManaType::Blue,
                    output: engine::types::mana::ManaSourceOutput::Concrete(ManaType::Blue),
                    atomic_combination: None,
                    restrictions: Vec::new(),
                    penalty: engine::types::mana::ManaSourcePenalty::None,
                    taps_for_mana: Vec::new(),
                },
            },
            metadata: ActionMetadata::for_actor(Some(P0), TacticalClass::Mana),
        }
    }

    fn convoke_candidate_ctx<'a>(
        state: &'a GameState,
        decision: &'a AiDecisionContext,
        config: &'a crate::config::AiConfig,
        context: &'a AiContext,
    ) -> PolicyContext<'a> {
        PolicyContext {
            state,
            decision,
            candidate: &decision.candidates[0],
            ai_player: P0,
            config,
            context,
            cast_facts: None,
            search_depth: crate::policies::context::SearchDepth::Root,
        }
    }

    /// CR 702.51a + CR 702.126a: a dual-purpose permanent (an artifact land
    /// producing {U} natively) must not be tapped for its Colorless Improvise
    /// marker while the pending cast's {U} pip is still outstanding — that
    /// would strand the pip and dead-end `ManaPayment` (the Metallic Rebuke
    /// bug: crates/phase-ai/src/search.rs's `fallback_action` panic).
    #[test]
    fn rejects_convoke_colorless_tap_when_native_ability_still_covers_colored_demand() {
        let mut state = GameState::new_two_player(42);
        state.pending_cast = Some(Box::new(PendingCast::new(
            ObjectId(900),
            CardId(900),
            ResolvedAbility::new(
                Effect::Draw {
                    count: engine::types::ability::QuantityExpr::Fixed { value: 0 },
                    target: TargetFilter::Controller,
                },
                Vec::new(),
                ObjectId(900),
                P0,
            ),
            ManaCost::Cost {
                shards: vec![engine::types::mana::ManaCostShard::Blue],
                generic: 2,
            },
        )));
        let object_id = ObjectId(901);
        let decision =
            improvise_mana_payment_decision(vec![native_blue_tap_candidate(object_id)], object_id);
        let config = create_config(AiDifficulty::VeryHard, Platform::Wasm);
        let context = AiContext::empty(&config.weights);
        let ctx = convoke_candidate_ctx(&state, &decision, &config, &context);
        assert_eq!(assess_candidate(&ctx), GateDecision::Reject);
    }

    /// The sibling native-ability tap for the same permanent must stay
    /// `Allow` — the fix removes only the redundant Colorless path, not the
    /// source's usability.
    #[test]
    fn allows_native_tap_when_colorless_marker_is_gated() {
        let mut state = GameState::new_two_player(42);
        state.pending_cast = Some(Box::new(PendingCast::new(
            ObjectId(900),
            CardId(900),
            ResolvedAbility::new(
                Effect::Draw {
                    count: engine::types::ability::QuantityExpr::Fixed { value: 0 },
                    target: TargetFilter::Controller,
                },
                Vec::new(),
                ObjectId(900),
                P0,
            ),
            ManaCost::Cost {
                shards: vec![engine::types::mana::ManaCostShard::Blue],
                generic: 2,
            },
        )));
        let object_id = ObjectId(901);
        let decision =
            improvise_mana_payment_decision(vec![native_blue_tap_candidate(object_id)], object_id);
        let config = create_config(AiDifficulty::VeryHard, Platform::Wasm);
        let context = AiContext::empty(&config.weights);
        let ctx = PolicyContext {
            state: &state,
            decision: &decision,
            candidate: &decision.candidates[1],
            ai_player: P0,
            config: &config,
            context: &context,
            cast_facts: None,
            search_depth: crate::policies::context::SearchDepth::Root,
        };
        assert_eq!(assess_candidate(&ctx), GateDecision::Allow);
    }

    /// Once colored demand is satisfied (a generic-only remaining cost), the
    /// Colorless marker is fine again — this isn't a blanket ban on
    /// Improvise/Convoke for dual-purpose permanents.
    #[test]
    fn allows_convoke_colorless_tap_once_colored_demand_is_satisfied() {
        let mut state = GameState::new_two_player(42);
        state.pending_cast = Some(Box::new(PendingCast::new(
            ObjectId(900),
            CardId(900),
            ResolvedAbility::new(
                Effect::Draw {
                    count: engine::types::ability::QuantityExpr::Fixed { value: 0 },
                    target: TargetFilter::Controller,
                },
                Vec::new(),
                ObjectId(900),
                P0,
            ),
            ManaCost::Cost {
                shards: Vec::new(),
                generic: 3,
            },
        )));
        let object_id = ObjectId(901);
        let decision =
            improvise_mana_payment_decision(vec![native_blue_tap_candidate(object_id)], object_id);
        let config = create_config(AiDifficulty::VeryHard, Platform::Wasm);
        let context = AiContext::empty(&config.weights);
        let ctx = convoke_candidate_ctx(&state, &decision, &config, &context);
        assert_eq!(assess_candidate(&ctx), GateDecision::Allow);
    }

    /// A permanent with no sibling native colored option (a plain
    /// non-mana-producing artifact) is unaffected by the gate — it's scoped
    /// to true tap-channel dominance, not all Colorless taps.
    #[test]
    fn allows_convoke_colorless_tap_on_permanent_with_no_native_colored_option() {
        let mut state = GameState::new_two_player(42);
        state.pending_cast = Some(Box::new(PendingCast::new(
            ObjectId(900),
            CardId(900),
            ResolvedAbility::new(
                Effect::Draw {
                    count: engine::types::ability::QuantityExpr::Fixed { value: 0 },
                    target: TargetFilter::Controller,
                },
                Vec::new(),
                ObjectId(900),
                P0,
            ),
            ManaCost::Cost {
                shards: vec![engine::types::mana::ManaCostShard::Blue],
                generic: 2,
            },
        )));
        let object_id = ObjectId(901);
        let decision = improvise_mana_payment_decision(Vec::new(), object_id);
        let config = create_config(AiDifficulty::VeryHard, Platform::Wasm);
        let context = AiContext::empty(&config.weights);
        let ctx = convoke_candidate_ctx(&state, &decision, &config, &context);
        assert_eq!(assess_candidate(&ctx), GateDecision::Allow);
    }

    /// CR 702.51a: the production Convoke candidate path (`mana_payment_actions`
    /// via `candidate_actions_broad`) offers a Colorless marker AND a matching
    /// colored marker for the same creature when its color is in the cost. The
    /// Improvise-only tests above build a synthetic native-mana sibling and
    /// never exercise this real Convoke-generated pair -- confirmed missing by
    /// review on #6840: `sibling_native_tap_pays_demand` didn't recognize a
    /// colored `TapForConvoke` on the same object as a dominating sibling, so a
    /// real Convoke spell with a colored pip could still dead-end.
    #[test]
    fn rejects_convoke_colorless_tap_when_real_convoke_colored_sibling_covers_demand() {
        let mut scenario = GameScenario::new();
        let creature = scenario.add_creature(P0, "Convoke Creature", 2, 2).id();
        let mut runner = scenario.build();
        {
            let state = runner.state_mut();
            state.objects.get_mut(&creature).unwrap().color =
                vec![engine::types::mana::ManaColor::Blue];
            state.pending_cast = Some(Box::new(PendingCast::new(
                ObjectId(900),
                CardId(900),
                ResolvedAbility::new(
                    Effect::Draw {
                        count: engine::types::ability::QuantityExpr::Fixed { value: 0 },
                        target: TargetFilter::Controller,
                    },
                    Vec::new(),
                    ObjectId(900),
                    P0,
                ),
                ManaCost::Cost {
                    shards: vec![engine::types::mana::ManaCostShard::Blue],
                    generic: 1,
                },
            )));
            state.waiting_for = WaitingFor::ManaPayment {
                player: P0,
                convoke_mode: Some(engine::types::game_state::ConvokeMode::Convoke),
            };
        }
        let state = runner.state();
        let candidates = engine::ai_support::candidate_actions_broad(state);
        let colorless = candidates
            .iter()
            .find(|c| {
                matches!(
                    c.action,
                    GameAction::TapForConvoke {
                        object_id,
                        mana_type: ManaType::Colorless,
                    } if object_id == creature
                )
            })
            .expect("production candidate path must offer the Colorless convoke tap")
            .clone();
        let colored = candidates
            .iter()
            .find(|c| {
                matches!(
                    c.action,
                    GameAction::TapForConvoke {
                        object_id,
                        mana_type: ManaType::Blue,
                    } if object_id == creature
                )
            })
            .expect("production candidate path must offer the matching colored convoke tap")
            .clone();

        let decision = AiDecisionContext {
            waiting_for: state.waiting_for.clone(),
            candidates: candidates.clone(),
        };
        let config = create_config(AiDifficulty::VeryHard, Platform::Wasm);
        let context = AiContext::empty(&config.weights);

        let colorless_ctx = PolicyContext {
            state,
            decision: &decision,
            candidate: &colorless,
            ai_player: P0,
            config: &config,
            context: &context,
            cast_facts: None,
            search_depth: crate::policies::context::SearchDepth::Root,
        };
        assert_eq!(assess_candidate(&colorless_ctx), GateDecision::Reject);

        let colored_ctx = PolicyContext {
            state,
            decision: &decision,
            candidate: &colored,
            ai_player: P0,
            config: &config,
            context: &context,
            cast_facts: None,
            search_depth: crate::policies::context::SearchDepth::Root,
        };
        assert_eq!(assess_candidate(&colored_ctx), GateDecision::Allow);
    }

    /// Builds an Improvise-eligible artifact with one `Activated` mana ability
    /// producing Blue under the given `cost`, plus a `{1}{U}` pending cast, and
    /// returns the production `ManaPayment` candidates
    /// (`candidate_actions_broad` -> `mana_payment_actions`) for it.
    fn improvise_artifact_with_mana_ability_candidates(
        cost: Option<engine::types::ability::AbilityCost>,
    ) -> (
        engine::game::scenario::GameRunner,
        ObjectId,
        Vec<CandidateAction>,
    ) {
        let mut scenario = GameScenario::new();
        let artifact = scenario.add_creature(P0, "Improvise Artifact", 0, 0).id();
        let mut runner = scenario.build();
        {
            let state = runner.state_mut();
            let obj = state.objects.get_mut(&artifact).unwrap();
            obj.card_types.core_types.push(CoreType::Artifact);
            let mut mana_ability = engine::types::ability::AbilityDefinition::new(
                engine::types::ability::AbilityKind::Activated,
                Effect::Mana {
                    produced: engine::types::ability::ManaProduction::Fixed {
                        colors: vec![engine::types::mana::ManaColor::Blue],
                        contribution: engine::types::ability::ManaContribution::Base,
                    },
                    restrictions: vec![],
                    grants: vec![],
                    expiry: None,
                    target: None,
                },
            );
            mana_ability.cost = cost;
            std::sync::Arc::make_mut(&mut obj.abilities).push(mana_ability);

            state.pending_cast = Some(Box::new(PendingCast::new(
                ObjectId(900),
                CardId(900),
                ResolvedAbility::new(
                    Effect::Draw {
                        count: engine::types::ability::QuantityExpr::Fixed { value: 0 },
                        target: TargetFilter::Controller,
                    },
                    Vec::new(),
                    ObjectId(900),
                    P0,
                ),
                ManaCost::Cost {
                    shards: vec![engine::types::mana::ManaCostShard::Blue],
                    generic: 1,
                },
            )));
            state.waiting_for = WaitingFor::ManaPayment {
                player: P0,
                convoke_mode: Some(engine::types::game_state::ConvokeMode::Improvise),
            };
        }
        let candidates = engine::ai_support::candidate_actions_broad(runner.state());
        (runner, artifact, candidates)
    }

    fn find_colorless_convoke_candidate(
        candidates: &[CandidateAction],
        object_id: ObjectId,
    ) -> CandidateAction {
        candidates
            .iter()
            .find(|c| {
                matches!(
                    c.action,
                    GameAction::TapForConvoke {
                        object_id: o,
                        mana_type: ManaType::Colorless,
                    } if o == object_id
                )
            })
            .expect("production candidate path must offer the Colorless convoke tap")
            .clone()
    }

    /// Review finding on #6840: a tapless mana ability (e.g. a
    /// sacrifice-based one) on the SAME permanent as the Colorless Improvise
    /// marker does not compete for the tap -- both can legally be used in the
    /// same payment (Colorless first, then sacrifice the permanent for its
    /// ability), so it must not gate the Colorless action. Drives the real
    /// production `ManaPayment` candidate set, not a synthetic sibling.
    #[test]
    fn allows_colorless_improvise_tap_when_sibling_mana_ability_is_tapless() {
        let (runner, artifact, candidates) = improvise_artifact_with_mana_ability_candidates(Some(
            engine::types::ability::AbilityCost::Sacrifice(
                engine::types::ability::SacrificeCost::count(TargetFilter::Any, 1),
            ),
        ));
        let state = runner.state();
        let colorless = find_colorless_convoke_candidate(&candidates, artifact);
        let decision = AiDecisionContext {
            waiting_for: state.waiting_for.clone(),
            candidates: candidates.clone(),
        };
        let config = create_config(AiDifficulty::VeryHard, Platform::Wasm);
        let context = AiContext::empty(&config.weights);
        let ctx = PolicyContext {
            state,
            decision: &decision,
            candidate: &colorless,
            ai_player: P0,
            config: &config,
            context: &context,
            cast_facts: None,
            search_depth: crate::policies::context::SearchDepth::Root,
        };
        assert_eq!(assess_candidate(&ctx), GateDecision::Allow);
    }

    /// Regression guard for the fix above: a genuine tap-cost native mana
    /// ability on the same permanent still gates the Colorless marker, via
    /// the real production candidate path.
    #[test]
    fn rejects_colorless_improvise_tap_when_sibling_mana_ability_taps() {
        let (runner, artifact, candidates) = improvise_artifact_with_mana_ability_candidates(Some(
            engine::types::ability::AbilityCost::Tap,
        ));
        let state = runner.state();
        let colorless = find_colorless_convoke_candidate(&candidates, artifact);
        let decision = AiDecisionContext {
            waiting_for: state.waiting_for.clone(),
            candidates: candidates.clone(),
        };
        let config = create_config(AiDifficulty::VeryHard, Platform::Wasm);
        let context = AiContext::empty(&config.weights);
        let ctx = PolicyContext {
            state,
            decision: &decision,
            candidate: &colorless,
            ai_player: P0,
            config: &config,
            context: &context,
            cast_facts: None,
            search_depth: crate::policies::context::SearchDepth::Root,
        };
        assert_eq!(assess_candidate(&ctx), GateDecision::Reject);
    }

    // ---------------------------------------------------------------------
    // S7: temporary-combat-modifier ACTIVATIONS outside a live combat window.
    // ---------------------------------------------------------------------

    /// Desolation Prowler's real Oracle text, verified against
    /// `data/card-data.json`: it parses to `Pump { Fixed 2, Fixed 2, SelfRef }`
    /// with cost `PayLife 2`, `duration: UntilEndOfTurn` and restriction
    /// `OnlyOnceEachTurn`.
    const PROWLER_ORACLE: &str =
        "Pay 2 life: This creature gets +2/+2 until end of turn. Activate only once each turn.";

    /// Nantuko Husk's real Oracle text (sacrifice outlet — the payoff IS the
    /// cost, so the pump window must not gate it).
    const HUSK_ORACLE: &str = "Sacrifice a creature: This creature gets +2/+2 until end of turn.";

    /// Shivan Dragon's firebreathing (repeatable mana sink).
    const FIREBREATHING_ORACLE: &str = "{R}: This creature gets +1/+0 until end of turn.";

    /// Non-vacuity guard: every fixture below depends on its Oracle text still
    /// parsing to an "until end of turn" pump. Without this, a parser change
    /// would silently turn the `assert_ne!(.., Reject)` tests green for the
    /// wrong reason.
    fn assert_parses_as_temporary_pump(state: &GameState, source_id: ObjectId) {
        let ability = &state.objects.get(&source_id).unwrap().abilities[0];
        assert!(
            ability_is_temporary_combat_modifier(ability),
            "fixture Oracle text no longer parses to an until-end-of-turn pump"
        );
    }

    fn gate_activation(state: &GameState, source_id: ObjectId) -> GateDecision {
        let config = create_config(AiDifficulty::VeryHard, Platform::Wasm);
        let decision = AiDecisionContext {
            waiting_for: state.waiting_for.clone(),
            candidates: Vec::new(),
        };
        let candidate = CandidateAction {
            action: GameAction::ActivateAbility {
                source_id,
                ability_index: 0,
            },
            metadata: ActionMetadata::for_actor(Some(P0), TacticalClass::Ability),
        };
        let context = AiContext::empty(&config.weights);
        let ctx = PolicyContext {
            state,
            decision: &decision,
            candidate: &candidate,
            ai_player: P0,
            config: &config,
            context: &context,
            cast_facts: None,
            search_depth: crate::policies::context::SearchDepth::Root,
        };
        assess_candidate(&ctx)
    }

    /// Put the AI at priority in `phase` on `active_player`'s turn.
    fn set_priority_window(state: &mut GameState, phase: Phase, active_player: PlayerId) {
        state.phase = phase;
        state.active_player = active_player;
        state.priority_player = P0;
        state.waiting_for = WaitingFor::Priority { player: P0 };
    }

    /// CR 514.2: the +2/+2 ends at cleanup, and no combat or stack window can
    /// consume it from the opponent's end step — the 2 life buys nothing.
    #[test]
    fn activated_ueot_pump_rejected_at_opponents_end_step() {
        let mut scenario = GameScenario::new();
        let prowler = scenario
            .add_creature_from_oracle(P0, "Desolation Prowler", 2, 2, PROWLER_ORACLE)
            .id();
        let mut runner = scenario.build();
        let state = runner.state_mut();
        set_priority_window(state, Phase::End, P1);

        assert_parses_as_temporary_pump(state, prowler);
        assert_eq!(gate_activation(state, prowler), GateDecision::Reject);
    }

    #[test]
    fn activated_ueot_pump_rejected_in_own_postcombat_main_with_no_combat() {
        let mut scenario = GameScenario::new();
        let prowler = scenario
            .add_creature_from_oracle(P0, "Desolation Prowler", 2, 2, PROWLER_ORACLE)
            .id();
        let mut runner = scenario.build();
        let state = runner.state_mut();
        set_priority_window(state, Phase::PostCombatMain, P0);

        assert_parses_as_temporary_pump(state, prowler);
        assert_eq!(gate_activation(state, prowler), GateDecision::Reject);
    }

    /// After blockers are declared, a 2/2 blocked by a 3/3 dies and kills
    /// nothing; +2/+2 flips both halves of the exchange, so the activation is a
    /// real play and must reach scoring.
    #[test]
    fn activated_ueot_pump_allowed_after_blocks_when_it_changes_the_outcome() {
        let mut scenario = GameScenario::new();
        let prowler = scenario
            .add_creature_from_oracle(P0, "Desolation Prowler", 2, 2, PROWLER_ORACLE)
            .id();
        let blocker = scenario.add_creature(P1, "Blocker", 3, 3).id();
        let mut runner = scenario.build();
        let state = runner.state_mut();
        set_priority_window(state, Phase::DeclareBlockers, P0);
        state.combat = Some(CombatState {
            attackers: vec![AttackerInfo::attacking_player(prowler, P1)],
            blocker_assignments: [(prowler, vec![blocker])].into_iter().collect(),
            blocker_to_attacker: [(blocker, vec![prowler])].into_iter().collect(),
            ..Default::default()
        });

        assert_parses_as_temporary_pump(state, prowler);
        assert_ne!(gate_activation(state, prowler), GateDecision::Reject);
    }

    /// The mirror of the case above, and the proof that the new branch is a
    /// real gate rather than a blanket allow inside combat: blocked by a 6/6,
    /// +2/+2 neither saves the Prowler nor kills the blocker, so the 2 life is
    /// still wasted.
    #[test]
    fn activated_ueot_pump_rejected_after_blocks_when_outcome_is_unchanged() {
        let mut scenario = GameScenario::new();
        let prowler = scenario
            .add_creature_from_oracle(P0, "Desolation Prowler", 2, 2, PROWLER_ORACLE)
            .id();
        let blocker = scenario.add_creature(P1, "Colossus", 6, 6).id();
        let mut runner = scenario.build();
        let state = runner.state_mut();
        set_priority_window(state, Phase::DeclareBlockers, P0);
        state.combat = Some(CombatState {
            attackers: vec![AttackerInfo::attacking_player(prowler, P1)],
            blocker_assignments: [(prowler, vec![blocker])].into_iter().collect(),
            blocker_to_attacker: [(blocker, vec![prowler])].into_iter().collect(),
            ..Default::default()
        });

        assert_parses_as_temporary_pump(state, prowler);
        assert_eq!(gate_activation(state, prowler), GateDecision::Reject);
    }

    /// A pump whose definition carries no `UntilEndOfTurn` duration outlives
    /// the combat window, so "no live window" proves nothing about its value.
    #[test]
    fn activated_pump_without_ueot_duration_not_gated() {
        let mut scenario = GameScenario::new();
        let prowler = scenario
            .add_creature_from_oracle(P0, "Desolation Prowler", 2, 2, PROWLER_ORACLE)
            .id();
        let mut runner = scenario.build();
        let state = runner.state_mut();
        set_priority_window(state, Phase::End, P1);
        let object = state.objects.get_mut(&prowler).unwrap();
        Arc::make_mut(&mut object.abilities)[0].duration = None;

        assert_ne!(gate_activation(state, prowler), GateDecision::Reject);
    }

    /// The reactive exception carried over from the spell path: with a 3-damage
    /// burn spell on the stack aimed at the 2/2 Prowler, +2/+2 saves it.
    #[test]
    fn activated_pump_in_response_to_burn_allowed() {
        let mut scenario = GameScenario::new();
        let prowler = scenario
            .add_creature_from_oracle(P0, "Desolation Prowler", 2, 2, PROWLER_ORACLE)
            .id();
        let mut runner = scenario.build();
        let state = runner.state_mut();
        set_priority_window(state, Phase::PostCombatMain, P1);
        state.stack.push_back(StackEntry {
            id: ObjectId(300),
            source_id: ObjectId(301),
            controller: P1,
            kind: StackEntryKind::Spell {
                ability: Some(Box::new(ResolvedAbility::new(
                    Effect::DealDamage {
                        amount: engine::types::ability::QuantityExpr::Fixed { value: 3 },
                        target: TargetFilter::Any,
                        damage_source: None,
                        excess: None,
                    },
                    vec![TargetRef::Object(prowler)],
                    ObjectId(301),
                    P1,
                ))),
                card_id: CardId(301),
                casting_variant: Default::default(),
                actual_mana_spent: 0,
            },
        });

        assert_parses_as_temporary_pump(state, prowler);
        assert_ne!(gate_activation(state, prowler), GateDecision::Reject);
    }

    /// Sacrifice-outlet shape (Nantuko Husk): the sacrifice IS the payoff, and
    /// `free_outlet_activation` / `self_cost_value` own that judgment, so the
    /// pump window must not veto it even at the opponent's end step.
    #[test]
    fn activated_sacrifice_outlet_pump_not_gated() {
        let mut scenario = GameScenario::new();
        let husk = scenario
            .add_creature_from_oracle(P0, "Nantuko Husk", 2, 2, HUSK_ORACLE)
            .id();
        let mut runner = scenario.build();
        let state = runner.state_mut();
        set_priority_window(state, Phase::End, P1);

        assert_parses_as_temporary_pump(state, husk);
        assert_ne!(gate_activation(state, husk), GateDecision::Reject);
    }

    /// A sorcery-speed-only pump has no later, stronger window to be saved
    /// for, so "pass and use it in combat instead" is not available advice.
    /// No printed card carries this shape today, so the restriction is
    /// synthesized onto the Prowler ability.
    #[test]
    fn activated_as_sorcery_pump_not_gated() {
        let mut scenario = GameScenario::new();
        let prowler = scenario
            .add_creature_from_oracle(P0, "Desolation Prowler", 2, 2, PROWLER_ORACLE)
            .id();
        let mut runner = scenario.build();
        let state = runner.state_mut();
        set_priority_window(state, Phase::PostCombatMain, P0);
        let object = state.objects.get_mut(&prowler).unwrap();
        Arc::make_mut(&mut object.abilities)[0]
            .activation_restrictions
            .push(ActivationRestriction::AsSorcery);

        assert_parses_as_temporary_pump(state, prowler);
        assert_ne!(gate_activation(state, prowler), GateDecision::Reject);
    }

    /// CR 509.1h: with no blocker declared for it the Dragon is an unblocked
    /// creature, and CR 510.1b assigns its combat damage to the defending
    /// player — so a repeatable mana sink still pays off even when it changes
    /// no combat outcome the gate can prove. How much that mana is worth is a
    /// policy judgment, so the activated branch stands down here.
    #[test]
    fn firebreathing_unblocked_attacker_is_not_gated() {
        let mut scenario = GameScenario::new();
        let dragon = scenario
            .add_creature_from_oracle(P0, "Shivan Dragon", 5, 5, FIREBREATHING_ORACLE)
            .id();
        scenario.with_life(P1, 20);
        let mut runner = scenario.build();
        let state = runner.state_mut();
        set_priority_window(state, Phase::DeclareBlockers, P0);
        state.combat = Some(CombatState {
            attackers: vec![AttackerInfo::attacking_player(dragon, P1)],
            ..Default::default()
        });

        assert_parses_as_temporary_pump(state, dragon);
        assert_ne!(gate_activation(state, dragon), GateDecision::Reject);
    }

    /// CR 510.3: the active player gets priority again after combat damage is
    /// dealt, still inside a window that maps to `CombatAfterBlocks`. The same
    /// unblocked attacker has already connected, so the mana-sink stand-down
    /// must not apply — pumping now is pure waste.
    #[test]
    fn activated_pump_on_unblocked_attacker_after_damage_is_rejected() {
        let mut scenario = GameScenario::new();
        let dragon = scenario
            .add_creature_from_oracle(P0, "Shivan Dragon", 5, 5, FIREBREATHING_ORACLE)
            .id();
        scenario.with_life(P1, 20);
        let mut runner = scenario.build();
        let state = runner.state_mut();
        set_priority_window(state, Phase::DeclareBlockers, P0);
        state.combat = Some(CombatState {
            attackers: vec![AttackerInfo::attacking_player(dragon, P1)],
            regular_damage_done: true,
            ..Default::default()
        });

        assert_parses_as_temporary_pump(state, dragon);
        assert_eq!(gate_activation(state, dragon), GateDecision::Reject);
    }
}
