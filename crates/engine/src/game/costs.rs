//! Ability cost payment authority (L2).
//!
//! This module is the single authority that executes payment of an ability's
//! cost (CLAUDE.md: "Single authority for ability costs"). It owns the only
//! `match` over `AbilityCost` that mutates player/object state to pay a cost,
//! plus the CR 616.1 replacement-pause bookkeeping. Both activation-time
//! (CR 601.2g/h) and resolution-time (CR 118.12) payment flow through it; the
//! caller selects the regime via [`PaymentScope`], which carries the genuine
//! scope differences (CR-confirmed in the unification plan §2): quantity
//! resolution context, mana payment context, and PayLife helper selection
//! (the activation helper additionally applies cast/activation life-payment
//! prohibition statics; plan R4 keeps such forks explicit in the arm).
//!
//! Originally extracted from `casting.rs` as a pure code-motion seam (Phase 1);
//! Phase 2 introduced [`PaymentScope`] and routed the resolution-time
//! `Effect::PayCost` arms (`effects/pay.rs`) through this authority, deleting
//! their duplicate Mana/ManaDynamic/PayLife/PayEnergy/Composite/Discard
//! implementations. The activation flow, the `WaitingFor::PayCost`
//! emission/resume handlers, the affordability aggregate
//! (`can_pay_ability_cost_now`), the cost finder helpers, and the mana planner
//! all remain in `casting.rs`; `casting.rs` re-exports the moved symbols via
//! `pub(crate) use` shims so existing call sites compile unchanged.
//!
//! L1-primitives-only rule (TARGET invariant): code here pays costs through
//! L1 resource primitives (`life_costs`, `effects::counters`, `sacrifice`,
//! `effects::discard`, `zones`, `effects::attach`, and the mana payment path
//! in `casting.rs`) and must never re-implement resource math beyond a direct
//! L1 call. This rule binds the L3 resume handlers too: the
//! `WaitingFor::PayCost` / `WardDiscardChoice` / `WardSacrificeChoice` resume
//! handlers (in `engine.rs`/`engine_payment_choices.rs`) match on
//! `PayCostKind`/`WaitingFor` variants and may call L1 primitives
//! (`sacrifice_permanent`, `discard_as_cost`, …) to execute a player's concrete
//! selection, but they must never match on `AbilityCost` or re-implement the
//! resource math that lives here (risk R8). Known exceptions carried over
//! verbatim, to be collapsed in Phase 5: the `PayEnergy` arm hand-rolls the
//! energy decrement (pending a `players::pay_energy` L1 helper) and the `Tap`
//! arm sets `tapped` directly.

use std::collections::HashSet;

use crate::types::ability::{AbilityCost, EffectKind, TargetFilter, REMOVE_COUNTER_COST_ALL};
use crate::types::events::GameEvent;
use crate::types::game_state::{GameState, WaitingFor};
use crate::types::identifiers::ObjectId;
use crate::types::player::PlayerId;
use crate::types::statics::StaticMode;
use crate::types::zones::Zone;

use super::casting::{
    ability_mana_payment_excluded_sources, can_pay_effect_mana_cost_after_auto_tap,
    find_eligible_discard_targets, pay_ability_mana_cost, pay_ability_mana_cost_excluding,
    pay_effect_mana_cost,
};
use super::engine::EngineError;
use super::quantity::{resolve_quantity, resolve_quantity_with_targets};
use super::speed::{effective_speed, set_speed};
use crate::types::ability::ResolvedAbility;

