use crate::types::ability::{
    AbilityCost, AdditionalCost, BeholdCostAction, TapCreaturesAggregate, TargetFilter,
};
use crate::types::events::GameEvent;
use crate::types::game_state::{
    CollectEvidenceResume, GameState, PendingCast, PendingManaAbility, WaitingFor,
};
use crate::types::identifiers::ObjectId;
use crate::types::mana::ManaCost;
use crate::types::player::PlayerId;
use crate::types::zones::{ExileCostSourceZone, Zone};

use super::engine::EngineError;
use super::{casting, casting_costs, mana_abilities};
use casting_costs::{CostSelection, SpellCostPayment};

pub(super) fn cancel_pending_cast(
    state: &mut GameState,
    player: PlayerId,
    pending_cast: &PendingCast,
    events: &mut Vec<GameEvent>,
) -> WaitingFor {
    casting::handle_cancel_cast(state, pending_cast, events);
    WaitingFor::Priority { player }
}

pub(super) fn handle_target_selection_select_targets(
    state: &mut GameState,
    player: PlayerId,
    targets: Vec<crate::types::ability::TargetRef>,
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    casting::handle_select_targets(state, player, targets, events)
}

pub(super) fn handle_target_selection_choose_target(
    state: &mut GameState,
    player: PlayerId,
    target: Option<crate::types::ability::TargetRef>,
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    casting::handle_choose_target(state, player, target, events)
}

pub(super) fn handle_optional_cost_choice(
    state: &mut GameState,
    player: PlayerId,
    pending_cast: PendingCast,
    cost: &AdditionalCost,
    pay: bool,
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    casting_costs::handle_decide_additional_cost(state, player, pending_cast, cost, pay, events)
}

pub(super) fn handle_defiler_payment(
    state: &mut GameState,
    player: PlayerId,
    pending_cast: PendingCast,
    life_cost: u32,
    mana_reduction: &crate::types::mana::ManaCost,
    pay: bool,
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    casting_costs::handle_defiler_payment(
        state,
        player,
        pending_cast,
        life_cost,
        mana_reduction,
        pay,
        events,
    )
}

pub(super) fn handle_discard_for_cost(
    state: &mut GameState,
    player: PlayerId,
    pending_cast: PendingCast,
    count: usize,
    legal_cards: &[ObjectId],
    chosen: &[ObjectId],
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    casting::handle_discard_for_cost(
        state,
        player,
        pending_cast,
        count,
        legal_cards,
        chosen,
        events,
    )
}

pub(super) fn handle_activation_cost_one_of_choice(
    state: &mut GameState,
    player: PlayerId,
    pending: PendingCast,
    costs: &[AbilityCost],
    index: usize,
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    casting::handle_activation_cost_one_of_choice(state, player, pending, costs, index, events)
}

pub(super) fn handle_sacrifice_for_cost(
    state: &mut GameState,
    player: PlayerId,
    pending_cast: PendingCast,
    paid_cost: Option<SpellCostPayment<'_>>,
    selection: CostSelection<'_>,
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    casting::handle_sacrifice_for_cost(state, player, pending_cast, paid_cost, selection, events)
}

pub(super) fn handle_return_to_hand_for_cost(
    state: &mut GameState,
    player: PlayerId,
    pending_cast: PendingCast,
    count: usize,
    permanents: &[ObjectId],
    chosen: &[ObjectId],
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    casting::handle_return_to_hand_for_cost(
        state,
        player,
        pending_cast,
        count,
        permanents,
        chosen,
        events,
    )
}

#[allow(clippy::too_many_arguments)]
pub(super) fn handle_tap_creatures_for_spell_cost(
    state: &mut GameState,
    player: PlayerId,
    pending_cast: PendingCast,
    count: usize,
    aggregate: Option<TapCreaturesAggregate>,
    creatures: &[ObjectId],
    chosen: &[ObjectId],
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    casting_costs::handle_tap_creatures_for_spell_cost(
        state,
        player,
        pending_cast,
        count,
        aggregate,
        creatures,
        chosen,
        events,
    )
}

#[allow(clippy::too_many_arguments)]
pub(super) fn handle_behold_for_cost(
    state: &mut GameState,
    player: PlayerId,
    pending_cast: PendingCast,
    count: usize,
    choices: &[ObjectId],
    action: BeholdCostAction,
    chosen: &[ObjectId],
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    casting_costs::handle_behold_for_cost(
        state,
        player,
        pending_cast,
        count,
        choices,
        action,
        chosen,
        events,
    )
}

pub(super) fn handle_tap_creatures_for_mana_ability(
    state: &mut GameState,
    count: usize,
    creatures: &[ObjectId],
    pending_mana_ability: &PendingManaAbility,
    chosen: &[ObjectId],
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    mana_abilities::handle_tap_creatures_for_mana_ability(
        state,
        count,
        creatures,
        pending_mana_ability,
        chosen,
        events,
    )
}

pub(super) fn handle_discard_for_mana_ability(
    state: &mut GameState,
    count: usize,
    legal_cards: &[ObjectId],
    pending_mana_ability: &PendingManaAbility,
    chosen: &[ObjectId],
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    mana_abilities::handle_discard_for_mana_ability(
        state,
        count,
        legal_cards,
        pending_mana_ability,
        chosen,
        events,
    )
}

