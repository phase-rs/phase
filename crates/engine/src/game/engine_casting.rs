use crate::types::ability::AdditionalCost;
use crate::types::events::GameEvent;
use crate::types::game_state::{
    CollectEvidenceResume, GameState, PendingCast, PendingManaAbility, WaitingFor,
};
use crate::types::identifiers::ObjectId;
use crate::types::mana::ManaCost;
use crate::types::player::PlayerId;
use crate::types::zones::Zone;

use super::engine::EngineError;
use super::{casting, casting_costs, mana_abilities};

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

pub(super) fn handle_sacrifice_for_cost(
    state: &mut GameState,
    player: PlayerId,
    pending_cast: PendingCast,
    count: usize,
    permanents: &[ObjectId],
    chosen: &[ObjectId],
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    casting::handle_sacrifice_for_cost(
        state,
        player,
        pending_cast,
        count,
        permanents,
        chosen,
        events,
    )
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

pub(super) fn handle_blight_choice(
    state: &mut GameState,
    player: PlayerId,
    pending_cast: PendingCast,
    count: usize,
    creatures: &[ObjectId],
    chosen: &[ObjectId],
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    casting_costs::handle_blight_choice(
        state,
        player,
        pending_cast,
        count,
        creatures,
        chosen,
        events,
    )
}

pub(super) fn handle_tap_creatures_for_spell_cost(
    state: &mut GameState,
    player: PlayerId,
    pending_cast: PendingCast,
    count: usize,
    creatures: &[ObjectId],
    chosen: &[ObjectId],
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    casting_costs::handle_tap_creatures_for_spell_cost(
        state,
        player,
        pending_cast,
        count,
        creatures,
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

pub(super) fn handle_exile_from_graveyard_for_cost(
    state: &mut GameState,
    player: PlayerId,
    pending_cast: PendingCast,
    count: usize,
    legal_cards: &[ObjectId],
    chosen: &[ObjectId],
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    casting_costs::handle_exile_from_graveyard_for_cost(
        state,
        player,
        pending_cast,
        count,
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

    casting_costs::pay_and_push_adventure(
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
