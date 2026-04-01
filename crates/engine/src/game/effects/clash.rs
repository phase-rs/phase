use crate::types::ability::{EffectError, EffectKind, ResolvedAbility};
use crate::types::events::{ClashResult, GameEvent};
use crate::types::game_state::{GameState, WaitingFor};
use crate::types::identifiers::ObjectId;
use crate::types::player::PlayerId;

/// CR 701.30: Clash with an opponent.
///
/// CR 701.30a: Each player reveals the top card of their library.
/// CR 701.30c: Revealed simultaneously; choices in APNAP order.
/// CR 701.30d: Higher mana value wins.
///
/// Sets `optional_effect_performed` on the ability context so that sub-abilities
/// gated by `AbilityCondition::IfYouDo` ("if you win the clash") execute only
/// when the controller wins.
pub fn resolve(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let controller = ability.controller;

    // CR 701.30b: Choose an opponent. For 2-player, it's the other player.
    let opponent = state
        .players
        .iter()
        .find(|p| p.id != controller && !p.is_eliminated)
        .map(|p| p.id)
        .unwrap_or(PlayerId(1));

    // CR 701.30a: Reveal top card of each player's library.
    let controller_top = top_card_of_library(state, controller);
    let opponent_top = top_card_of_library(state, opponent);

    let controller_mv = controller_top.and_then(|id| card_mana_value(state, id));
    let opponent_mv = opponent_top.and_then(|id| card_mana_value(state, id));

    // CR 701.30d: A player wins if their card has a higher mana value.
    let result = match (controller_mv, opponent_mv) {
        (Some(c), Some(o)) if c > o => ClashResult::Won,
        (Some(c), Some(o)) if c < o => ClashResult::Lost,
        (Some(_), None) => ClashResult::Won,
        (None, Some(_)) => ClashResult::Lost,
        _ => ClashResult::Tied,
    };

    // Add revealed cards for visibility.
    state.last_revealed_ids.clear();
    if let Some(id) = controller_top {
        state.last_revealed_ids.push(id);
    }
    if let Some(id) = opponent_top {
        state.last_revealed_ids.push(id);
    }

    events.push(GameEvent::Clash {
        controller,
        opponent,
        controller_mana_value: controller_mv,
        opponent_mana_value: opponent_mv,
        result,
    });
    events.push(GameEvent::EffectResolved {
        kind: EffectKind::Clash,
        source_id: ability.source_id,
    });

    // Build the sub_ability chain with updated context for optional_effect_performed.
    let original_sub = ability.sub_ability.as_ref().map(|sub| {
        let mut sub_clone = sub.as_ref().clone();
        sub_clone.context = ability.context.clone();
        sub_clone.context.optional_effect_performed = result == ClashResult::Won;
        sub_clone
    });

    // CR 701.30c: Each player puts their revealed card on top or bottom of their
    // library. Choices are made in APNAP order (controller first, then opponent).
    // ClashCardPlacement tracks the sequential choices; the engine handler advances
    // through the remaining queue before popping the pending_continuation.
    // Stash helper: ClashCardPlacement arms set pending_continuation manually
    // (not via the automatic stash in resolve_ability_chain) because clash injects
    // a modified context with optional_effect_performed set to the clash result.
    // The early-exit in resolve_ability_chain detects pending_continuation and skips
    // redundant sub_ability processing.
    let stash_sub = |state: &mut GameState| {
        if let Some(sub) = original_sub {
            state.pending_continuation = Some(Box::new(sub));
        }
    };

    match (controller_top, opponent_top) {
        (Some(c_card), Some(o_card)) => {
            // CR 701.30c + CR 101.4: APNAP — controller chooses first, opponent queued.
            state.waiting_for = WaitingFor::ClashCardPlacement {
                player: controller,
                card: c_card,
                remaining: vec![(opponent, o_card)],
            };
            stash_sub(state);
        }
        (Some(c_card), None) => {
            state.waiting_for = WaitingFor::ClashCardPlacement {
                player: controller,
                card: c_card,
                remaining: vec![],
            };
            stash_sub(state);
        }
        (None, Some(o_card)) => {
            state.waiting_for = WaitingFor::ClashCardPlacement {
                player: opponent,
                card: o_card,
                remaining: vec![],
            };
            stash_sub(state);
        }
        (None, None) => {
            // Both libraries empty — no cards to place. The sub_ability chain
            // (including IfYouDo gating) continues naturally via resolve_ability_chain.
            // Context is correct: both MVs are None → Tied → optional_effect_performed
            // stays false (the default), matching the natural ability.context propagation.
        }
    }

    Ok(())
}

/// Get the top card ObjectId of a player's library, if any.
fn top_card_of_library(state: &GameState, player: PlayerId) -> Option<ObjectId> {
    state
        .players
        .iter()
        .find(|p| p.id == player)?
        .library
        .last()
        .copied()
}

/// Get the mana value of a card by its object ID.
fn card_mana_value(state: &GameState, object_id: ObjectId) -> Option<u32> {
    let obj = state.objects.get(&object_id)?;
    Some(obj.mana_cost.mana_value())
}
