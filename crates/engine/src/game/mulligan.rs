use crate::game::ability_utils::build_resolved_from_def;
use crate::game::effects::resolve_ability_chain;
use crate::types::ability::AbilityKind;
use crate::types::actions::MulliganChoice;
use crate::types::events::GameEvent;
use crate::types::format::GameFormat;
use crate::types::game_state::{
    GameState, MulliganBottomEntry, MulliganDecisionEntry, OpeningHandBottomReason,
    PendingBeginGameAbility, WaitingFor,
};
use crate::types::identifiers::ObjectId;
use crate::types::player::PlayerId;
use crate::types::zones::Zone;

use super::turns;
use super::zones;

/// CR 103.5: A player's starting hand size is normally seven cards.
const STARTING_HAND_SIZE: usize = 7;
/// CR 103.5 (final sentence): a player may take mulligans until their opening
/// hand would be zero cards. In a standard game that means at most 7 mulligans
/// (7→6→5→4→3→2→1→0; the 8th would be 0). CR 103.5c adds that in free-first
/// formats the first mulligan is uncounted, so the cap shifts up by one to 8
/// — the player may still be brought all the way down to a 1-card opening
/// hand after exhausting their bottoms allowance.
const MAX_MULLIGANS: u8 = 7;

/// CR 103.5 + 103.5c: maximum number of `Mulligan` submissions a player may
/// make before being force-removed from `pending`. In free-first formats the
/// first mulligan doesn't count toward this cap.
fn max_mulligans_for(free_first: bool) -> u8 {
    if free_first {
        MAX_MULLIGANS + 1
    } else {
        MAX_MULLIGANS
    }
}

/// Card name that grants the CR 103.5b "you could mulligan" action implemented
/// here. Match is case-insensitive and exact (CR 201.2 — name is the printed
/// English name on the card). The rule applies to every card with this name,
/// not to a specific printing.
const SERUM_POWDER_NAME: &str = "Serum Powder";

/// CR 103.5c + Commander RC supplement: whether `state` grants a free first
/// mulligan. True for any multiplayer game (≥3 seats), and for duels in
/// formats where `GameFormat::grants_free_first_mulligan()` holds.
fn free_first_mulligan(state: &GameState) -> bool {
    state.seat_order.len() > 2 || state.format_config.format.grants_free_first_mulligan()
}

/// CR 103.5: Cards a player must put on the bottom of their library after
/// keeping with `mulligan_count` mulligans taken (free-first discount applied
/// when the game grants one).
fn bottom_count_for(mulligan_count: u8, free_first: bool) -> u8 {
    if free_first {
        mulligan_count.saturating_sub(1)
    } else {
        mulligan_count
    }
}

/// CR 103.5 + CR 103.5c: Number of cards a player keeps after deciding to keep
/// with `mulligan_count` mulligans taken (free-first discount applied when the
/// game grants one). Starting hand size minus the bottoms owed.
pub fn kept_hand_size_after(mulligan_count: u8, free_first: bool) -> usize {
    STARTING_HAND_SIZE.saturating_sub(bottom_count_for(mulligan_count, free_first) as usize)
}

/// CR 103.4: Start the mulligan process — shuffle libraries and draw 7 for each player.
///
/// CR 103.5 + 103.5b: All players decide simultaneously. The returned
/// `WaitingFor::MulliganDecision` carries every living player in seat order;
/// each may submit `MulliganDecision { choice }` in any arrival order, with
/// `MulliganChoice::Keep`, `Mulligan`, or `UseSerumPowder { object_id }`.
///
/// CR 103.5d deferred: Two-Headed Giant team mulligans are not modeled — the
/// engine has the format enum but no team/seating semantics yet.
pub fn start_mulligan(state: &mut GameState, events: &mut Vec<GameEvent>) -> WaitingFor {
    events.push(GameEvent::MulliganStarted);
    state.prepaid_mulligan_bottoms.clear();

    // Shuffle every player's library.
    let GameState { players, rng, .. } = &mut *state;
    for player in players.iter_mut() {
        crate::util::im_ext::shuffle_vector(&mut player.library, rng);
    }

    // Draw the opening hand for each player in seat order.
    let seat_order = state.seat_order.clone();
    for &player_id in &seat_order {
        draw_n(state, player_id, STARTING_HAND_SIZE, events);
    }

    let forced_pending = tiny_leaders_forced_mulligan_pending(state);
    if !forced_pending.is_empty() {
        return WaitingFor::OpeningHandBottomCards {
            pending: forced_pending,
            reason: OpeningHandBottomReason::TinyLeadersMultiCommander,
        };
    }

    normal_mulligan_decision(state)
}

fn normal_mulligan_decision(state: &GameState) -> WaitingFor {
    let pending = state
        .seat_order
        .iter()
        .map(|&player| MulliganDecisionEntry {
            player,
            mulligan_count: state
                .prepaid_mulligan_bottoms
                .get(&player)
                .copied()
                .unwrap_or(0),
        })
        .collect();

    WaitingFor::MulliganDecision {
        pending,
        free_first_mulligan: free_first_mulligan(state),
    }
}

fn tiny_leaders_forced_mulligan_pending(state: &GameState) -> Vec<MulliganBottomEntry> {
    if state.format_config.format != GameFormat::TinyLeaders {
        return Vec::new();
    }

    state
        .seat_order
        .iter()
        .filter(|&&player| {
            state
                .deck_pools
                .iter()
                .find(|pool| pool.player == player)
                .map(|pool| {
                    pool.current_commander
                        .iter()
                        .map(|entry| entry.count)
                        .sum::<u32>()
                })
                .unwrap_or(0)
                > 1
        })
        .map(|&player| MulliganBottomEntry { player, count: 1 })
        .collect()
}

