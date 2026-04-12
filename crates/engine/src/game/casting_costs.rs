use std::collections::HashSet;

use crate::types::ability::{AbilityCost, AdditionalCost, ResolvedAbility};
use crate::types::events::GameEvent;
use crate::types::game_state::{
    CastingVariant, ConvokeMode, DistributionUnit, GameState, PendingCast, StackEntry,
    StackEntryKind, WaitingFor,
};
use crate::types::identifiers::{CardId, ObjectId};
use crate::types::keywords::Keyword;
use crate::types::mana::{ManaCost, ManaCostShard, ManaType};
use crate::types::player::PlayerId;
use crate::types::zones::Zone;

use super::casting::emit_targeting_events;
use super::engine::EngineError;
use super::mana_abilities;
use super::mana_payment;
use super::mana_sources::{self, ManaSourceOption};
use super::restrictions;
use super::stack;
use super::zones;

use super::ability_utils::{
    assign_targets_in_chain, auto_select_targets_for_ability, begin_target_selection_for_ability,
    build_target_slots, flatten_targets_in_chain,
};

/// Handle the player's decision on an additional cost (kicker, blight, "or pay").
///
/// For `Optional`: `pay=true` pays the cost and sets `additional_cost_paid`, `pay=false` skips.
/// For `Choice`: `pay=true` pays the first cost, `pay=false` pays the second cost.
pub(crate) fn handle_decide_additional_cost(
    state: &mut GameState,
    player: PlayerId,
    pending: PendingCast,
    additional_cost: &AdditionalCost,
    pay: bool,
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    let mut ability = pending.ability;

    let cost_to_pay = match additional_cost {
        // CR 702.33a: Kicker is an optional additional cost.
        AdditionalCost::Optional(cost) => {
            if pay {
                ability.context.additional_cost_paid = true;
                Some(cost.clone())
            } else {
                None
            }
        }
        AdditionalCost::Choice(preferred, fallback) => {
            if pay {
                ability.context.additional_cost_paid = true;
                Some(preferred.clone())
            } else {
                Some(fallback.clone())
            }
        }
        AdditionalCost::Required(cost) => {
            // Required costs are always paid — the choice prompt should not be reached,
            // but handle defensively by always paying.
            ability.context.additional_cost_paid = true;
            Some(cost.clone())
        }
    };

    let updated_pending = PendingCast { ability, ..pending };

    if let Some(cost) = cost_to_pay {
        pay_additional_cost(state, player, cost, updated_pending, events)
    } else {
        pay_and_push(
            state,
            player,
            updated_pending.object_id,
            updated_pending.card_id,
            updated_pending.ability,
            &updated_pending.cost,
            updated_pending.casting_variant,
            updated_pending.distribute,
            events,
        )
    }
}

/// Complete the discard-for-cost flow: discard selected cards, then continue casting.
pub(crate) fn handle_discard_for_cost(
    state: &mut GameState,
    player: PlayerId,
    pending: PendingCast,
    expected: usize,
    legal_cards: &[ObjectId],
    chosen: &[ObjectId],
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    if chosen.len() != expected {
        return Err(EngineError::InvalidAction(format!(
            "Must discard exactly {} card(s), got {}",
            expected,
            chosen.len()
        )));
    }
    for card_id in chosen {
        if !legal_cards.contains(card_id) {
            return Err(EngineError::InvalidAction(
                "Selected card not in hand".to_string(),
            ));
        }
    }

    // CR 601.2h: Discard each chosen card through the replacement pipeline
    // so Madness (CR 702.35) etc. can intercept.
    for &card_id in chosen {
        match super::effects::discard::discard_as_cost(state, card_id, player, events) {
            super::effects::discard::DiscardOutcome::Complete => {}
            super::effects::discard::DiscardOutcome::NeedsReplacementChoice(_) => {
                // CR 118.3: Replacement choice during cost payment is extremely rare.
                // TODO: Surface replacement choice to player during cost payment.
                // For now, proceed — the discard was not completed, but the
                // replacement pipeline has already handled the event.
            }
        }
    }

    pay_and_push(
        state,
        player,
        pending.object_id,
        pending.card_id,
        pending.ability,
        &pending.cost,
        pending.casting_variant,
        pending.distribute,
        events,
    )
}

/// CR 118.3 + CR 601.2b: Complete sacrifice-as-cost after player selection.
pub(crate) fn handle_sacrifice_for_cost(
    state: &mut GameState,
    player: PlayerId,
    pending: PendingCast,
    count: usize,
    legal_permanents: &[ObjectId],
    chosen: &[ObjectId],
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    if chosen.len() != count {
        return Err(EngineError::InvalidAction(format!(
            "Must sacrifice exactly {} permanent(s), got {}",
            count,
            chosen.len()
        )));
    }
    for id in chosen {
        if !legal_permanents.contains(id) {
            return Err(EngineError::InvalidAction(
                "Selected permanent not eligible for sacrifice".to_string(),
            ));
        }
    }

    // Sacrifice each chosen permanent
    for &id in chosen {
        super::sacrifice::sacrifice_permanent(state, id, player, events)
            .map_err(|e| EngineError::InvalidAction(format!("{e}")))?;
    }

    // Resume path depends on whether this is a spell or activated ability
    if let Some(ability_index) = pending.activation_ability_index {
        push_activated_ability_to_stack(
            state,
            player,
            pending.object_id,
            ability_index,
            pending.ability,
            pending.activation_cost.as_ref(),
            events,
        )
    } else {
        pay_and_push(
            state,
            player,
            pending.object_id,
            pending.card_id,
            pending.ability,
            &pending.cost,
            pending.casting_variant,
            pending.distribute,
            events,
        )
    }
}

/// CR 702.34a: Tap creatures cost — complete the tap-creatures cost after player selection.
pub(crate) fn handle_tap_creatures_for_spell_cost(
    state: &mut GameState,
    player: PlayerId,
    pending: PendingCast,
    count: usize,
    legal_creatures: &[ObjectId],
    chosen: &[ObjectId],
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    if chosen.len() != count {
        return Err(EngineError::InvalidAction(format!(
            "Must tap exactly {} creature(s), got {}",
            count,
            chosen.len()
        )));
    }
    for id in chosen {
        if !legal_creatures.contains(id) {
            return Err(EngineError::InvalidAction(
                "Selected creature not eligible for tapping".to_string(),
            ));
        }
    }

    // Tap each chosen creature
    for &id in chosen {
        if let Some(obj) = state.objects.get_mut(&id) {
            obj.tapped = true;
        }
        events.push(GameEvent::PermanentTapped {
            object_id: id,
            caused_by: None,
        });
    }

    // Resume path depends on whether this is a spell or activated ability
    if let Some(ability_index) = pending.activation_ability_index {
        push_activated_ability_to_stack(
            state,
            player,
            pending.object_id,
            ability_index,
            pending.ability,
            pending.activation_cost.as_ref(),
            events,
        )
    } else {
        pay_and_push(
            state,
            player,
            pending.object_id,
            pending.card_id,
            pending.ability,
            &pending.cost,
            pending.casting_variant,
            pending.distribute,
            events,
        )
    }
}