pub(super) fn handle_choose_mana_color(
    state: &mut GameState,
    pending_mana_ability: &PendingManaAbility,
    prompt: &crate::types::game_state::ManaChoicePrompt,
    chosen: crate::types::game_state::ManaChoice,
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    mana_abilities::handle_choose_mana_color(state, pending_mana_ability, prompt, chosen, events)
}

/// CR 605.3a: Bulk-activate identical, choice-free sibling mana sources with the
/// color just chosen (the player's other Treasures, etc.). Thin forward to the
/// engine authority in `mana_abilities`.
pub(super) fn batch_activate_mana_siblings(
    state: &mut GameState,
    pending_mana_ability: &PendingManaAbility,
    chosen: &crate::types::game_state::ManaChoice,
    count: u32,
    events: &mut Vec<GameEvent>,
) -> Result<(), EngineError> {
    mana_abilities::batch_activate_mana_siblings(state, pending_mana_ability, chosen, count, events)
}

pub(super) fn handle_pay_mana_ability_mana(
    state: &mut GameState,
    options: &[Vec<crate::types::mana::ManaType>],
    pending_mana_ability: &PendingManaAbility,
    payment: &[crate::types::mana::ManaType],
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    mana_abilities::handle_pay_mana_ability_mana(
        state,
        options,
        pending_mana_ability,
        payment,
        events,
    )
}

#[allow(clippy::too_many_arguments)]
pub(super) fn handle_exile_for_cost(
    state: &mut GameState,
    player: PlayerId,
    zone: ExileCostSourceZone,
    pending_cast: PendingCast,
    count: usize,
    legal_cards: &[ObjectId],
    chosen: &[ObjectId],
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    casting_costs::handle_exile_for_cost(
        state,
        player,
        zone,
        pending_cast,
        count,
        legal_cards,
        chosen,
        events,
    )
}

#[allow(clippy::too_many_arguments)]
pub(super) fn handle_exile_aggregate_for_cost(
    state: &mut GameState,
    player: PlayerId,
    zone: crate::types::zones::Zone,
    function: crate::types::ability::AggregateFunction,
    property: crate::types::ability::ObjectProperty,
    comparator: crate::types::ability::Comparator,
    value: i32,
    filter: &TargetFilter,
    pending_cast: PendingCast,
    legal_cards: &[ObjectId],
    chosen: &[ObjectId],
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    casting_costs::handle_exile_aggregate_for_cost(
        state,
        player,
        zone,
        function,
        property,
        comparator,
        value,
        filter,
        pending_cast,
        legal_cards,
        chosen,
        events,
    )
}

#[allow(clippy::too_many_arguments)]
pub(super) fn handle_exile_permanent_for_cost(
    state: &mut GameState,
    player: PlayerId,
    filter: Option<TargetFilter>,
    pending_cast: PendingCast,
    count: usize,
    legal_cards: &[ObjectId],
    chosen: &[ObjectId],
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    casting_costs::handle_exile_permanent_for_cost(
        state,
        player,
        filter,
        pending_cast,
        count,
        legal_cards,
        chosen,
        events,
    )
}

#[allow(clippy::too_many_arguments)]
pub(super) fn handle_exile_materials_for_cost(
    state: &mut GameState,
    player: PlayerId,
    materials: TargetFilter,
    pending_cast: PendingCast,
    bounds: (usize, usize),
    legal_cards: &[ObjectId],
    chosen: &[ObjectId],
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    casting_costs::handle_exile_materials_for_cost(
        state,
        player,
        materials,
        pending_cast,
        bounds,
        legal_cards,
        chosen,
        events,
    )
}

pub(super) fn handle_collect_evidence_cancel(
    state: &mut GameState,
    player: PlayerId,
    resume: &CollectEvidenceResume,
    events: &mut Vec<GameEvent>,
) -> WaitingFor {
    if let CollectEvidenceResume::Casting { pending_cast } = resume {
        casting::handle_cancel_cast(state, pending_cast, events);
    }
    WaitingFor::Priority { player }
}

pub(super) fn handle_harmonize_tap_choice(
    state: &mut GameState,
    player: PlayerId,
    eligible_creatures: &[ObjectId],
    pending_cast: PendingCast,
    creature_id: Option<ObjectId>,
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    let mut pending = pending_cast;

    if let Some(creature_id) = creature_id {
        if !eligible_creatures.contains(&creature_id) {
            return Err(EngineError::ActionNotAllowed(
                "Creature not eligible for Harmonize tap".into(),
            ));
        }

        let obj = state
            .objects
            .get(&creature_id)
            .ok_or_else(|| EngineError::InvalidAction("Creature no longer exists".into()))?;
        if obj.zone != Zone::Battlefield || obj.tapped {
            return Err(EngineError::InvalidAction(
                "Creature is no longer eligible for Harmonize tap".into(),
            ));
        }

        let power = obj.power.unwrap_or(0).max(0) as u32;

        if let Some(obj) = state.objects.get_mut(&creature_id) {
            obj.tapped = true;
        }
        events.push(GameEvent::PermanentTapped {
            object_id: creature_id,
            caused_by: None,
        });

        if let ManaCost::Cost {
            ref mut generic, ..
        } = pending.cost
        {
            *generic = generic.saturating_sub(power);
        }
    }

    let base_cost = pending.base_cost.clone();
    casting_costs::pay_and_push_adventure(
        state,
        player,
        pending.object_id,
        pending.card_id,
        pending.ability,
        &pending.cost,
        base_cost,
        pending.casting_variant,
        pending.cast_timing_permission,
        pending.distribute,
        pending.origin_zone,
        pending.payment_mode,
        events,
    )
}
