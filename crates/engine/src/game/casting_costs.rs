use std::collections::HashSet;

use crate::types::ability::{AbilityCost, AdditionalCost, ResolvedAbility};
use crate::types::events::GameEvent;
use crate::types::game_state::{
    CastingVariant, ConvokeMode, DistributionUnit, GameState, PendingCast, StackEntry,
    StackEntryKind, WaitingFor,
};
use crate::types::identifiers::{CardId, ObjectId};
use crate::types::keywords::Keyword;
use crate::types::mana::{ManaCost, ManaCostShard, ManaType, PaymentContext};
use crate::types::player::PlayerId;
use crate::types::zones::Zone;

use super::casting::emit_targeting_events;
use super::engine::EngineError;
use super::mana_abilities;
use super::mana_payment;
use super::mana_sources::{self, ManaSourceOption};
use super::restrictions;
use super::stack;

use super::ability_utils::{
    assign_targets_in_chain, auto_select_targets_for_ability, begin_target_selection_for_ability,
    build_target_slots, flatten_targets_in_chain,
};
use super::life_costs::{pay_life_as_cost, PayLifeCostResult};

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
            updated_pending.origin_zone,
            events,
        )
    }
}

/// Complete the discard-for-cost flow: discard selected cards, then continue casting.
pub(crate) fn handle_discard_for_cost(
    state: &mut GameState,
    player: PlayerId,
    mut pending: PendingCast,
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

    // CR 117.1 + CR 202.3: Capture the discarded card's mana value BEFORE
    // it leaves the hand, so `QuantityRef::CostPaidObjectManaValue` can
    // resolve at ability resolution. Volrath the Fallen ("Volrath gets
    // +X/+X until end of turn, where X is the discarded card's mana value").
    if let Some(&first) = chosen.first() {
        if let Some(mv) = state
            .objects
            .get(&first)
            .map(|obj| obj.mana_cost.mana_value())
        {
            pending
                .ability
                .set_cost_paid_object_mana_value_recursive(mv);
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
            pending.origin_zone,
            events,
        )
    }
}

/// CR 118.3 + CR 601.2b: Complete sacrifice-as-cost after player selection.
pub(crate) fn handle_sacrifice_for_cost(
    state: &mut GameState,
    player: PlayerId,
    mut pending: PendingCast,
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

    // CR 117.1 + CR 202.3: Capture the sacrificed object's mana value BEFORE
    // it leaves the battlefield, stamping it onto the resolving ability for
    // later read by `QuantityRef::CostPaidObjectManaValue` (Food Chain,
    // Burnt Offering, Metamorphosis). Uses the first chosen permanent — the
    // class only ever sacrifices one cost-tracked object.
    if let Some(&first) = chosen.first() {
        if let Some(mv) = state
            .objects
            .get(&first)
            .map(|obj| obj.mana_cost.mana_value())
        {
            pending
                .ability
                .set_cost_paid_object_mana_value_recursive(mv);
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
            pending.origin_zone,
            events,
        )
    }
}

/// CR 118.3 + CR 601.2b: Complete return-to-hand-as-cost after player selection.
pub(crate) fn handle_return_to_hand_for_cost(
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
            "Must return exactly {} permanent(s), got {}",
            count,
            chosen.len()
        )));
    }
    for id in chosen {
        if !legal_permanents.contains(id) {
            return Err(EngineError::InvalidAction(
                "Selected permanent not eligible to return".to_string(),
            ));
        }
    }

    for &id in chosen {
        super::zones::move_to_zone(state, id, Zone::Hand, events);
    }

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
            pending.origin_zone,
            events,
        )
    }
}