/// CR 702.138a: Escape cost requires exiling other cards from your graveyard.
/// Complete the exile-from-graveyard cost after player selection.
pub(crate) fn handle_exile_from_graveyard_for_cost(
    state: &mut GameState,
    player: PlayerId,
    pending: PendingCast,
    expected: usize,
    legal_cards: &[ObjectId],
    chosen: &[ObjectId],
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    if chosen.len() != expected {
        return Err(EngineError::InvalidAction(format!(
            "Must exile exactly {} card(s), got {}",
            expected,
            chosen.len()
        )));
    }
    for id in chosen {
        if !legal_cards.contains(id) {
            return Err(EngineError::InvalidAction(
                "Selected card not eligible for exile".to_string(),
            ));
        }
    }

    // Re-validate: chosen cards must still be in graveyard
    for &id in chosen {
        let still_in_graveyard = state
            .players
            .get(player.0 as usize)
            .is_some_and(|p| p.graveyard.contains(&id));
        if !still_in_graveyard {
            return Err(EngineError::InvalidAction(
                "Selected card is no longer in graveyard".to_string(),
            ));
        }
    }

    // Exile each chosen card
    for &id in chosen {
        super::zones::move_to_zone(state, id, Zone::Exile, events);
    }

    pay_and_push(
        state,
        player,
        pending.object_id,
        pending.card_id,
        pending.ability,
        &pending.cost,
        pending.casting_variant,
        pending.distribute,
        events,
    )
}

/// Push an activated ability to the stack after costs are paid.
/// Shared by: direct path in `handle_activate_ability`, sacrifice detour, and
/// waterbend/ManaPayment finalization in the PassPriority handler.
pub(super) fn push_activated_ability_to_stack(
    state: &mut GameState,
    player: PlayerId,
    source_id: ObjectId,
    ability_index: usize,
    resolved: ResolvedAbility,
    remaining_cost: Option<&crate::types::ability::AbilityCost>,
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    // Pay remaining sub-costs (Tap, Mana, etc.) — the Sacrifice arm in pay_ability_cost
    // is a no-op for non-SelfRef targets, so the already-paid sacrifice is idempotent.
    if let Some(cost) = remaining_cost {
        if super::casting::variable_speed_payment_range(
            cost,
            super::speed::effective_speed(state, player),
        )
        .is_some()
        {
            return Ok(super::casting::begin_variable_speed_payment(
                state,
                player,
                source_id,
                resolved,
                cost.clone(),
                ability_index,
            ));
        }
        super::casting::pay_ability_cost(state, player, source_id, cost, events)?;
    }

    // CR 602.2b: Check if the ability has targets that need selection.
    // This handles cases where cost payment (sacrifice, waterbend) detoured
    // before target selection in handle_activate_ability.
    let target_slots = build_target_slots(state, &resolved)?;
    if !target_slots.is_empty() {
        if let Some(targets) =
            auto_select_targets_for_ability(state, &resolved, &target_slots, &[])?
        {
            let mut resolved = resolved;
            assign_targets_in_chain(&mut resolved, &targets)?;

            let assigned_targets = flatten_targets_in_chain(&resolved);
            emit_targeting_events(state, &assigned_targets, source_id, player, events);

            return push_ability_entry(state, player, source_id, ability_index, resolved, events);
        }

        // Targets need interactive selection
        let selection = begin_target_selection_for_ability(state, &resolved, &target_slots, &[])?;
        let mut pending_act = PendingCast::new(
            source_id,
            CardId(0),
            resolved,
            crate::types::mana::ManaCost::NoCost,
        );
        pending_act.activation_cost = remaining_cost.cloned();
        pending_act.activation_ability_index = Some(ability_index);
        return Ok(WaitingFor::TargetSelection {
            player,
            pending_cast: Box::new(pending_act),
            target_slots,
            selection,
        });
    }

    let assigned_targets = flatten_targets_in_chain(&resolved);
    emit_targeting_events(state, &assigned_targets, source_id, player, events);

    push_ability_entry(state, player, source_id, ability_index, resolved, events)
}

/// Final step: create stack entry and record activation.
fn push_ability_entry(
    state: &mut GameState,
    player: PlayerId,
    source_id: ObjectId,
    ability_index: usize,
    resolved: ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    let entry_id = ObjectId(state.next_object_id);
    state.next_object_id += 1;

    stack::push_to_stack(
        state,
        StackEntry {
            id: entry_id,
            source_id,
            controller: player,
            kind: StackEntryKind::ActivatedAbility {
                source_id,
                ability: resolved,
            },
        },
        events,
    );

    restrictions::record_ability_activation(state, source_id, ability_index);
    events.push(GameEvent::AbilityActivated { source_id });
    state.priority_passes.clear();
    state.priority_pass_count = 0;

    Ok(WaitingFor::Priority { player })
}

/// Check for an additional cost on the object being cast. If one exists,
/// return `WaitingFor::OptionalCostChoice` so the player can decide;
/// otherwise proceed directly to `pay_and_push`.
///
/// This function sits between targeting and payment in the casting pipeline:
/// `CastSpell → [ModeChoice] → [TargetSelection] → [AdditionalCostChoice] → pay_and_push → Stack`
#[allow(clippy::too_many_arguments)]
pub(super) fn check_additional_cost_or_pay(
    state: &mut GameState,
    player: PlayerId,
    object_id: ObjectId,
    card_id: CardId,
    ability: ResolvedAbility,
    cost: &crate::types::mana::ManaCost,
    casting_variant: CastingVariant,
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    check_additional_cost_or_pay_with_distribute(
        state,
        player,
        object_id,
        card_id,
        ability,
        cost,
        casting_variant,
        None,
        events,
    )
}

/// CR 601.2d: Extended version of `check_additional_cost_or_pay` that threads the
/// `distribute` flag through PendingCast creation so X-spell distribution
/// survives to the `(ManaPayment, PassPriority)` handler.
#[allow(clippy::too_many_arguments)]
pub(super) fn check_additional_cost_or_pay_with_distribute(
    state: &mut GameState,
    player: PlayerId,
    object_id: ObjectId,
    card_id: CardId,
    ability: ResolvedAbility,
    cost: &crate::types::mana::ManaCost,
    casting_variant: CastingVariant,
    distribute: Option<DistributionUnit>,
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    // CR 207.2c + CR 601.2f: Strive per-target cost increase.
    // Targets are chosen in CR 601.2c; costs are determined in CR 601.2f.
    // Add strive_cost * (num_targets - 1) to the total casting cost.
    let strive_adjusted_cost;
    let cost = if let Some(strive_cost) = state
        .objects
        .get(&object_id)
        .and_then(|obj| obj.strive_cost.clone())
    {
        let target_count = super::ability_utils::flatten_targets_in_chain(&ability).len();
        if target_count > 1 {
            strive_adjusted_cost = (1..target_count).fold(cost.clone(), |acc, _| {
                super::restrictions::add_mana_cost(&acc, &strive_cost)
            });
            &strive_adjusted_cost
        } else {
            cost
        }
    } else {
        cost
    };

    let additional = state
        .objects
        .get(&object_id)
        .and_then(|obj| obj.additional_cost.clone());

    if let Some(additional_cost) = additional {
        match &additional_cost {
            AdditionalCost::Required(req_cost) => {
                // Required additional costs bypass the choice prompt — pay directly.
                let mut pending = PendingCast::new(object_id, card_id, ability, cost.clone());
                pending.casting_variant = casting_variant;
                return pay_additional_cost(state, player, req_cost.clone(), pending, events);
            }
            AdditionalCost::Optional(_) | AdditionalCost::Choice(_, _) => {
                let mut pending = PendingCast::new(object_id, card_id, ability, cost.clone());
                pending.casting_variant = casting_variant;
                return Ok(WaitingFor::OptionalCostChoice {
                    player,
                    cost: additional_cost,
                    pending_cast: Box::new(pending),
                });
            }
        }
    }

    // CR 107.14: If this is an energy-from-exile cast, pay energy before pushing to stack.
    let energy_cost = state.objects.get(&object_id).and_then(|obj| {
        if obj.zone == Zone::Exile
            && obj.casting_permissions.iter().any(|p| {
                matches!(
                    p,
                    crate::types::ability::CastingPermission::ExileWithEnergyCost
                )
            })
        {
            Some(obj.mana_cost.mana_value())
        } else {
            None
        }
    });
    if let Some(energy_mv) = energy_cost {
        let mut pending = PendingCast::new(object_id, card_id, ability, cost.clone());
        pending.casting_variant = casting_variant;
        return pay_additional_cost(
            state,
            player,
            AbilityCost::PayEnergy { amount: energy_mv },
            pending,
            events,
        );
    }

    // CR 702.138a: Escape requires exiling N other cards from graveyard.
    if casting_variant == CastingVariant::Escape {
        if let Some((_, exile_count)) = super::keywords::effective_escape_data(state, object_id) {
            let mut pending = PendingCast::new(object_id, card_id, ability, cost.clone());
            pending.casting_variant = casting_variant;
            return pay_additional_cost(
                state,
                player,
                AbilityCost::Exile {
                    count: exile_count,
                    zone: Some(Zone::Graveyard),
                    filter: None,
                },
                pending,
                events,
            );
        }
    }

    // CR 702.34a: Flashback with non-mana cost requires paying the cost as an additional cost.
    if casting_variant == CastingVariant::Flashback {
        if let Some(crate::types::keywords::FlashbackCost::NonMana(non_mana_cost)) =
            super::keywords::effective_flashback_cost(state, object_id)
        {
            let mut pending = PendingCast::new(object_id, card_id, ability, cost.clone());
            pending.casting_variant = casting_variant;
            pending.distribute = distribute;
            return pay_additional_cost(state, player, non_mana_cost, pending, events);
        }
    }

    // CR 601.2b: Check for Defiler cost reduction — optional life payment for colored mana
    // reduction on matching-color permanent spells.
    if let Some((life_cost, mana_reduction)) = find_defiler_reduction(state, player, object_id) {
        let mut pending = PendingCast::new(object_id, card_id, ability, cost.clone());
        pending.casting_variant = casting_variant;
        pending.distribute = distribute;
        return Ok(WaitingFor::DefilerPayment {
            player,
            life_cost,
            mana_reduction,
            pending_cast: Box::new(pending),
        });
    }

    pay_and_push(
        state,
        player,
        object_id,
        card_id,
        ability,
        cost,
        casting_variant,
        distribute,
        events,
    )
}