/// Selects the payment regime for `pay_ability_cost_inner`. The two variants
/// capture the only CR-confirmed differences between activation-time and
/// resolution-time payment (unification plan §2):
///
/// - **Quantity resolution.** Activation resolves dynamic amounts with
///   `resolve_quantity(state, expr, player, source)`; resolution resolves them
///   against the payer-adjusted [`ResolvedAbility`] via
///   `resolve_quantity_with_targets` so event/target refs
///   (`Power { CostPaidObject }`, …) read the right object (CR 608.2k).
/// - **Mana payment context.** Activation uses the CR 601.2g mana-ability
///   window (`pay_ability_mana_cost`); resolution uses the effect-context
///   auto-tap path (`pay_effect_mana_cost`, CR 118.12).
///
/// Failure semantics are also scope-conditioned and handled by the caller:
/// activation maps [`PaymentOutcome::Failed`] to `EngineError::ActionNotAllowed`
/// (CR 601.2h "Unpayable costs can't be paid"); resolution maps it to
/// `cost_payment_failed_flag` (CR 118.12 "if [a player] can't").
pub(crate) enum PaymentScope<'a> {
    Activation {
        excluded_sources: &'a HashSet<ObjectId>,
    },
    /// `ability` is the PAYER-ADJUSTED `ResolvedAbility` clone (controller
    /// swapped to the resolved payer, per `effects/pay.rs`). All
    /// quantity-resolving arms read it via `resolve_quantity_with_targets`.
    Resolution { ability: &'a ResolvedAbility },
}

/// A cost payment could not be completed. The reason string is the human-
/// readable failure carried over from the original `EngineError` messages;
/// the activation adapter re-wraps it as `EngineError::ActionNotAllowed`, the
/// resolution adapter discards it and sets `cost_payment_failed_flag`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PaymentFailure {
    pub reason: String,
}

impl PaymentFailure {
    fn new(reason: impl Into<String>) -> Self {
        Self {
            reason: reason.into(),
        }
    }
}