/// CR 103.5 + 103.5b: Resolve one player's `MulliganDecision { choice }` action.
///
/// - `Keep` removes the player from `pending`. The player has locked in their
///   hand for the game; their bottom-cards selection is deferred to the second
///   phase (CR 103.5 second sentence: "all players who decided to take
///   mulligans do so at the same time" — bottoms happen after every player has
///   kept).
/// - `Mulligan` increments that player's `mulligan_count`, shuffles their hand
///   back into their library, and redraws the starting hand size. The player
///   remains in `pending` to decide again.
/// - `UseSerumPowder { object_id }` (CR 103.5b + Serum Powder Oracle text)
///   exiles **every** card from the player's hand — including the named Serum
///   Powder itself — and redraws the same number of cards. The player's
///   `mulligan_count` is *not* incremented (this is not a mulligan). The
///   player remains in `pending` and may then keep, mulligan, or use another
///   Serum Powder if their new hand contains one.
///
/// If `Mulligan` brings the player to the maximum mulligan count (CR 103.5
/// final sentence: a player may not take a mulligan that would result in a
/// zero-card hand), the player is force-removed from `pending` and will
/// bottom every card in their hand.
///
/// When `pending` becomes empty, advance to `MulliganBottomCards` (or, if no
/// one owes bottoms, directly to `finish_mulligans`).
pub fn handle_mulligan_decision(
    state: &mut GameState,
    player: PlayerId,
    choice: MulliganChoice,
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, String> {
    let free_first = free_first_mulligan(state);

    // Snapshot the current pending list (we own a clone because the engine
    // borrows `state.waiting_for` immutably during match dispatch).
    let WaitingFor::MulliganDecision { pending, .. } = &state.waiting_for else {
        return Err("handle_mulligan_decision called outside MulliganDecision".to_string());
    };
    let mut pending = pending.clone();

    let idx = pending
        .iter()
        .position(|e| e.player == player)
        .ok_or_else(|| format!("Player {:?} is not in the mulligan pending set", player))?;
    let current_count = pending[idx].mulligan_count;

    match choice {
        MulliganChoice::Keep => {
            // Record the final mulligan_count for the bottoms phase. Track in
            // state.final_mulligan_counts indexed by PlayerId — populated as
            // each player locks in their hand.
            record_final_count(state, player, current_count);
            pending.remove(idx);
        }
        MulliganChoice::Mulligan => {
            let new_count = current_count + 1;
            shuffle_hand_into_library(state, player, events);
            draw_n(state, player, STARTING_HAND_SIZE, events);

            if new_count >= max_mulligans_for(free_first) {
                // CR 103.5 + 103.5c: A player may take mulligans until their
                // opening hand would be zero cards. In free-first formats the
                // first mulligan is uncounted, so the cap is one higher.
                // Force-remove from pending; the bottoms phase will bottom
                // every card in their hand.
                record_final_count(state, player, new_count);
                pending.remove(idx);
            } else {
                pending[idx].mulligan_count = new_count;
            }
        }
        MulliganChoice::UseSerumPowder { object_id } => {
            // CR 103.5b + Serum Powder Oracle text: validate the referenced
            // object is in the actor's hand and is named "Serum Powder"
            // (CR 201.2 — name match is exact), then exile the entire hand
            // and redraw the same number of cards. Mulligan count unchanged.
            handle_serum_powder(state, player, object_id, events)?;
            // Player remains in `pending` with the same mulligan_count.
        }
    }

    Ok(advance_after_decision(state, pending, free_first, events))
}

/// CR 103.5b + Serum Powder Oracle text: "Any time you could mulligan and this
/// card is in your hand, you may exile all the cards from your hand, then draw
/// that many cards."
///
/// Validates `serum_powder_id` is in `player`'s hand and is named "Serum
/// Powder" (case-insensitive — CR 201.2 names are case-canonical but card data
/// casing should still tolerate variation). Then moves every card in the
/// hand — including the Serum Powder itself — to exile, and draws that many
/// cards. Does not shuffle, does not change the library, does not increment
/// the mulligan counter.
fn handle_serum_powder(
    state: &mut GameState,
    player: PlayerId,
    serum_powder_id: ObjectId,
    events: &mut Vec<GameEvent>,
) -> Result<(), String> {
    let player_data = state
        .players
        .iter()
        .find(|p| p.id == player)
        .ok_or_else(|| format!("Player {:?} not found", player))?;

    if !player_data.hand.contains(&serum_powder_id) {
        return Err(format!(
            "Serum Powder object {:?} is not in player {:?}'s hand",
            serum_powder_id, player
        ));
    }

    let referenced = state
        .objects
        .get(&serum_powder_id)
        .ok_or_else(|| format!("Object {:?} not found", serum_powder_id))?;
    if !referenced.name.eq_ignore_ascii_case(SERUM_POWDER_NAME) {
        return Err(format!(
            "Object {:?} is named {:?}, not Serum Powder — only Serum Powder cards may use this action",
            serum_powder_id, referenced.name
        ));
    }

    // CR 103.5b: Exile every card from the hand (including the Powder). The
    // exiled cards are gone for the rest of the game (per the official ruling
    // on Serum Powder, 2017-11-17).
    let hand_ids: Vec<ObjectId> = state
        .players
        .iter()
        .find(|p| p.id == player)
        .expect("player exists")
        .hand
        .iter()
        .copied()
        .collect();
    let exiled_count = hand_ids.len();

    for card_id in hand_ids {
        zones::move_to_zone(state, card_id, Zone::Exile, events);
    }

    // CR 103.5b + Serum Powder Oracle text: "draw that many cards" — draw
    // exactly the number we just exiled, regardless of the configured
    // starting hand size. (In practice these are equal because the player is
    // in the mulligan-decision phase with a full hand, but the rule is
    // phrased as "that many" so we honor it literally.)
    draw_n(state, player, exiled_count, events);

    Ok(())
}

/// CR 103.5: Stash the locked-in mulligan count for `player` so the bottoms
/// phase knows how many cards they owe.
fn record_final_count(state: &mut GameState, player: PlayerId, count: u8) {
    state.final_mulligan_counts.insert(player, count);
}

/// CR 103.5: After updating `pending`, either re-emit `MulliganDecision` or
/// transition to the bottom-cards phase (or finish entirely).
fn advance_after_decision(
    state: &mut GameState,
    pending: Vec<MulliganDecisionEntry>,
    free_first: bool,
    events: &mut Vec<GameEvent>,
) -> WaitingFor {
    if !pending.is_empty() {
        return WaitingFor::MulliganDecision {
            pending,
            free_first_mulligan: free_first,
        };
    }

    // All players have locked in their hands. Build the bottoms-phase pending
    // list from each player's final mulligan count.
    enter_bottom_phase(state, events)
}

/// CR 103.5: Enter the bottoms phase. Each player who took at least one
/// counted mulligan (after free-first discount) must put N cards on the
/// bottom of their library. Players choose simultaneously.
fn enter_bottom_phase(state: &mut GameState, events: &mut Vec<GameEvent>) -> WaitingFor {
    let free_first = free_first_mulligan(state);
    let pending: Vec<MulliganBottomEntry> = state
        .seat_order
        .iter()
        .filter(|&&player_id| super::players::is_alive(state, player_id))
        .filter_map(|&player_id| {
            // CR 800.4a: A player who conceded during the decision phase is
            // skipped — they cannot submit bottoms and would deadlock the game.
            let count = state
                .final_mulligan_counts
                .get(&player_id)
                .copied()
                .unwrap_or(0);
            let prepaid = state
                .prepaid_mulligan_bottoms
                .get(&player_id)
                .copied()
                .unwrap_or(0);
            let bottom = bottom_count_for(count, free_first).saturating_sub(prepaid);
            if bottom > 0 {
                Some(MulliganBottomEntry {
                    player: player_id,
                    count: bottom,
                })
            } else {
                None
            }
        })
        .collect();

    if pending.is_empty() {
        state.final_mulligan_counts.clear();
        state.prepaid_mulligan_bottoms.clear();
        finish_mulligans(state, events)
    } else {
        WaitingFor::MulliganBottomCards { pending }
    }
}

/// TL:R 906.6a/e: Resolve a forced opening-hand bottom before any normal
/// mulligan decisions or Serum Powder-style actions are available.
pub fn handle_opening_hand_bottom(
    state: &mut GameState,
    player: PlayerId,
    cards: Vec<ObjectId>,
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, String> {
    let WaitingFor::OpeningHandBottomCards { pending, .. } = &state.waiting_for else {
        return Err("handle_opening_hand_bottom called outside OpeningHandBottomCards".to_string());
    };
    let mut pending = pending.clone();

    let idx = pending
        .iter()
        .position(|e| e.player == player)
        .ok_or_else(|| {
            format!(
                "Player {:?} is not in the opening-bottom pending set",
                player
            )
        })?;
    let expected_count = pending[idx].count;

    validate_bottom_selection(state, player, &cards, expected_count)?;
    for card_id in cards {
        zones::move_to_library_position(state, card_id, false, events);
    }

    *state.prepaid_mulligan_bottoms.entry(player).or_insert(0) += expected_count;
    pending.remove(idx);

    if pending.is_empty() {
        Ok(normal_mulligan_decision(state))
    } else {
        Ok(WaitingFor::OpeningHandBottomCards {
            pending,
            reason: OpeningHandBottomReason::TinyLeadersMultiCommander,
        })
    }
}

/// CR 103.5: Resolve one player's `SelectCards { cards }` during the bottoms
/// phase. Validates the count and contents, moves cards to the bottom of the
/// library, removes the player from `pending`. When `pending` is empty,
/// advance to `finish_mulligans`.
pub fn handle_mulligan_bottom(
    state: &mut GameState,
    player: PlayerId,
    cards: Vec<ObjectId>,
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, String> {
    let WaitingFor::MulliganBottomCards { pending } = &state.waiting_for else {
        return Err("handle_mulligan_bottom called outside MulliganBottomCards".to_string());
    };
    let mut pending = pending.clone();

    let idx = pending
        .iter()
        .position(|e| e.player == player)
        .ok_or_else(|| format!("Player {:?} is not in the bottoms pending set", player))?;
    let expected_count = pending[idx].count;

    validate_bottom_selection(state, player, &cards, expected_count)?;

    for card_id in cards {
        zones::move_to_library_position(state, card_id, false, events);
    }

    pending.remove(idx);

    if pending.is_empty() {
        state.final_mulligan_counts.clear();
        state.prepaid_mulligan_bottoms.clear();
        Ok(finish_mulligans(state, events))
    } else {
        Ok(WaitingFor::MulliganBottomCards { pending })
    }
}

fn validate_bottom_selection(
    state: &GameState,
    player: PlayerId,
    cards: &[ObjectId],
    expected_count: u8,
) -> Result<(), String> {
    if cards.len() != expected_count as usize {
        return Err(format!(
            "Expected {} cards to bottom, got {}",
            expected_count,
            cards.len()
        ));
    }

    let player_data = state
        .players
        .iter()
        .find(|p| p.id == player)
        .expect("player exists");
    for &card_id in cards {
        if !player_data.hand.contains(&card_id) {
            return Err(format!("Card {:?} is not in player's hand", card_id));
        }
    }
    Ok(())
}

/// Queue all BeginGame abilities for cards in each player's opening hand.
fn queue_begin_game_abilities(state: &mut GameState) {
    let mut begin_game: Vec<PendingBeginGameAbility> = state
        .seat_order
        .clone()
        .into_iter()
        .flat_map(|player_id| {
            let player = state
                .players
                .iter()
                .find(|p| p.id == player_id)
                .expect("player exists");
            player
                .hand
                .iter()
                .filter_map(|&obj_id| {
                    let obj = state.objects.get(&obj_id)?;
                    let ability = obj
                        .abilities
                        .iter()
                        .find(|a| a.kind == AbilityKind::BeginGame)?;
                    Some(PendingBeginGameAbility {
                        ability: build_resolved_from_def(ability, obj_id, player_id),
                    })
                })
                .collect::<Vec<_>>()
        })
        .collect();

    begin_game.reverse();
    state.pending_begin_game_abilities = begin_game;
}

/// CR 103.6: Drain beginning-of-game abilities after mulligans, prompting for
/// optional abilities before the first turn receives priority.
pub fn resume_begin_game_abilities(
    state: &mut GameState,
    events: &mut Vec<GameEvent>,
) -> WaitingFor {
    while let Some(pending) = state.pending_begin_game_abilities.pop() {
        let _ = resolve_ability_chain(state, &pending.ability, events, 0);
        if !matches!(state.waiting_for, WaitingFor::Priority { .. }) {
            return state.waiting_for.clone();
        }
    }

    state.resolving_begin_game_abilities = false;
    turns::auto_advance(state, events)
}

/// CR 103.5 + CR 800.4a: Re-entry point for elimination cleanup — drives the
/// flow to the bottoms phase as if the decision phase had ended naturally.
pub(crate) fn enter_bottom_phase_public(
    state: &mut GameState,
    events: &mut Vec<GameEvent>,
) -> WaitingFor {
    enter_bottom_phase(state, events)
}

/// TL:R 906.6a: Re-entry point after pruning an opening-hand bottom prompt.
pub(crate) fn enter_normal_mulligan_public(state: &GameState) -> WaitingFor {
    normal_mulligan_decision(state)
}

/// CR 103.5 + CR 800.4a: Re-entry point for elimination cleanup — drives the
/// flow to game start as if all bottoms had been submitted.
pub(crate) fn finish_mulligans_public(
    state: &mut GameState,
    events: &mut Vec<GameEvent>,
) -> WaitingFor {
    finish_mulligans(state, events)
}

/// All players have kept. Start the game properly.
fn finish_mulligans(state: &mut GameState, events: &mut Vec<GameEvent>) -> WaitingFor {
    queue_begin_game_abilities(state);
    state.resolving_begin_game_abilities = true;
    resume_begin_game_abilities(state, events)
}

fn shuffle_hand_into_library(state: &mut GameState, player: PlayerId, events: &mut Vec<GameEvent>) {
    let hand_ids: Vec<ObjectId> = state
        .players
        .iter()
        .find(|p| p.id == player)
        .expect("player exists")
        .hand
        .iter()
        .copied()
        .collect();

    for card_id in hand_ids {
        zones::move_to_zone(state, card_id, Zone::Library, events);
    }

    // Shuffle library
    let GameState { players, rng, .. } = state;
    let player_data = players
        .iter_mut()
        .find(|p| p.id == player)
        .expect("player exists");
    crate::util::im_ext::shuffle_vector(&mut player_data.library, rng);
}

fn draw_n(state: &mut GameState, player_id: PlayerId, count: usize, events: &mut Vec<GameEvent>) {
    for _ in 0..count {
        let player = state
            .players
            .iter()
            .find(|p| p.id == player_id)
            .expect("player exists");

        if player.library.is_empty() {
            break;
        }

        let top_card = player.library[0];
        zones::move_to_zone(state, top_card, Zone::Hand, events);
    }

    events.push(GameEvent::CardsDrawn {
        player_id,
        count: count as u32,
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::deck_loading::DeckEntry;
    use crate::game::zones::create_object;
    use crate::types::ability::{AbilityDefinition, Effect, TargetFilter};
    use crate::types::actions::GameAction;
    use crate::types::card::CardFace;
    use crate::types::format::FormatConfig;
    use crate::types::game_state::PlayerDeckPool;
    use crate::types::identifiers::CardId;

    /// Test helper: decide for `player`, advancing `state.waiting_for` in place.
    /// Mirrors the engine dispatch contract: callers must update `state.waiting_for`
    /// from the returned WaitingFor before the next call.
    fn decide(
        state: &mut GameState,
        player: PlayerId,
        keep: bool,
        events: &mut Vec<GameEvent>,
    ) -> WaitingFor {
        let choice = if keep {
            MulliganChoice::Keep
        } else {
            MulliganChoice::Mulligan
        };
        let wf = handle_mulligan_decision(state, player, choice, events)
            .expect("handle_mulligan_decision");
        state.waiting_for = wf.clone();
        wf
    }

    /// Test helper: submit a Serum Powder action on behalf of `player`.
    fn use_serum_powder(
        state: &mut GameState,
        player: PlayerId,
        object_id: ObjectId,
        events: &mut Vec<GameEvent>,
    ) -> Result<WaitingFor, String> {
        let wf = handle_mulligan_decision(
            state,
            player,
            MulliganChoice::UseSerumPowder { object_id },
            events,
        )?;
        state.waiting_for = wf.clone();
        Ok(wf)
    }

    fn bottom(
        state: &mut GameState,
        player: PlayerId,
        cards: Vec<ObjectId>,
        events: &mut Vec<GameEvent>,
    ) -> Result<WaitingFor, String> {
        let wf = handle_mulligan_bottom(state, player, cards, events)?;
        state.waiting_for = wf.clone();
        Ok(wf)
    }

    fn opening_bottom(
        state: &mut GameState,
        player: PlayerId,
        cards: Vec<ObjectId>,
        events: &mut Vec<GameEvent>,
    ) -> Result<WaitingFor, String> {
        let wf = handle_opening_hand_bottom(state, player, cards, events)?;
        state.waiting_for = wf.clone();
        Ok(wf)
    }

    fn setup_with_libraries(cards_per_player: usize) -> GameState {
        setup_n_player_with_libraries(2, cards_per_player)
    }

    fn setup_n_player_with_libraries(num_players: u8, cards_per_player: usize) -> GameState {
        let mut state = if num_players == 2 {
            GameState::new_two_player(42)
        } else {
            GameState::new(
                crate::types::format::FormatConfig::standard(),
                num_players,
                42,
            )
        };
        state.turn_number = 1;
        state.phase = crate::types::phase::Phase::Untap;

        for player_idx in 0..num_players {
            for i in 0..cards_per_player {
                create_object(
                    &mut state,
                    CardId((player_idx as u64) * 100 + i as u64),
                    PlayerId(player_idx),
                    format!("Card {} P{}", i, player_idx),
                    Zone::Library,
                );
            }
        }

        state
    }

    fn deck_entry(name: &str, count: u32) -> DeckEntry {
        DeckEntry {
            card: CardFace {
                name: name.to_string(),
                ..Default::default()
            },
            count,
        }
    }

    fn pending_decision_players(wf: &WaitingFor) -> Vec<PlayerId> {
        match wf {
            WaitingFor::MulliganDecision { pending, .. } => {
                pending.iter().map(|e| e.player).collect()
            }
            _ => vec![],
        }
    }

    fn decision_count_for(wf: &WaitingFor, player: PlayerId) -> Option<u8> {
        match wf {
            WaitingFor::MulliganDecision { pending, .. } => pending
                .iter()
                .find(|e| e.player == player)
                .map(|e| e.mulligan_count),
            _ => None,
        }
    }

    fn pending_bottom_for(wf: &WaitingFor, player: PlayerId) -> Option<u8> {
        match wf {
            WaitingFor::MulliganBottomCards { pending } => {
                pending.iter().find(|e| e.player == player).map(|e| e.count)
            }
            _ => None,
        }
    }

    #[test]
    fn start_mulligan_draws_seven_for_each_player() {
        let mut state = setup_with_libraries(20);
        let mut events = Vec::new();

        let waiting = start_mulligan(&mut state, &mut events);

        assert_eq!(state.players[0].hand.len(), 7);
        assert_eq!(state.players[1].hand.len(), 7);
        assert_eq!(state.players[0].library.len(), 13);
        assert_eq!(state.players[1].library.len(), 13);
        assert_eq!(
            pending_decision_players(&waiting),
            vec![PlayerId(0), PlayerId(1)],
            "both players should be pending at game start"
        );
    }

    #[test]
    fn start_mulligan_emits_event() {
        let mut state = setup_with_libraries(20);
        let mut events = Vec::new();

        start_mulligan(&mut state, &mut events);

        assert!(events
            .iter()
            .any(|e| matches!(e, GameEvent::MulliganStarted)));
    }

    #[test]
    fn tiny_leaders_multi_commander_bottoms_before_normal_mulligan() {
        let mut state = GameState::new(FormatConfig::tiny_leaders(), 2, 42);
        state.turn_number = 1;
        state.phase = crate::types::phase::Phase::Untap;
        for player_idx in 0..2u8 {
            for i in 0..20 {
                create_object(
                    &mut state,
                    CardId((player_idx as u64) * 100 + i as u64),
                    PlayerId(player_idx),
                    format!("Card {} P{}", i, player_idx),
                    Zone::Library,
                );
            }
        }
        state.deck_pools = vec![
            PlayerDeckPool {
                player: PlayerId(0),
                current_commander: std::sync::Arc::new(vec![
                    deck_entry("Tiny Leader A", 1),
                    deck_entry("Tiny Leader B", 1),
                ]),
                ..Default::default()
            },
            PlayerDeckPool {
                player: PlayerId(1),
                current_commander: std::sync::Arc::new(vec![deck_entry("Tiny Leader C", 1)]),
                ..Default::default()
            },
        ];
        let mut events = Vec::new();

        let waiting = start_mulligan(&mut state, &mut events);

        assert!(matches!(
            waiting,
            WaitingFor::OpeningHandBottomCards { ref pending, .. }
                if pending == &vec![MulliganBottomEntry { player: PlayerId(0), count: 1 }]
        ));
    }

    #[test]
    fn tiny_leaders_opening_bottom_counts_as_first_mulligan_bottom() {
        let mut state = GameState::new(FormatConfig::tiny_leaders(), 2, 42);
        state.turn_number = 1;
        state.phase = crate::types::phase::Phase::Untap;
        for player_idx in 0..2u8 {
            for i in 0..20 {
                create_object(
                    &mut state,
                    CardId((player_idx as u64) * 100 + i as u64),
                    PlayerId(player_idx),
                    format!("Card {} P{}", i, player_idx),
                    Zone::Library,
                );
            }
        }
        state.deck_pools = vec![
            PlayerDeckPool {
                player: PlayerId(0),
                current_commander: std::sync::Arc::new(vec![
                    deck_entry("Tiny Leader A", 1),
                    deck_entry("Tiny Leader B", 1),
                ]),
                ..Default::default()
            },
            PlayerDeckPool {
                player: PlayerId(1),
                current_commander: std::sync::Arc::new(vec![deck_entry("Tiny Leader C", 1)]),
                ..Default::default()
            },
        ];
        let mut events = Vec::new();
        state.waiting_for = start_mulligan(&mut state, &mut events);
        let bottomed = state.players[0].hand[0];

        let waiting = opening_bottom(&mut state, PlayerId(0), vec![bottomed], &mut events)
            .expect("opening bottom");

        assert_eq!(state.prepaid_mulligan_bottoms.get(&PlayerId(0)), Some(&1));
        assert_eq!(
            decision_count_for(&waiting, PlayerId(0)),
            Some(1),
            "forced opening bottom starts normal mulligans at one mulligan taken"
        );

        decide(&mut state, PlayerId(0), true, &mut events);
        let waiting = decide(&mut state, PlayerId(1), true, &mut events);
        assert!(
            matches!(waiting, WaitingFor::Priority { .. }),
            "keeping after the forced bottom should not owe another bottom, got {:?}",
            waiting
        );
    }

    #[test]
    fn keep_removes_player_from_pending() {
        let mut state = setup_with_libraries(20);
        let mut events = Vec::new();
        state.waiting_for = start_mulligan(&mut state, &mut events);

        let waiting = decide(&mut state, PlayerId(0), true, &mut events);
        assert_eq!(
            pending_decision_players(&waiting),
            vec![PlayerId(1)],
            "P0 should be removed; P1 still pending"
        );
    }

    #[test]
    fn mulligan_keeps_player_in_pending_and_increments_count() {
        let mut state = setup_with_libraries(20);
        let mut events = Vec::new();
        state.waiting_for = start_mulligan(&mut state, &mut events);

        let waiting = decide(&mut state, PlayerId(0), false, &mut events);
        assert_eq!(
            decision_count_for(&waiting, PlayerId(0)),
            Some(1),
            "P0 mulligan_count should increment to 1"
        );
        assert!(
            pending_decision_players(&waiting).contains(&PlayerId(0)),
            "P0 should remain pending after mulligan"
        );
    }

    #[test]
    fn keep_after_mulligan_defers_bottoms_until_all_keep() {
        let mut state = setup_with_libraries(20);
        let mut events = Vec::new();
        state.waiting_for = start_mulligan(&mut state, &mut events);

        // P0 mulligans once then keeps; P1 still pending → still decision phase.
        decide(&mut state, PlayerId(0), false, &mut events);
        let waiting = decide(&mut state, PlayerId(0), true, &mut events);
        assert!(
            matches!(waiting, WaitingFor::MulliganDecision { .. }),
            "should still be decision phase while P1 is pending, got {:?}",
            waiting
        );

        // P1 keeps → enters bottoms phase for P0 only.
        let waiting = decide(&mut state, PlayerId(1), true, &mut events);
        assert_eq!(
            pending_bottom_for(&waiting, PlayerId(0)),
            Some(1),
            "P0 owes 1 bottom card after 1 mulligan in 2-player Standard"
        );
        assert_eq!(
            pending_bottom_for(&waiting, PlayerId(1)),
            None,
            "P1 owes nothing"
        );
    }

    #[test]
    fn mulligan_redraws_seven() {
        let mut state = setup_with_libraries(20);
        let mut events = Vec::new();
        state.waiting_for = start_mulligan(&mut state, &mut events);

        assert_eq!(state.players[0].hand.len(), 7);

        decide(&mut state, PlayerId(0), false, &mut events);

        assert_eq!(state.players[0].hand.len(), 7);
    }

    #[test]
    fn handle_bottom_cards_puts_on_bottom() {
        let mut state = setup_with_libraries(20);
        let mut events = Vec::new();
        state.waiting_for = start_mulligan(&mut state, &mut events);

        // P0 mulligans then keeps; P1 keeps → enter bottoms phase.
        decide(&mut state, PlayerId(0), false, &mut events);
        decide(&mut state, PlayerId(0), true, &mut events);
        decide(&mut state, PlayerId(1), true, &mut events);

        let card_to_bottom = state.players[0].hand[0];
        let result = bottom(&mut state, PlayerId(0), vec![card_to_bottom], &mut events);
        assert!(result.is_ok());
        assert_eq!(state.players[0].hand.len(), 6);
        assert_eq!(*state.players[0].library.back().unwrap(), card_to_bottom);
    }

    #[test]
    fn handle_bottom_cards_wrong_count_errors() {
        let mut state = setup_with_libraries(20);
        let mut events = Vec::new();
        state.waiting_for = start_mulligan(&mut state, &mut events);

        // Drive into bottoms phase: P0 mulligans+keeps, P1 keeps.
        decide(&mut state, PlayerId(0), false, &mut events);
        decide(&mut state, PlayerId(0), true, &mut events);
        decide(&mut state, PlayerId(1), true, &mut events);

        let result = handle_mulligan_bottom(&mut state, PlayerId(0), vec![], &mut events);
        assert!(result.is_err());
    }

    #[test]
    fn both_players_keep_starts_game() {
        let mut state = setup_with_libraries(20);
        let mut events = Vec::new();
        state.waiting_for = start_mulligan(&mut state, &mut events);

        let waiting = decide(&mut state, PlayerId(0), true, &mut events);
        assert!(matches!(waiting, WaitingFor::MulliganDecision { .. }));

        let waiting = decide(&mut state, PlayerId(1), true, &mut events);
        assert!(matches!(waiting, WaitingFor::Priority { .. }));
    }

    /// CR 103.5: 4-player pod, every player submits in non-turn order; all keep.
    /// All four mulligan decisions complete simultaneously and the game starts.
    #[test]
    fn four_player_concurrent_keep_in_any_order() {
        let mut state = setup_n_player_with_libraries(4, 20);
        let mut events = Vec::new();
        state.waiting_for = start_mulligan(&mut state, &mut events);

        // Submit in reverse seat order.
        let _ = decide(&mut state, PlayerId(3), true, &mut events);
        let _ = decide(&mut state, PlayerId(0), true, &mut events);
        let _ = decide(&mut state, PlayerId(2), true, &mut events);
        let waiting = decide(&mut state, PlayerId(1), true, &mut events);

        assert!(
            matches!(waiting, WaitingFor::Priority { .. }),
            "all four players kept → game should start, got {:?}",
            waiting
        );
    }

    /// CR 103.5: 4-player pod, partial — two keep, two mulligan.
    /// Pending shrinks to the mulliganing players only.
    #[test]
    fn four_player_partial_keep_pending_shrinks() {
        let mut state = setup_n_player_with_libraries(4, 20);
        let mut events = Vec::new();
        state.waiting_for = start_mulligan(&mut state, &mut events);

        decide(&mut state, PlayerId(0), true, &mut events);
        decide(&mut state, PlayerId(1), true, &mut events);
        decide(&mut state, PlayerId(2), false, &mut events);
        let waiting = decide(&mut state, PlayerId(3), false, &mut events);

        let pending = pending_decision_players(&waiting);
        assert_eq!(
            pending,
            vec![PlayerId(2), PlayerId(3)],
            "only mulliganing players should remain pending"
        );
        assert_eq!(decision_count_for(&waiting, PlayerId(2)), Some(1));
        assert_eq!(decision_count_for(&waiting, PlayerId(3)), Some(1));
    }

    /// CR 103.5: 4-player pod bottoms phase — three players owe bottoms,
    /// they submit in non-seat order, all resolve concurrently.
    #[test]
    fn four_player_concurrent_bottom_in_any_order() {
        // Need a 4-player game without free-first-mulligan so all three mulligans
        // produce bottoms. Multiplayer (≥3 seats) always grants free first per
        // CR 103.5c, so a single mulligan is free. Take TWO mulligans per player
        // to ensure each owes one bottom card.
        let mut state = setup_n_player_with_libraries(4, 30);
        let mut events = Vec::new();
        state.waiting_for = start_mulligan(&mut state, &mut events);

        // P0, P2, P3 each mulligan twice then keep; P1 keeps immediately.
        for &pid in &[PlayerId(0), PlayerId(2), PlayerId(3)] {
            decide(&mut state, pid, false, &mut events);
            decide(&mut state, pid, false, &mut events);
            decide(&mut state, pid, true, &mut events);
        }
        let waiting = decide(&mut state, PlayerId(1), true, &mut events);

        // Bottoms phase: P0/P2/P3 each owe 1 (2 mulligans - 1 free).
        assert_eq!(pending_bottom_for(&waiting, PlayerId(0)), Some(1));
        assert_eq!(pending_bottom_for(&waiting, PlayerId(2)), Some(1));
        assert_eq!(pending_bottom_for(&waiting, PlayerId(3)), Some(1));
        assert_eq!(pending_bottom_for(&waiting, PlayerId(1)), None);

        // Submit bottom cards in non-seat order.
        let card3 = state.players[3].hand[0];
        let card0 = state.players[0].hand[0];
        let card2 = state.players[2].hand[0];
        bottom(&mut state, PlayerId(3), vec![card3], &mut events).unwrap();
        bottom(&mut state, PlayerId(0), vec![card0], &mut events).unwrap();
        let waiting = bottom(&mut state, PlayerId(2), vec![card2], &mut events).unwrap();

        assert!(
            matches!(waiting, WaitingFor::Priority { .. }),
            "all bottoms submitted → game should start, got {:?}",
            waiting
        );
    }

    #[test]
    fn optional_begin_game_ability_prompts_before_resolving() {
        let mut state = setup_with_libraries(20);
        let mut events = Vec::new();
        state.waiting_for = start_mulligan(&mut state, &mut events);

        let leyline_id = state.players[0].hand[0];
        let mut begin_game = AbilityDefinition::new(
            AbilityKind::BeginGame,
            Effect::ChangeZone {
                destination: Zone::Battlefield,
                target: TargetFilter::SelfRef,
                origin: Some(Zone::Hand),
                owner_library: false,
                enter_transformed: false,
                enters_under: None,
                enter_tapped: false,
                enters_attacking: false,
                up_to: false,
                enter_with_counters: vec![],
                face_down_profile: None,
            },
        )
        .description("If this card is in your opening hand, you may begin the game with it on the battlefield.".to_string());
        begin_game.optional = true;
        let abilities = &mut state
            .objects
            .get_mut(&leyline_id)
            .expect("opening hand card exists")
            .abilities;
        std::sync::Arc::make_mut(abilities).push(begin_game);

        decide(&mut state, PlayerId(0), true, &mut events);
        decide(&mut state, PlayerId(1), true, &mut events);

        assert!(matches!(
            state.waiting_for,
            WaitingFor::OptionalEffectChoice {
                player: PlayerId(0),
                source_id,
                ..
            } if source_id == leyline_id
        ));
        assert_eq!(state.objects[&leyline_id].zone, Zone::Hand);

        let result = crate::game::engine::apply(
            &mut state,
            PlayerId(0),
            GameAction::DecideOptionalEffect { accept: true },
        )
        .expect("accepting begin-game effect should resolve");

        assert_eq!(state.objects[&leyline_id].zone, Zone::Battlefield);
        assert!(matches!(result.waiting_for, WaitingFor::Priority { .. }));
        assert!(!state.resolving_begin_game_abilities);
    }

    #[test]
    fn multiplayer_first_mulligan_is_free() {
        let mut state = setup_n_player_with_libraries(3, 30);
        let mut events = Vec::new();
        state.waiting_for = start_mulligan(&mut state, &mut events);

        // CR 103.5c: First mulligan in multiplayer doesn't count.
        let waiting = decide(&mut state, PlayerId(0), false, &mut events);
        assert_eq!(
            decision_count_for(&waiting, PlayerId(0)),
            Some(1),
            "Mulligan count should increment to 1"
        );

        // Keep after first mulligan — drive into bottoms phase by keeping others too.
        decide(&mut state, PlayerId(0), true, &mut events);
        decide(&mut state, PlayerId(1), true, &mut events);
        let waiting = decide(&mut state, PlayerId(2), true, &mut events);

        assert_eq!(
            pending_bottom_for(&waiting, PlayerId(0)),
            None,
            "P0 had 1 free mulligan → owes 0 bottom cards"
        );
        assert!(
            matches!(waiting, WaitingFor::Priority { .. }),
            "with no bottoms owed, game should start immediately"
        );
    }

    #[test]
    fn multiplayer_two_mulligans_bottoms_one() {
        let mut state = setup_n_player_with_libraries(3, 30);
        let mut events = Vec::new();
        state.waiting_for = start_mulligan(&mut state, &mut events);

        decide(&mut state, PlayerId(0), false, &mut events);
        decide(&mut state, PlayerId(0), false, &mut events);
        decide(&mut state, PlayerId(0), true, &mut events);
        decide(&mut state, PlayerId(1), true, &mut events);
        let waiting = decide(&mut state, PlayerId(2), true, &mut events);
        assert_eq!(
            pending_bottom_for(&waiting, PlayerId(0)),
            Some(1),
            "After 2 mulligans in multiplayer, P0 should bottom 1 card"
        );
    }

    #[test]
    fn ai_starting_player_can_submit_mulligan_decision() {
        use crate::game::engine::{apply, start_game_with_starting_player};
        use crate::types::actions::GameAction;
        use crate::types::format::FormatConfig;

        let mut state = GameState::new(FormatConfig::commander(), 2, 42);
        for player_idx in 0..2u8 {
            for i in 0..10 {
                create_object(
                    &mut state,
                    CardId((player_idx as u64) * 100 + i as u64),
                    PlayerId(player_idx),
                    format!("Card {} P{}", i, player_idx),
                    Zone::Library,
                );
            }
        }
        let c0 = create_object(
            &mut state,
            CardId(200),
            PlayerId(0),
            "P0 Cmd".to_string(),
            Zone::Command,
        );
        let c1 = create_object(
            &mut state,
            CardId(201),
            PlayerId(1),
            "P1 Cmd".to_string(),
            Zone::Command,
        );
        state.objects.get_mut(&c0).unwrap().is_commander = true;
        state.objects.get_mut(&c1).unwrap().is_commander = true;

        let result = start_game_with_starting_player(&mut state, PlayerId(1));

        // CR 103.5: Both players are pending simultaneously at start.
        assert!(
            matches!(result.waiting_for, WaitingFor::MulliganDecision { .. }),
            "expected MulliganDecision, got {:?}",
            result.waiting_for
        );
        let pending = pending_decision_players(&result.waiting_for);
        assert!(
            pending.contains(&PlayerId(0)) && pending.contains(&PlayerId(1)),
            "both players should be pending, got {:?}",
            pending
        );

        // P1 (AI) is authorized as a member of the pending set.
        assert!(crate::game::turn_control::is_authorized_submitter(
            &state,
            PlayerId(1)
        ));

        let r = apply(
            &mut state,
            PlayerId(1),
            GameAction::MulliganDecision {
                choice: MulliganChoice::Keep,
            },
        );
        assert!(
            r.is_ok(),
            "AI P1 should be authorized to submit MulliganDecision, got {:?}",
            r
        );
    }

    /// Commander Rules Committee free-mulligan rule supplements CR 103.5c
    /// (which covers only multiplayer and Brawl). A 2-player Commander
    /// duel grants a free first mulligan.
    #[test]
    fn commander_first_mulligan_is_free_in_duel() {
        use crate::types::format::FormatConfig;

        let mut state = GameState::new(FormatConfig::commander(), 2, 42);
        state.turn_number = 1;
        state.phase = crate::types::phase::Phase::Untap;
        for player_idx in 0..2u8 {
            for i in 0..20 {
                create_object(
                    &mut state,
                    CardId((player_idx as u64) * 100 + i as u64),
                    PlayerId(player_idx),
                    format!("Card {} P{}", i, player_idx),
                    Zone::Library,
                );
            }
        }

        let mut events = Vec::new();
        state.waiting_for = start_mulligan(&mut state, &mut events);

        decide(&mut state, PlayerId(0), false, &mut events);
        decide(&mut state, PlayerId(0), true, &mut events);
        let waiting = decide(&mut state, PlayerId(1), true, &mut events);
        assert_eq!(
            pending_bottom_for(&waiting, PlayerId(0)),
            None,
            "Commander duel: first mulligan should be free — no MulliganBottomCards"
        );
    }

    /// CR 103.5c: A Brawl duel grants a free first mulligan.
    #[test]
    fn brawl_first_mulligan_is_free_in_duel() {
        use crate::types::format::FormatConfig;

        let mut state = GameState::new(FormatConfig::brawl(), 2, 42);
        state.turn_number = 1;
        state.phase = crate::types::phase::Phase::Untap;
        for player_idx in 0..2u8 {
            for i in 0..20 {
                create_object(
                    &mut state,
                    CardId((player_idx as u64) * 100 + i as u64),
                    PlayerId(player_idx),
                    format!("Card {} P{}", i, player_idx),
                    Zone::Library,
                );
            }
        }

        let mut events = Vec::new();
        state.waiting_for = start_mulligan(&mut state, &mut events);

        decide(&mut state, PlayerId(0), false, &mut events);
        decide(&mut state, PlayerId(0), true, &mut events);
        let waiting = decide(&mut state, PlayerId(1), true, &mut events);
        assert_eq!(pending_bottom_for(&waiting, PlayerId(0)), None);
    }

    /// CR 103.5c only applies to multiplayer (3+ players) and Brawl. A
    /// Standard 1v1 duel must require bottoming 1 card after 1 mulligan.
    #[test]
    fn standard_duel_has_no_free_mulligan() {
        use crate::types::format::FormatConfig;

        let mut state = GameState::new(FormatConfig::standard(), 2, 42);
        state.turn_number = 1;
        state.phase = crate::types::phase::Phase::Untap;
        for player_idx in 0..2u8 {
            for i in 0..20 {
                create_object(
                    &mut state,
                    CardId((player_idx as u64) * 100 + i as u64),
                    PlayerId(player_idx),
                    format!("Card {} P{}", i, player_idx),
                    Zone::Library,
                );
            }
        }

        let mut events = Vec::new();
        state.waiting_for = start_mulligan(&mut state, &mut events);

        decide(&mut state, PlayerId(0), false, &mut events);
        decide(&mut state, PlayerId(0), true, &mut events);
        let waiting = decide(&mut state, PlayerId(1), true, &mut events);
        assert_eq!(
            pending_bottom_for(&waiting, PlayerId(0)),
            Some(1),
            "Standard duel: after 1 mulligan, should bottom 1 card"
        );
    }

    /// Inject a Serum Powder into `player`'s hand and return its object id.
    /// Replaces the object at `hand[slot]` so the hand size stays at 7.
    fn inject_serum_powder(state: &mut GameState, player: PlayerId, slot: usize) -> ObjectId {
        let object_id = state.players.iter().find(|p| p.id == player).unwrap().hand[slot];
        state
            .objects
            .get_mut(&object_id)
            .expect("hand object exists")
            .name = SERUM_POWDER_NAME.to_string();
        object_id
    }

    /// CR 103.5b + Serum Powder Oracle text: using Serum Powder exiles every
    /// card in hand (including the Powder) and redraws the same number.
    /// Mulligan count is unchanged; player stays pending.
    #[test]
    fn serum_powder_exiles_entire_hand_and_redraws() {
        let mut state = setup_with_libraries(20);
        let mut events = Vec::new();
        state.waiting_for = start_mulligan(&mut state, &mut events);

        let powder_id = inject_serum_powder(&mut state, PlayerId(0), 0);
        let original_hand: Vec<ObjectId> = state.players[0].hand.iter().copied().collect();
        let original_hand_size = original_hand.len();
        let library_before = state.players[0].library.len();

        let waiting = use_serum_powder(&mut state, PlayerId(0), powder_id, &mut events)
            .expect("use_serum_powder");

        // Every original hand card — including the Powder — is now in exile.
        for card_id in &original_hand {
            assert_eq!(
                state.objects[card_id].zone,
                Zone::Exile,
                "card {:?} should be exiled",
                card_id
            );
        }
        // The Powder itself is in exile (not back in hand or library).
        assert_eq!(state.objects[&powder_id].zone, Zone::Exile);

        // Hand was refilled to the same size from the top of the library.
        assert_eq!(state.players[0].hand.len(), original_hand_size);
        assert_eq!(
            state.players[0].library.len(),
            library_before - original_hand_size
        );

        // None of the newly drawn cards are from the exiled set.
        for new_card in state.players[0].hand.iter() {
            assert!(
                !original_hand.contains(new_card),
                "new hand should not contain exiled card {:?}",
                new_card
            );
        }

        // Mulligan count unchanged (still 0); player still pending.
        assert_eq!(decision_count_for(&waiting, PlayerId(0)), Some(0));
        assert!(pending_decision_players(&waiting).contains(&PlayerId(0)));
    }

    /// CR 103.5b: After using Serum Powder the player may immediately keep.
    /// They owe no bottom cards (mulligan count never incremented).
    #[test]
    fn serum_powder_then_keep_owes_no_bottoms() {
        let mut state = setup_with_libraries(30);
        let mut events = Vec::new();
        state.waiting_for = start_mulligan(&mut state, &mut events);

        let powder_id = inject_serum_powder(&mut state, PlayerId(0), 0);
        use_serum_powder(&mut state, PlayerId(0), powder_id, &mut events)
            .expect("use_serum_powder");

        // P0 keeps the refreshed hand; P1 keeps. Bottoms phase should be skipped.
        decide(&mut state, PlayerId(0), true, &mut events);
        let waiting = decide(&mut state, PlayerId(1), true, &mut events);
        assert!(
            matches!(waiting, WaitingFor::Priority { .. }),
            "Serum Powder is not a mulligan; P0 should owe 0 bottoms — game should start, got {:?}",
            waiting
        );
    }

    /// CR 103.5b: Attempting `UseSerumPowder` on an object whose name is not
    /// "Serum Powder" is rejected.
    #[test]
    fn serum_powder_rejects_non_powder_object() {
        let mut state = setup_with_libraries(20);
        let mut events = Vec::new();
        state.waiting_for = start_mulligan(&mut state, &mut events);

        // Pick a hand object that is NOT a Powder.
        let non_powder = state.players[0].hand[0];

        let result = handle_mulligan_decision(
            &mut state,
            PlayerId(0),
            MulliganChoice::UseSerumPowder {
                object_id: non_powder,
            },
            &mut events,
        );
        assert!(
            result.is_err(),
            "non-Powder object must be rejected, got {:?}",
            result
        );
    }

    /// CR 103.5b: Attempting `UseSerumPowder` on an object that is in another
    /// player's hand (or not in any hand) is rejected.
    #[test]
    fn serum_powder_rejects_object_not_in_actor_hand() {
        let mut state = setup_with_libraries(20);
        let mut events = Vec::new();
        state.waiting_for = start_mulligan(&mut state, &mut events);

        // Inject a Powder into P1's hand, but try to use it from P0.
        let p1_powder = inject_serum_powder(&mut state, PlayerId(1), 0);

        let result = handle_mulligan_decision(
            &mut state,
            PlayerId(0),
            MulliganChoice::UseSerumPowder {
                object_id: p1_powder,
            },
            &mut events,
        );
        assert!(
            result.is_err(),
            "Powder not in actor's hand must be rejected, got {:?}",
            result
        );
    }

    /// CR 103.5b: Other pending players are unaffected by one player's Serum
    /// Powder use — their entries remain in `pending` and they may still act.
    #[test]
    fn serum_powder_does_not_disturb_other_pending_players() {
        let mut state = setup_n_player_with_libraries(4, 30);
        let mut events = Vec::new();
        state.waiting_for = start_mulligan(&mut state, &mut events);

        let powder_id = inject_serum_powder(&mut state, PlayerId(2), 0);
        let waiting = use_serum_powder(&mut state, PlayerId(2), powder_id, &mut events)
            .expect("use_serum_powder");

        let pending = pending_decision_players(&waiting);
        assert!(pending.contains(&PlayerId(0)));
        assert!(pending.contains(&PlayerId(1)));
        assert!(
            pending.contains(&PlayerId(2)),
            "P2 should still be pending after Powder use"
        );
        assert!(pending.contains(&PlayerId(3)));
        // P0/P1/P3 still at count 0.
        for &pid in &[PlayerId(0), PlayerId(1), PlayerId(3)] {
            assert_eq!(decision_count_for(&waiting, pid), Some(0));
        }
        // P2's mulligan_count also still 0 (Powder is not a mulligan).
        assert_eq!(decision_count_for(&waiting, PlayerId(2)), Some(0));
    }

    /// CR 103.5b: A player may use Serum Powder multiple times in a row if
    /// each redraw produces another Powder.
    #[test]
    fn serum_powder_can_chain_when_redraw_yields_another_powder() {
        let mut state = setup_with_libraries(40);
        let mut events = Vec::new();
        state.waiting_for = start_mulligan(&mut state, &mut events);

        // First Powder in hand.
        let first_powder = inject_serum_powder(&mut state, PlayerId(0), 0);
        // Re-name a card at the top of the library so that after exile+redraw,
        // a fresh Powder lands in hand at index 0.
        let top_of_library = state.players[0].library[0];
        state.objects.get_mut(&top_of_library).unwrap().name = SERUM_POWDER_NAME.to_string();

        // Use first Powder.
        use_serum_powder(&mut state, PlayerId(0), first_powder, &mut events).unwrap();

        // The renamed top-of-library object should now be in hand.
        assert!(state.players[0].hand.contains(&top_of_library));
        assert_eq!(
            state.objects[&top_of_library].name, SERUM_POWDER_NAME,
            "redrawn Powder's name should be preserved"
        );

        // Use it again. Should succeed.
        let waiting = use_serum_powder(&mut state, PlayerId(0), top_of_library, &mut events)
            .expect("second Powder use");
        assert_eq!(decision_count_for(&waiting, PlayerId(0)), Some(0));
        assert_eq!(state.objects[&top_of_library].zone, Zone::Exile);
    }

    /// CR 103.5 + 103.5c: In a non-free-first format (Standard duel), the 7th
    /// `Mulligan` brings the player to a 0-card opening hand and must
    /// force-remove them from `pending`. The 8th is never accepted because
    /// they were already force-removed.
    #[test]
    fn max_mulligans_standard_duel_caps_at_seven() {
        let mut state = setup_with_libraries(60);
        let mut events = Vec::new();
        state.waiting_for = start_mulligan(&mut state, &mut events);

        // Seven mulligans in a row. The 7th hits the cap and force-removes P0.
        for _ in 0..7 {
            decide(&mut state, PlayerId(0), false, &mut events);
        }
        assert!(
            !pending_decision_players(&state.waiting_for).contains(&PlayerId(0)),
            "P0 should be force-removed after 7 mulligans in a non-free-first format"
        );
        assert_eq!(
            state.final_mulligan_counts.get(&PlayerId(0)).copied(),
            Some(7),
            "P0's locked-in mulligan count should be 7"
        );

        // 8th submission must be rejected — P0 is no longer in pending.
        let result = handle_mulligan_decision(
            &mut state,
            PlayerId(0),
            MulliganChoice::Mulligan,
            &mut events,
        );
        assert!(
            result.is_err(),
            "8th Mulligan must be rejected in non-free-first format"
        );
    }

    /// CR 103.5 + 103.5c: In a free-first format (Commander duel, Brawl, or
    /// any multiplayer game), the player gets one uncounted mulligan, so the
    /// 8th `Mulligan` submission is still permitted (bringing them to a
    /// 1-card opening hand: 7→6→5→4→3→2→1). Only the 9th would hit the cap.
    #[test]
    fn max_mulligans_free_first_format_permits_eighth() {
        use crate::types::format::FormatConfig;

        let mut state = GameState::new(FormatConfig::commander(), 2, 42);
        state.turn_number = 1;
        state.phase = crate::types::phase::Phase::Untap;
        for player_idx in 0..2u8 {
            for i in 0..60 {
                create_object(
                    &mut state,
                    CardId((player_idx as u64) * 100 + i as u64),
                    PlayerId(player_idx),
                    format!("Card {} P{}", i, player_idx),
                    Zone::Library,
                );
            }
        }
        let mut events = Vec::new();
        state.waiting_for = start_mulligan(&mut state, &mut events);

        // 7 mulligans must keep P0 still in pending (free-first → cap is 8).
        for _ in 0..7 {
            decide(&mut state, PlayerId(0), false, &mut events);
        }
        assert!(
            pending_decision_players(&state.waiting_for).contains(&PlayerId(0)),
            "free-first format: P0 should still be pending after 7 mulligans"
        );
        assert_eq!(
            decision_count_for(&state.waiting_for, PlayerId(0)),
            Some(7),
            "P0 mulligan_count should be 7"
        );

        // 8th mulligan is the cap-triggering submission in a free-first format.
        decide(&mut state, PlayerId(0), false, &mut events);
        assert!(
            !pending_decision_players(&state.waiting_for).contains(&PlayerId(0)),
            "8th mulligan force-removes in free-first format"
        );
        assert_eq!(
            state.final_mulligan_counts.get(&PlayerId(0)).copied(),
            Some(8),
            "P0's locked-in mulligan count should be 8"
        );
    }

    /// CR 103.5: 4-player simultaneous mulligan — submissions arrive in a
    /// non-seat order (P3, P1, P0, P2) and the game still starts cleanly.
    /// Regression for the assumption that ordering matters during the
    /// simultaneous-decision phase.
    #[test]
    fn four_player_keep_in_arbitrary_order_starts_game() {
        let mut state = setup_n_player_with_libraries(4, 20);
        let mut events = Vec::new();
        state.waiting_for = start_mulligan(&mut state, &mut events);

        for &pid in &[PlayerId(3), PlayerId(1), PlayerId(0), PlayerId(2)] {
            let _ = decide(&mut state, pid, true, &mut events);
        }
        assert!(
            matches!(state.waiting_for, WaitingFor::Priority { .. }),
            "game should start after all four kept in non-seat order, got {:?}",
            state.waiting_for
        );
    }

    /// CR 103.5: Kept hand size = 7 minus the bottom count owed, with no
    /// free-first discount (Standard / non-free-first format).
    #[test]
    fn kept_hand_size_after_normal() {
        assert_eq!(kept_hand_size_after(0, false), 7);
        assert_eq!(kept_hand_size_after(3, false), 4);
        assert_eq!(kept_hand_size_after(4, false), 3);
        // Boundary: 7 mulligans bottoms the whole hand → kept hand floors at 0.
        assert_eq!(kept_hand_size_after(7, false), 0);
    }

    /// CR 103.5c: Kept hand size in a free-first format (Commander / cEDH /
    /// multiplayer). The first mulligan is discounted, so count 1 still yields
    /// a 7-card kept hand, and later counts are shifted up by one.
    #[test]
    fn kept_hand_size_after_free_first() {
        assert_eq!(kept_hand_size_after(0, true), 7);
        assert_eq!(kept_hand_size_after(1, true), 7);
        assert_eq!(kept_hand_size_after(4, true), 4);
        assert_eq!(kept_hand_size_after(5, true), 3);
        // Boundary: 8 mulligans (one free) bottoms 7 → kept hand floors at 0.
        assert_eq!(kept_hand_size_after(8, true), 0);
    }

    /// CR 103.5 + CR 800.4a: A player who concedes during the mulligan
    /// decision phase must not appear in the bottoms phase entry list, even
    /// if they kept with a non-zero mulligan count beforehand. Tested in a
    /// 3-player pod so the game does not end on concede.
    #[test]
    fn concede_during_mulligan_excludes_from_bottoms() {
        use crate::game::engine::apply;
        use crate::types::actions::GameAction;

        // Multiplayer (3 seats) grants free-first per CR 103.5c — take TWO
        // mulligans so P0's locked-in bottoms count is non-zero (2 - 1 = 1).
        let mut state = setup_n_player_with_libraries(3, 30);
        let mut events = Vec::new();
        state.waiting_for = start_mulligan(&mut state, &mut events);

        // P0 mulligans twice then keeps (locks in mulligan_count = 2 → owes 1 bottom).
        decide(&mut state, PlayerId(0), false, &mut events);
        decide(&mut state, PlayerId(0), false, &mut events);
        decide(&mut state, PlayerId(0), true, &mut events);
        assert_eq!(
            state.final_mulligan_counts.get(&PlayerId(0)).copied(),
            Some(2),
            "P0 should have a final mulligan count of 2 after keeping"
        );

        // P0 concedes before P1/P2 keep. Game does not end (2 living players
        // remain). Elimination prunes pending and final_mulligan_counts.
        let _ = apply(
            &mut state,
            PlayerId(0),
            GameAction::Concede {
                player_id: PlayerId(0),
            },
        )
        .expect("concede");
        assert!(
            !state.final_mulligan_counts.contains_key(&PlayerId(0)),
            "eliminated player should be pruned from final_mulligan_counts"
        );

        // P1 and P2 keep → bottoms phase should not include P0.
        decide(&mut state, PlayerId(1), true, &mut events);
        let waiting = decide(&mut state, PlayerId(2), true, &mut events);
        assert_eq!(
            pending_bottom_for(&waiting, PlayerId(0)),
            None,
            "conceded P0 must not be in bottoms phase, got {:?}",
            waiting
        );
    }
}
