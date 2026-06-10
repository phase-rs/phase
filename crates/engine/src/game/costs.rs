//! Ability-activation cost payment authority (L2).
//!
//! This module is the single authority that executes payment of an activated
//! ability's cost (CLAUDE.md: "Single authority for ability costs"). It owns
//! the only `match` over `AbilityCost` that mutates player/object state to pay
//! a cost, plus the CR 616.1 replacement-pause bookkeeping.
//!
//! Extracted verbatim from `casting.rs` as a pure code-motion seam (Phase 1 of
//! the cost-payment unification plan). The activation flow, the
//! `WaitingFor::PayCost` emission/resume handlers, the affordability aggregate
//! (`can_pay_ability_cost_now`), the cost finder helpers, and the mana planner
//! all remain in `casting.rs` for now; `casting.rs` re-exports the symbols
//! moved here via `pub(crate) use` shims so existing call sites compile
//! unchanged.
//!
//! L1-primitives-only rule (TARGET invariant): code here pays costs through
//! L1 resource primitives (`life_costs`, `effects::counters`, `sacrifice`,
//! `effects::discard`, `zones`, `effects::attach`, and the mana payment path
//! in `casting.rs`) and must never re-implement resource math beyond a direct
//! L1 call. Known exceptions carried over verbatim by the Phase-1 pure move,
//! to be collapsed in Phase 2/5: the `PayEnergy` arm hand-rolls the energy
//! decrement (pending a `players::pay_energy` L1 helper) and the `Tap` arm
//! sets `tapped` directly.

use std::collections::HashSet;

use crate::types::ability::{AbilityCost, TargetFilter, REMOVE_COUNTER_COST_ALL};
use crate::types::events::GameEvent;
use crate::types::game_state::GameState;
use crate::types::identifiers::ObjectId;
use crate::types::player::PlayerId;
use crate::types::statics::StaticMode;
use crate::types::zones::Zone;

use super::casting::{
    ability_mana_payment_excluded_sources, pay_ability_mana_cost, pay_ability_mana_cost_excluding,
};
use super::engine::EngineError;
use super::quantity::resolve_quantity;
use super::speed::{effective_speed, set_speed};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum AbilityCostPaymentOutcome {
    Complete,
    Paused { remaining_cost: Option<AbilityCost> },
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
) -> Result<AbilityCostPaymentOutcome, EngineError> {
    let excluded_sources = ability_mana_payment_excluded_sources(cost, source_id);
    pay_ability_cost_inner(state, player, source_id, cost, events, &excluded_sources)
}