/// Build a [`PaymentOutcome::Failed`] from a reason string.
fn payment_failed(reason: impl Into<String>) -> PaymentOutcome {
    PaymentOutcome::Failed {
        reason: PaymentFailure::new(reason),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum PaymentOutcome {
    /// The cost was paid in full.
    Paid,
    /// CR 616.1: a replacement-effect choice interrupted payment. Reserved
    /// exclusively for the `pause_cost_payment_for_replacement_choice` path.
    Paused { remaining_cost: Option<AbilityCost> },
    /// CR 601.2h / CR 118.12: the cost was not (fully) paid. The caller maps
    /// this to the scope-appropriate failure channel (see [`PaymentScope`]).
    Failed { reason: PaymentFailure },
}

fn combine_remaining_costs(
    paused_remaining: Option<AbilityCost>,
    following_costs: &[AbilityCost],
) -> Option<AbilityCost> {
    let mut costs = Vec::new();
    if let Some(cost) = paused_remaining {
        costs.push(cost);
    }
    costs.extend(following_costs.iter().cloned());
    match costs.len() {
        0 => None,
        1 => costs.into_iter().next(),
        _ => Some(AbilityCost::Composite { costs }),
    }
}

/// Resolve a cost's dynamic amount in the active scope (plan §2): activation
/// uses `resolve_quantity` (player + source); resolution uses
/// `resolve_quantity_with_targets` against the payer-adjusted ability so
/// event/target refs read the right object (CR 608.2k).
fn resolve_cost_quantity(
    state: &GameState,
    expr: &crate::types::ability::QuantityExpr,
    player: PlayerId,
    source_id: ObjectId,
    scope: &PaymentScope,
) -> i32 {
    match scope {
        PaymentScope::Activation { .. } => resolve_quantity(state, expr, player, source_id),
        PaymentScope::Resolution { ability } => resolve_quantity_with_targets(state, expr, ability),
    }
}

/// CR 601.2h + CR 616.1: Pause cost payment for a competing replacement effect.
pub(crate) fn pause_cost_payment_for_replacement_choice(
    state: &mut GameState,
    choice_player: PlayerId,
) {
    state.waiting_for = super::replacement::replacement_choice_waiting_for(choice_player, state);
}

/// Pay an activated ability's cost. Handles auto-payable cost components
/// (`Tap`, `Mana`, `PayLife`, `Composite`, and self-referential zone costs)
/// and passes through cost types that require interactive resolution.
pub fn pay_ability_cost(
    state: &mut GameState,
    player: PlayerId,
    source_id: ObjectId,
    cost: &AbilityCost,
    events: &mut Vec<GameEvent>,
) -> Result<(), EngineError> {
    pay_ability_cost_for_activation(state, player, source_id, cost, events).map(|_| ())
}

pub(crate) fn pay_ability_cost_for_activation(
    state: &mut GameState,
    player: PlayerId,
    source_id: ObjectId,
    cost: &AbilityCost,
    events: &mut Vec<GameEvent>,
) -> Result<PaymentOutcome, EngineError> {
    let excluded_sources = ability_mana_payment_excluded_sources(cost, source_id);
    let outcome = pay_ability_cost_inner(
        state,
        player,
        source_id,
        cost,
        events,
        &PaymentScope::Activation {
            excluded_sources: &excluded_sources,
        },
    )?;
    // CR 601.2h: "Unpayable costs can't be paid." Activation scope maps a
    // payment failure to an illegal action — the authority's `Failed` is the
    // activation flow's `Err(ActionNotAllowed)`, preserving the pre-Phase-2
    // contract so the `if let Paused` call sites are unaffected.
    match outcome {
        PaymentOutcome::Failed { reason } => Err(EngineError::ActionNotAllowed(reason.reason)),
        paid_or_paused => Ok(paid_or_paused),
    }
}

/// CR 118.12: Pay an ability's cost during the resolution of an
/// `Effect::PayCost`. `ability` is the payer-adjusted clone (see
/// [`PaymentScope::Resolution`]); `payer` is its resolved controller.
pub(crate) fn pay_ability_cost_for_resolution(
    state: &mut GameState,
    payer: PlayerId,
    cost: &AbilityCost,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<PaymentOutcome, EngineError> {
    pay_ability_cost_inner(
        state,
        payer,
        ability.source_id,
        cost,
        events,
        &PaymentScope::Resolution { ability },
    )
}

fn pay_ability_cost_inner(
    state: &mut GameState,
    player: PlayerId,
    source_id: ObjectId,
    cost: &AbilityCost,
    events: &mut Vec<GameEvent>,
    scope: &PaymentScope,
) -> Result<PaymentOutcome, EngineError> {
    match cost {
        AbilityCost::Tap => {
            let obj = state
                .objects
                .get(&source_id)
                .ok_or_else(|| EngineError::InvalidAction("Object not found".to_string()))?;
            if obj.zone != Zone::Battlefield {
                return Err(EngineError::ActionNotAllowed(
                    "Cannot activate tap ability: source is not on the battlefield".to_string(),
                ));
            }
            if obj.tapped {
                return Err(EngineError::ActionNotAllowed(
                    "Cannot activate tap ability: permanent is tapped".to_string(),
                ));
            }
            let obj = state.objects.get_mut(&source_id).unwrap();
            obj.tapped = true;
            events.push(GameEvent::PermanentTapped {
                object_id: source_id,
                caused_by: None,
            });
        }
        AbilityCost::Mana { cost } => match scope {
            // CR 601.2g: Activation pays through the mana-ability window. CR
            // 106.6: restriction enforcement routes through `allows_activation`
            // (not `allows_spell`) via the activation context built from the
            // source permanent's types.
            PaymentScope::Activation { excluded_sources } => {
                if excluded_sources.is_empty() {
                    pay_ability_mana_cost(state, player, source_id, cost, events)?;
                } else {
                    pay_ability_mana_cost_excluding(
                        state,
                        player,
                        source_id,
                        cost,
                        events,
                        excluded_sources,
                    )?;
                }
            }
            // CR 118.12: Resolution-time mana payment uses the effect-context
            // auto-tap path. Pre-flight then pay; either step failing is a
            // payment failure (not an engine error).
            PaymentScope::Resolution { .. } => {
                if !can_pay_effect_mana_cost_after_auto_tap(state, player, source_id, cost)
                    || pay_effect_mana_cost(state, player, source_id, cost, events).is_err()
                {
                    return Ok(payment_failed("insufficient mana"));
                }
            }
        },
        // CR 118.4 + CR 107.3c: Dynamic-generic mana. At activation it should
        // have been announced/resolved upstream (error). At resolution it
        // resolves the dynamic generic against the payer-adjusted ability and
        // pays it via the effect-context auto-tap path.
        AbilityCost::ManaDynamic { quantity } => match scope {
            PaymentScope::Activation { .. } => {
                return Ok(payment_failed(
                    "ManaDynamic cost should be resolved upstream",
                ));
            }
            PaymentScope::Resolution { .. } => {
                let amount = resolve_cost_quantity(state, quantity, player, source_id, scope);
                let mana_cost = crate::types::mana::ManaCost::generic(amount.max(0) as u32);
                if !can_pay_effect_mana_cost_after_auto_tap(state, player, source_id, &mana_cost)
                    || pay_effect_mana_cost(state, player, source_id, &mana_cost, events).is_err()
                {
                    return Ok(payment_failed("insufficient mana"));
                }
            }
        },
        AbilityCost::Composite { costs } => {
            for (index, sub_cost) in costs.iter().enumerate() {
                let outcome =
                    pay_ability_cost_inner(state, player, source_id, sub_cost, events, scope)?;
                match outcome {
                    PaymentOutcome::Paid => {}
                    PaymentOutcome::Paused { remaining_cost } => {
                        return Ok(PaymentOutcome::Paused {
                            remaining_cost: combine_remaining_costs(
                                remaining_cost,
                                &costs[index + 1..],
                            ),
                        });
                    }
                    // CR 601.2h: Partial payments are not allowed; resolution-
                    // scope callers pre-gate the whole composite via
                    // `can_pay`, so a mid-composite `Failed` propagates without
                    // committing the remaining sub-costs.
                    failed @ PaymentOutcome::Failed { .. } => return Ok(failed),
                }
            }
        }
        // CR 119.4: Paying life IS losing life. Activation applies direct
        // "can't pay life" statics (`pay_life_as_cast_or_activation_cost`);
        // resolution routes through `pay_life_as_cost` (CR 118.12).
        AbilityCost::PayLife { amount } => {
            let amount = resolve_cost_quantity(state, amount, player, source_id, scope);
            let amount = u32::try_from(amount.max(0)).unwrap_or(0);
            let result = match scope {
                PaymentScope::Activation { .. } => {
                    super::life_costs::pay_life_as_cast_or_activation_cost(
                        state, player, amount, events,
                    )
                }
                PaymentScope::Resolution { .. } => {
                    super::life_costs::pay_life_as_cost(state, player, amount, events)
                }
            };
            match result {
                super::life_costs::PayLifeCostResult::Paid { .. } => {}
                super::life_costs::PayLifeCostResult::InsufficientLife
                | super::life_costs::PayLifeCostResult::Prohibited => {
                    return Ok(payment_failed("Cannot pay life cost"));
                }
            }
        }
        // CR 118.3: Sacrifice as a cost — sacrifice the source (SelfRef) or a chosen permanent.
        AbilityCost::Sacrifice { target, .. } => {
            if matches!(target, TargetFilter::SelfRef) {
                if super::static_abilities::player_cant_sacrifice_as_cost(state, player, source_id)
                {
                    return Ok(payment_failed("Cannot sacrifice this permanent as a cost"));
                }
                match super::sacrifice::sacrifice_permanent(state, source_id, player, events)? {
                    super::sacrifice::SacrificeOutcome::Complete => {}
                    super::sacrifice::SacrificeOutcome::NeedsReplacementChoice(choice_player) => {
                        pause_cost_payment_for_replacement_choice(state, choice_player);
                        return Ok(PaymentOutcome::Paused {
                            remaining_cost: None,
                        });
                    }
                }
            } else {
                // Non-self sacrifice costs (e.g., "Sacrifice a creature") are handled
                // by the interactive WaitingFor::SacrificeForCost flow — they are
                // intercepted before reaching pay_ability_cost.
            }
        }
        // CR 207.2c + CR 602.1: Discard the source card itself as part of the cost (Channel).
        AbilityCost::Discard {
            self_scope: crate::types::ability::DiscardSelfScope::SourceCard,
            ..
        } => match super::effects::discard::discard_as_cost(state, source_id, player, events) {
            super::effects::discard::DiscardOutcome::Complete => {}
            super::effects::discard::DiscardOutcome::NeedsReplacementChoice(choice_player) => {
                pause_cost_payment_for_replacement_choice(state, choice_player);
                return Ok(PaymentOutcome::Paused {
                    remaining_cost: None,
                });
            }
        },
        // CR 118.12 + CR 701.9: Resolution-time "discard N cards of your choice"
        // cost (e.g. "discard a card"). The choice of which cards to discard is
        // acquired via a `WaitingFor::DiscardChoice` round-trip when there is a
        // real choice; when the eligible set exactly fills the requirement the
        // discard auto-pays. This shape is resolution-only — the activation
        // flow surfaces hand-discard costs through the `WaitingFor::PayCost`
        // detour before payment, so the activation scope falls through to the
        // interactive-pass-through arm below.
        AbilityCost::Discard {
            count,
            filter,
            selection: crate::types::ability::CardSelectionMode::Chosen,
            self_scope: crate::types::ability::DiscardSelfScope::FromHand,
        } if matches!(scope, PaymentScope::Resolution { .. }) => {
            let count =
                resolve_cost_quantity(state, count, player, source_id, scope).max(0) as usize;
            let eligible = find_eligible_discard_targets(state, player, source_id, filter.as_ref());
            if eligible.len() < count {
                return Ok(payment_failed("not enough cards to discard"));
            }
            if count == 0 {
                // CR 118.12: record the (zero) paid count for downstream chain
                // steps that read `QuantityRef::EventContextAmount`.
                state.last_effect_count = Some(0);
                return Ok(PaymentOutcome::Paid);
            }
            // Forced-choice fast path (plan R4): when the eligible set exactly
            // fills the requirement there is no choice to surface, so the
            // discard executes immediately. This is a runtime check, not a
            // classifier fact.
            if eligible.len() == count {
                for card_id in eligible {
                    if let super::effects::discard::DiscardOutcome::NeedsReplacementChoice(
                        choice_player,
                    ) = super::effects::discard::discard_as_cost(state, card_id, player, events)
                    {
                        pause_cost_payment_for_replacement_choice(state, choice_player);
                        return Ok(PaymentOutcome::Paused {
                            remaining_cost: None,
                        });
                    }
                }
                state.last_effect_count = Some(count as i32);
            } else {
                state.waiting_for = WaitingFor::DiscardChoice {
                    player,
                    count,
                    cards: eligible,
                    source_id,
                    effect_kind: EffectKind::PayCost,
                    up_to: false,
                    unless_filter: None,
                };
            }
        }
        // CR 118.3: A self-ref "exile this card" activation cost — the source
        // exiles itself from whatever zone the cost names. Covers exile-from-
        // graveyard costs (CR 702.97a Scavenge, Renew), the exile-from-hand
        // cost of CR 702.62a Suspend ("you may pay [cost] and exile it"), and
        // the exile-from-hand cost of CR 702.170a Plot ("you may exile this
        // card from your hand and pay [cost]"). The source is identified by
        // SelfRef; no player choice is needed, so this is an auto-payable cost
        // (no WaitingFor round-trip). Non-self exile costs (targeted exile from
        // any zone) are still handled by the catch-all below.
        AbilityCost::Exile {
            filter: Some(TargetFilter::SelfRef),
            zone,
            count: 1,
        } => {
            let obj = state.objects.get(&source_id).ok_or_else(|| {
                EngineError::InvalidAction("Source object not found for exile cost".to_string())
            })?;
            // CR 118.3 + CR 602.2b: an explicit zone validates the source's
            // location during cost payment; a missing zone exiles the source
            // from whatever zone it is currently in (e.g. a land's "Exile this
            // land" paid from the battlefield).
            if let Some(z) = zone {
                if obj.zone != *z {
                    return Ok(payment_failed(format!(
                        "Cannot exile self for cost: source is not in {z:?}"
                    )));
                }
            }
            super::zones::move_to_zone(state, source_id, Zone::Exile, events);
        }
        // CR 702.167a: Craft's materials are exiled by the interactive
        // `WaitingFor::PayCost { kind: ExileMaterials }` detour before this
        // resume runs, so this arm is an idempotent no-op (mirrors the non-self
        // `Sacrifice` arm above). It exists as its own arm — not folded into the
        // catch-all — so a future change to the materials payment shape forces a
        // deliberate decision here.
        AbilityCost::ExileMaterials { .. } => {}
        // Waterbend cost was already paid via ManaPayment before reaching pay_ability_cost.
        AbilityCost::Waterbend { .. } => {}
        // CR 118.3: An effect performed as a cost. Resolve the effect on the
        // source before the ability's own effect fires. Currently handles
        // PutCounter on self (Devoted Druid, Chainbreaker, etc.).
        AbilityCost::EffectCost { effect } => {
            use crate::types::ability::Effect;
            match effect.as_ref() {
                Effect::PutCounter {
                    counter_type,
                    count,
                    target: TargetFilter::SelfRef,
                } => {
                    let count = resolve_cost_quantity(state, count, player, source_id, scope);
                    if !super::effects::counters::add_counter_with_replacement(
                        state,
                        player,
                        source_id,
                        counter_type.clone(),
                        count.unsigned_abs(),
                        events,
                    ) {
                        return Ok(PaymentOutcome::Paused {
                            remaining_cost: None,
                        });
                    }
                }
                _ => {
                    return Ok(payment_failed(format!(
                        "Effect-as-cost not yet resolvable: {effect:?}"
                    )));
                }
            }
        }
        AbilityCost::Unimplemented { description } => {
            return Ok(payment_failed(format!(
                "Cost not implemented: {description}"
            )));
        }
        // CR 107.14: A player can pay {E} only if they have enough energy.
        // CR 107.3c: Resolve the `QuantityExpr` so dynamic amounts read game
        // state at payment time.
        AbilityCost::PayEnergy { amount } => {
            let amount = u32::try_from(
                resolve_cost_quantity(state, amount, player, source_id, scope).max(0),
            )
            .unwrap_or(0);
            let player_state = &mut state.players[player.0 as usize];
            if player_state.energy < amount {
                return Ok(payment_failed("Not enough energy"));
            }
            player_state.energy -= amount;
            events.push(GameEvent::EnergyChanged {
                player,
                delta: -(amount as i32),
            });
        }
        AbilityCost::PaySpeed { amount } => {
            let amount = resolve_cost_quantity(state, amount, player, source_id, scope);
            let amount = u8::try_from(amount.max(0)).unwrap_or(u8::MAX);
            let current_speed = effective_speed(state, player);
            if amount > current_speed {
                return Ok(payment_failed("Not enough speed"));
            }
            set_speed(state, player, Some(current_speed - amount), events);
        }
        // CR 701.3d: Explicit unattach cost. Legality is pre-gated by
        // `AbilityCost::is_payable`; payment clears both sides of the
        // attachment graph and keeps the Equipment on the battlefield.
        AbilityCost::Unattach => {
            let obj = state.objects.get(&source_id).ok_or_else(|| {
                EngineError::InvalidAction("Source object not found for unattach cost".to_string())
            })?;
            if obj.zone != Zone::Battlefield
                || obj.controller != player
                || !obj
                    .card_types
                    .subtypes
                    .iter()
                    .any(|subtype| subtype == "Equipment")
            {
                return Ok(payment_failed(
                    "Cannot unattach: source is not a controlled battlefield Equipment",
                ));
            }
            if obj.attached_to.is_none() {
                return Ok(payment_failed("Cannot unattach: source is not attached"));
            }
            if let Some(old_target) = super::effects::attach::unattach(state, source_id) {
                events.push(GameEvent::Unattached {
                    attachment_id: source_id,
                    old_target,
                });
            }
        }
        // CR 606.4: Loyalty abilities use loyalty counter adjustment as their cost.
        // Called after target selection when the ability was initiated interactively.
        // Routes through the single-authority counter resolver so replacement
        // effects (Vorinclex, Doubling Season) can apply per CR 614.1a and
        // obj.loyalty stays in sync with counters[Loyalty] (CR 306.5b).
        AbilityCost::Loyalty { amount } => {
            let amount = *amount;
            match amount.cmp(&0) {
                std::cmp::Ordering::Greater => {
                    if !super::effects::counters::add_counter_with_replacement(
                        state,
                        player,
                        source_id,
                        crate::types::counter::CounterType::Loyalty,
                        amount as u32,
                        events,
                    ) {
                        return Ok(PaymentOutcome::Paused {
                            remaining_cost: None,
                        });
                    }
                }
                std::cmp::Ordering::Less => {
                    super::effects::counters::remove_counter_with_replacement(
                        state,
                        source_id,
                        crate::types::counter::CounterType::Loyalty,
                        (-amount) as u32,
                        events,
                    );
                }
                std::cmp::Ordering::Equal => {}
            }
        }
        // CR 118.3 + CR 122: Remove-counter cost. The SelfRef form ("Remove N
        // {type} counters from ~") is auto-payable — no player choice is needed,
        // so it lands here rather than in an interactive WaitingFor round-trip.
        // Routes through the single-authority counter resolver so replacement
        // effects (Vorinclex, Doubling Season) apply per CR 614.1a and
        // obj.loyalty/obj.defense stay in sync per CR 306.5b / CR 310.4c.
        // Legality (CR 118.3: "can't pay a cost without having the necessary
        // resources") is enforced upstream by `AbilityCost::is_payable` in
        // cost_payability.rs before activation is committed.
        AbilityCost::RemoveCounter {
            count,
            counter_type,
            target: None,
            ..
        } => {
            if *count == REMOVE_COUNTER_COST_ALL
                && matches!(counter_type, crate::types::counter::CounterMatch::Any)
            {
                let counters: Vec<_> = state
                    .objects
                    .get(&source_id)
                    .map(|obj| {
                        obj.counters
                            .iter()
                            .map(|(ty, count)| (ty.clone(), *count))
                            .collect()
                    })
                    .unwrap_or_default();
                for (counter_type, count) in counters {
                    super::effects::counters::remove_counter_with_replacement(
                        state,
                        source_id,
                        counter_type,
                        count,
                        events,
                    );
                }
                return Ok(PaymentOutcome::Paid);
            }
            // CR 601.2h: Resolve `CounterMatch::Any` to the concrete counter
            // type currently present on the source before the replacement
            // pipeline sees it — `remove_counter_with_replacement` operates on
            // a single concrete kind. `OfType(t)` passes through unchanged.
            if let Some(resolved) = super::effects::counters::resolve_counter_match_for_removal(
                state,
                source_id,
                counter_type,
            ) {
                let count = if *count == REMOVE_COUNTER_COST_ALL {
                    state
                        .objects
                        .get(&source_id)
                        .and_then(|obj| obj.counters.get(&resolved))
                        .copied()
                        .unwrap_or(0)
                } else {
                    *count
                };
                super::effects::counters::remove_counter_with_replacement(
                    state, source_id, resolved, count, events,
                );
            }
        }
        // Targeted remove-counter costs are paid by the interactive
        // WaitingFor::RemoveCounterForCost path before automatic cost
        // components resume here. This arm intentionally no-ops so composite
        // activation costs can still pay their remaining automatic pieces.
        AbilityCost::RemoveCounter {
            target: Some(_), ..
        } => {}
        // CR 701.43a: "To exert a permanent, its controller chooses to have it
        // not untap during its controller's next untap step." Modeled as a
        // transient continuous effect with `StaticMode::CantUntap` scoped to
        // `Duration::UntilNextStepOf { step: Untap, player: Controller }` on the source permanent,
        // identical to the "doesn't untap during its controller's next untap
        // step" pattern already handled by the layer system (see
        // `layers::prune_controller_untap_step_effects`).
        //
        // CR 701.43b: "A permanent can be exerted even if it's not tapped or
        // has already been exerted in a turn." Pushing a second identical
        // effect is harmless — both expire during the same untap step.
        //
        // CR 701.43c: "An object that isn't on the battlefield can't be
        // exerted." Enforced here so off-battlefield activations (which
        // shouldn't reach this site for Exert costs on permanents) fail
        // loudly rather than creating a dangling effect.
        AbilityCost::Exert => {
            let obj = state.objects.get(&source_id).ok_or_else(|| {
                EngineError::InvalidAction("Source object not found for exert cost".to_string())
            })?;
            if obj.zone != Zone::Battlefield {
                return Ok(payment_failed(
                    "Cannot exert: source is not on the battlefield",
                ));
            }
            let controller = obj.controller;
            state.add_transient_continuous_effect(
                source_id,
                controller,
                crate::types::ability::Duration::UntilNextStepOf {
                    step: crate::types::phase::Phase::Untap,
                    player: crate::types::ability::PlayerScope::Controller,
                },
                TargetFilter::SpecificObject { id: source_id },
                vec![
                    crate::types::ability::ContinuousModification::AddStaticMode {
                        mode: StaticMode::CantUntap,
                    },
                ],
                None,
            );
        }
        // Other cost types require interactive resolution and are intercepted
        // before reaching pay_ability_cost, or are not yet auto-payable.
        AbilityCost::Untap
        | AbilityCost::Discard { .. }
        | AbilityCost::Exile { .. }
        | AbilityCost::CollectEvidence { .. }
        | AbilityCost::TapCreatures { .. }
        | AbilityCost::ReturnToHand { .. }
        | AbilityCost::Mill { .. }
        | AbilityCost::Blight { .. }
        | AbilityCost::Reveal { .. }
        | AbilityCost::Behold { .. }
        | AbilityCost::NinjutsuFamily { .. } => {
            // At Activation these shapes are intercepted by the interactive
            // WaitingFor detours before payment is invoked, so passing through
            // is sound. At Resolution there is no interceptor: falling through
            // to `Paid` would report a cost as paid that was never paid
            // (CR 118.3 / CR 601.2h). Fail loudly so the adapter's
            // `cost_payment_failed_flag` branch fires instead.
            if matches!(scope, PaymentScope::Resolution { .. }) {
                return Ok(payment_failed(
                    "unsupported resolution-time AbilityCost payment shape",
                ));
            }
        }
        // CR 118.12a: `OneOf` (disjunctive unless-cost) is intercepted at
        // `surface_unless_payment` and never reaches an auto-payment site.
        AbilityCost::OneOf { .. } => {
            return Ok(payment_failed(
                "OneOf cost is only valid as an unless-cost and must be \
                 resolved interactively via UnlessPaymentChooseCost",
            ));
        }
        // CR 702.24a: `PerCounter` is expanded into a concrete cost at the
        // unless-payment entry point (Task 6 wires resolution). It must never
        // reach an auto-payment site as-is — the multiplier has to be resolved
        // against the live game state first.
        AbilityCost::PerCounter { .. } => {
            return Ok(payment_failed(
                "PerCounter cost must be expanded against game state before \
                 reaching pay_ability_cost",
            ));
        }
    }
    Ok(PaymentOutcome::Paid)
}