/// CR 601.2b: Find the first applicable Defiler cost reduction for a spell being cast.
/// Returns `Some((life_cost, mana_reduction))` if a controlled Defiler permanent has
/// `DefilerCostReduction` matching one of the spell's colors and the spell is a permanent spell.
fn find_defiler_reduction(
    state: &GameState,
    caster: PlayerId,
    spell_id: ObjectId,
) -> Option<(u32, crate::types::mana::ManaCost)> {
    use crate::types::statics::StaticMode;

    let spell = state.objects.get(&spell_id)?;

    // Defiler only applies to permanent spells (not instants/sorceries)
    let is_permanent = spell.card_types.core_types.iter().any(|ct| {
        matches!(
            ct,
            crate::types::card_type::CoreType::Creature
                | crate::types::card_type::CoreType::Artifact
                | crate::types::card_type::CoreType::Enchantment
                | crate::types::card_type::CoreType::Planeswalker
        )
    });
    if !is_permanent {
        return None;
    }

    let spell_colors = &spell.color;
    if spell_colors.is_empty() {
        return None;
    }

    for &bf_id in &state.battlefield {
        let bf_obj = state.objects.get(&bf_id)?;
        if bf_obj.controller != caster {
            continue;
        }
        for def in &bf_obj.static_definitions {
            if let StaticMode::DefilerCostReduction {
                color,
                life_cost,
                mana_reduction,
            } = &def.mode
            {
                if spell_colors.contains(color) {
                    return Some((*life_cost, mana_reduction.clone()));
                }
            }
        }
    }

    None
}

/// CR 601.2b: Handle the player's decision on Defiler life payment.
/// If accepted, pays life and reduces the spell's mana cost, then continues to mana payment.
/// If declined, continues with the original cost.
pub(crate) fn handle_defiler_payment(
    state: &mut GameState,
    player: PlayerId,
    pending: PendingCast,
    life_cost: u32,
    mana_reduction: &crate::types::mana::ManaCost,
    pay: bool,
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    let mut cost = pending.cost.clone();

    if pay {
        // Pay life
        let player_state = &mut state.players[player.0 as usize];
        player_state.life -= life_cost as i32;
        events.push(GameEvent::LifeChanged {
            player_id: player,
            amount: -(life_cost as i32),
        });

        // Reduce mana cost — remove matching colored shards from the spell cost
        if let (
            crate::types::mana::ManaCost::Cost {
                shards: spell_shards,
                ..
            },
            crate::types::mana::ManaCost::Cost {
                shards: reduction_shards,
                generic: reduction_generic,
            },
        ) = (&mut cost, mana_reduction)
        {
            // Remove colored shards from spell cost that match the reduction
            for shard in reduction_shards {
                if let Some(pos) = spell_shards.iter().position(|s| s == shard) {
                    spell_shards.remove(pos);
                }
            }
            // Also reduce generic if the reduction specifies generic mana
            if let crate::types::mana::ManaCost::Cost {
                generic: spell_generic,
                ..
            } = &mut cost
            {
                *spell_generic = spell_generic.saturating_sub(*reduction_generic);
            }
        }
    }

    pay_and_push(
        state,
        player,
        pending.object_id,
        pending.card_id,
        pending.ability,
        &cost,
        pending.casting_variant,
        pending.distribute,
        events,
    )
}

