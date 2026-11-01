use super::ResolutionProgress;
use crate::types::ability::{EffectError, EffectKind, ResolvedAbility};
use crate::types::events::{ClashResult, GameEvent};
use crate::types::game_state::{GameState, WaitingFor};
use crate::types::identifiers::ObjectId;
use crate::types::player::PlayerId;

pub(crate) struct ClashStep {
    pub progress: ResolutionProgress,
    pub result: Option<ClashResult>,
}

/// CR 701.30: Clash with an opponent.
///
/// CR 701.30b: "Clash with an opponent" means "Choose an opponent. You and that
/// opponent each clash." The choice of opponent belongs to the clashing player
/// and is made as the ability resolves. With two or more opponents the engine
/// pauses on `WaitingFor::ClashChooseOpponent` for that choice; with a single
/// opponent there is nothing to choose, so the clash proceeds immediately.
///
/// CR 701.30a: Each player reveals the top card of their library.
/// CR 701.30c: Revealed simultaneously; choices in APNAP order.
/// CR 701.30d: Higher mana value wins.
///
/// Returns the result to `finish_clash_resolution`, which binds the
/// result-aware tail context once the outcome is known.
pub(crate) fn resolve(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<ClashStep, EffectError> {
    let controller = ability.controller;

    // CR 701.30b: "Choose an opponent. You and that opponent each clash." — a CHOICE, not
    // a target (CR 115.10a), so the seat is judged by `player_exists_for_choice` and NOT
    // by the targeting-only exclusions. `p.id != controller` stays: that is opponent
    // SCOPE, not legality.
    let candidates: Vec<PlayerId> = state
        .players
        .iter()
        .filter(|p| {
            p.id != controller && crate::game::players::player_exists_for_choice(state, p.id)
        })
        .map(|p| p.id)
        .collect();

    // CR 701.30b: With no opponents there is no legal clash opponent. The
    // effect does nothing; any result-gated rider remains unperformed.
    if candidates.is_empty() {
        return Ok(ClashStep {
            progress: ResolutionProgress::Complete,
            result: None,
        });
    }

    // CR 701.30b: With two or more opponents the choice is a genuine decision
    // made by the controller — pause for it. The chosen opponent (validated
    // against `candidates`) is fed back through `perform_clash`.
    if candidates.len() >= 2 {
        state.waiting_for = WaitingFor::ClashChooseOpponent {
            player: controller,
            candidates,
            ability: Box::new(ability.clone()),
        };
        return Ok(ClashStep {
            progress: ResolutionProgress::Waiting,
            result: None,
        });
    }

    // CR 701.30b: One opponent — no decision to make. The single opponent is
    // used directly so two-player games never see an extra prompt.
    let opponent = candidates[0];
    perform_clash(state, ability, opponent, events)
}

/// CR 701.30a-d: Carry out a clash against an already-chosen `opponent`: reveal
/// both top cards, decide the result by mana value, and queue the APNAP
/// top/bottom placement. Shared by the immediate (≤1 opponent) path and the
/// resumed `WaitingFor::ClashChooseOpponent` path so both behave identically.
pub(crate) fn perform_clash(
    state: &mut GameState,
    ability: &ResolvedAbility,
    opponent: PlayerId,
    events: &mut Vec<GameEvent>,
) -> Result<ClashStep, EffectError> {
    let controller = ability.controller;

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
        subject: None,
    });

    // CR 701.30c + CR 101.4: all participants reveal before choosing in
    // APNAP order. Placement is committed only after the final answer.
    let mut placements: Vec<_> = crate::game::players::apnap_order(state)
        .into_iter()
        .filter_map(|player| {
            if player == controller {
                controller_top.map(|card| (player, card))
            } else if player == opponent {
                opponent_top.map(|card| (player, card))
            } else {
                None
            }
        })
        .collect();
    let progress = if placements.is_empty() {
        ResolutionProgress::Complete
    } else {
        let (player, card) = placements.remove(0);
        state.waiting_for = WaitingFor::ClashCardPlacement {
            player,
            card,
            remaining: placements,
            bottom_cards: Vec::new(),
        };
        ResolutionProgress::Waiting
    };
    Ok(ClashStep {
        progress,
        result: Some(result),
    })
}

/// Get the top card ObjectId of a player's library, if any.
fn top_card_of_library(state: &GameState, player: PlayerId) -> Option<ObjectId> {
    let seat = state.players.iter().find(|p| p.id == player)?.id;
    // CR 701.30a: a clashing player reveals the top card, index 0.
    state.library_of(seat).front().copied()
}

/// Get the mana value of a card by its object ID.
///
/// CR 202.3d + CR 709.4b: A clashed card is on top of a library (off the stack),
/// so a split card reports its combined mana value; `effective_mana_value`
/// no-ops for single-face cards.
fn card_mana_value(state: &GameState, object_id: ObjectId) -> Option<u32> {
    let obj = state.objects.get(&object_id)?;
    Some(obj.effective_mana_value())
}