fn pay_ability_cost_inner(
    state: &mut GameState,
    player: PlayerId,
    source_id: ObjectId,
    cost: &AbilityCost,
    events: &mut Vec<GameEvent>,
    excluded_sources: &HashSet<ObjectId>,
) -> Result<AbilityCostPaymentOutcome, EngineError> {
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
        AbilityCost::Mana { cost } => {
            // CR 106.6: Ability activation — restriction enforcement routes
            // through `allows_activation` (not `allows_spell`) via the
            // activation context built from the source permanent's types.
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
        AbilityCost::Composite { costs } => {
            for (index, sub_cost) in costs.iter().enumerate() {
                let outcome = pay_ability_cost_inner(
                    state,
                    player,
                    source_id,
                    sub_cost,
                    events,
                    excluded_sources,
                )?;
                if let AbilityCostPaymentOutcome::Paused { remaining_cost } = outcome {
                    return Ok(AbilityCostPaymentOutcome::Paused {
                        remaining_cost: combine_remaining_costs(
                            remaining_cost,
                            &costs[index + 1..],
                        ),
                    });
                }
            }
        }
        AbilityCost::PayLife { amount } => {
            let amount = resolve_quantity(state, amount, player, source_id);
            let amount = u32::try_from(amount.max(0)).unwrap_or(0);
            match super::life_costs::pay_life_as_cast_or_activation_cost(
                state, player, amount, events,
            ) {
                super::life_costs::PayLifeCostResult::Paid { .. } => {}
                super::life_costs::PayLifeCostResult::InsufficientLife
                | super::life_costs::PayLifeCostResult::Prohibited => {
                    return Err(EngineError::ActionNotAllowed(
                        "Cannot pay life cost".to_string(),
                    ));
                }
            }
        }
        // CR 118.3: Sacrifice as a cost — sacrifice the source (SelfRef) or a chosen permanent.
        AbilityCost::Sacrifice { target, .. } => {
            if matches!(target, TargetFilter::SelfRef) {
                if super::static_abilities::player_cant_sacrifice_as_cost(state, player, source_id)
                {
                    return Err(EngineError::ActionNotAllowed(
                        "Cannot sacrifice this permanent as a cost".to_string(),
                    ));
                }
                match super::sacrifice::sacrifice_permanent(state, source_id, player, events)? {
                    super::sacrifice::SacrificeOutcome::Complete => {}
                    super::sacrifice::SacrificeOutcome::NeedsReplacementChoice(choice_player) => {
                        pause_cost_payment_for_replacement_choice(state, choice_player);
                        return Ok(AbilityCostPaymentOutcome::Paused {
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
                return Ok(AbilityCostPaymentOutcome::Paused {
                    remaining_cost: None,
                });
            }
        },
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
                    return Err(EngineError::ActionNotAllowed(format!(
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
                    let count = super::quantity::resolve_quantity(state, count, player, source_id);
                    if !super::effects::counters::add_counter_with_replacement(
                        state,
                        player,
                        source_id,
                        counter_type.clone(),
                        count.unsigned_abs(),
                        events,
                    ) {
                        return Ok(AbilityCostPaymentOutcome::Paused {
                            remaining_cost: None,
                        });
                    }
                }
                _ => {
                    return Err(EngineError::ActionNotAllowed(format!(
                        "Effect-as-cost not yet resolvable: {:?}",
                        effect
                    )));
                }
            }
        }
        AbilityCost::Unimplemented { description } => {
            return Err(EngineError::ActionNotAllowed(format!(
                "Cost not implemented: {description}",
            )));
        }
        AbilityCost::PayEnergy { amount } => {
            // CR 107.14: A player can pay {E} only if they have enough energy.
            // CR 107.3c: Resolve the `QuantityExpr` so dynamic amounts read game
            // state at payment time.
            let amount = u32::try_from(resolve_quantity(state, amount, player, source_id).max(0))
                .unwrap_or(0);
            let player_state = &mut state.players[player.0 as usize];
            if player_state.energy < amount {
                return Err(EngineError::ActionNotAllowed("Not enough energy".into()));
            }
            player_state.energy -= amount;
            events.push(GameEvent::EnergyChanged {
                player,
                delta: -(amount as i32),
            });
        }
        AbilityCost::PaySpeed { amount } => {
            let amount = resolve_quantity(state, amount, player, source_id);
            let amount = u8::try_from(amount.max(0)).unwrap_or(u8::MAX);
            let current_speed = effective_speed(state, player);
            if amount > current_speed {
                return Err(EngineError::ActionNotAllowed("Not enough speed".into()));
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
                return Err(EngineError::ActionNotAllowed(
                    "Cannot unattach: source is not a controlled battlefield Equipment".to_string(),
                ));
            }
            if obj.attached_to.is_none() {
                return Err(EngineError::ActionNotAllowed(
                    "Cannot unattach: source is not attached".to_string(),
                ));
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
                        return Ok(AbilityCostPaymentOutcome::Paused {
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
                return Ok(AbilityCostPaymentOutcome::Complete);
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
                return Err(EngineError::ActionNotAllowed(
                    "Cannot exert: source is not on the battlefield".to_string(),
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
        // CR 118.4 + CR 107.3c: Dynamic-generic mana primarily appears in
        // unless-pay contexts (post-2026-05-09 fold). It should not reach an
        // activation-time payment path, where the X is normally announced
        // and resolved upstream.
        AbilityCost::ManaDynamic { .. } => {
            return Err(EngineError::ActionNotAllowed(
                "ManaDynamic cost should be resolved upstream".into(),
            ));
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
        | AbilityCost::NinjutsuFamily { .. } => {}
        // CR 118.12a: `OneOf` (disjunctive unless-cost) is intercepted at
        // `surface_unless_payment` and never reaches an auto-payment site.
        AbilityCost::OneOf { .. } => {
            return Err(EngineError::ActionNotAllowed(
                "OneOf cost is only valid as an unless-cost and must be \
                 resolved interactively via UnlessPaymentChooseCost"
                    .into(),
            ));
        }
        // CR 702.24a: `PerCounter` is expanded into a concrete cost at the
        // unless-payment entry point (Task 6 wires resolution). It must never
        // reach an auto-payment site as-is — the multiplier has to be resolved
        // against the live game state first.
        AbilityCost::PerCounter { .. } => {
            return Err(EngineError::ActionNotAllowed(
                "PerCounter cost must be expanded against game state before \
                 reaching pay_ability_cost"
                    .into(),
            ));
        }
    }
    Ok(AbilityCostPaymentOutcome::Complete)
}