/// CR 601.2b: Pay an additional cost, returning a WaitingFor if interactive input is needed
/// (e.g. choosing which card to discard), or continuing to pay_and_push if atomic.
fn pay_additional_cost(
    state: &mut GameState,
    player: PlayerId,
    cost: AbilityCost,
    pending: PendingCast,
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    match cost {
        AbilityCost::PayLife { amount } => {
            // CR 118.3: A player can pay life as a cost only if their life >= amount.
            let player_state = &mut state.players[player.0 as usize];
            player_state.life -= amount as i32;
            events.push(GameEvent::LifeChanged {
                player_id: player,
                amount: -(amount as i32),
            });
        }
        AbilityCost::Blight { count } => {
            // Place blight counters on caster's lands
            let lands: Vec<ObjectId> = state
                .battlefield
                .iter()
                .copied()
                .filter(|id| {
                    state.objects.get(id).is_some_and(|obj| {
                        obj.controller == player
                            && obj
                                .card_types
                                .core_types
                                .contains(&crate::types::card_type::CoreType::Land)
                    })
                })
                .collect();
            for (i, &land_id) in lands.iter().enumerate() {
                if i >= count as usize {
                    break;
                }
                if let Some(obj) = state.objects.get_mut(&land_id) {
                    *obj.counters
                        .entry(crate::types::counter::CounterType::Generic(
                            "blight".to_string(),
                        ))
                        .or_insert(0) += 1;
                }
            }
        }
        AbilityCost::Discard { count, .. } => {
            // CR 601.2b: Discard requires interactive card selection — return a WaitingFor.
            let eligible: Vec<ObjectId> = state.players[player.0 as usize]
                .hand
                .iter()
                .copied()
                .filter(|id| *id != pending.object_id)
                .collect();
            return Ok(WaitingFor::DiscardForCost {
                player,
                count: count as usize,
                cards: eligible,
                pending_cast: Box::new(pending),
            });
        }
        AbilityCost::Mana { cost: mana_cost } => {
            // Add mana cost to the pending payment (handled by pay_and_push → pay_mana_cost)
            let combined = super::restrictions::add_mana_cost(&pending.cost, &mana_cost);
            return pay_and_push(
                state,
                player,
                pending.object_id,
                pending.card_id,
                pending.ability,
                &combined,
                pending.casting_variant,
                pending.distribute,
                events,
            );
        }
        AbilityCost::Sacrifice { ref target, .. } => {
            if matches!(target, crate::types::ability::TargetFilter::SelfRef) {
                // CR 118.3: Self-sacrifice is atomic — no player choice needed
                super::sacrifice::sacrifice_permanent(state, pending.object_id, player, events)
                    .map_err(|e| EngineError::InvalidAction(format!("{e}")))?;
            } else {
                // CR 118.3: Non-self sacrifice needs interactive selection
                let eligible = super::casting::find_eligible_sacrifice_targets(
                    state,
                    player,
                    pending.object_id,
                    target,
                );
                if eligible.is_empty() {
                    return Err(EngineError::ActionNotAllowed(
                        "No eligible permanents to sacrifice".into(),
                    ));
                }
                return Ok(WaitingFor::SacrificeForCost {
                    player,
                    count: 1,
                    permanents: eligible,
                    pending_cast: Box::new(pending),
                });
            }
        }
        AbilityCost::PayEnergy { amount } => {
            // CR 107.14: A player can pay {E} only if they have enough energy.
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
        AbilityCost::Waterbend { cost: wb_cost } => {
            // Waterbend: combine waterbend mana with spell mana, enter ManaPayment with Waterbend mode.
            let combined = restrictions::add_mana_cost(&pending.cost, &wb_cost);
            state.pending_cast = Some(Box::new(PendingCast {
                cost: combined,
                ..pending
            }));
            return Ok(enter_payment_step(
                state,
                player,
                Some(ConvokeMode::Waterbend),
            ));
        }
        AbilityCost::Exile {
            count,
            zone: Some(Zone::Graveyard),
            ..
        } => {
            // CR 702.138a: Escape — exile N other cards from graveyard.
            let eligible: Vec<ObjectId> = state
                .players
                .get(player.0 as usize)
                .map(|p| {
                    p.graveyard
                        .iter()
                        .copied()
                        .filter(|id| *id != pending.object_id)
                        .collect()
                })
                .unwrap_or_default();
            if eligible.len() < count as usize {
                return Err(EngineError::ActionNotAllowed(
                    "Not enough cards in graveyard for Escape cost".into(),
                ));
            }
            return Ok(WaitingFor::ExileFromGraveyardForCost {
                player,
                count: count as usize,
                cards: eligible,
                pending_cast: Box::new(pending),
            });
        }
        AbilityCost::CollectEvidence { amount } => {
            return super::effects::collect_evidence::begin_cost_payment(
                state, player, amount, pending,
            );
        }
        AbilityCost::TapCreatures { count, ref filter } => {
            // CR 702.34a: Tap untapped creatures matching filter as a cost.
            let eligible: Vec<ObjectId> = state
                .battlefield
                .iter()
                .copied()
                .filter(|id| {
                    state.objects.get(id).is_some_and(|obj| {
                        obj.controller == player
                            && !obj.tapped
                            && obj.id != pending.object_id
                            && super::filter::matches_target_filter(
                                state,
                                obj.id,
                                filter,
                                &super::filter::FilterContext::from_source(
                                    state,
                                    pending.object_id,
                                ),
                            )
                    })
                })
                .collect();
            if eligible.len() < count as usize {
                return Err(EngineError::ActionNotAllowed(
                    "Not enough eligible creatures to tap".into(),
                ));
            }
            return Ok(WaitingFor::TapCreaturesForSpellCost {
                player,
                count: count as usize,
                creatures: eligible,
                pending_cast: Box::new(pending),
            });
        }
        _ => {
            // Other cost types (Exile, etc.) — not yet interactive
        }
    }

    pay_and_push(
        state,
        player,
        pending.object_id,
        pending.card_id,
        pending.ability,
        &pending.cost,
        pending.casting_variant,
        pending.distribute,
        events,
    )
}

#[allow(clippy::too_many_arguments)]
pub(super) fn pay_and_push(
    state: &mut GameState,
    player: PlayerId,
    object_id: ObjectId,
    card_id: CardId,
    ability: ResolvedAbility,
    cost: &crate::types::mana::ManaCost,
    casting_variant: CastingVariant,
    distribute: Option<DistributionUnit>,
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    // CR 702.180a/b: Harmonize — offer optional creature tap to reduce generic mana cost.
    // CR 601.2b: Creature chosen and tapped as part of cost payment step.
    // CR 302.6: Summoning sickness does not restrict tapping for costs.
    if casting_variant == CastingVariant::Harmonize {
        let has_generic =
            matches!(cost, crate::types::mana::ManaCost::Cost { generic, .. } if *generic > 0);
        if has_generic {
            let eligible: Vec<ObjectId> = state
                .objects
                .values()
                .filter(|o| {
                    o.controller == player
                        && o.zone == Zone::Battlefield
                        && !o.tapped
                        && o.card_types
                            .core_types
                            .contains(&crate::types::card_type::CoreType::Creature)
                        && o.power.is_some_and(|p| p > 0)
                })
                .map(|o| o.id)
                .collect();
            if !eligible.is_empty() {
                let mut pending = PendingCast::new(object_id, card_id, ability, cost.clone());
                pending.casting_variant = casting_variant;
                return Ok(WaitingFor::HarmonizeTapChoice {
                    player,
                    eligible_creatures: eligible,
                    pending_cast: Box::new(pending),
                });
            }
        }
    }

    pay_and_push_adventure(
        state,
        player,
        object_id,
        card_id,
        ability,
        cost,
        casting_variant,
        distribute,
        events,
    )
}

#[allow(clippy::too_many_arguments)]
pub(super) fn pay_and_push_adventure(
    state: &mut GameState,
    player: PlayerId,
    object_id: ObjectId,
    card_id: CardId,
    ability: ResolvedAbility,
    cost: &crate::types::mana::ManaCost,
    casting_variant: CastingVariant,
    distribute: Option<DistributionUnit>,
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    // CR 702.51a: Convoke lets players tap creatures to reduce mana cost.
    // CR 702.51: Check for Convoke or Waterbend keyword on the spell.
    let convoke_mode = state.objects.get(&object_id).and_then(|_| {
        let effective_keywords = super::casting::effective_spell_keywords(state, player, object_id);
        if effective_keywords
            .iter()
            .any(|k| matches!(k, Keyword::Convoke))
        {
            Some(ConvokeMode::Convoke)
        } else if effective_keywords
            .iter()
            .any(|k| matches!(k, Keyword::Waterbend))
        {
            Some(ConvokeMode::Waterbend)
        } else {
            None
        }
    });
    // Gate on eligible creatures/artifacts being present.
    let convoke_mode = convoke_mode.filter(|_| {
        state
            .objects
            .values()
            .any(|o| o.is_convoke_eligible(player))
    });

    // Enter the payment step if cost needs player input (X) or convoke/waterbend is active.
    // `enter_payment_step` diverts to `ChooseXValue` when the cost has an unchosen X,
    // per CR 601.2f (X chosen before mana is paid).
    let has_x = cost_has_x(cost);
    if has_x || convoke_mode.is_some() {
        let mut pending = PendingCast::new(object_id, card_id, ability, cost.clone());
        pending.casting_variant = casting_variant;
        pending.distribute = distribute;
        state.pending_cast = Some(Box::new(pending));
        return Ok(enter_payment_step(state, player, convoke_mode));
    }

    finalize_cast(
        state,
        player,
        object_id,
        card_id,
        ability,
        cost,
        casting_variant,
        events,
    )
}

/// Pay mana, move spell to stack, and return Priority.
/// Shared finalization path used by both `pay_and_push_adventure` (normal casting)
/// and the `(ManaPayment, PassPriority)` handler (after interactive mana payment).
#[allow(clippy::too_many_arguments)]
pub(super) fn finalize_cast(
    state: &mut GameState,
    player: PlayerId,
    object_id: ObjectId,
    card_id: CardId,
    ability: ResolvedAbility,
    cost: &crate::types::mana::ManaCost,
    casting_variant: CastingVariant,
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    // CR 700.14: Snapshot pool size before payment to compute actual mana spent.
    let pool_before = state
        .players
        .iter()
        .find(|p| p.id == player)
        .map(|p| p.mana_pool.total())
        .unwrap_or(0);

    super::casting::pay_mana_cost(state, player, object_id, cost, events)?;

    // CR 700.14: Compute actual mana deducted from pool (not declared cost).
    let pool_after = state
        .players
        .iter()
        .find(|p| p.id == player)
        .map(|p| p.mana_pool.total())
        .unwrap_or(0);
    let actual_mana_spent = pool_before.saturating_sub(pool_after) as u32;

    // Record commander cast before moving (need to check zone before move)
    let (was_in_command_zone, source_zone) = state
        .objects
        .get(&object_id)
        .map(|obj| (obj.zone == Zone::Command && obj.is_commander, obj.zone))
        .unwrap_or((false, Zone::Hand));

    // CR 603.4: Record the zone the spell was cast from so ETB triggers can
    // evaluate conditions like "if you cast it from your hand".
    let mut ability = ability;
    ability.context.cast_from_zone = Some(source_zone);

    // Emit targeting events before the spell moves to the stack
    emit_targeting_events(
        state,
        &flatten_targets_in_chain(&ability),
        object_id,
        player,
        events,
    );

    // Determine whether this spell has a meaningful on-resolve ability.
    // Permanent spells with no Spell-kind AbilityDefinition get a placeholder
    // Unimplemented effect through the cost pipeline (from continue_with_no_ability).
    // Only those become `ability: None` on the stack — they simply enter the
    // battlefield on resolution. All other spells keep their ResolvedAbility.
    let is_placeholder = matches!(
        ability.effect,
        crate::types::ability::Effect::Unimplemented { .. }
    ) && ability.targets.is_empty();
    let stack_ability = if !is_placeholder {
        Some(ability)
    } else {
        // CR 603.4: For permanent spells with no spell ability, store cast_from_zone
        // directly on the object since there's no ability context to carry it.
        if let Some(obj) = state.objects.get_mut(&object_id) {
            obj.cast_from_zone = Some(source_zone);
        }
        None
    };

    // Move card from hand/command zone to stack zone
    zones::move_to_zone(state, object_id, Zone::Stack, events);

    // Track commander cast count for tax calculation
    if was_in_command_zone {
        super::commander::record_commander_cast(state, object_id);
    }

    // Push stack entry
    stack::push_to_stack(
        state,
        StackEntry {
            id: object_id,
            source_id: object_id,
            controller: player,
            kind: StackEntryKind::Spell {
                card_id,
                ability: stack_ability,
                casting_variant,
                actual_mana_spent,
            },
        },
        events,
    );

    state.priority_passes.clear();
    state.priority_pass_count = 0;

    events.push(GameEvent::SpellCast {
        card_id,
        controller: player,
        object_id,
    });

    // CR 601.2a: Record permission usage when spell is finalized onto the stack.
    // This prevents casting a second spell via the same source before the first resolves.
    // Only once-per-turn permissions need tracking; unlimited permissions (Conduit of Worlds) skip.
    if let CastingVariant::GraveyardPermission {
        source,
        once_per_turn: true,
    } = casting_variant
    {
        state.graveyard_cast_permissions_used.insert(source);
    }

    let obj = state
        .objects
        .get(&object_id)
        .expect("spell object still exists after stack push")
        .clone();
    restrictions::record_spell_cast(state, player, &obj);

    // CR 601.2f: Consume any one-shot pending cost reductions now that the spell is finalized.
    super::casting::consume_pending_spell_cost_reduction(state, player);

    // CR 700.14: Track cumulative mana spent on spells this turn for Expend triggers.
    // Uses actual mana deducted from pool (accounts for cost reduction, convoke, etc.).
    if actual_mana_spent > 0 {
        let cumulative = state
            .mana_spent_on_spells_this_turn
            .entry(player)
            .or_insert(0);
        *cumulative += actual_mana_spent;
        let new_cumulative = *cumulative;
        events.push(GameEvent::ManaExpended {
            player_id: player,
            amount_spent: actual_mana_spent,
            new_cumulative,
        });
    }

    Ok(WaitingFor::Priority { player })
}

/// Find and mark the first unused land producing `needed` color. Returns true if found.
fn tap_matching_land(
    available: &[ManaSourceOption],
    used_sources: &mut HashSet<ObjectId>,
    to_tap: &mut Vec<ManaSourceOption>,
    needed: ManaType,
) -> bool {
    let Some(option) = available
        .iter()
        .find(|option| option.mana_type == needed && !used_sources.contains(&option.object_id))
    else {
        return false;
    };

    used_sources.insert(option.object_id);
    to_tap.push(*option);
    true
}

/// Auto-tap mana sources controlled by `player` to produce enough mana for `cost`.
///
/// Considers all permanent types with mana abilities: lands, creatures (mana dorks),
/// artifacts, and sacrifice-for-mana sources (Treasure tokens).
///
/// Strategy: tap sources producing colors required by the cost first (colored shards),
/// then tap remaining sources for generic requirements.
///
/// `deprioritize_source` — if set, this permanent is tapped last (it's the permanent whose
/// activated ability we're paying for, so tapping other sources first is preferable UX).
///
/// Tier priority: pure land > non-land mana dork > land-creature > deprioritized > sacrifice source.
pub(super) fn auto_tap_mana_sources(
    state: &mut GameState,
    player: PlayerId,
    cost: &crate::types::mana::ManaCost,
    events: &mut Vec<GameEvent>,
    deprioritize_source: Option<ObjectId>,
) {
    use crate::types::card_type::CoreType;
    use crate::types::mana::ManaCost;

    let (shards, generic) = match cost {
        ManaCost::NoCost | ManaCost::SelfManaCost => return,
        ManaCost::Cost { shards, generic } if shards.is_empty() && *generic == 0 => return,
        ManaCost::Cost { shards, generic } => (shards, *generic),
    };

    // Build list of activatable mana options for ALL permanents this player controls.
    // CR 605.1b: Non-land permanents can have mana abilities.
    let mut available: Vec<ManaSourceOption> = state
        .battlefield
        .iter()
        .filter_map(|&oid| {
            let obj = state.objects.get(&oid)?;
            if obj.controller != player || obj.tapped {
                return None;
            }
            // Use land-specific function for lands (includes basic-subtype fallback),
            // general function for everything else (includes summoning sickness check).
            if obj.card_types.core_types.contains(&CoreType::Land) {
                Some(mana_sources::activatable_land_mana_options(
                    state, oid, player,
                ))
            } else {
                Some(mana_sources::activatable_mana_options(state, oid, player))
            }
        })
        .flatten()
        .collect();

    // Tier sort: 0 = pure land, 1 = non-land mana dork, 2 = land-creature (preserve for combat),
    //            3 = deprioritized source, 4 = sacrifice-for-mana (Treasure — irreversible)
    available.sort_by_key(|option| {
        if option.requires_sacrifice {
            return (4, false); // sacrifice is irreversible — always last
        }
        if deprioritize_source == Some(option.object_id) {
            return (3, option.requires_life_payment);
        }
        let obj = state.objects.get(&option.object_id);
        let is_land = obj.is_some_and(|o| o.card_types.core_types.contains(&CoreType::Land));
        let is_creature =
            obj.is_some_and(|o| o.card_types.core_types.contains(&CoreType::Creature));
        let base_tier = if is_land && is_creature {
            2 // animated lands (e.g. Earthbender) — preserve for combat
        } else if is_land {
            0
        } else {
            1 // non-land mana dork (creature, artifact, etc.)
        };
        (base_tier, option.requires_life_payment)
    });

    let mut to_tap: Vec<ManaSourceOption> = Vec::new();
    let mut used_sources: HashSet<ObjectId> = HashSet::new();

    // Phase 1: satisfy colored and hybrid shards by tapping matching sources
    let mut deferred_generic: usize = 0;
    for shard in shards {
        use crate::game::mana_payment::{shard_to_mana_type, ShardRequirement};
        match shard_to_mana_type(*shard) {
            ShardRequirement::Single(color) | ShardRequirement::Phyrexian(color) => {
                tap_matching_land(&available, &mut used_sources, &mut to_tap, color);
            }
            ShardRequirement::Hybrid(a, b) => {
                if !tap_matching_land(&available, &mut used_sources, &mut to_tap, a) {
                    tap_matching_land(&available, &mut used_sources, &mut to_tap, b);
                }
            }
            ShardRequirement::TwoGenericHybrid(color) => {
                // Prefer 1 matching-color source over 2 generic sources
                if !tap_matching_land(&available, &mut used_sources, &mut to_tap, color) {
                    deferred_generic += 2;
                }
            }
            ShardRequirement::ColorlessHybrid(color) => {
                if !tap_matching_land(
                    &available,
                    &mut used_sources,
                    &mut to_tap,
                    ManaType::Colorless,
                ) {
                    tap_matching_land(&available, &mut used_sources, &mut to_tap, color);
                }
            }
            ShardRequirement::HybridPhyrexian(a, b) => {
                if !tap_matching_land(&available, &mut used_sources, &mut to_tap, a) {
                    tap_matching_land(&available, &mut used_sources, &mut to_tap, b);
                }
            }
            ShardRequirement::Snow | ShardRequirement::X => {
                deferred_generic += 1;
            }
        }
    }

    // Phase 2: satisfy generic cost + deferred shards with any remaining sources
    let mut remaining_generic = generic as usize + deferred_generic;
    for option in &available {
        if remaining_generic == 0 {
            break;
        }
        if used_sources.insert(option.object_id) {
            to_tap.push(*option);
            remaining_generic = remaining_generic.saturating_sub(1);
        }
    }

    // Phase 3: activate each selected mana source.
    // Sources with an explicit ability delegate to resolve_mana_ability (the single
    // authority for cost payment — handles tap, sacrifice, and future cost types).
    // The basic-land-subtype fallback (ability_index: None) uses inline tap + produce.
    for option in to_tap {
        if let Some(idx) = option.ability_index {
            let ability_def = state
                .objects
                .get(&option.object_id)
                .and_then(|obj| obj.abilities.get(idx))
                .cloned();
            if let Some(ability_def) = ability_def {
                // color_override tells resolve_mana_ability which color to produce
                // for AnyOneColor sources (e.g., Treasure → specific color needed).
                // Errors are non-fatal here: auto-tap runs synchronously during payment,
                // so sources can't change state between collection and resolution. If a
                // source is somehow invalid (e.g., removed by a replacement effect), we
                // skip it silently — the player can still manually tap other sources.
                let _ = mana_abilities::resolve_mana_ability(
                    state,
                    option.object_id,
                    player,
                    &ability_def,
                    events,
                    Some(option.mana_type),
                );
            }
        } else {
            // Basic-land-subtype fallback — no explicit ability, just tap + produce.
            if let Some(obj) = state.objects.get_mut(&option.object_id) {
                if !obj.tapped {
                    obj.tapped = true;
                    events.push(GameEvent::PermanentTapped {
                        object_id: option.object_id,
                        caused_by: None,
                    });
                }
            }
            mana_payment::produce_mana(
                state,
                option.object_id,
                option.mana_type,
                player,
                true,
                events,
            );
        }
    }
}

/// Compute the maximum legal value of X the caster can choose for a pending cast.
///
/// Upper bound = (mana currently in pool) + (mana producible from untapped,
/// free-to-tap sources under the caster's control) − (fixed portion of cost).
///
/// Free-to-tap = mana abilities whose only cost is {T}: i.e., `ManaSourceOption`
/// entries with `requires_sacrifice == false` and `requires_life_payment == false`.
/// Costed mana abilities (e.g. "1, T: Add {C}") are excluded for v1 — they cascade
/// and would require a search to bound precisely. Treasure tokens are likewise
/// excluded because they require sacrifice.
///
/// Each untapped producer counts once, regardless of how many color options it
/// offers (a shock land is still one tap → one mana).
///
/// This is an upper bound used for UI display and AI action enumeration only.
/// `ManaPayment` remains the authoritative check for whether the full colored
/// cost is actually payable after the player commits an X value.
///
/// CR 107.1b + CR 601.2f: X is chosen as part of determining total cost,
/// before mana is paid.
pub fn max_x_value(state: &GameState, player: PlayerId, cost: &ManaCost) -> u32 {
    let ManaCost::Cost { shards, generic } = cost else {
        return 0;
    };
    let x_count = shards
        .iter()
        .filter(|s| matches!(s, ManaCostShard::X))
        .count() as u32;
    if x_count == 0 {
        return 0;
    }

    let fixed_portion: u32 = shards
        .iter()
        .filter(|s| !matches!(s, ManaCostShard::X))
        .map(|s| s.mana_value_contribution())
        .sum::<u32>()
        + *generic;

    let pool = state
        .players
        .iter()
        .find(|p| p.id == player)
        .map_or(0, |p| p.mana_pool.total() as u32);

    let free_producers: u32 = state
        .battlefield
        .iter()
        .filter(|&&id| {
            mana_sources::activatable_mana_options(state, id, player)
                .iter()
                .any(|opt| !opt.requires_sacrifice && !opt.requires_life_payment)
        })
        .count() as u32;

    // CR 107.1b: Each `ManaCostShard::X` in the cost contributes `value` generic,
    // so for `{X}{X}` each point of X costs 2 mana. Dividing by `x_count` yields
    // the largest X the caster can actually afford.
    let remaining = (pool + free_producers).saturating_sub(fixed_portion);
    remaining / x_count
}

/// Single authority for transitioning into the payment step of a cast.
///
/// If the pending cast's cost contains `ManaCostShard::X` and no X value has
/// been chosen yet, diverts to `WaitingFor::ChooseXValue` so the caster can
/// pick X before paying mana (CR 601.2f). Otherwise enters `WaitingFor::ManaPayment`
/// normally. `convoke_mode` passes through either way.
///
/// All sites that would otherwise construct `WaitingFor::ManaPayment` during a
/// cast must go through this helper so X-selection is never bypassed.
pub fn enter_payment_step(
    state: &GameState,
    player: PlayerId,
    convoke_mode: Option<ConvokeMode>,
) -> WaitingFor {
    if let Some(pending) = state.pending_cast.as_ref() {
        if pending.ability.chosen_x.is_none() && cost_has_x(&pending.cost) {
            let max = max_x_value(state, player, &pending.cost);
            let pending_cast = pending.clone();
            return WaitingFor::ChooseXValue {
                player,
                max,
                pending_cast,
                convoke_mode,
            };
        }
    }
    WaitingFor::ManaPayment {
        player,
        convoke_mode,
    }
}

/// Return true if the given cost contains a `ManaCostShard::X` shard.
pub fn cost_has_x(cost: &crate::types::mana::ManaCost) -> bool {
    match cost {
        crate::types::mana::ManaCost::Cost { shards, .. } => {
            shards.iter().any(|s| matches!(s, ManaCostShard::X))
        }
        _ => false,
    }
}

/// Extract a mana sub-cost containing X from an activated ability cost.
///
/// CR 107.1b + CR 601.2f: X must be chosen before mana is paid. For composite
/// activation costs (e.g., `Tap + Pay {X}`), the mana sub-cost with X is
/// routed through `ChooseXValue`/`ManaPayment` while the remaining sub-costs
/// (Tap, Sacrifice, etc.) are deferred to after payment via the pending cast's
/// `activation_cost`.
///
/// Returns `Some((mana_cost, remaining))` where `mana_cost` is the extracted
/// Mana cost and `remaining` is the rest of the cost (None if the whole cost
/// was the Mana sub-cost). Returns `None` if no X mana cost is present.
pub fn extract_x_mana_cost(
    cost: &crate::types::ability::AbilityCost,
) -> Option<(
    crate::types::mana::ManaCost,
    Option<crate::types::ability::AbilityCost>,
)> {
    use crate::types::ability::AbilityCost;
    match cost {
        AbilityCost::Mana { cost: mana } if cost_has_x(mana) => Some((mana.clone(), None)),
        AbilityCost::Composite { costs } => {
            let idx = costs
                .iter()
                .position(|sub| matches!(sub, AbilityCost::Mana { cost: m } if cost_has_x(m)))?;
            let mut remaining = costs.clone();
            let AbilityCost::Mana { cost: extracted } = remaining.remove(idx) else {
                unreachable!("position guarantees Mana variant")
            };
            let remaining_cost = match remaining.len() {
                0 => None,
                1 => Some(remaining.into_iter().next().unwrap()),
                _ => Some(AbilityCost::Composite { costs: remaining }),
            };
            Some((extracted, remaining_cost))
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::zones::create_object;
    use crate::types::ability::{
        AbilityCost, AbilityDefinition, AbilityKind, Effect, QuantityExpr,
    };
    use crate::types::card_type::CoreType;
    use crate::types::identifiers::CardId;
    use crate::types::mana::{ManaColor, ManaCost, ManaCostShard, ManaType};

    fn make_pending(source_id: ObjectId) -> PendingCast {
        PendingCast {
            object_id: source_id,
            card_id: CardId(0),
            ability: ResolvedAbility::new(
                Effect::Scry {
                    count: QuantityExpr::Fixed { value: 1 },
                },
                Vec::new(),
                source_id,
                PlayerId(0),
            ),
            cost: ManaCost::NoCost,
            activation_cost: None,
            activation_ability_index: Some(0),
            target_constraints: Vec::new(),
            casting_variant: CastingVariant::Normal,
            distribute: None,
        }
    }

    fn create_starting_town(state: &mut GameState, card_id: CardId) -> ObjectId {
        let town = create_object(
            state,
            card_id,
            PlayerId(0),
            "Starting Town".to_string(),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&town).unwrap();
        obj.card_types.core_types.push(CoreType::Land);
        obj.abilities.push(
            AbilityDefinition::new(
                AbilityKind::Activated,
                Effect::Mana {
                    produced: crate::types::ability::ManaProduction::Colorless {
                        count: QuantityExpr::Fixed { value: 1 },
                    },
                    restrictions: vec![],
                    grants: vec![],
                    expiry: None,
                },
            )
            .cost(AbilityCost::Tap),
        );
        obj.abilities.push(
            AbilityDefinition::new(
                AbilityKind::Activated,
                Effect::Mana {
                    produced: crate::types::ability::ManaProduction::AnyOneColor {
                        count: QuantityExpr::Fixed { value: 1 },
                        color_options: vec![ManaColor::White, ManaColor::Blue],
                    },
                    restrictions: vec![],
                    grants: vec![],
                    expiry: None,
                },
            )
            .cost(AbilityCost::Composite {
                costs: vec![AbilityCost::Tap, AbilityCost::PayLife { amount: 1 }],
            }),
        );
        town
    }

    #[test]
    fn auto_tap_prefers_non_life_mana_sources_when_equivalent() {
        let mut state = GameState::new_two_player(42);
        create_starting_town(&mut state, CardId(10));
        let island = create_object(
            &mut state,
            CardId(11),
            PlayerId(0),
            "Island".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&island).unwrap();
            obj.card_types.core_types.push(CoreType::Land);
            obj.card_types.subtypes.push("Island".to_string());
        }

        let mut events = Vec::new();
        auto_tap_mana_sources(
            &mut state,
            PlayerId(0),
            &ManaCost::Cost {
                shards: vec![ManaCostShard::Blue],
                generic: 1,
            },
            &mut events,
            None,
        );

        assert_eq!(
            state.players[0].life, 20,
            "auto-pay should avoid paying life"
        );
        assert_eq!(state.players[0].mana_pool.count_color(ManaType::Blue), 1);
        assert_eq!(
            state.players[0].mana_pool.count_color(ManaType::Colorless),
            1
        );
        assert!(
            !events
                .iter()
                .any(|e| matches!(e, GameEvent::LifeChanged { amount: -1, .. })),
            "auto-pay should not emit a life payment when an equivalent non-life line exists"
        );
    }

    #[test]
    fn sacrifice_for_cost_valid_selection() {
        let mut state = GameState::new_two_player(42);
        let source = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Seer".to_string(),
            Zone::Battlefield,
        );
        let creature_a = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Goblin A".to_string(),
            Zone::Battlefield,
        );
        let creature_b = create_object(
            &mut state,
            CardId(3),
            PlayerId(0),
            "Goblin B".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&creature_a)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);
        state
            .objects
            .get_mut(&creature_b)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);

        // Give source an ability so push_activated_ability_to_stack can record activation
        state.objects.get_mut(&source).unwrap().abilities =
            vec![crate::types::ability::AbilityDefinition::new(
                AbilityKind::Activated,
                Effect::Scry {
                    count: QuantityExpr::Fixed { value: 1 },
                },
            )];

        let pending = make_pending(source);
        let legal = vec![creature_a, creature_b];
        let chosen = vec![creature_a];
        let mut events = Vec::new();

        let result = handle_sacrifice_for_cost(
            &mut state,
            PlayerId(0),
            pending,
            1,
            &legal,
            &chosen,
            &mut events,
        );

        assert!(result.is_ok());
        // creature_a should be in graveyard
        assert!(state.players[0].graveyard.contains(&creature_a));
        // creature_b should still be on battlefield
        assert!(state.battlefield.contains(&creature_b));
    }

    #[test]
    fn sacrifice_for_cost_wrong_count() {
        let mut state = GameState::new_two_player(42);
        let source = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Seer".to_string(),
            Zone::Battlefield,
        );
        let creature = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Goblin".to_string(),
            Zone::Battlefield,
        );

        let pending = make_pending(source);
        let legal = vec![creature];
        let mut events = Vec::new();

        // Select 0 when count=1
        let result = handle_sacrifice_for_cost(
            &mut state,
            PlayerId(0),
            pending,
            1,
            &legal,
            &[],
            &mut events,
        );
        assert!(result.is_err());
    }

    #[test]
    fn sacrifice_for_cost_illegal_permanent() {
        let mut state = GameState::new_two_player(42);
        let source = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Seer".to_string(),
            Zone::Battlefield,
        );
        let legal_creature = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Goblin".to_string(),
            Zone::Battlefield,
        );
        let illegal_creature = create_object(
            &mut state,
            CardId(3),
            PlayerId(0),
            "Other".to_string(),
            Zone::Battlefield,
        );

        let pending = make_pending(source);
        let legal = vec![legal_creature]; // Only legal_creature is eligible
        let chosen = vec![illegal_creature]; // Trying to sacrifice non-eligible
        let mut events = Vec::new();

        let result = handle_sacrifice_for_cost(
            &mut state,
            PlayerId(0),
            pending,
            1,
            &legal,
            &chosen,
            &mut events,
        );
        assert!(result.is_err());
    }

    // -- Strive cost calculation tests ------------------------------------------

    #[test]
    fn strive_surcharge_with_three_targets() {
        // CR 207.2c + CR 601.2f: Strive adds per-target surcharge.
        // Base cost {2}{R}, strive cost {1}{R}, 3 targets -> {2}{R} + 2*{1}{R} = {4}{R}{R}{R}
        use crate::types::mana::ManaCostShard;
        let base = ManaCost::Cost {
            shards: vec![ManaCostShard::Red],
            generic: 2,
        };
        let strive_cost = ManaCost::Cost {
            shards: vec![ManaCostShard::Red],
            generic: 1,
        };
        let target_count = 3usize;
        let adjusted = (1..target_count).fold(base.clone(), |acc, _| {
            super::restrictions::add_mana_cost(&acc, &strive_cost)
        });
        // Total mana value: 2+1 (base) + 2*(1+1) = 3 + 4 = 7
        assert_eq!(adjusted.mana_value(), 7);
        match adjusted {
            ManaCost::Cost { generic, shards } => {
                assert_eq!(generic, 4); // 2 + 1 + 1
                assert_eq!(
                    shards
                        .iter()
                        .filter(|s| matches!(s, ManaCostShard::Red))
                        .count(),
                    3
                ); // R + R + R
            }
            _ => panic!("expected ManaCost::Cost"),
        }
    }

    #[test]
    fn strive_no_surcharge_with_one_target() {
        // CR 601.2f: Strive only adds cost for targets beyond the first.
        // With 1 target, no surcharge is added.
        use crate::types::mana::ManaCostShard;
        let base = ManaCost::Cost {
            shards: vec![ManaCostShard::Blue],
            generic: 1,
        };
        let target_count = 1usize;
        // No fold iterations when target_count == 1
        let adjusted = if target_count > 1 {
            let strive_cost = ManaCost::Cost {
                shards: vec![ManaCostShard::Blue],
                generic: 2,
            };
            (1..target_count).fold(base.clone(), |acc, _| {
                super::restrictions::add_mana_cost(&acc, &strive_cost)
            })
        } else {
            base.clone()
        };
        assert_eq!(adjusted.mana_value(), base.mana_value());
    }

    #[test]
    fn strive_surcharge_with_two_targets() {
        // CR 207.2c + CR 601.2f: With 2 targets, add strive cost once.
        use crate::types::mana::ManaCostShard;
        let base = ManaCost::Cost {
            shards: vec![ManaCostShard::Blue],
            generic: 1,
        };
        let strive_cost = ManaCost::Cost {
            shards: vec![ManaCostShard::Blue],
            generic: 2,
        };
        let target_count = 2usize;
        let adjusted = (1..target_count).fold(base.clone(), |acc, _| {
            super::restrictions::add_mana_cost(&acc, &strive_cost)
        });
        // {1}{U} + {2}{U} = {3}{U}{U}
        assert_eq!(adjusted.mana_value(), 5);
    }

    // --- CR 601.2b: Defiler cost reduction tests ---

    #[test]
    fn find_defiler_reduction_matches_color() {
        use crate::types::ability::StaticDefinition;
        use crate::types::mana::{ManaColor, ManaCostShard};
        use crate::types::statics::StaticMode;

        let mut state = GameState::new_two_player(42);

        // Create a green creature spell being cast
        let spell_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Grizzly Bears".to_string(),
            Zone::Hand,
        );
        state
            .objects
            .get_mut(&spell_id)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);
        state.objects.get_mut(&spell_id).unwrap().color = vec![ManaColor::Green];

        // Create Defiler of Vigor (green Defiler) on battlefield
        let defiler_id = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Defiler of Vigor".to_string(),
            Zone::Battlefield,
        );
        let reduction = ManaCost::Cost {
            shards: vec![ManaCostShard::Green],
            generic: 0,
        };
        state
            .objects
            .get_mut(&defiler_id)
            .unwrap()
            .static_definitions
            .push(StaticDefinition::new(StaticMode::DefilerCostReduction {
                color: ManaColor::Green,
                life_cost: 2,
                mana_reduction: reduction.clone(),
            }));

        let result = find_defiler_reduction(&state, PlayerId(0), spell_id);
        assert!(
            result.is_some(),
            "Should find Defiler reduction for green spell"
        );
        let (life, mana_red) = result.unwrap();
        assert_eq!(life, 2);
        assert_eq!(mana_red, reduction);
    }

    #[test]
    fn find_defiler_reduction_ignores_wrong_color() {
        use crate::types::ability::StaticDefinition;
        use crate::types::mana::{ManaColor, ManaCostShard};
        use crate::types::statics::StaticMode;

        let mut state = GameState::new_two_player(42);

        // Create a red creature spell
        let spell_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Goblin Guide".to_string(),
            Zone::Hand,
        );
        state
            .objects
            .get_mut(&spell_id)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);
        state.objects.get_mut(&spell_id).unwrap().color = vec![ManaColor::Red];

        // Create Defiler of Vigor (green) — should not match red spell
        let defiler_id = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Defiler of Vigor".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&defiler_id)
            .unwrap()
            .static_definitions
            .push(StaticDefinition::new(StaticMode::DefilerCostReduction {
                color: ManaColor::Green,
                life_cost: 2,
                mana_reduction: ManaCost::Cost {
                    shards: vec![ManaCostShard::Green],
                    generic: 0,
                },
            }));

        let result = find_defiler_reduction(&state, PlayerId(0), spell_id);
        assert!(
            result.is_none(),
            "Green Defiler should not reduce red spell"
        );
    }

    #[test]
    fn find_defiler_reduction_ignores_non_permanent() {
        use crate::types::ability::StaticDefinition;
        use crate::types::mana::{ManaColor, ManaCostShard};
        use crate::types::statics::StaticMode;

        let mut state = GameState::new_two_player(42);

        // Create a green instant spell (not a permanent)
        let spell_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Giant Growth".to_string(),
            Zone::Hand,
        );
        state
            .objects
            .get_mut(&spell_id)
            .unwrap()
            .card_types
            .core_types
            .push(crate::types::card_type::CoreType::Instant);
        state.objects.get_mut(&spell_id).unwrap().color = vec![ManaColor::Green];

        // Create Defiler
        let defiler_id = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Defiler of Vigor".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&defiler_id)
            .unwrap()
            .static_definitions
            .push(StaticDefinition::new(StaticMode::DefilerCostReduction {
                color: ManaColor::Green,
                life_cost: 2,
                mana_reduction: ManaCost::Cost {
                    shards: vec![ManaCostShard::Green],
                    generic: 0,
                },
            }));

        let result = find_defiler_reduction(&state, PlayerId(0), spell_id);
        assert!(
            result.is_none(),
            "Defiler should not reduce non-permanent spells"
        );
    }

    #[test]
    fn handle_defiler_payment_accepted_reduces_cost() {
        use crate::types::mana::ManaCostShard;

        let mut state = GameState::new_two_player(42);
        state.players[0].life = 20;

        let spell_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Test Creature".to_string(),
            Zone::Hand,
        );

        let ability = crate::types::ability::ResolvedAbility::new(
            Effect::Unimplemented {
                name: "Permanent".to_string(),
                description: None,
            },
            Vec::new(),
            spell_id,
            PlayerId(0),
        );

        let pending = PendingCast::new(
            spell_id,
            CardId(1),
            ability,
            ManaCost::Cost {
                shards: vec![ManaCostShard::Green, ManaCostShard::Green],
                generic: 2,
            },
        );

        let mana_reduction = ManaCost::Cost {
            shards: vec![ManaCostShard::Green],
            generic: 0,
        };

        let mut events = Vec::new();
        let _result = handle_defiler_payment(
            &mut state,
            PlayerId(0),
            pending,
            2,
            &mana_reduction,
            true,
            &mut events,
        );

        // Life should be reduced by 2
        assert_eq!(state.players[0].life, 18, "Life should decrease by 2");

        // Check that a LifeChanged event was emitted
        assert!(
            events.iter().any(|e| matches!(
                e,
                GameEvent::LifeChanged {
                    player_id,
                    amount: -2
                } if *player_id == PlayerId(0)
            )),
            "Should emit LifeChanged event"
        );
    }
}