/// Blight cost — put -1/-1 counters on chosen creatures after player selection.
pub(crate) fn handle_blight_choice(
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
            "Must blight exactly {} creature(s), got {}",
            count,
            chosen.len()
        )));
    }
    for id in chosen {
        if !legal_creatures.contains(id) {
            return Err(EngineError::InvalidAction(
                "Selected creature not eligible for blight".to_string(),
            ));
        }
    }

    // Put a -1/-1 counter on each chosen creature
    for &id in chosen {
        if let Some(obj) = state.objects.get_mut(&id) {
            *obj.counters
                .entry(crate::types::counter::CounterType::Minus1Minus1)
                .or_insert(0) += 1;
        }
    }

    // Resume casting
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
            pending.origin_zone,
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
            pending.origin_zone,
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
        pending.origin_zone,
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
    // Pay remaining sub-costs (Tap, Mana, etc.) — choice-based costs already paid
    // by a WaitingFor flow are no-ops here, so resuming with the full cost is idempotent.
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
    mut resolved: ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    let entry_id = ObjectId(state.next_object_id);
    state.next_object_id += 1;

    // CR 603.4: Stamp the printed-ability index for per-turn resolution tracking.
    resolved.ability_index = Some(ability_index);
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
    // CR 117.1b: Priority permits unbounded activation. `pending_activations`
    // is a per-priority-window AI-guard — see `GameState::pending_activations`.
    state.pending_activations.push((source_id, ability_index));
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
    origin_zone: Zone,
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
        origin_zone,
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
    origin_zone: Zone,
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
                // CR 601.2b: Required additional cost whose choice-of-object is
                // unavailable makes the spell uncastable.
                if !req_cost.is_payable(state, player, object_id) {
                    return Err(EngineError::ActionNotAllowed(
                        "Cannot pay required additional cost".to_string(),
                    ));
                }
                // Required additional costs bypass the choice prompt — pay directly.
                let mut pending = PendingCast::new(object_id, card_id, ability, cost.clone());
                pending.casting_variant = casting_variant;
                pending.origin_zone = origin_zone;
                return pay_additional_cost(state, player, req_cost.clone(), pending, events);
            }
            AdditionalCost::Optional(opt_cost) => {
                let mut pending = PendingCast::new(object_id, card_id, ability, cost.clone());
                pending.casting_variant = casting_variant;
                pending.origin_zone = origin_zone;
                // CR 601.2b: If the optional additional cost requires a choice
                // of object and no legal object exists, skip the prompt and
                // proceed as if the player declined to pay.
                if !opt_cost.is_payable(state, player, object_id) {
                    return pay_and_push(
                        state,
                        player,
                        object_id,
                        card_id,
                        pending.ability,
                        &pending.cost,
                        casting_variant,
                        distribute,
                        origin_zone,
                        events,
                    );
                }
                return Ok(WaitingFor::OptionalCostChoice {
                    player,
                    cost: additional_cost,
                    pending_cast: Box::new(pending),
                });
            }
            AdditionalCost::Choice(preferred, fallback) => {
                let mut pending = PendingCast::new(object_id, card_id, ability, cost.clone());
                pending.casting_variant = casting_variant;
                pending.origin_zone = origin_zone;
                // CR 601.2b: If the preferred branch is unpayable, fall through
                // to the fallback without prompting. If both are unpayable, the
                // spell cannot be cast.
                if !preferred.is_payable(state, player, object_id) {
                    if !fallback.is_payable(state, player, object_id) {
                        return Err(EngineError::ActionNotAllowed(
                            "Cannot pay either alternative additional cost".to_string(),
                        ));
                    }
                    return pay_additional_cost(state, player, fallback.clone(), pending, events);
                }
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
        pending.origin_zone = origin_zone;
        return pay_additional_cost(
            state,
            player,
            AbilityCost::PayEnergy { amount: energy_mv },
            pending,
            events,
        );
    }

    // CR 118.9 + CR 119.4: ExileWithAltAbilityCost — non-mana alternative cost
    // (e.g. Nashi's "pay life equal to its mana value rather than paying its
    // mana cost"). The mana cost was already overridden to zero in
    // `casting::cast_spell` via `alt_cost_from_exile`; here we route the stored
    // `AbilityCost` through `pay_additional_cost` so dynamic-quantity refs
    // (`EventContextSourceManaValue`, etc.) resolve at cast time against the
    // spell's mana value. Single-authority — `AbilityCost::PayLife` and friends
    // are paid through the same pipeline as flashback's non-mana cost.
    let alt_ability_cost = state.objects.get(&object_id).and_then(|obj| {
        if obj.zone == Zone::Exile {
            obj.casting_permissions.iter().find_map(|p| match p {
                crate::types::ability::CastingPermission::ExileWithAltAbilityCost {
                    cost, ..
                } => Some(cost.clone()),
                _ => None,
            })
        } else if obj.zone == Zone::Library && obj.owner == player {
            // CR 401.5 + CR 118.9 + CR 601.2a: Top-of-library cast with an
            // alt-cost rider (Bolas's Citadel: "pay life equal to its mana
            // value rather than paying its mana cost"). The static lives on
            // the granting permanent on the battlefield, not on the spell;
            // resolve through `top_of_library_permission_source` to fetch it.
            super::casting::top_of_library_permission_source(
                state,
                player,
                Some(crate::types::ability::CardPlayMode::Cast),
            )
            .and_then(|(top_id, _src, alt)| if top_id == object_id { alt } else { None })
        } else {
            None
        }
    });
    if let Some(alt_cost) = alt_ability_cost {
        let mut pending = PendingCast::new(object_id, card_id, ability, cost.clone());
        pending.casting_variant = casting_variant;
        pending.distribute = distribute;
        pending.origin_zone = origin_zone;
        return pay_additional_cost(state, player, alt_cost, pending, events);
    }

    // CR 702.138a: Escape requires exiling N other cards from graveyard.
    if casting_variant == CastingVariant::Escape {
        if let Some((_, exile_count)) = super::keywords::effective_escape_data(state, object_id) {
            let mut pending = PendingCast::new(object_id, card_id, ability, cost.clone());
            pending.casting_variant = casting_variant;
            pending.origin_zone = origin_zone;
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

    // CR 702.34a + CR 118.8: Flashback with a non-mana additional cost (Battle
    // Screech's "tap three white creatures") or a compound cost (Deep Analysis's
    // "{1}{U}, Pay 3 life") routes the residual non-mana sub-cost through
    // `pay_additional_cost`. The mana sub-cost (if any) was already extracted
    // into `cost` upstream by `split_flashback_cost_components` and is paid via
    // the normal mana-payment flow inside `pay_additional_cost`'s fall-through.
    if casting_variant == CastingVariant::Flashback {
        let flashback_cost = super::keywords::effective_flashback_cost(state, object_id);
        let (_mana, residual) =
            super::casting::split_flashback_cost_components(flashback_cost.as_ref());
        if let Some(non_mana_cost) = residual {
            let mut pending = PendingCast::new(object_id, card_id, ability, cost.clone());
            pending.casting_variant = casting_variant;
            pending.distribute = distribute;
            pending.origin_zone = origin_zone;
            return pay_additional_cost(state, player, non_mana_cost, pending, events);
        }
    }

    // CR 601.2b: Check for Defiler cost reduction — optional life payment for colored mana
    // reduction on matching-color permanent spells.
    if let Some((life_cost, mana_reduction)) = find_defiler_reduction(state, player, object_id) {
        let mut pending = PendingCast::new(object_id, card_id, ability, cost.clone());
        pending.casting_variant = casting_variant;
        pending.distribute = distribute;
        pending.origin_zone = origin_zone;
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
        origin_zone,
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

    // CR 702.26b + CR 604.1: `battlefield_active_statics` owns the gating.
    for (bf_obj, def) in super::functioning_abilities::battlefield_active_statics(state) {
        if bf_obj.controller != caster {
            continue;
        }
        {
            if let StaticMode::DefilerCostReduction {
                color,
                life_cost,
                mana_reduction,
            } = &def.mode
            {
                if spell_colors.contains(color) {
                    // CR 118.3 + CR 119.4b + CR 119.8: Don't offer the Defiler
                    // prompt when the caster can't actually pay the life — this
                    // keeps the UI from presenting an impossible choice.
                    if !super::life_costs::can_pay_life_cost(state, caster, *life_cost) {
                        return None;
                    }
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
        // CR 118.3b + CR 119.4 + CR 119.8: Defiler's optional life payment is a
        // cost — route through the single-authority helper so the replacement
        // pipeline and CantLoseLife lock are honored. If the cost can't be paid
        // (insufficient life or locked), fall through to casting without the
        // reduction — the Defiler prompt must not half-apply.
        let payment = pay_life_as_cost(state, player, life_cost, events);
        let reduction_applied = payment.is_paid();
        match payment {
            PayLifeCostResult::Paid { .. } => {}
            PayLifeCostResult::InsufficientLife | PayLifeCostResult::LockedCantLoseLife => {
                // Proceed with the original cost; no reduction.
            }
        }
        if !reduction_applied {
            return pay_and_push(
                state,
                player,
                pending.object_id,
                pending.card_id,
                pending.ability,
                &cost,
                pending.casting_variant,
                pending.distribute,
                pending.origin_zone,
                events,
            );
        }

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
        pending.origin_zone,
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
            // CR 118.3 + CR 119.4 + CR 119.8: Pay life as an additional cost via
            // the single-authority helper. Unpayable = spell cannot be cast.
            // CR 119.4 + CR 903.4: `amount` is a QuantityExpr so dynamic refs
            // (e.g. commander color identity count) resolve at cast time.
            let resolved =
                super::quantity::resolve_quantity(state, &amount, player, pending.object_id).max(0)
                    as u32;
            match pay_life_as_cost(state, player, resolved, events) {
                PayLifeCostResult::Paid { .. } => {}
                PayLifeCostResult::InsufficientLife | PayLifeCostResult::LockedCantLoseLife => {
                    return Err(EngineError::ActionNotAllowed(
                        "Cannot pay life cost".to_string(),
                    ));
                }
            }
        }
        AbilityCost::Blight { count } => {
            // Blight N — player chooses creature(s) to put -1/-1 counters on.
            // Per reminder text: "(You may put a -1/-1 counter on a creature you control.)"
            let creatures: Vec<ObjectId> = state
                .battlefield
                .iter()
                .copied()
                .filter(|id| {
                    state.objects.get(id).is_some_and(|obj| {
                        obj.controller == player
                            && obj
                                .card_types
                                .core_types
                                .contains(&crate::types::card_type::CoreType::Creature)
                    })
                })
                .collect();
            // CR 601.2b: Defense-in-depth — the upstream gate must have already
            // caught an empty eligibility set. Never construct a dead WaitingFor.
            if creatures.len() < count as usize {
                return Err(EngineError::ActionNotAllowed(
                    "Not enough creatures to blight".to_string(),
                ));
            }
            return Ok(WaitingFor::BlightChoice {
                player,
                count: count as usize,
                creatures,
                pending_cast: Box::new(pending),
            });
        }
        AbilityCost::Discard { count, filter, .. } => {
            let count = super::quantity::resolve_quantity(state, &count, player, pending.object_id)
                .max(0) as usize;
            // CR 601.2b: Discard requires interactive card selection — return a WaitingFor.
            let eligible = super::casting::find_eligible_discard_targets(
                state,
                player,
                pending.object_id,
                filter.as_ref(),
            );
            // CR 601.2b: Defense-in-depth — empty hand means no legal choice.
            if eligible.len() < count {
                return Err(EngineError::ActionNotAllowed(
                    "Not enough cards in hand to discard".to_string(),
                ));
            }
            return Ok(WaitingFor::DiscardForCost {
                player,
                count,
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
                pending.origin_zone,
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
        AbilityCost::ReturnToHand { count, ref filter } => {
            let eligible = super::casting::find_eligible_return_to_hand_targets(
                state,
                player,
                pending.object_id,
                filter.as_ref(),
            );
            if eligible.len() < count as usize {
                return Err(EngineError::ActionNotAllowed(
                    "Not enough eligible permanents to return".into(),
                ));
            }
            return Ok(WaitingFor::ReturnToHandForCost {
                player,
                count: count as usize,
                permanents: eligible,
                pending_cast: Box::new(pending),
            });
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
            return enter_payment_step(state, player, Some(ConvokeMode::Waterbend), events);
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
        pending.origin_zone,
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
    origin_zone: Zone,
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
                pending.origin_zone = origin_zone;
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
        origin_zone,
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
    origin_zone: Zone,
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
        pending.origin_zone = origin_zone;
        state.pending_cast = Some(Box::new(pending));
        return enter_payment_step(state, player, convoke_mode, events);
    }

    // CR 107.4f + CR 601.2f: Pause for interactive Phyrexian choice when the cost has
    // at least one shard with both mana and 2-life viable. The resume handler calls
    // `finalize_mana_payment_with_phyrexian_choices` which finishes the cast.
    if let Some(waiting) = maybe_pause_for_phyrexian_choice(state, player, object_id, cost, events)
    {
        let mut pending = PendingCast::new(object_id, card_id, ability, cost.clone());
        pending.casting_variant = casting_variant;
        pending.distribute = distribute;
        pending.origin_zone = origin_zone;
        state.pending_cast = Some(Box::new(pending));
        return Ok(waiting);
    }

    finalize_cast(
        state,
        player,
        object_id,
        card_id,
        ability,
        cost,
        casting_variant,
        origin_zone,
        events,
    )
}

/// CR 601.2i: Finalize a spell cast.
///
/// By the time this runs, `announce_spell_on_stack` has already pushed a
/// placeholder `StackEntry` with `ability: None, actual_mana_spent: 0`. The
/// object's `zone` field, however, is still at `origin_zone` — zone transition
/// is deferred here so continuous effects that granted castability (e.g.
/// "cards in your graveyard have escape") keep applying through cost payment.
/// This function:
///   1. Snapshots the mana pool, pays the declared cost, and records the actual
///      amount deducted (CR 700.14 — matters for cost reductions / convoke).
///   2. Moves the object from `origin_zone` to `Zone::Stack` now that the cast
///      is committed.
///   3. Updates the existing stack entry's `ability` (filling in the resolved
///      on-resolve effect) and `actual_mana_spent`.
///   4. Emits `SpellCast` (CR 603.6a — the trigger point for "whenever a player
///      casts a spell"), records commander cast taxes, and consumes any
///      graveyard-cast permissions / one-shot cost reductions.
///
/// Shared by `pay_and_push_adventure` (normal casting) and the
/// `(ManaPayment, PassPriority)` handler (after interactive mana payment).
#[allow(clippy::too_many_arguments)]
pub(super) fn finalize_cast(
    state: &mut GameState,
    player: PlayerId,
    object_id: ObjectId,
    card_id: CardId,
    ability: ResolvedAbility,
    cost: &crate::types::mana::ManaCost,
    casting_variant: CastingVariant,
    origin_zone: Zone,
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    finalize_cast_with_phyrexian_choices(
        state,
        player,
        object_id,
        card_id,
        ability,
        cost,
        casting_variant,
        origin_zone,
        None,
        events,
    )
}

/// CR 107.4f + CR 601.2f: Variant of `finalize_cast` that threads explicit per-shard
/// Phyrexian choices through `pay_mana_cost_with_choices`. `None` preserves
/// auto-decide behavior.
#[allow(clippy::too_many_arguments)]
pub(super) fn finalize_cast_with_phyrexian_choices(
    state: &mut GameState,
    player: PlayerId,
    object_id: ObjectId,
    card_id: CardId,
    ability: ResolvedAbility,
    cost: &crate::types::mana::ManaCost,
    casting_variant: CastingVariant,
    origin_zone: Zone,
    phyrexian_choices: Option<&[crate::types::game_state::ShardChoice]>,
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    // CR 702.85a: Evaluate the cascade resulting-MV constraint BEFORE mana is
    // paid. By this point the player has chosen X (CR 601.2b runs at
    // `enter_payment_step`/`ChooseXValue`), so `ability.chosen_x` reflects the
    // final cost-X. Evaluating here means a rejection has nothing to rewind:
    // no mana has left the pool, no `cost_x_paid` has been stamped, and no
    // targets are committed beyond the announcement-time selections (which
    // `handle_cascade_rejection` clears alongside popping the stack entry).
    //
    // For the constraint we synthesize the resulting MV from the printed cost
    // + chosen_x rather than reading `obj.cost_x_paid`, since the latter is
    // not stamped until after payment further below.
    let cascade_resulting_mv = state
        .objects
        .get(&object_id)
        .map(|obj| obj.mana_cost.mana_value() + ability.chosen_x.unwrap_or(0));
    if let Some(resulting_mv) = cascade_resulting_mv {
        match evaluate_cascade_constraint_with_resulting_mv(state, object_id, resulting_mv, events)
        {
            CascadeCheck::NotApplicable | CascadeCheck::Accepted => {}
            CascadeCheck::Rejected { exiled_misses } => {
                return handle_cascade_rejection(state, player, object_id, exiled_misses, events);
            }
        }
    }

    // CR 700.14: Snapshot pool size before payment to compute actual mana spent.
    let pool_before = state
        .players
        .iter()
        .find(|p| p.id == player)
        .map(|p| p.mana_pool.total())
        .unwrap_or(0);

    super::casting::pay_mana_cost_with_choices(
        state,
        player,
        object_id,
        cost,
        phyrexian_choices,
        events,
    )?;

    // CR 702.190a: Sneak alt-cost additionally requires returning an unblocked
    // attacker to its owner's hand. The spell was announced to the stack above;
    // the returned creature is paid here as part of cost payment, after mana
    // but before the stack entry is finalized with its ResolvedAbility. Also
    // scrub the returned creature from combat so it is no longer an attacker.
    if let CastingVariant::Sneak {
        returned_creature, ..
    } = casting_variant
    {
        super::zones::move_to_zone(state, returned_creature, Zone::Hand, events);
        if let Some(combat) = state.combat.as_mut() {
            combat
                .attackers
                .retain(|a| a.object_id != returned_creature);
            combat.blocker_assignments.remove(&returned_creature);
        }
    }

    // CR 700.14: Compute actual mana deducted from pool (not declared cost).
    let pool_after = state
        .players
        .iter()
        .find(|p| p.id == player)
        .map(|p| p.mana_pool.total())
        .unwrap_or(0);
    let actual_mana_spent = pool_before.saturating_sub(pool_after) as u32;

    // CR 603.4 + CR 903.8: `origin_zone` preserves the pre-announcement zone so
    // that "cast from hand/graveyard/exile" conditions evaluate correctly and
    // commander-tax bookkeeping fires only when casting from the command zone.
    // The actual Hand→Stack zone transition is deferred to later in this
    // function (see the `move_to_zone` call below), after mana payment has
    // completed against the origin zone.
    let was_in_command_zone = origin_zone == Zone::Command
        && state
            .objects
            .get(&object_id)
            .map(|obj| obj.is_commander)
            .unwrap_or(false);
    let source_zone = origin_zone;

    // CR 603.4: Record the zone the spell was cast from so ETB triggers can
    // evaluate conditions like "if you cast it from your hand".
    let mut ability = ability;
    ability.context.cast_from_zone = Some(source_zone);

    // Emit targeting events now that the cast is committed.
    emit_targeting_events(
        state,
        &flatten_targets_in_chain(&ability),
        object_id,
        player,
        events,
    );

    // CR 107.3m: Stash the paid X value directly on the permanent so replacement
    // effects ("enters with X counters") and ETB triggered abilities that
    // reference the cost X (via `QuantityRef::CostXPaid`) can resolve after the
    // spell leaves the stack. Set regardless of placeholder vs. real ability —
    // permanent spells with no on-resolve ability still need this for ETB
    // replacements on X-cost cards like Astral Cornucopia, Walking Ballista, etc.
    let cost_x_paid = ability.chosen_x;

    // Determine whether this spell has a meaningful on-resolve ability.
    // Permanent spells with no Spell-kind AbilityDefinition get a placeholder
    // Unimplemented effect through the cost pipeline (from continue_with_no_ability).
    // Only those remain `ability: None` on the stack — they simply enter the
    // battlefield on resolution. All other spells get their ResolvedAbility.
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

    // CR 107.3m: Apply the paid-X snapshot to the object (after the placeholder
    // branch has already taken a mutable borrow). Done unconditionally so that
    // non-placeholder paths (permanents whose on-resolve ability also references
    // CostXPaid, e.g. future cards) share the same source-of-truth lookup.
    if let Some(x) = cost_x_paid {
        if let Some(obj) = state.objects.get_mut(&object_id) {
            obj.cost_x_paid = Some(x);
        }
    }

    // CR 601.2a + CR 601.2i: The spell was announced onto the stack earlier,
    // but the object's `zone` field stayed at its origin through cost payment
    // so continuous effects that granted castability ("cards in your graveyard
    // have escape", "spells you cast from exile have convoke") continued to
    // apply. Now that the cast is committed, perform the Hand→Stack zone
    // transition so zone-change triggers, counterspell targeting
    // (`FilterProp::InZone { Stack }`), and on-resolution bookkeeping all see
    // the spell as living on the stack.
    super::zones::move_to_zone(state, object_id, Zone::Stack, events);

    // CR 601.2i: Update the existing stack entry (pushed at announcement) with
    // the finalized ability and the actual mana spent. The entry must still be
    // present — no one else can have pushed/popped between announce and
    // finalize within a single cast.
    let entry = state
        .stack
        .iter_mut()
        .rfind(|entry| entry.id == object_id)
        .expect("spell stack entry from announcement still present at finalize");
    entry.kind = StackEntryKind::Spell {
        card_id,
        ability: stack_ability,
        casting_variant,
        actual_mana_spent,
    };

    // Track commander cast count for tax calculation
    if was_in_command_zone {
        super::commander::record_commander_cast(state, object_id);
    }

    state.priority_passes.clear();
    state.priority_pass_count = 0;

    events.push(GameEvent::SpellCast {
        card_id,
        controller: player,
        object_id,
    });

    // CR 601.2a + CR 601.2b: Record permission usage when spell is finalized onto
    // the stack. This prevents casting a second spell via the same source before
    // the first resolves. Only `OncePerTurn` frequencies need tracking;
    // `Unlimited` permissions (Conduit of Worlds, Omniscience) skip.
    match casting_variant {
        CastingVariant::GraveyardPermission {
            source,
            frequency: crate::types::statics::CastFrequency::OncePerTurn,
        } => {
            state.graveyard_cast_permissions_used.insert(source);
        }
        CastingVariant::HandPermission {
            source,
            frequency: crate::types::statics::CastFrequency::OncePerTurn,
        } => {
            state.hand_cast_free_permissions_used.insert(source);
        }
        _ => {}
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

/// CR 702.85a: Outcome of evaluating a cascade cast-time constraint.
enum CascadeCheck {
    /// No cascade constraint on this object — the cast proceeds normally.
    NotApplicable,
    /// The constraint passed (resulting MV < source MV). The cast proceeds;
    /// the misses have already been bottom-shuffled as a side effect.
    Accepted,
    /// The constraint failed (resulting MV >= source MV). The cast must be
    /// aborted; the caller should unwind the announcement stack entry and
    /// route through `handle_cascade_rejection`.
    Rejected { exiled_misses: Vec<ObjectId> },
}

/// CR 702.85a: Inspect the casting object's `ExileWithAltCost` permissions for
/// a cascade constraint and evaluate it against the resulting spell's mana
/// value. Consumes the matched cascade permission (only); other permissions
/// with `constraint: None` (Suspend, Airbending, Discover, ...) are untouched.
///
/// On acceptance, bottom-shuffles the exiled misses here so both accept paths
/// (plain free cast + X-cost cast) share a single cleanup point.
///
/// `resulting_mv` is the resulting spell's mana value as seen by CR 702.85a's
/// "resulting spell's mana value" comparison — i.e. printed `mana_cost.mana_value()`
/// plus the chosen X. Caller is responsible for synthesizing this because X is
/// known at announcement time but `obj.cost_x_paid` is not stamped until after
/// mana payment.
fn evaluate_cascade_constraint_with_resulting_mv(
    state: &mut GameState,
    object_id: ObjectId,
    resulting_mv: u32,
    events: &mut Vec<GameEvent>,
) -> CascadeCheck {
    use crate::types::ability::{CastPermissionConstraint, CastingPermission};

    let index = match state.objects.get(&object_id) {
        Some(obj) => obj.casting_permissions.iter().position(|p| {
            matches!(
                p,
                CastingPermission::ExileWithAltCost {
                    constraint: Some(CastPermissionConstraint::CascadeResultingMvBelow { .. }),
                    ..
                }
            )
        }),
        None => return CascadeCheck::NotApplicable,
    };
    let index = match index {
        Some(i) => i,
        None => return CascadeCheck::NotApplicable,
    };

    let permission = state
        .objects
        .get_mut(&object_id)
        .expect("object present above")
        .casting_permissions
        .remove(index);
    let (source_mv, exiled_misses) = match permission {
        CastingPermission::ExileWithAltCost {
            constraint:
                Some(CastPermissionConstraint::CascadeResultingMvBelow {
                    source_mv,
                    exiled_misses,
                }),
            ..
        } => (source_mv, exiled_misses),
        _ => unreachable!("position() already filtered to this variant"),
    };

    if resulting_mv < source_mv {
        // CR 702.85a: "cards exiled this way that weren't cast" — the hit is
        // being cast, so only the misses bottom-shuffle.
        crate::game::effects::cascade::shuffle_to_bottom(state, &exiled_misses, events);
        CascadeCheck::Accepted
    } else {
        CascadeCheck::Rejected { exiled_misses }
    }
}

/// CR 702.85a: Unwind a cascade-rejected cast — remove the announcement-time
/// stack entry, bottom-shuffle the misses + hit card together in a random
/// order, and return priority to the caster.
fn handle_cascade_rejection(
    state: &mut GameState,
    player: PlayerId,
    object_id: ObjectId,
    exiled_misses: Vec<ObjectId>,
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    // CR 601.2a: Remove the announcement-time stack entry. The spell never
    // finishes entering the stack because we abort before the Hand→Stack
    // zone move in `finalize_cast_with_phyrexian_choices`.
    if let Some(pos) = state.stack.iter().rposition(|entry| entry.id == object_id) {
        state.stack.remove(pos);
    }

    // CR 702.85a: Misses + the hit (declined at cast time) all bottom-shuffle
    // together in a random order.
    let mut all_to_bottom = exiled_misses;
    all_to_bottom.push(object_id);
    crate::game::effects::cascade::shuffle_to_bottom(state, &all_to_bottom, events);

    // CR 601.2a: Priority returns to the would-be caster.
    state.priority_passes.clear();
    state.priority_pass_count = 0;
    Ok(WaitingFor::Priority { player })
}

/// Count distinct source objects that can produce any of the `acceptable` colors.
fn count_available_sources(
    available: &[ManaSourceOption],
    used: &HashSet<ObjectId>,
    acceptable: &[ManaType],
) -> usize {
    let mut seen = HashSet::new();
    for opt in available {
        // CR 605.3b: Filter-land combination rows contribute multi-mana
        // atomically. Any color in their combination satisfies the shard.
        if !used.contains(&opt.object_id) && option_satisfies(opt, acceptable) {
            seen.insert(opt.object_id);
        }
    }
    seen.len()
}

/// True iff this source option can contribute any of the acceptable colors.
/// For single-color rows, checks `mana_type` directly; for combination rows,
/// checks whether any color in the combination is acceptable.
fn option_satisfies(opt: &ManaSourceOption, acceptable: &[ManaType]) -> bool {
    match &opt.atomic_combination {
        Some(combo) => combo.iter().any(|t| acceptable.contains(t)),
        None => acceptable.contains(&opt.mana_type),
    }
}

/// Pick the source with the fewest alternative color options (LCV heuristic).
/// Among ties, the tier-sort order of `available` acts as tiebreaker (pure lands
/// before dorks before land-creatures before sacrifice sources).
fn find_least_flexible_source(
    available: &[ManaSourceOption],
    used: &HashSet<ObjectId>,
    acceptable: &[ManaType],
) -> Option<ManaSourceOption> {
    available
        .iter()
        .filter(|opt| !used.contains(&opt.object_id) && option_satisfies(opt, acceptable))
        .min_by_key(|opt| {
            available
                .iter()
                .filter(|o| o.object_id == opt.object_id)
                .count()
        })
        .cloned()
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

    // CR 601.2g: A player may spend mana from their mana pool to pay costs.
    // Plan against the *residual* cost (what the pool can't already cover) so
    // pre-floated mana isn't shadowed by redundant taps — e.g. Sol Ring + an
    // Island floated before casting a 3-mana spell must not tap three more
    // sources. Restriction-aware eligibility is delegated to
    // `reduce_cost_by_pool`, which mirrors the real payment path.
    let spell_meta =
        deprioritize_source.and_then(|sid| super::casting::build_spell_meta(state, player, sid));
    let any_color = super::static_abilities::player_can_spend_as_any_color(state, player);
    let spell_ctx = spell_meta.as_ref().map(PaymentContext::Spell);
    let residual = state
        .players
        .iter()
        .find(|p| p.id == player)
        .map(|p| {
            mana_payment::reduce_cost_by_pool(&p.mana_pool, cost, spell_ctx.as_ref(), any_color)
        })
        .unwrap_or_else(|| cost.clone());

    let (shards, generic) = match &residual {
        ManaCost::NoCost | ManaCost::SelfManaCost => return,
        ManaCost::Cost { shards, generic } if shards.is_empty() && *generic == 0 => return,
        ManaCost::Cost { shards, generic } => (shards.as_slice(), *generic),
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

    // CR 605.3b: Auto-tap sort key. Tier layout (preserved from the
    // pre-refactor sort; the enum factors the two scattered bool flags):
    //   outer (tier_byte): 0 = non-sacrifice mana source; 1 = sacrifice-for-mana
    //     (source will not come back — always last).
    //   middle (card_tier): 0 = pure land, 1 = non-land mana dork,
    //     2 = land-creature (preserve for combat), 3 = deprioritized source
    //     (spell's own source).
    //   inner (priority_amount): penalty sub-tier + fixed-amount tiebreak
    //     (e.g. painland-1 < painland-2 < painland-None). Replaces the
    //     collapsed `harms_controller` bool — amounts now rank.
    // The entire penalty axis is consulted only via `ManaSourcePenalty`
    // methods, so a future variant (e.g. `DiscardsOnActivation`) updates
    // the ordering at one place, not seven.
    available.sort_by_key(|option| {
        let obj = state.objects.get(&option.object_id);
        let is_land = obj.is_some_and(|o| o.card_types.core_types.contains(&CoreType::Land));
        let is_creature =
            obj.is_some_and(|o| o.card_types.core_types.contains(&CoreType::Creature));
        let card_tier: u32 = if deprioritize_source == Some(option.object_id) {
            3
        } else if is_land && is_creature {
            2
        } else if is_land {
            0
        } else {
            1
        };
        (
            option.penalty.tier_byte() as u32,
            card_tier,
            option.penalty.priority_amount(),
        )
    });

    let mut to_tap: Vec<ManaSourceOption> = Vec::new();
    let mut used_sources: HashSet<ObjectId> = HashSet::new();

    // Build the typed shard-requirements list first — used by both the
    // combination pre-pass and the main MCV/LCV loop.
    let mut deferred_generic: usize = 0;
    let mut needs: Vec<(Vec<ManaType>, bool)> = Vec::new();
    for shard in shards {
        use crate::game::mana_payment::{shard_to_mana_type, ShardRequirement};
        match shard_to_mana_type(*shard) {
            ShardRequirement::Single(color) | ShardRequirement::Phyrexian(color) => {
                needs.push((vec![color], false));
            }
            ShardRequirement::Hybrid(a, b) | ShardRequirement::HybridPhyrexian(a, b) => {
                needs.push((vec![a, b], false));
            }
            ShardRequirement::TwoGenericHybrid(color) => {
                needs.push((vec![color], true));
            }
            ShardRequirement::ColorlessHybrid(color) => {
                needs.push((vec![ManaType::Colorless, color], false));
            }
            ShardRequirement::Snow | ShardRequirement::X => {
                deferred_generic += 1;
            }
        }
    }

    let mut assigned = vec![false; needs.len()];

    // Phase 0 (combo pre-pass): CR 605.3b + CR 106.1a — filter-land rows
    // produce a full multi-mana combination atomically. A naive per-shard
    // loop can't see that tapping one filter land satisfies two colored
    // requirements. Pre-allocate combination sources against pairs of
    // still-unfilled shards before falling through to the single-color loop.
    assign_combination_sources(
        &available,
        &needs,
        &mut assigned,
        &mut used_sources,
        &mut to_tap,
    );

    // Phase 1: Assign remaining single-color sources to shards using MCV/LCV.
    // The naive greedy approach (tap first matching source per shard) fails when
    // a flexible source (dual land, multi-color dork) gets consumed for a color
    // that a single-purpose source could have provided, leaving no source for
    // a color only the flexible source can produce.
    //
    // MCV: process the most constrained shard first (fewest available sources).
    // LCV: for each shard, prefer the least flexible source (fewest color options).
    for _ in 0..needs.len() {
        let mut best_idx = None;
        let mut min_sources = usize::MAX;
        for (i, (acceptable, _)) in needs.iter().enumerate() {
            if assigned[i] {
                continue;
            }
            let count = count_available_sources(&available, &used_sources, acceptable);
            if count < min_sources {
                min_sources = count;
                best_idx = Some(i);
            }
        }
        let Some(idx) = best_idx else { break };
        let (ref acceptable, two_generic_fallback) = needs[idx];
        if let Some(option) = find_least_flexible_source(&available, &used_sources, acceptable) {
            used_sources.insert(option.object_id);
            to_tap.push(option);
        } else if two_generic_fallback {
            deferred_generic += 2;
        }
        assigned[idx] = true;
    }

    // Phase 2: satisfy generic cost + deferred shards with any remaining sources.
    // Skip combination sources — their value is in covering colored shards;
    // spending a full 2-mana combination on a single generic is wasteful.
    let mut remaining_generic = generic as usize + deferred_generic;
    for option in &available {
        if remaining_generic == 0 {
            break;
        }
        if option.atomic_combination.is_some() {
            continue;
        }
        if used_sources.insert(option.object_id) {
            to_tap.push(option.clone());
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
                // color_override tells resolve_mana_ability how to resolve the
                // ability's choice dimension. `SingleColor` replays a per-color
                // pick (AnyOneColor/ChoiceAmongExiledColors); `Combination`
                // carries a pre-chosen multi-mana sequence (filter lands).
                // Errors are non-fatal here: auto-tap runs synchronously during payment,
                // so sources can't change state between collection and resolution. If a
                // source is somehow invalid (e.g., removed by a replacement effect), we
                // skip it silently — the player can still manually tap other sources.
                let override_value = match option.atomic_combination {
                    Some(combo) => crate::types::game_state::ProductionOverride::Combination(combo),
                    None => {
                        crate::types::game_state::ProductionOverride::SingleColor(option.mana_type)
                    }
                };
                let _ = mana_abilities::resolve_mana_ability(
                    state,
                    option.object_id,
                    player,
                    &ability_def,
                    events,
                    Some(override_value),
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

/// CR 605.3b + CR 106.1a: Greedy pre-pass for `ManaProduction::ChoiceAmongCombinations`
/// (Shadowmoor/Eventide filter lands). Walks every source permanent that has
/// combination rows, picks the combination that covers the most still-unfilled
/// shards, and marks the source used + shards assigned. Runs before the
/// single-color shard assigner so a filter land's 2 mana is allocated
/// atomically instead of one shard at a time.
///
/// Uniqueness guarantee: every combination row for the same `object_id` shares
/// an `atomic_combination`-bearing identity, but only one such row can be
/// selected per object — when a combo is picked the object is inserted into
/// `used_sources`, blocking further rows of every combination variant.
fn assign_combination_sources(
    available: &[ManaSourceOption],
    needs: &[(Vec<ManaType>, bool)],
    assigned: &mut [bool],
    used_sources: &mut HashSet<ObjectId>,
    to_tap: &mut Vec<ManaSourceOption>,
) {
    // Build per-object candidate list: for each object that has any
    // `atomic_combination`-bearing rows, collect all of its combination rows.
    let mut combo_objects: Vec<ObjectId> = Vec::new();
    for opt in available {
        if opt.atomic_combination.is_some()
            && !combo_objects.contains(&opt.object_id)
            && !used_sources.contains(&opt.object_id)
        {
            combo_objects.push(opt.object_id);
        }
    }

    for oid in combo_objects {
        if used_sources.contains(&oid) {
            continue;
        }
        // Collect this object's combination rows in tier order.
        let candidates: Vec<&ManaSourceOption> = available
            .iter()
            .filter(|o| o.object_id == oid && o.atomic_combination.is_some())
            .collect();
        if candidates.is_empty() {
            continue;
        }

        // Score each candidate combo by the number of still-unfilled shards
        // it can satisfy. A combo's colors are consumed in sequence against
        // unmet needs: the same color unit can only satisfy one shard.
        let mut best_score = 0usize;
        let mut best_combo: Option<(&ManaSourceOption, Vec<usize>)> = None;
        for cand in &candidates {
            let combo = cand
                .atomic_combination
                .as_ref()
                .expect("combination row invariant");
            let (score, covered) = score_combination(combo, needs, assigned);
            if score > best_score {
                best_score = score;
                best_combo = Some((cand, covered));
            }
        }

        // Only commit the combo if it covers at least one colored shard. A
        // combo that covers no colored shards would waste its second mana on
        // generic — Phase 2 picks single-color sources for generic more
        // efficiently.
        if let Some((chosen, covered_indices)) = best_combo {
            used_sources.insert(chosen.object_id);
            to_tap.push((*chosen).clone());
            for idx in covered_indices {
                assigned[idx] = true;
            }
        }
    }
}

/// Simulate applying a combination's mana to still-unfilled shard needs.
/// Returns `(count_of_shards_covered, indices_of_covered_needs)` — each unit
/// of mana in the combination may cover at most one shard. Preference is
/// first-match in need order, mirroring Phase 1's MCV behaviour at a coarser
/// grain (Phase 1 already re-orders per-shard scarcity, so here a naive
/// first-fit is sufficient for the filter-land class).
fn score_combination(
    combo: &[ManaType],
    needs: &[(Vec<ManaType>, bool)],
    assigned: &[bool],
) -> (usize, Vec<usize>) {
    let mut locally_consumed: Vec<bool> = assigned.to_vec();
    let mut covered = Vec::new();
    for mana in combo {
        for (i, (acceptable, _)) in needs.iter().enumerate() {
            if locally_consumed[i] {
                continue;
            }
            if acceptable.contains(mana) {
                locally_consumed[i] = true;
                covered.push(i);
                break;
            }
        }
    }
    (covered.len(), covered)
}

/// Compute the maximum legal value of X the caster can choose for a pending cast.
///
/// Upper bound = (mana currently in pool) + (mana producible from untapped,
/// free-to-tap sources under the caster's control) − (fixed portion of cost).
///
/// Free-to-tap = mana abilities whose activation imposes no irreversible cost
/// on the player: i.e., `ManaSourceOption` entries classified as
/// `ManaSourcePenalty::None` (`penalty.is_free()`). Costed mana abilities
/// (e.g. "1, T: Add {C}") are excluded for v1 — they cascade and would
/// require a search to bound precisely. Treasure tokens are likewise
/// excluded because they sacrifice the source; pain lands and pay-life
/// sources are excluded because activating them for extra X damages or
/// drains the caster.
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
                .any(|opt| opt.penalty.is_free())
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
/// Decides, in order:
/// 1. **`ChooseXValue`** — the cost still contains an unchosen X (CR 601.2f).
/// 2. **Auto-finalize** — the concretized cost contains no hybrid/Phyrexian shards
///    and convoke is not active, so `pay_mana_cost` can deterministically satisfy it.
///    The `ManaPayment` state is skipped entirely; we proceed directly to stack push.
///    This mirrors Arena's "cast and resolve" feel for unambiguous costs.
/// 3. **`ManaPayment`** — player input is required (hybrid choice, Phyrexian life
///    payment, or convoke tap selection).
///
/// All sites that would otherwise construct `WaitingFor::ManaPayment` during a
/// cast must go through this helper so X-selection and auto-pay are never bypassed.
pub fn enter_payment_step(
    state: &mut GameState,
    player: PlayerId,
    convoke_mode: Option<ConvokeMode>,
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    if let Some(pending) = state.pending_cast.as_ref() {
        if pending.ability.chosen_x.is_none() && cost_has_x(&pending.cost) {
            let max = max_x_value(state, player, &pending.cost);
            let pending_cast = pending.clone();
            return Ok(WaitingFor::ChooseXValue {
                player,
                max,
                pending_cast,
                convoke_mode,
            });
        }
    }

    // CR 601.2h: Auto-finalize when no player-level decision remains. Convoke requires
    // the caster to choose which creatures to tap, so it always surfaces the modal.
    if convoke_mode.is_none() {
        if let Some(pending) = state.pending_cast.as_ref() {
            if mana_payment::classify_payment(&pending.cost)
                == mana_payment::PaymentClassification::Unambiguous
            {
                return finalize_mana_payment(state, player, events);
            }
        }
    }

    Ok(WaitingFor::ManaPayment {
        player,
        convoke_mode,
    })
}

/// Pay the pending cast's mana cost and transition to the next game state.
///
/// Dispatches on the shape of `state.pending_cast`:
/// - **Activated ability** — pay mana, then push the ability to the stack.
/// - **X-spell with distribution** (`Fireball`-like) — pay mana to determine X total,
///   then either auto-split (even-damage) or enter `DistributeAmong` (interactive).
/// - **Normal spell** — delegate to `finalize_cast` which pays mana and pushes.
///
/// Called both from the `(ManaPayment, PassPriority)` branch in the main engine
/// dispatcher and from `enter_payment_step` when classification skips the modal.
/// This is the single authority for completing a mana payment.
pub fn finalize_mana_payment(
    state: &mut GameState,
    player: PlayerId,
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    // CR 107.4f + CR 601.2f: Pause for per-shard Phyrexian choice if the cost contains
    // Phyrexian mana AND at least one shard has both mana and life options available.
    // `PendingCast` stays in `state.pending_cast` across the pause — the resume handler
    // in `engine.rs` calls `finalize_mana_payment_with_phyrexian_choices`.
    if let Some(pending_ref) = state.pending_cast.as_ref() {
        let cost = pending_ref.cost.clone();
        let source_id = pending_ref.object_id;
        if let Some(waiting) =
            maybe_pause_for_phyrexian_choice(state, player, source_id, &cost, events)
        {
            return Ok(waiting);
        }
    }

    let pending = state
        .pending_cast
        .take()
        .ok_or_else(|| EngineError::InvalidAction("No pending cast to finalize".to_string()))?;

    if let Some(ability_index) = pending.activation_ability_index {
        super::casting::pay_mana_cost(state, player, pending.object_id, &pending.cost, events)?;
        return push_activated_ability_to_stack(
            state,
            player,
            pending.object_id,
            ability_index,
            pending.ability,
            pending.activation_cost.as_ref(),
            events,
        );
    }

    if let Some(unit) = pending.distribute {
        // CR 601.2d: X-spell distribution — pay mana first to determine X, then
        // trigger DistributeAmong with total = X.
        let pool_before = state
            .players
            .iter()
            .find(|pl| pl.id == player)
            .map(|pl| pl.mana_pool.total())
            .unwrap_or(0);

        super::casting::pay_mana_cost(state, player, pending.object_id, &pending.cost, events)?;

        let pool_after = state
            .players
            .iter()
            .find(|pl| pl.id == player)
            .map(|pl| pl.mana_pool.total())
            .unwrap_or(0);
        // CR 107.1b + CR 601.2f: Prefer the explicit `chosen_x` set during
        // `WaitingFor::ChooseXValue`. Fallback to inference (total paid minus
        // non-X colored/generic costs) preserves behavior for any legacy paths
        // that bypass ChooseX. ManaCost::mana_value() excludes X (CR 202.3e).
        let non_x_cost = pending.cost.mana_value();
        let total_paid = pool_before.saturating_sub(pool_after) as u32;
        let x_value = pending
            .ability
            .chosen_x
            .unwrap_or_else(|| total_paid.saturating_sub(non_x_cost));

        let targets = super::ability_utils::flatten_targets_in_chain(&pending.ability);
        // Store pending cast for post-distribution resumption. Use `ManaCost::NoCost`
        // since mana was already paid above — `finalize_cast` must not re-deduct.
        let mut pending_resumed = PendingCast::new(
            pending.object_id,
            pending.card_id,
            pending.ability,
            crate::types::mana::ManaCost::NoCost,
        );
        pending_resumed.casting_variant = pending.casting_variant;
        pending_resumed.origin_zone = pending.origin_zone;

        // CR 601.2d: "divided evenly, rounded down" — EvenSplitDamage bypasses
        // interactive distribution. Remainder is intentionally lost per Oracle text.
        if unit == DistributionUnit::EvenSplitDamage && !targets.is_empty() {
            let num = targets.len() as u32;
            let per_target = x_value / num;
            let distribution: Vec<_> = targets.iter().map(|t| (t.clone(), per_target)).collect();
            pending_resumed.ability.distribution = Some(distribution);
            state.pending_cast = Some(Box::new(pending_resumed));

            let pending = state.pending_cast.take().unwrap();
            return finalize_cast(
                state,
                player,
                pending.object_id,
                pending.card_id,
                pending.ability,
                &pending.cost,
                pending.casting_variant,
                pending.origin_zone,
                events,
            );
        }

        state.pending_cast = Some(Box::new(pending_resumed));
        return Ok(WaitingFor::DistributeAmong {
            player,
            total: x_value,
            targets,
            unit,
        });
    }

    finalize_cast(
        state,
        player,
        pending.object_id,
        pending.card_id,
        pending.ability,
        &pending.cost,
        pending.casting_variant,
        pending.origin_zone,
        events,
    )
}

/// CR 107.4f + CR 601.2f: Resume cast completion after the caster submits their
/// per-shard Phyrexian choices. Mirrors `finalize_mana_payment` but threads the
/// explicit choices through `pay_mana_cost_with_choices`.
///
/// Caller (engine dispatcher) is responsible for validating choice count and current
/// affordability via `compute_phyrexian_shards` before invoking this helper. If the
/// revalidation fails, the caller returns `EngineError::ActionNotAllowed` instead.
pub fn finalize_mana_payment_with_phyrexian_choices(
    state: &mut GameState,
    player: PlayerId,
    phyrexian_choices: &[crate::types::game_state::ShardChoice],
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    let pending = state
        .pending_cast
        .take()
        .ok_or_else(|| EngineError::InvalidAction("No pending cast to finalize".to_string()))?;

    if let Some(ability_index) = pending.activation_ability_index {
        super::casting::pay_mana_cost_with_choices(
            state,
            player,
            pending.object_id,
            &pending.cost,
            Some(phyrexian_choices),
            events,
        )?;
        return push_activated_ability_to_stack(
            state,
            player,
            pending.object_id,
            ability_index,
            pending.ability,
            pending.activation_cost.as_ref(),
            events,
        );
    }

    if let Some(unit) = pending.distribute {
        // CR 601.2d: X + distribution + Phyrexian is extremely rare (no known current cards).
        // Fall through to the auto-decision distribution path for safety — the Phyrexian
        // choices were already consumed via pay_mana_cost_with_choices above (the X-spell
        // distribution path is orthogonal).
        let pool_before = state
            .players
            .iter()
            .find(|pl| pl.id == player)
            .map(|pl| pl.mana_pool.total())
            .unwrap_or(0);

        super::casting::pay_mana_cost_with_choices(
            state,
            player,
            pending.object_id,
            &pending.cost,
            Some(phyrexian_choices),
            events,
        )?;

        let pool_after = state
            .players
            .iter()
            .find(|pl| pl.id == player)
            .map(|pl| pl.mana_pool.total())
            .unwrap_or(0);
        let non_x_cost = pending.cost.mana_value();
        let total_paid = pool_before.saturating_sub(pool_after) as u32;
        let x_value = pending
            .ability
            .chosen_x
            .unwrap_or_else(|| total_paid.saturating_sub(non_x_cost));

        let targets = super::ability_utils::flatten_targets_in_chain(&pending.ability);
        let mut pending_resumed = PendingCast::new(
            pending.object_id,
            pending.card_id,
            pending.ability,
            crate::types::mana::ManaCost::NoCost,
        );
        pending_resumed.casting_variant = pending.casting_variant;
        pending_resumed.origin_zone = pending.origin_zone;

        if unit == DistributionUnit::EvenSplitDamage && !targets.is_empty() {
            let num = targets.len() as u32;
            let per_target = x_value / num;
            let distribution: Vec<_> = targets.iter().map(|t| (t.clone(), per_target)).collect();
            pending_resumed.ability.distribution = Some(distribution);
            state.pending_cast = Some(Box::new(pending_resumed));

            let pending = state.pending_cast.take().unwrap();
            return finalize_cast(
                state,
                player,
                pending.object_id,
                pending.card_id,
                pending.ability,
                &pending.cost,
                pending.casting_variant,
                pending.origin_zone,
                events,
            );
        }

        state.pending_cast = Some(Box::new(pending_resumed));
        return Ok(WaitingFor::DistributeAmong {
            player,
            total: x_value,
            targets,
            unit,
        });
    }

    finalize_cast_with_phyrexian_choices(
        state,
        player,
        pending.object_id,
        pending.card_id,
        pending.ability,
        &pending.cost,
        pending.casting_variant,
        pending.origin_zone,
        Some(phyrexian_choices),
        events,
    )
}

/// CR 107.4f + CR 601.2f: Determine whether this cast needs to pause for per-shard
/// Phyrexian payment choice, and construct the matching `WaitingFor::PhyrexianPayment`
/// if so.
///
/// Auto-taps mana sources first (idempotent: already-tapped lands are skipped) so the
/// shard-options computation reflects the pool the caster will actually spend from.
/// Returns `Some(WaitingFor::PhyrexianPayment {...})` when at least one Phyrexian shard
/// has `ShardOptions::ManaOrLife`; otherwise returns `None` so the caller proceeds with
/// the existing auto-decision path.
pub(super) fn maybe_pause_for_phyrexian_choice(
    state: &mut GameState,
    player: PlayerId,
    source_id: ObjectId,
    cost: &crate::types::mana::ManaCost,
    events: &mut Vec<GameEvent>,
) -> Option<WaitingFor> {
    // Fast reject: no Phyrexian shards → no pause.
    match cost {
        crate::types::mana::ManaCost::Cost { shards, .. } => {
            if !shards.iter().any(|s| {
                matches!(
                    mana_payment::shard_to_mana_type(*s),
                    mana_payment::ShardRequirement::Phyrexian(..)
                        | mana_payment::ShardRequirement::HybridPhyrexian(..)
                )
            }) {
                return None;
            }
        }
        _ => return None,
    }

    // CR 601.2h + CR 605: Auto-tap mana sources before shard-options computation so
    // the simulation reflects the actual post-tap pool.
    auto_tap_mana_sources(state, player, cost, events, Some(source_id));

    let spell_meta = super::casting::build_spell_meta(state, player, source_id);
    let spell_ctx = spell_meta.as_ref().map(PaymentContext::Spell);
    let any_color = super::static_abilities::player_can_spend_as_any_color(state, player);
    let max_life = super::life_costs::max_phyrexian_life_payments(state, player);

    let shards = {
        let player_data = state.players.iter().find(|p| p.id == player)?;
        mana_payment::compute_phyrexian_shards(
            &player_data.mana_pool,
            cost,
            spell_ctx.as_ref(),
            any_color,
            max_life,
        )
    };

    // CR 107.4f + CR 601.2f: Pause iff the choice is meaningful — at least one shard
    // has both options viable. Trivial-choice shards auto-resolve without pausing.
    let has_meaningful_choice = shards.iter().any(|s| {
        matches!(
            s.options,
            crate::types::game_state::ShardOptions::ManaOrLife
        )
    });
    if !has_meaningful_choice {
        return None;
    }

    Some(WaitingFor::PhyrexianPayment {
        player,
        spell_object: source_id,
        shards,
    })
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
    use std::sync::Arc;

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
                    target: crate::types::ability::TargetFilter::Controller,
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
            origin_zone: Zone::Hand,
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
        Arc::make_mut(&mut obj.abilities).push(
            AbilityDefinition::new(
                AbilityKind::Activated,
                Effect::Mana {
                    produced: crate::types::ability::ManaProduction::Colorless {
                        count: QuantityExpr::Fixed { value: 1 },
                    },
                    restrictions: vec![],
                    grants: vec![],
                    expiry: None,
                    target: None,
                },
            )
            .cost(AbilityCost::Tap),
        );
        Arc::make_mut(&mut obj.abilities).push(
            AbilityDefinition::new(
                AbilityKind::Activated,
                Effect::Mana {
                    produced: crate::types::ability::ManaProduction::AnyOneColor {
                        count: QuantityExpr::Fixed { value: 1 },
                        color_options: vec![ManaColor::White, ManaColor::Blue],
                        contribution: crate::types::ability::ManaContribution::Base,
                    },
                    restrictions: vec![],
                    grants: vec![],
                    expiry: None,
                    target: None,
                },
            )
            .cost(AbilityCost::Composite {
                costs: vec![
                    AbilityCost::Tap,
                    AbilityCost::PayLife {
                        amount: QuantityExpr::Fixed { value: 1 },
                    },
                ],
            }),
        );
        town
    }

    /// CR 605.3b + CR 106.1a: Build a Sunken-Ruins-style filter land with both
    /// the secondary `{T}: Add {C}` ability and the primary
    /// `{U/B}, {T}: Add {U}{U}, {U}{B}, or {B}{B}` ability.
    fn create_filter_land(
        state: &mut GameState,
        name: &str,
        a: ManaColor,
        b: ManaColor,
    ) -> ObjectId {
        let land = create_object(
            state,
            CardId(900),
            PlayerId(0),
            name.to_string(),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&land).unwrap();
        obj.card_types.core_types.push(CoreType::Land);
        Arc::make_mut(&mut obj.abilities).push(
            AbilityDefinition::new(
                AbilityKind::Activated,
                Effect::Mana {
                    produced: crate::types::ability::ManaProduction::Colorless {
                        count: QuantityExpr::Fixed { value: 1 },
                    },
                    restrictions: vec![],
                    grants: vec![],
                    expiry: None,
                    target: None,
                },
            )
            .cost(AbilityCost::Tap),
        );
        // Only the combinations ability is what we exercise in auto-tap tests.
        Arc::make_mut(&mut obj.abilities).push(
            AbilityDefinition::new(
                AbilityKind::Activated,
                Effect::Mana {
                    produced: crate::types::ability::ManaProduction::ChoiceAmongCombinations {
                        options: vec![vec![a, a], vec![a, b], vec![b, b]],
                    },
                    restrictions: vec![],
                    grants: vec![],
                    expiry: None,
                    target: None,
                },
            )
            .cost(AbilityCost::Tap),
        );
        land
    }

    #[test]
    fn auto_tap_filter_land_covers_mixed_shards() {
        // Cost `{U}{B}` with a single Sunken Ruins available: the combo
        // pre-pass must pick the `{U}{B}` combination and tap the land once,
        // producing both colors atomically.
        let mut state = GameState::new_two_player(42);
        create_filter_land(
            &mut state,
            "Sunken Ruins",
            ManaColor::Blue,
            ManaColor::Black,
        );

        let mut events = Vec::new();
        auto_tap_mana_sources(
            &mut state,
            PlayerId(0),
            &ManaCost::Cost {
                shards: vec![ManaCostShard::Blue, ManaCostShard::Black],
                generic: 0,
            },
            &mut events,
            None,
        );

        assert_eq!(state.players[0].mana_pool.count_color(ManaType::Blue), 1);
        assert_eq!(state.players[0].mana_pool.count_color(ManaType::Black), 1);
    }

    #[test]
    fn auto_tap_filter_land_picks_double_color_combination() {
        // Cost `{U}{U}`: combo pre-pass must pick `{U}{U}` (covers both
        // shards), not `{U}{B}` (wastes black).
        let mut state = GameState::new_two_player(42);
        create_filter_land(
            &mut state,
            "Sunken Ruins",
            ManaColor::Blue,
            ManaColor::Black,
        );

        let mut events = Vec::new();
        auto_tap_mana_sources(
            &mut state,
            PlayerId(0),
            &ManaCost::Cost {
                shards: vec![ManaCostShard::Blue, ManaCostShard::Blue],
                generic: 0,
            },
            &mut events,
            None,
        );

        assert_eq!(
            state.players[0].mana_pool.count_color(ManaType::Blue),
            2,
            "auto-tap should pick {{U}}{{U}} combination"
        );
        assert_eq!(state.players[0].mana_pool.count_color(ManaType::Black), 0);
    }

    #[test]
    fn auto_tap_filter_land_covers_colored_plus_generic() {
        // CR 605.3b: Cost `{U}{1}`. Combo pre-pass picks `{U}{U}` — the first
        // {U} covers the shard, the second lands in the pool and can pay the
        // {1} generic (via the regular payment path). Auto-tap's job is to
        // ensure sufficient mana enters the pool; actual shard/generic
        // consumption happens in the downstream payment step.
        let mut state = GameState::new_two_player(42);
        create_filter_land(
            &mut state,
            "Sunken Ruins",
            ManaColor::Blue,
            ManaColor::Black,
        );

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
            state.players[0].mana_pool.total(),
            2,
            "filter land produces 2 blue mana — covers shard + generic"
        );
        assert_eq!(state.players[0].mana_pool.count_color(ManaType::Blue), 2);
    }

    #[test]
    fn auto_tap_does_not_use_combo_for_pure_generic() {
        // CR 605.3b: Pure generic cost `{1}` with a filter land available.
        // The combo pre-pass must NOT commit the combo (no shards to cover)
        // because spending a 2-mana combination on 1 generic wastes half
        // the production. Phase 2 prefers the land's secondary
        // `{T}: Add {C}` (non-combo) ability for the generic instead.
        let mut state = GameState::new_two_player(42);
        create_filter_land(
            &mut state,
            "Sunken Ruins",
            ManaColor::Blue,
            ManaColor::Black,
        );

        let mut events = Vec::new();
        auto_tap_mana_sources(
            &mut state,
            PlayerId(0),
            &ManaCost::Cost {
                shards: vec![],
                generic: 1,
            },
            &mut events,
            None,
        );

        // The secondary `{T}: Add {C}` should satisfy the generic with a
        // single colorless mana — NOT the combo (which would produce 2 mana
        // of a random colored combination for only 1 generic).
        assert_eq!(state.players[0].mana_pool.total(), 1);
        assert_eq!(
            state.players[0].mana_pool.count_color(ManaType::Colorless),
            1,
            "pure generic should draw from `{{T}}: Add {{C}}`, not the combination"
        );
        assert_eq!(state.players[0].mana_pool.count_color(ManaType::Blue), 0);
        assert_eq!(state.players[0].mana_pool.count_color(ManaType::Black), 0);
    }

    #[test]
    fn auto_tap_filter_land_does_not_prompt_user() {
        // Regression: the filter-land activation must short-circuit the
        // `WaitingFor::ChooseManaColor` prompt during auto-tap — the caller
        // picks the combination via `ProductionOverride::Combination`.
        // If the prompt surfaced, `resolve_mana_ability` would return Ok but
        // with no mana added to the pool. Verify mana actually landed.
        let mut state = GameState::new_two_player(42);
        create_filter_land(&mut state, "Mystic Gate", ManaColor::White, ManaColor::Blue);

        let mut events = Vec::new();
        auto_tap_mana_sources(
            &mut state,
            PlayerId(0),
            &ManaCost::Cost {
                shards: vec![ManaCostShard::White, ManaCostShard::Blue],
                generic: 0,
            },
            &mut events,
            None,
        );

        // CR 605.3b: combination mana lands in the pool atomically, no prompt.
        assert_eq!(state.players[0].mana_pool.total(), 2);
        assert!(events
            .iter()
            .any(|e| matches!(e, GameEvent::PermanentTapped { .. })));
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
    fn auto_tap_skips_sources_when_pool_already_covers_cost() {
        // CR 601.2g regression: if the player has already tapped Sol Ring ({C}{C})
        // and an Island ({U}) before casting a {2}{U} spell, auto-tap must NOT
        // tap three more untapped lands — the floating pool already covers the
        // entire cost.
        use crate::types::mana::ManaUnit;
        let mut state = GameState::new_two_player(42);

        // Three untapped basic lands as potential victims if auto-tap misbehaves.
        let mut lands = Vec::new();
        for i in 0..3 {
            let land = create_object(
                &mut state,
                CardId(200 + i),
                PlayerId(0),
                "Island".to_string(),
                Zone::Battlefield,
            );
            let obj = state.objects.get_mut(&land).unwrap();
            obj.card_types.core_types.push(CoreType::Land);
            obj.card_types.subtypes.push("Island".to_string());
            lands.push(land);
        }

        // Pre-float {C}{C}{U} into the pool (as if the player tapped sources
        // before initiating the cast).
        let floated_source = ObjectId(99);
        for color in [ManaType::Colorless, ManaType::Colorless, ManaType::Blue] {
            state.players[0].mana_pool.add(ManaUnit {
                color,
                source_id: floated_source,
                snow: false,
                restrictions: Vec::new(),
                grants: vec![],
                expiry: None,
            });
        }

        let mut events = Vec::new();
        auto_tap_mana_sources(
            &mut state,
            PlayerId(0),
            &ManaCost::Cost {
                shards: vec![ManaCostShard::Blue],
                generic: 2,
            },
            &mut events,
            None,
        );

        // Pool unchanged — reduce_cost_by_pool consumed the residual to NoCost.
        assert_eq!(
            state.players[0].mana_pool.total(),
            3,
            "pool must not grow when it already covers the cost"
        );
        // No permanents tapped, no mana produced.
        for land in &lands {
            assert!(
                !state.objects.get(land).unwrap().tapped,
                "no land should be tapped when floating mana covers the cost"
            );
        }
        assert!(
            !events
                .iter()
                .any(|e| matches!(e, GameEvent::PermanentTapped { .. })),
            "auto-tap must emit no PermanentTapped events when pool covers cost"
        );
    }

    #[test]
    fn auto_tap_taps_only_the_shortfall_when_pool_partially_covers() {
        // CR 601.2g: If the pool covers part of the cost, auto-tap must only
        // produce the residual — not the full cost. Pool has {U}; cost is
        // {2}{U}; expect exactly 2 additional sources tapped (for the {2}).
        use crate::types::mana::ManaUnit;
        let mut state = GameState::new_two_player(42);

        let mut lands = Vec::new();
        for i in 0..4 {
            let land = create_object(
                &mut state,
                CardId(300 + i),
                PlayerId(0),
                "Island".to_string(),
                Zone::Battlefield,
            );
            let obj = state.objects.get_mut(&land).unwrap();
            obj.card_types.core_types.push(CoreType::Land);
            obj.card_types.subtypes.push("Island".to_string());
            lands.push(land);
        }

        state.players[0].mana_pool.add(ManaUnit {
            color: ManaType::Blue,
            source_id: ObjectId(99),
            snow: false,
            restrictions: Vec::new(),
            grants: vec![],
            expiry: None,
        });

        let mut events = Vec::new();
        auto_tap_mana_sources(
            &mut state,
            PlayerId(0),
            &ManaCost::Cost {
                shards: vec![ManaCostShard::Blue],
                generic: 2,
            },
            &mut events,
            None,
        );

        // Pool grew by exactly 2 (the residual {2} → two {U} from Islands).
        // Original {U} stays floating; two new units produced.
        assert_eq!(
            state.players[0].mana_pool.total(),
            3,
            "pool should grow by exactly the residual — 2 mana for the generic {{2}}"
        );
        let tapped_count = lands
            .iter()
            .filter(|l| state.objects.get(l).unwrap().tapped)
            .count();
        assert_eq!(
            tapped_count, 2,
            "exactly 2 lands should tap for the residual; the pre-floated {{U}} covers the shard"
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
            Arc::new(vec![crate::types::ability::AbilityDefinition::new(
                AbilityKind::Activated,
                Effect::Scry {
                    count: QuantityExpr::Fixed { value: 1 },
                    target: crate::types::ability::TargetFilter::Controller,
                },
            )]);

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

    /// CR 603.6c + CR 118.3: Sacrificing a permanent as part of a cost is a
    /// game event that triggers other abilities (e.g., Crime Novelist's
    /// "whenever you sacrifice an artifact"). Regression: cost-payment
    /// sacrifices must emit `PermanentSacrificed` so observer triggers fire,
    /// just like spell-effect sacrifices do.
    ///
    /// Covers the broader "sacrifice-cost-trigger" class — Crime Novelist,
    /// Syr Ginger, Mayhem Devil, Cruel Celebrant, Korvold etc.
    #[test]
    fn sacrifice_as_cost_emits_event_for_observer_trigger() {
        use crate::game::triggers::process_triggers;
        use crate::types::ability::TriggerDefinition;
        use crate::types::ability::{ControllerRef, TargetFilter, TypeFilter, TypedFilter};
        use crate::types::triggers::TriggerMode;

        let mut state = GameState::new_two_player(42);

        // Source: an artifact with an activated ability whose cost sacrifices
        // a Treasure (an artifact). Effect doesn't matter — we just need the
        // sacrifice cost to fire.
        let source = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Source".to_string(),
            Zone::Battlefield,
        );
        state.objects.get_mut(&source).unwrap().abilities =
            Arc::new(vec![crate::types::ability::AbilityDefinition::new(
                AbilityKind::Activated,
                Effect::Scry {
                    count: QuantityExpr::Fixed { value: 1 },
                    target: TargetFilter::Controller,
                },
            )]);

        // Treasure-like artifact controlled by player 0 — sacrificed as cost.
        let treasure = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Treasure".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&treasure).unwrap();
            obj.card_types.core_types.push(CoreType::Artifact);
        }

        // Observer: Crime-Novelist-style trigger.
        // "Whenever you sacrifice an artifact, ..." => valid_card = Typed{Artifact, controller: You}.
        let observer = create_object(
            &mut state,
            CardId(3),
            PlayerId(0),
            "Crime Novelist".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&observer).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.entered_battlefield_turn = Some(1);
            let mut trig = TriggerDefinition::new(TriggerMode::Sacrificed);
            trig.valid_card = Some(TargetFilter::Typed(TypedFilter {
                type_filters: vec![TypeFilter::Artifact],
                controller: Some(ControllerRef::You),
                properties: vec![],
            }));
            // Trigger executes a draw so we can detect it on the stack.
            trig.execute = Some(Box::new(crate::types::ability::AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::Draw {
                    count: QuantityExpr::Fixed { value: 1 },
                    target: TargetFilter::Controller,
                },
            )));
            obj.trigger_definitions.push(trig);
        }

        // Pay the cost via the cost-payment helper directly — same path
        // taken when an activated ability's sacrifice subcost resumes after
        // `WaitingFor::SacrificeForCost`.
        let pending = make_pending(source);
        let mut events = Vec::new();
        handle_sacrifice_for_cost(
            &mut state,
            PlayerId(0),
            pending,
            1,
            &[treasure],
            &[treasure],
            &mut events,
        )
        .expect("cost-payment sacrifice succeeds");

        // The cost-payment path must emit `PermanentSacrificed` — same event
        // the spell-effect sacrifice path emits — so observer triggers can fire.
        assert!(
            events.iter().any(|e| matches!(
                e,
                GameEvent::PermanentSacrificed { object_id, .. } if *object_id == treasure
            )),
            "cost-payment sacrifice must emit PermanentSacrificed (got: {:?})",
            events
                .iter()
                .filter(|e| !matches!(e, GameEvent::ZoneChanged { .. }))
                .collect::<Vec<_>>()
        );

        // Run the trigger pass over the cost-payment events. Observer's
        // Sacrificed-mode trigger must register on the stack.
        let stack_before = state.stack.len();
        process_triggers(&mut state, &events);
        assert!(
            state.stack.len() > stack_before,
            "observer's `whenever you sacrifice an artifact` trigger must fire \
             when an artifact is sacrificed as part of an activated-ability cost"
        );
    }

    /// CR 603.6c + CR 603.10a: Sacrificing an artifact TOKEN as a cost must
    /// fire `whenever <artifact> is put into a graveyard from the battlefield`
    /// triggers (Syr Ginger). The token does cease to exist after SBAs (CR
    /// 704.5d), but the leaves-battlefield event still fires per CR 603.10a
    /// (last-known information). Cost-payment must emit the same `ZoneChanged`
    /// event that effect-sacrifice emits.
    #[test]
    fn sacrifice_token_as_cost_fires_dies_zone_trigger() {
        use crate::game::triggers::process_triggers;
        use crate::types::ability::TriggerDefinition;
        use crate::types::ability::{ControllerRef, TargetFilter, TypeFilter, TypedFilter};
        use crate::types::triggers::TriggerMode;

        let mut state = GameState::new_two_player(42);

        let source = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Source".to_string(),
            Zone::Battlefield,
        );
        state.objects.get_mut(&source).unwrap().abilities =
            Arc::new(vec![crate::types::ability::AbilityDefinition::new(
                AbilityKind::Activated,
                Effect::Scry {
                    count: QuantityExpr::Fixed { value: 1 },
                    target: TargetFilter::Controller,
                },
            )]);

        // Artifact TOKEN (e.g., Treasure / Food) controlled by player 0.
        let token = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Treasure Token".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&token).unwrap();
            obj.card_types.core_types.push(CoreType::Artifact);
            obj.is_token = true;
        }

        // Syr-Ginger-style observer: ChangesZone Battlefield → Graveyard,
        // valid_card = Artifact controller=You. Note: `Another` is not
        // exercised here — the sacrificed token is a different object.
        let observer = create_object(
            &mut state,
            CardId(3),
            PlayerId(0),
            "Syr Ginger".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&observer).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.entered_battlefield_turn = Some(1);
            let mut trig = TriggerDefinition::new(TriggerMode::ChangesZone);
            trig.origin = Some(Zone::Battlefield);
            trig.destination = Some(Zone::Graveyard);
            trig.valid_card = Some(TargetFilter::Typed(TypedFilter {
                type_filters: vec![TypeFilter::Artifact],
                controller: Some(ControllerRef::You),
                properties: vec![],
            }));
            trig.execute = Some(Box::new(crate::types::ability::AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::Draw {
                    count: QuantityExpr::Fixed { value: 1 },
                    target: TargetFilter::Controller,
                },
            )));
            obj.trigger_definitions.push(trig);
        }

        let pending = make_pending(source);
        let mut events = Vec::new();
        handle_sacrifice_for_cost(
            &mut state,
            PlayerId(0),
            pending,
            1,
            &[token],
            &[token],
            &mut events,
        )
        .expect("cost-payment sacrifice succeeds");

        // Cost-payment must emit ZoneChanged (battlefield → graveyard) for the
        // sacrificed token — Dies / leaves-battlefield triggers depend on it.
        assert!(
            events.iter().any(|e| matches!(
                e,
                GameEvent::ZoneChanged {
                    object_id,
                    from: Some(Zone::Battlefield),
                    to: Zone::Graveyard,
                    ..
                } if *object_id == token
            )),
            "cost-payment sacrifice must emit ZoneChanged battlefield→graveyard"
        );

        let stack_before = state.stack.len();
        process_triggers(&mut state, &events);
        assert!(
            state.stack.len() > stack_before,
            "observer's `whenever an artifact is put into a graveyard from the battlefield` \
             trigger must fire when an artifact token is sacrificed as activation cost"
        );
    }

    /// End-to-end repro for L9-9: activate a Treasure-style mana ability
    /// (`{T}, Sacrifice this artifact: Add one mana of any color`). After
    /// `GameAction::ActivateAbility` resolves, Crime Novelist's sacrifice
    /// trigger must land on the stack via `run_post_action_pipeline`.
    #[test]
    fn mana_ability_sacrifice_cost_fires_observer_trigger_end_to_end() {
        use crate::game::engine::apply_as_current;
        use crate::types::ability::TriggerDefinition;
        use crate::types::ability::{
            AbilityCost, ControllerRef, ManaContribution, ManaProduction, TargetFilter, TypeFilter,
            TypedFilter,
        };
        use crate::types::mana::ManaColor;
        use crate::types::phase::Phase;
        use crate::types::triggers::TriggerMode;
        use crate::types::GameAction;

        let mut state = GameState::new_two_player(42);
        state.turn_number = 2;
        state.phase = Phase::PreCombatMain;
        state.active_player = PlayerId(0);
        state.priority_player = PlayerId(0);
        state.waiting_for = WaitingFor::Priority {
            player: PlayerId(0),
        };

        // Treasure token: `{T}, Sacrifice: Add one mana of any color`.
        let treasure = create_object(
            &mut state,
            CardId(100),
            PlayerId(0),
            "Treasure".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&treasure).unwrap();
            obj.card_types.core_types.push(CoreType::Artifact);
            obj.card_types.subtypes.push("Treasure".to_string());
            obj.is_token = true;
            obj.entered_battlefield_turn = Some(1); // CR 302.1: avoid summoning sickness for {T}
            Arc::make_mut(&mut obj.abilities).push(
                crate::types::ability::AbilityDefinition::new(
                    AbilityKind::Activated,
                    Effect::Mana {
                        produced: ManaProduction::AnyOneColor {
                            count: QuantityExpr::Fixed { value: 1 },
                            color_options: vec![
                                ManaColor::White,
                                ManaColor::Blue,
                                ManaColor::Black,
                                ManaColor::Red,
                                ManaColor::Green,
                            ],
                            contribution: ManaContribution::Base,
                        },
                        restrictions: vec![],
                        grants: vec![],
                        expiry: None,
                        target: None,
                    },
                )
                .cost(AbilityCost::Composite {
                    costs: vec![
                        AbilityCost::Tap,
                        AbilityCost::Sacrifice {
                            target: TargetFilter::SelfRef,
                            count: 1,
                        },
                    ],
                }),
            );
        }

        // Crime-Novelist-style observer: Sacrificed-mode trigger on Artifact
        // controlled by `You`. Trigger executes a draw so it's detectable on
        // the stack (mana abilities don't use the stack — but the *triggered*
        // ability fired by the sacrifice does).
        let observer = create_object(
            &mut state,
            CardId(101),
            PlayerId(0),
            "Crime Novelist".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&observer).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.entered_battlefield_turn = Some(1);
            let mut trig = TriggerDefinition::new(TriggerMode::Sacrificed);
            trig.valid_card = Some(TargetFilter::Typed(TypedFilter {
                type_filters: vec![TypeFilter::Artifact],
                controller: Some(ControllerRef::You),
                properties: vec![],
            }));
            trig.execute = Some(Box::new(crate::types::ability::AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::Draw {
                    count: QuantityExpr::Fixed { value: 1 },
                    target: TargetFilter::Controller,
                },
            )));
            obj.trigger_definitions.push(trig);
        }

        // Activate the Treasure's mana ability — this is a "any color" choice,
        // so we expect a ChooseManaColor prompt before resolution.
        apply_as_current(
            &mut state,
            GameAction::ActivateAbility {
                source_id: treasure,
                ability_index: 0,
            },
        )
        .expect("activation succeeds");

        // If the engine prompts for a mana color, pick one.
        if matches!(state.waiting_for, WaitingFor::ChooseManaColor { .. }) {
            apply_as_current(
                &mut state,
                GameAction::ChooseManaColor {
                    choice: crate::types::game_state::ManaChoice::SingleColor(
                        crate::types::mana::ManaType::Red,
                    ),
                },
            )
            .expect("color choice succeeds");
        }

        // Crime Novelist's Sacrificed trigger must have fired and landed
        // on the stack — even though the source mana ability did not.
        assert!(
            state.stack.iter().any(|entry| entry.source_id == observer),
            "Crime Novelist's sacrifice trigger must land on the stack \
             when a Treasure is sacrificed as part of an activated mana \
             ability cost (got stack: {:?}, treasure zone: {:?})",
            state.stack.iter().map(|e| e.source_id).collect::<Vec<_>>(),
            state.objects.get(&treasure).map(|o| o.zone),
        );
    }

    /// End-to-end repro for L9-9 (Syr Ginger class): activate a Treasure
    /// mana ability whose cost sacrifices the Treasure. Syr Ginger's
    /// ChangesZone (Battlefield → Graveyard) trigger must fire — same fix
    /// path as Crime Novelist, since `process_triggers` scans both
    /// `PermanentSacrificed` and `ZoneChanged` events emitted by the
    /// sacrifice cost step.
    #[test]
    fn mana_ability_sacrifice_cost_fires_dies_zone_trigger_end_to_end() {
        use crate::game::engine::apply_as_current;
        use crate::types::ability::TriggerDefinition;
        use crate::types::ability::{
            AbilityCost, ControllerRef, ManaContribution, ManaProduction, TargetFilter, TypeFilter,
            TypedFilter,
        };
        use crate::types::mana::ManaColor;
        use crate::types::phase::Phase;
        use crate::types::triggers::TriggerMode;
        use crate::types::GameAction;

        let mut state = GameState::new_two_player(42);
        state.turn_number = 2;
        state.phase = Phase::PreCombatMain;
        state.active_player = PlayerId(0);
        state.priority_player = PlayerId(0);
        state.waiting_for = WaitingFor::Priority {
            player: PlayerId(0),
        };

        let treasure = create_object(
            &mut state,
            CardId(200),
            PlayerId(0),
            "Treasure".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&treasure).unwrap();
            obj.card_types.core_types.push(CoreType::Artifact);
            obj.card_types.subtypes.push("Treasure".to_string());
            obj.is_token = true;
            obj.entered_battlefield_turn = Some(1);
            Arc::make_mut(&mut obj.abilities).push(
                crate::types::ability::AbilityDefinition::new(
                    AbilityKind::Activated,
                    Effect::Mana {
                        produced: ManaProduction::AnyOneColor {
                            count: QuantityExpr::Fixed { value: 1 },
                            color_options: vec![
                                ManaColor::White,
                                ManaColor::Blue,
                                ManaColor::Black,
                                ManaColor::Red,
                                ManaColor::Green,
                            ],
                            contribution: ManaContribution::Base,
                        },
                        restrictions: vec![],
                        grants: vec![],
                        expiry: None,
                        target: None,
                    },
                )
                .cost(AbilityCost::Composite {
                    costs: vec![
                        AbilityCost::Tap,
                        AbilityCost::Sacrifice {
                            target: TargetFilter::SelfRef,
                            count: 1,
                        },
                    ],
                }),
            );
        }

        // Syr Ginger-style observer: ChangesZone Battlefield → Graveyard,
        // valid_card = Artifact controller=You.
        let observer = create_object(
            &mut state,
            CardId(201),
            PlayerId(0),
            "Syr Ginger".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&observer).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.entered_battlefield_turn = Some(1);
            let mut trig = TriggerDefinition::new(TriggerMode::ChangesZone);
            trig.origin = Some(Zone::Battlefield);
            trig.destination = Some(Zone::Graveyard);
            trig.valid_card = Some(TargetFilter::Typed(TypedFilter {
                type_filters: vec![TypeFilter::Artifact],
                controller: Some(ControllerRef::You),
                properties: vec![],
            }));
            trig.execute = Some(Box::new(crate::types::ability::AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::Draw {
                    count: QuantityExpr::Fixed { value: 1 },
                    target: TargetFilter::Controller,
                },
            )));
            obj.trigger_definitions.push(trig);
        }

        apply_as_current(
            &mut state,
            GameAction::ActivateAbility {
                source_id: treasure,
                ability_index: 0,
            },
        )
        .expect("activation succeeds");

        if matches!(state.waiting_for, WaitingFor::ChooseManaColor { .. }) {
            apply_as_current(
                &mut state,
                GameAction::ChooseManaColor {
                    choice: crate::types::game_state::ManaChoice::SingleColor(
                        crate::types::mana::ManaType::Red,
                    ),
                },
            )
            .expect("color choice succeeds");
        }

        assert!(
            state.stack.iter().any(|entry| entry.source_id == observer),
            "Syr Ginger's `whenever an artifact is put into a graveyard from \
             the battlefield` trigger must land on the stack when a Treasure \
             token is sacrificed as part of an activated mana ability cost"
        );
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

    #[test]
    fn auto_tap_assigns_flexible_sources_optimally() {
        // Reproduces the Spider Manifestation + Brightglass Gearhulk scenario:
        // Cost {G}{G}{W}{W}, sources: Forest({G}), Spider({R}/{G}),
        // Hushwood({G}/{W}), Air Temple({W}).
        // Greedy approach taps Hushwood for {G} first, leaving no second {W}.
        // MCV/LCV assigns: Forest→{G}, Spider→{G}, Air Temple→{W}, Hushwood→{W}.
        let mut state = GameState::new_two_player(42);

        let forest = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Forest".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&forest)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Land);
        state
            .objects
            .get_mut(&forest)
            .unwrap()
            .card_types
            .subtypes
            .push("Forest".to_string());

        let spider = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Spider Manifestation".to_string(),
            Zone::Battlefield,
        );
        let spider_obj = state.objects.get_mut(&spider).unwrap();
        spider_obj.card_types.core_types.push(CoreType::Creature);
        spider_obj.entered_battlefield_turn = Some(1);
        Arc::make_mut(&mut spider_obj.abilities).push(
            AbilityDefinition::new(
                AbilityKind::Activated,
                Effect::Mana {
                    produced: crate::types::ability::ManaProduction::AnyOneColor {
                        count: QuantityExpr::Fixed { value: 1 },
                        color_options: vec![ManaColor::Red, ManaColor::Green],
                        contribution: crate::types::ability::ManaContribution::Base,
                    },
                    restrictions: vec![],
                    grants: vec![],
                    expiry: None,
                    target: None,
                },
            )
            .cost(AbilityCost::Tap),
        );

        let hushwood = create_object(
            &mut state,
            CardId(3),
            PlayerId(0),
            "Hushwood Verge".to_string(),
            Zone::Battlefield,
        );
        let hushwood_obj = state.objects.get_mut(&hushwood).unwrap();
        hushwood_obj.card_types.core_types.push(CoreType::Land);
        Arc::make_mut(&mut hushwood_obj.abilities).push(
            AbilityDefinition::new(
                AbilityKind::Activated,
                Effect::Mana {
                    produced: crate::types::ability::ManaProduction::Fixed {
                        colors: vec![ManaColor::Green],
                        contribution: crate::types::ability::ManaContribution::Base,
                    },
                    restrictions: vec![],
                    grants: vec![],
                    expiry: None,
                    target: None,
                },
            )
            .cost(AbilityCost::Tap),
        );
        Arc::make_mut(&mut hushwood_obj.abilities).push(
            AbilityDefinition::new(
                AbilityKind::Activated,
                Effect::Mana {
                    produced: crate::types::ability::ManaProduction::Fixed {
                        colors: vec![ManaColor::White],
                        contribution: crate::types::ability::ManaContribution::Base,
                    },
                    restrictions: vec![],
                    grants: vec![],
                    expiry: None,
                    target: None,
                },
            )
            .cost(AbilityCost::Tap),
        );

        let air_temple = create_object(
            &mut state,
            CardId(4),
            PlayerId(0),
            "Abandoned Air Temple".to_string(),
            Zone::Battlefield,
        );
        let air_obj = state.objects.get_mut(&air_temple).unwrap();
        air_obj.card_types.core_types.push(CoreType::Land);
        Arc::make_mut(&mut air_obj.abilities).push(
            AbilityDefinition::new(
                AbilityKind::Activated,
                Effect::Mana {
                    produced: crate::types::ability::ManaProduction::Fixed {
                        colors: vec![ManaColor::White],
                        contribution: crate::types::ability::ManaContribution::Base,
                    },
                    restrictions: vec![],
                    grants: vec![],
                    expiry: None,
                    target: None,
                },
            )
            .cost(AbilityCost::Tap),
        );

        state.turn_number = 3;
        let mut events = Vec::new();
        auto_tap_mana_sources(
            &mut state,
            PlayerId(0),
            &ManaCost::Cost {
                shards: vec![
                    ManaCostShard::Green,
                    ManaCostShard::Green,
                    ManaCostShard::White,
                    ManaCostShard::White,
                ],
                generic: 0,
            },
            &mut events,
            None,
        );

        let pool = &state.players[0].mana_pool;
        assert_eq!(
            pool.count_color(ManaType::Green),
            2,
            "should produce 2 green"
        );
        assert_eq!(
            pool.count_color(ManaType::White),
            2,
            "should produce 2 white"
        );
    }

    mod cascade_constraint {
        use super::*;
        use crate::types::ability::{CastPermissionConstraint, CastingPermission};
        use crate::types::mana::ManaCostShard;

        fn exile_card(state: &mut GameState, owner: PlayerId, name: &str) -> ObjectId {
            let card_id = CardId(state.next_object_id);
            create_object(state, card_id, owner, name.to_string(), Zone::Exile)
        }

        fn setup_x_cost_hit(source_mv: u32, chosen_x: u32) -> (GameState, ObjectId, Vec<ObjectId>) {
            let mut state = GameState::new_two_player(42);
            let miss_a = exile_card(&mut state, PlayerId(0), "Miss A");
            let miss_b = exile_card(&mut state, PlayerId(0), "Miss B");

            let hit = exile_card(&mut state, PlayerId(0), "X Spell Hit");
            let hit_obj = state.objects.get_mut(&hit).unwrap();
            hit_obj.mana_cost = ManaCost::Cost {
                shards: vec![ManaCostShard::X],
                generic: 0,
            };
            hit_obj.cost_x_paid = Some(chosen_x);
            hit_obj
                .casting_permissions
                .push(CastingPermission::ExileWithAltCost {
                    cost: ManaCost::zero(),
                    cast_transformed: false,
                    constraint: Some(CastPermissionConstraint::CascadeResultingMvBelow {
                        source_mv,
                        exiled_misses: vec![miss_a, miss_b],
                    }),
                });

            (state, hit, vec![miss_a, miss_b])
        }

        /// CR 702.85a + CR 202.3b + CR 107.3b: X=3 with source MV 4 — resulting
        /// spell MV is 3, which is strictly less than 4, so the cast is
        /// accepted. Misses bottom-shuffle; the cascade permission is consumed.
        #[test]
        fn accepts_when_resulting_mv_below_source() {
            let (mut state, hit, misses) = setup_x_cost_hit(4, 3);
            let mut events = Vec::new();
            let resulting_mv = state.objects.get(&hit).unwrap().mana_cost.mana_value()
                + state.objects.get(&hit).unwrap().cost_x_paid.unwrap_or(0);
            let outcome = evaluate_cascade_constraint_with_resulting_mv(
                &mut state,
                hit,
                resulting_mv,
                &mut events,
            );
            assert!(matches!(outcome, CascadeCheck::Accepted));

            let hit_obj = state.objects.get(&hit).unwrap();
            assert!(
                hit_obj.casting_permissions.is_empty(),
                "cascade permission must be consumed on accept"
            );

            for miss in &misses {
                assert_eq!(
                    state.objects.get(miss).map(|o| o.zone),
                    Some(Zone::Library),
                    "misses must be bottom-shuffled on accept"
                );
            }
            assert_eq!(
                state.objects.get(&hit).map(|o| o.zone),
                Some(Zone::Exile),
                "hit card continues through normal cast flow — not bottom-shuffled"
            );
        }

        /// CR 702.85a: X=4 with source MV 4 — resulting MV is 4, which is NOT
        /// strictly less than 4, so the cast is rejected. The permission is
        /// still consumed, and the returned misses match the original set for
        /// the caller to bottom-shuffle together with the hit.
        #[test]
        fn rejects_when_resulting_mv_equals_source() {
            let (mut state, hit, misses) = setup_x_cost_hit(4, 4);
            let mut events = Vec::new();
            let resulting_mv = state.objects.get(&hit).unwrap().mana_cost.mana_value()
                + state.objects.get(&hit).unwrap().cost_x_paid.unwrap_or(0);
            let outcome = evaluate_cascade_constraint_with_resulting_mv(
                &mut state,
                hit,
                resulting_mv,
                &mut events,
            );
            match outcome {
                CascadeCheck::Rejected { exiled_misses } => {
                    assert_eq!(exiled_misses, misses);
                }
                other => panic!("Expected Rejected, got {:?}", matches_name(&other)),
            }

            let hit_obj = state.objects.get(&hit).unwrap();
            assert!(
                hit_obj.casting_permissions.is_empty(),
                "cascade permission must be consumed on reject too"
            );

            for miss in &misses {
                assert_eq!(
                    state.objects.get(miss).map(|o| o.zone),
                    Some(Zone::Exile),
                    "misses stay put until handle_cascade_rejection runs"
                );
            }
        }

        /// CR 702.85a: X=5 with source MV 4 — resulting MV exceeds source, so
        /// the cast is rejected. Confirms strict inequality is enforced above
        /// as well as at the equality boundary.
        #[test]
        fn rejects_when_resulting_mv_above_source() {
            let (mut state, hit, _misses) = setup_x_cost_hit(4, 5);
            let mut events = Vec::new();
            let resulting_mv = state.objects.get(&hit).unwrap().mana_cost.mana_value()
                + state.objects.get(&hit).unwrap().cost_x_paid.unwrap_or(0);
            let outcome = evaluate_cascade_constraint_with_resulting_mv(
                &mut state,
                hit,
                resulting_mv,
                &mut events,
            );
            assert!(matches!(outcome, CascadeCheck::Rejected { .. }));
        }

        /// CR 702.85a + CR 601.2a: The rejection handler pops the
        /// announcement-time stack entry, bottom-shuffles misses + the hit in
        /// random order, and returns priority to the caster.
        #[test]
        fn rejection_handler_pops_stack_and_bottom_shuffles_all() {
            let (mut state, hit, misses) = setup_x_cost_hit(4, 4);

            state.stack.push_back(StackEntry {
                id: hit,
                source_id: hit,
                controller: PlayerId(0),
                kind: StackEntryKind::Spell {
                    card_id: CardId(0),
                    ability: None,
                    casting_variant: CastingVariant::Normal,
                    actual_mana_spent: 0,
                },
            });
            let stack_depth_before = state.stack.len();

            let mut events = Vec::new();
            let waiting_for =
                handle_cascade_rejection(&mut state, PlayerId(0), hit, misses.clone(), &mut events)
                    .expect("rejection handler must succeed");

            assert_eq!(
                state.stack.len(),
                stack_depth_before - 1,
                "announcement stack entry must be popped"
            );
            assert!(
                !state.stack.iter().any(|e| e.id == hit),
                "no stack entry for the rejected cast may remain"
            );

            for id in misses.iter().chain(std::iter::once(&hit)) {
                assert_eq!(
                    state.objects.get(id).map(|o| o.zone),
                    Some(Zone::Library),
                    "misses and hit must bottom-shuffle together on rejection"
                );
            }

            match waiting_for {
                WaitingFor::Priority { player } => assert_eq!(player, PlayerId(0)),
                other => panic!("Expected Priority for caster, got {:?}", other),
            }
        }

        fn matches_name(check: &CascadeCheck) -> &'static str {
            match check {
                CascadeCheck::NotApplicable => "NotApplicable",
                CascadeCheck::Accepted => "Accepted",
                CascadeCheck::Rejected { .. } => "Rejected",
            }
        }
    }
}
