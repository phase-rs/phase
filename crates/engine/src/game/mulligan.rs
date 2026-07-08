use crate::game::ability_utils::build_resolved_from_def;
use crate::game::effects::resolve_ability_chain;
use crate::types::ability::AbilityKind;
use crate::types::actions::MulliganChoice;
use crate::types::events::GameEvent;
use crate::types::format::GameFormat;
use crate::types::game_state::{
    GameState, MulliganBottomEntry, MulliganDecisionEntry, MulliganDecisionPhase,
    OpeningHandBottomReason, PendingBeginGameAbility, PendingMulliganAction, WaitingFor,
};
use crate::types::identifiers::ObjectId;
use crate::types::player::PlayerId;
use crate::types::zones::Zone;

use super::turns;

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

/// CR 103.3 + CR 103.5: Start the mulligan process — shuffle libraries and draw 7 for each player.
///
/// CR 103.5 + 103.5b: All players decide simultaneously. The returned
/// `WaitingFor::MulliganDecision` carries every living player in seat order;
/// each may submit `MulliganDecision { choice }` in any arrival order, with
/// `MulliganChoice::Keep`, `Mulligan`, or `UseSerumPowder { object_id }`.
///
/// CR 805.3a (Two-Headed Giant mulligans, via CR 810.2's shared team turns
/// option): the printed rule sequences decisions team-by-team, but since
/// every player here decides independently and all decisions are applied
/// simultaneously once `pending` empties, the team-by-team sequencing has no
/// observable effect on the engine's simultaneous-decision model — submission
/// order is already unconstrained for every multiplayer format.
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
            phase: MulliganDecisionPhase::Declare,
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
/// - `Keep` locks in the hand (CR 103.5). If the player still owes bottoms
///   against the `prepaid_mulligan_bottoms` ledger, their entry transitions to
///   `BottomCards { then: Keep }`; otherwise they are removed from `pending`.
/// - `Mulligan` increments that player's `mulligan_count`, shuffles their hand
///   back into their library, redraws the starting hand size, and RESETS that
///   player's bottoms ledger to 0 (CR 103.5 — a fresh redraw invalidates prior
///   credit). The player remains in `pending` to decide again. At the mulligan
///   cap (CR 103.5 final sentence) the mulligan is treated as an implicit Keep
///   at the new count.
/// - `UseSerumPowder { object_id }` (CR 103.5b + Serum Powder Oracle text) is a
///   declare-point action. If bottoms are still owed, the entry transitions to
///   `BottomCards { then: UseSerumPowder { object_id } }` and the exile+redraw
///   is deferred until that obligation resolves; otherwise the Powder's effect
///   runs immediately. The player's `mulligan_count` is *not* incremented (this
///   is not a mulligan) and the entry returns to `Declare` afterward.
///
/// A decision is rejected if the player's entry is not in the `Declare` phase
/// (they owe bottoms first).
///
/// When `pending` becomes empty, advance directly to `finish_mulligans` — each
/// player's bottoms are resolved at their own declare point, so there is no
/// separate batch bottoms phase.
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

    if !matches!(pending[idx].phase, MulliganDecisionPhase::Declare) {
        return Err(format!(
            "Player {:?} owes bottom cards before making another mulligan decision",
            player
        ));
    }
    let current_count = pending[idx].mulligan_count;

    match choice {
        MulliganChoice::Keep => {
            resolve_declare_point(
                state,
                &mut pending,
                idx,
                free_first,
                PendingMulliganAction::Keep,
                events,
            )?;
        }
        MulliganChoice::Mulligan => {
            let new_count = current_count + 1;
            shuffle_hand_into_library(state, player, events);
            draw_n(state, player, STARTING_HAND_SIZE, events);
            // CR 103.5: a fresh redraw makes any prior "already bottomed"
            // credit meaningless — the obligation for the new count starts
            // from scratch.
            state.prepaid_mulligan_bottoms.insert(player, 0);
            pending[idx].mulligan_count = new_count;

            if new_count >= max_mulligans_for(free_first) {
                // CR 103.5 final sentence: this is the last legal mulligan.
                // Treat it as an implicit Keep at the new count.
                resolve_declare_point(
                    state,
                    &mut pending,
                    idx,
                    free_first,
                    PendingMulliganAction::Keep,
                    events,
                )?;
            }
        }
        MulliganChoice::UseSerumPowder { object_id } => {
            // CR 103.5b + Serum Powder Oracle text + CR 201.2: reject an
            // invalid reference at declare time, before any owed-bottom
            // sub-phase is created.
            validate_serum_powder_reference(state, player, object_id)?;
            resolve_declare_point(
                state,
                &mut pending,
                idx,
                free_first,
                PendingMulliganAction::UseSerumPowder { object_id },
                events,
            )?;
        }
    }

    Ok(advance_after_decision(state, pending, free_first, events))
}

/// CR 103.5 + 103.5b: Shared declare-point resolution for `Keep` and
/// `UseSerumPowder` (and the implicit-Keep at the mulligan cap). Computes the
/// still-owed bottom count against the ledger and either completes the action
/// immediately (owed == 0, calling `handle_serum_powder` right away for the
/// UseSerumPowder case) or parks the entry in `BottomCards`.
fn resolve_declare_point(
    state: &mut GameState,
    pending: &mut Vec<MulliganDecisionEntry>,
    idx: usize,
    free_first: bool,
    then: PendingMulliganAction,
    events: &mut Vec<GameEvent>,
) -> Result<(), String> {
    let player = pending[idx].player;
    let mulligan_count = pending[idx].mulligan_count;
    let prepaid = state
        .prepaid_mulligan_bottoms
        .get(&player)
        .copied()
        .unwrap_or(0);
    let owed = bottom_count_for(mulligan_count, free_first).saturating_sub(prepaid);

    if owed == 0 {
        match then {
            PendingMulliganAction::Keep => {
                pending.remove(idx);
            }
            PendingMulliganAction::UseSerumPowder { object_id } => {
                handle_serum_powder(state, player, object_id, events)?;
                // Stays in `Declare`, same mulligan_count — Serum Powder is
                // not a mulligan.
            }
        }
    } else {
        pending[idx].phase = MulliganDecisionPhase::BottomCards { count: owed, then };
    }
    Ok(())
}

/// CR 103.5b + Serum Powder Oracle text + CR 201.2: Validate that
/// `serum_powder_id` is a legal Serum Powder activation for `player` — in
/// their hand, and named "Serum Powder" (case-insensitive). Called at
/// declare-time, before any owed-bottom `BottomCards` sub-phase is created, so
/// an invalid reference is rejected immediately rather than after prompting
/// for an unrelated bottom-cards selection.
fn validate_serum_powder_reference(
    state: &GameState,
    player: PlayerId,
    serum_powder_id: ObjectId,
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
    Ok(())
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
    // Primary validation is delegated to `validate_serum_powder_reference`.
    // This is called from a second call site (`handle_mulligan_bottom`'s `then`
    // dispatch) as well as the declare-point fast path, so re-validating here
    // is defense-in-depth against a caller that skips the declare-time check.
    validate_serum_powder_reference(state, player, serum_powder_id)?;

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

    // CR 103.5: pregame procedure — route through the zone pipeline under the
    // `PregameProcedure` exempt cause (no effect exists pregame to replace a
    // mulligan move; PLAN §3).
    for card_id in hand_ids {
        let req = crate::game::zone_pipeline::ZoneMoveRequest::pregame(card_id, Zone::Exile);
        crate::game::zone_pipeline::move_object(state, req, events);
    }

    // CR 103.5b + Serum Powder Oracle text: "draw that many cards" — draw
    // exactly the number we just exiled, regardless of the configured
    // starting hand size. (In practice these are equal because the player is
    // in the mulligan-decision phase with a full hand, but the rule is
    // phrased as "that many" so we honor it literally.)
    draw_n(state, player, exiled_count, events);

    Ok(())
}

/// CR 103.5: After updating `pending`, either re-emit `MulliganDecision` or,
/// once every player is out of `pending`, finish the mulligan flow directly.
/// Bottoming is resolved per-entry at each declare point, so there is no
/// separate batch bottoms phase.
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
    state.prepaid_mulligan_bottoms.clear();
    finish_mulligans(state, events)
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
    // CR 103.5: pregame bottoming — route to the library bottom through the
    // pipeline's library-placement arm under the `PregameProcedure` exempt
    // cause (folds the raw `move_to_library_position` sibling in).
    for card_id in cards {
        let req = crate::game::zone_pipeline::ZoneMoveRequest::pregame(card_id, Zone::Library)
            .at_library_position(crate::types::ability::LibraryPosition::Bottom);
        crate::game::zone_pipeline::move_object(state, req, events);
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

/// CR 103.5: Resolve one player's `SelectCards { cards }` while their entry is
/// in the `BottomCards` sub-phase of `MulliganDecision`. Validates the count
/// and hand-membership, rejects the earmarked Serum Powder object (if `then`
/// is `UseSerumPowder`) from being selected as one of the bottomed cards —
/// that object is committed to its own activation. Moves the selected cards to
/// the bottom of the library, credits the ledger, then dispatches `then`.
pub fn handle_mulligan_bottom(
    state: &mut GameState,
    player: PlayerId,
    cards: Vec<ObjectId>,
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, String> {
    let WaitingFor::MulliganDecision {
        pending,
        free_first_mulligan,
    } = &state.waiting_for
    else {
        return Err("handle_mulligan_bottom called outside MulliganDecision".to_string());
    };
    let free_first = *free_first_mulligan;
    let mut pending = pending.clone();

    let idx = pending
        .iter()
        .position(|e| e.player == player)
        .ok_or_else(|| format!("Player {:?} is not in the mulligan pending set", player))?;

    let MulliganDecisionPhase::BottomCards {
        count: expected_count,
        then,
    } = pending[idx].phase
    else {
        return Err(format!(
            "Player {:?} has no owed bottom-cards obligation",
            player
        ));
    };

    // Engine invariant (no CR citation — not an explicit CR clause): the Serum
    // Powder object earmarked by a pending `UseSerumPowder { object_id }`
    // continuation cannot itself be selected as one of the bottomed cards; it
    // is committed to its own activation.
    if let PendingMulliganAction::UseSerumPowder { object_id } = then {
        if cards.contains(&object_id) {
            return Err(format!(
                "Cannot bottom Serum Powder object {:?} — it is committed to its own activation",
                object_id
            ));
        }
    }

    validate_bottom_selection(state, player, &cards, expected_count)?;

    // CR 103.5: pregame bottoming — route to the library bottom through the
    // pipeline's library-placement arm under the `PregameProcedure` exempt
    // cause (folds the raw `move_to_library_position` sibling in).
    for card_id in cards {
        let req = crate::game::zone_pipeline::ZoneMoveRequest::pregame(card_id, Zone::Library)
            .at_library_position(crate::types::ability::LibraryPosition::Bottom);
        crate::game::zone_pipeline::move_object(state, req, events);
    }

    *state.prepaid_mulligan_bottoms.entry(player).or_insert(0) += expected_count;

    match then {
        PendingMulliganAction::Keep => {
            pending.remove(idx);
        }
        PendingMulliganAction::UseSerumPowder { object_id } => {
            handle_serum_powder(state, player, object_id, events)?;
            pending[idx].phase = MulliganDecisionPhase::Declare;
        }
    }

    Ok(advance_after_decision(state, pending, free_first, events))
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
        // CR 103.6: Beginning-game abilities resolve after mulligans and
        // before the first turn receives priority. Seed a priority sentinel so
        // skipped or noninteractive abilities cannot leave the stale
        // MulliganDecision state as the apparent pause point.
        state.waiting_for = WaitingFor::Priority {
            player: pending.ability.controller,
        };
        let _ = resolve_ability_chain(state, &pending.ability, events, 0);
        if !matches!(state.waiting_for, WaitingFor::Priority { .. }) {
            return state.waiting_for.clone();
        }
    }

    state.resolving_begin_game_abilities = false;
    crate::game::planechase::reveal_starting_plane(state);
    turns::auto_advance(state, events)
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

    // CR 103.5: pregame mulligan — return the hand to the library through the
    // pipeline under the `PregameProcedure` exempt cause, then shuffle once.
    //
    // The requests MUST go through the library-placement arm
    // (`.at_library_position(Bottom)` — insertion order is irrelevant because
    // the explicit single shuffle immediately follows): a placement-less
    // Library-destination request runs the delivery tail, whose CR 701.24a
    // auto-shuffle arm fires PER CARD — a 7-card mulligan would emit seven
    // `ShuffledLibrary` player-action events (pre-pipeline count: zero) and
    // consume seven extra full-library shuffles from the seeded RNG stream,
    // diverging same-seed games. Pinned by
    // `mulligan_shuffle_back_emits_no_shuffled_library_events`.
    for card_id in hand_ids {
        let req = crate::game::zone_pipeline::ZoneMoveRequest::pregame(card_id, Zone::Library)
            .at_library_position(crate::types::ability::LibraryPosition::Bottom);
        crate::game::zone_pipeline::move_object(state, req, events);
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
        // CR 103.5: pregame draw — route through the pipeline under the
        // `PregameProcedure` exempt cause.
        let req = crate::game::zone_pipeline::ZoneMoveRequest::pregame(top_card, Zone::Hand);
        crate::game::zone_pipeline::move_object(state, req, events);
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

    /// Test helper: read the `(count, then)` of a pending player's `BottomCards`
    /// phase, if any.
    fn bottom_phase_for(wf: &WaitingFor, player: PlayerId) -> Option<(u8, PendingMulliganAction)> {
        match wf {
            WaitingFor::MulliganDecision { pending, .. } => pending
                .iter()
                .find(|e| e.player == player)
                .and_then(|e| match e.phase {
                    MulliganDecisionPhase::BottomCards { count, then } => Some((count, then)),
                    MulliganDecisionPhase::Declare => None,
                }),
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

    /// CR 103.5: a mulligan shuffles the hand back as ONE shuffle, and that
    /// shuffle is the mulligan's own event-less `shuffle_vector` — the
    /// pre-pipeline behavior emitted ZERO `ShuffledLibrary` player-action
    /// events. Pins that count so the zone-pipeline migration cannot leak the
    /// CR 701.24a per-card auto-shuffle from the delivery tail (which would
    /// emit one event per returned card and consume extra RNG, diverging
    /// same-seed games across versions).
    #[test]
    fn mulligan_shuffle_back_emits_no_shuffled_library_events() {
        let mut state = setup_with_libraries(20);
        let mut events = Vec::new();
        let wf = start_mulligan(&mut state, &mut events);
        state.waiting_for = wf;

        events.clear();
        decide(&mut state, PlayerId(0), false, &mut events);

        let shuffle_events = events
            .iter()
            .filter(|e| {
                matches!(
                    e,
                    GameEvent::PlayerPerformedAction {
                        action: crate::types::events::PlayerActionKind::ShuffledLibrary,
                        ..
                    }
                )
            })
            .count();
        assert_eq!(
            shuffle_events, 0,
            "mulligan shuffle-back must not emit per-card ShuffledLibrary events \
             (pre-pipeline count: 0 — the single real shuffle is event-less shuffle_vector)"
        );
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

    /// CR 103.5 + 103.5b: `Keep` with an owed bottom enters `BottomCards`
    /// immediately at declare time — independent of whether other players are
    /// still deciding. This replaces the old batch model where bottoms were
    /// deferred until every player had kept.
    #[test]
    fn keep_after_mulligan_enters_bottom_cards_immediately_independent_of_other_pending() {
        let mut state = setup_with_libraries(20);
        let mut events = Vec::new();
        state.waiting_for = start_mulligan(&mut state, &mut events);

        decide(&mut state, PlayerId(0), false, &mut events);
        let waiting = decide(&mut state, PlayerId(0), true, &mut events);

        assert!(
            matches!(waiting, WaitingFor::MulliganDecision { .. }),
            "still MulliganDecision (P1 hasn't acted), got {:?}",
            waiting
        );
        assert_eq!(
            bottom_phase_for(&waiting, PlayerId(0)),
            Some((1, PendingMulliganAction::Keep)),
            "P0 should be in BottomCards immediately, before P1 has acted at all"
        );
        assert!(
            pending_decision_players(&waiting).contains(&PlayerId(1)),
            "P1 is still in Declare phase, untouched by P0's bottoming"
        );

        let card_to_bottom = state.players[0].hand[0];
        let waiting = bottom(&mut state, PlayerId(0), vec![card_to_bottom], &mut events).unwrap();
        assert!(
            !pending_decision_players(&waiting).contains(&PlayerId(0)),
            "P0 is fully locked in and removed from pending"
        );

        let waiting = decide(&mut state, PlayerId(1), true, &mut events);
        assert!(matches!(waiting, WaitingFor::Priority { .. }));
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

    /// Four players can independently reach and resolve BottomCards in any
    /// order, none blocking another.
    #[test]
    fn four_player_concurrent_bottom_in_any_order() {
        let mut state = setup_n_player_with_libraries(4, 30);
        let mut events = Vec::new();
        state.waiting_for = start_mulligan(&mut state, &mut events);

        for &pid in &[PlayerId(0), PlayerId(2), PlayerId(3)] {
            decide(&mut state, pid, false, &mut events);
            decide(&mut state, pid, false, &mut events);
            decide(&mut state, pid, true, &mut events);
        }
        let waiting = decide(&mut state, PlayerId(1), true, &mut events);

        assert_eq!(
            bottom_phase_for(&waiting, PlayerId(0)),
            Some((1, PendingMulliganAction::Keep))
        );
        assert_eq!(
            bottom_phase_for(&waiting, PlayerId(2)),
            Some((1, PendingMulliganAction::Keep))
        );
        assert_eq!(
            bottom_phase_for(&waiting, PlayerId(3)),
            Some((1, PendingMulliganAction::Keep))
        );
        assert_eq!(bottom_phase_for(&waiting, PlayerId(1)), None);

        let card3 = state.players[3].hand[0];
        let card0 = state.players[0].hand[0];
        let card2 = state.players[2].hand[0];
        bottom(&mut state, PlayerId(3), vec![card3], &mut events).unwrap();
        bottom(&mut state, PlayerId(0), vec![card0], &mut events).unwrap();
        let waiting = bottom(&mut state, PlayerId(2), vec![card2], &mut events).unwrap();

        assert!(matches!(waiting, WaitingFor::Priority { .. }));
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
                enter_tapped: crate::types::zones::EtbTapState::Unspecified,
                enters_attacking: false,
                up_to: false,
                enter_with_counters: vec![],
                conditional_enter_with_counters: vec![],
                face_down_profile: None,
             enters_modified_if: None },
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

    /// CR 103.5c: In multiplayer, the first mulligan is free (doesn't count
    /// toward bottoms). After a single mulligan + Keep, nothing is owed.
    #[test]
    fn multiplayer_first_mulligan_is_free() {
        let mut state = setup_n_player_with_libraries(3, 30);
        let mut events = Vec::new();
        state.waiting_for = start_mulligan(&mut state, &mut events);

        let waiting = decide(&mut state, PlayerId(0), false, &mut events);
        assert_eq!(decision_count_for(&waiting, PlayerId(0)), Some(1));

        decide(&mut state, PlayerId(0), true, &mut events);
        decide(&mut state, PlayerId(1), true, &mut events);
        let waiting = decide(&mut state, PlayerId(2), true, &mut events);

        assert_eq!(bottom_phase_for(&waiting, PlayerId(0)), None);
        assert!(matches!(waiting, WaitingFor::Priority { .. }));
    }

    /// CR 103.5c: In multiplayer, the second mulligan is the first COUNTED one
    /// — after 2 mulligans total (1 free), 1 card is owed at Keep.
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
            bottom_phase_for(&waiting, PlayerId(0)),
            Some((1, PendingMulliganAction::Keep))
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
        assert_eq!(bottom_phase_for(&waiting, PlayerId(0)), None);
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
        assert_eq!(bottom_phase_for(&waiting, PlayerId(0)), None);
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
            bottom_phase_for(&waiting, PlayerId(0)),
            Some((1, PendingMulliganAction::Keep))
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

    /// CR 103.5 + 103.5c: In a non-free-first format, the 7th `Mulligan` brings
    /// the player to a 0-card opening hand and is treated as an implicit Keep at
    /// that count — the entry transitions to `BottomCards` (still pending). A
    /// further `MulliganDecision` submission is rejected.
    #[test]
    fn max_mulligans_standard_duel_caps_at_seven() {
        let mut state = setup_with_libraries(60);
        let mut events = Vec::new();
        state.waiting_for = start_mulligan(&mut state, &mut events);

        for _ in 0..7 {
            decide(&mut state, PlayerId(0), false, &mut events);
        }

        assert!(
            pending_decision_players(&state.waiting_for).contains(&PlayerId(0)),
            "P0 stays pending, now mid-BottomCards from the implicit Keep at the cap"
        );
        assert_eq!(
            decision_count_for(&state.waiting_for, PlayerId(0)),
            Some(7),
            "P0's mulligan_count should be 7"
        );
        assert_eq!(
            bottom_phase_for(&state.waiting_for, PlayerId(0)),
            Some((7, PendingMulliganAction::Keep)),
            "P0 owes the full bottom_count_for(7, false) = 7, matching kept_hand_size_after(7,false) = 0"
        );

        let result = handle_mulligan_decision(
            &mut state,
            PlayerId(0),
            MulliganChoice::Mulligan,
            &mut events,
        );
        assert!(
            result.is_err(),
            "no MulliganDecision is legal while mid-BottomCards"
        );
    }

    /// CR 103.5 + 103.5c: In a free-first format, the mulligan cap is 8
    /// (`max_mulligans_for(true) = MAX_MULLIGANS + 1`). The 8th `Mulligan`
    /// force-routes to an implicit Keep, owing `bottom_count_for(8, true) = 7`.
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

        for _ in 0..7 {
            decide(&mut state, PlayerId(0), false, &mut events);
        }
        assert!(
            pending_decision_players(&state.waiting_for).contains(&PlayerId(0)),
            "free-first format: P0 should still be pending after 7 mulligans"
        );
        assert_eq!(decision_count_for(&state.waiting_for, PlayerId(0)), Some(7));
        assert_eq!(
            bottom_phase_for(&state.waiting_for, PlayerId(0)),
            None,
            "still in Declare — the 7th mulligan does not hit the free-first cap of 8"
        );

        let waiting = decide(&mut state, PlayerId(0), false, &mut events);
        assert_eq!(decision_count_for(&waiting, PlayerId(0)), Some(8));
        assert_eq!(
            bottom_phase_for(&waiting, PlayerId(0)),
            Some((7, PendingMulliganAction::Keep)),
            "8th mulligan force-routes to Keep, owing bottom_count_for(8, true) = 7 (kept_hand_size_after(8, true) = 0), got {:?}",
            waiting
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

    /// CR 800.4a: A player who concedes while mid-`BottomCards { then: Keep }`
    /// is pruned from `pending` and from `prepaid_mulligan_bottoms`. Remaining
    /// players complete the flow normally.
    #[test]
    fn concede_during_mulligan_excludes_from_bottoms() {
        use crate::game::engine::apply;
        use crate::types::actions::GameAction;

        let mut state = setup_n_player_with_libraries(3, 30);
        let mut events = Vec::new();
        state.waiting_for = start_mulligan(&mut state, &mut events);

        decide(&mut state, PlayerId(0), false, &mut events);
        decide(&mut state, PlayerId(0), false, &mut events);
        let waiting = decide(&mut state, PlayerId(0), true, &mut events);
        assert_eq!(
            bottom_phase_for(&waiting, PlayerId(0)),
            Some((1, PendingMulliganAction::Keep)),
            "P0 should owe 1 bottom card (2 mulligans, 1 free) before conceding"
        );

        let _ = apply(
            &mut state,
            PlayerId(0),
            GameAction::Concede {
                player_id: PlayerId(0),
            },
        )
        .expect("concede");

        assert!(
            !pending_decision_players(&state.waiting_for).contains(&PlayerId(0)),
            "conceded P0 must be pruned from pending, got {:?}",
            state.waiting_for
        );
        assert!(
            !state.prepaid_mulligan_bottoms.contains_key(&PlayerId(0)),
            "conceded P0's prepaid ledger entry must be pruned"
        );

        decide(&mut state, PlayerId(1), true, &mut events);
        let waiting = decide(&mut state, PlayerId(2), true, &mut events);
        assert!(
            matches!(waiting, WaitingFor::Priority { .. }),
            "remaining players should be able to complete the mulligan flow after P0's concession, got {:?}",
            waiting
        );
    }

    /// CR 103.5: Two mulligans then `Keep` bottoms the FULL cumulative count
    /// (2), in a single resolution.
    #[test]
    fn mull_to_five_bottoms_full_cumulative_count_not_incremental_delta() {
        let mut state = setup_with_libraries(20);
        let mut events = Vec::new();
        state.waiting_for = start_mulligan(&mut state, &mut events);

        decide(&mut state, PlayerId(0), false, &mut events);
        decide(&mut state, PlayerId(0), false, &mut events);
        let waiting = decide(&mut state, PlayerId(0), true, &mut events);

        assert_eq!(
            bottom_phase_for(&waiting, PlayerId(0)),
            Some((2, PendingMulliganAction::Keep)),
            "P0 should owe the full cumulative count of 2, not an incremental delta, got {:?}",
            waiting
        );

        let cards_to_bottom: Vec<ObjectId> =
            state.players[0].hand.iter().take(2).copied().collect();
        let waiting =
            bottom(&mut state, PlayerId(0), cards_to_bottom, &mut events).expect("bottom");

        assert_eq!(
            state.players[0].hand.len(),
            5,
            "hand should land at kept_hand_size_after(2, false) = 5 in one resolution"
        );
        assert!(!pending_decision_players(&waiting).contains(&PlayerId(0)));
    }

    /// CR 103.5: HEADLINE TEST for the reset/accumulate discipline. Reaches a
    /// 2-mulligan "mull to 5" checkpoint via `UseSerumPowder` (not `Keep`, since
    /// `Keep` would lock the player out of further mulligans), takes a genuine
    /// third `Mulligan`, then `Keep`s — asserts the owed amount is the FULL
    /// cumulative count for mulligan_count=3 (3), not an incremental delta off
    /// the count-2 payment already made (which would incorrectly yield 1).
    #[test]
    fn mull_to_five_then_third_mulligan_owes_full_amount_not_incremental_delta() {
        let mut state = setup_with_libraries(40);
        let mut events = Vec::new();
        state.waiting_for = start_mulligan(&mut state, &mut events);

        decide(&mut state, PlayerId(0), false, &mut events);
        decide(&mut state, PlayerId(0), false, &mut events);

        let powder_id = inject_serum_powder(&mut state, PlayerId(0), 0);

        let waiting = use_serum_powder(&mut state, PlayerId(0), powder_id, &mut events)
            .expect("use_serum_powder declare point");
        assert_eq!(
            bottom_phase_for(&waiting, PlayerId(0)),
            Some((2, PendingMulliganAction::UseSerumPowder { object_id: powder_id })),
            "UseSerumPowder at count=2 should owe the full cumulative count of 2 before the Powder's own effect applies, got {:?}",
            waiting
        );

        let hand_before_bottom: Vec<ObjectId> = state.players[0].hand.iter().copied().collect();
        let cards_to_bottom: Vec<ObjectId> = hand_before_bottom
            .iter()
            .copied()
            .filter(|&id| id != powder_id)
            .take(2)
            .collect();
        let waiting = bottom(&mut state, PlayerId(0), cards_to_bottom, &mut events)
            .expect("bottom resolves the owed BottomCards obligation");

        assert_eq!(state.players[0].hand.len(), 5);
        assert_eq!(state.objects[&powder_id].zone, Zone::Exile);
        assert_eq!(
            decision_count_for(&waiting, PlayerId(0)),
            Some(2),
            "Serum Powder use does not change mulligan_count"
        );
        assert_eq!(
            bottom_phase_for(&waiting, PlayerId(0)),
            None,
            "back in Declare after the Powder's effect resolves"
        );

        let waiting = decide(&mut state, PlayerId(0), false, &mut events);
        assert_eq!(decision_count_for(&waiting, PlayerId(0)), Some(3));
        assert_eq!(state.players[0].hand.len(), STARTING_HAND_SIZE);

        let waiting = decide(&mut state, PlayerId(0), true, &mut events);
        assert_eq!(
            bottom_phase_for(&waiting, PlayerId(0)),
            Some((3, PendingMulliganAction::Keep)),
            "owed must be the full cumulative count for mulligan_count=3 (3), not an incremental delta off the count-2 payment already made — got {:?}",
            waiting
        );
    }

    /// CR 103.5 + 103.5b: `UseSerumPowder` with an owed bottom must NOT run its
    /// exile+redraw effect until the `BottomCards` obligation is resolved.
    #[test]
    fn use_serum_powder_resolves_owed_bottom_before_exiling_hand() {
        let mut state = setup_with_libraries(20);
        let mut events = Vec::new();
        state.waiting_for = start_mulligan(&mut state, &mut events);

        decide(&mut state, PlayerId(0), false, &mut events);
        let powder_id = inject_serum_powder(&mut state, PlayerId(0), 0);
        let hand_before: Vec<ObjectId> = state.players[0].hand.iter().copied().collect();

        let waiting = use_serum_powder(&mut state, PlayerId(0), powder_id, &mut events)
            .expect("use_serum_powder declare point");
        assert_eq!(
            bottom_phase_for(&waiting, PlayerId(0)),
            Some((
                1,
                PendingMulliganAction::UseSerumPowder {
                    object_id: powder_id
                }
            ))
        );

        assert_eq!(state.players[0].hand.len(), 7, "hand must still be 7 cards");
        assert!(state.players[0].hand.contains(&powder_id));
        for card_id in &hand_before {
            assert_eq!(
                state.objects[card_id].zone,
                Zone::Hand,
                "card {:?} must remain in hand until the bottom obligation resolves",
                card_id
            );
        }

        let card_to_bottom = *hand_before.iter().find(|&&id| id != powder_id).unwrap();
        bottom(&mut state, PlayerId(0), vec![card_to_bottom], &mut events).expect("bottom");

        assert_eq!(state.objects[&card_to_bottom].zone, Zone::Library);
        assert_eq!(*state.players[0].library.back().unwrap(), card_to_bottom);
        assert_eq!(state.objects[&powder_id].zone, Zone::Exile);
        assert_eq!(state.players[0].hand.len(), 6);
    }

    /// Engine invariant (no CR citation — not an explicit CR clause): the
    /// Serum Powder object earmarked by a pending `UseSerumPowder { object_id }`
    /// continuation cannot be selected as one of the bottomed cards.
    #[test]
    fn use_serum_powder_cannot_bottom_the_earmarked_powder_itself() {
        let mut state = setup_with_libraries(20);
        let mut events = Vec::new();
        state.waiting_for = start_mulligan(&mut state, &mut events);

        decide(&mut state, PlayerId(0), false, &mut events);
        let powder_id = inject_serum_powder(&mut state, PlayerId(0), 0);
        let waiting = use_serum_powder(&mut state, PlayerId(0), powder_id, &mut events)
            .expect("use_serum_powder declare point");
        assert_eq!(
            bottom_phase_for(&waiting, PlayerId(0)),
            Some((
                1,
                PendingMulliganAction::UseSerumPowder {
                    object_id: powder_id
                }
            ))
        );

        let result = bottom(&mut state, PlayerId(0), vec![powder_id], &mut events);
        assert!(
            result.is_err(),
            "bottoming the earmarked Powder itself must be rejected, got {:?}",
            result
        );
        assert_eq!(
            bottom_phase_for(&state.waiting_for, PlayerId(0)),
            Some((
                1,
                PendingMulliganAction::UseSerumPowder {
                    object_id: powder_id
                }
            ))
        );
        assert_eq!(state.players[0].hand.len(), 7);
    }

    /// CR 103.5 + 103.5b: HEADLINE TEST, the direct fix for the round-3
    /// "double-charge" regression. First `UseSerumPowder` resolves an owed-1
    /// bottom (`prepaid` becomes 1); a second, distinct Serum Powder found in
    /// the redrawn hand is used at the SAME `mulligan_count` — asserts no
    /// recharge occurs.
    #[test]
    fn use_serum_powder_twice_in_a_row_at_same_count_does_not_recharge() {
        let mut state = setup_with_libraries(40);
        let mut events = Vec::new();
        state.waiting_for = start_mulligan(&mut state, &mut events);

        decide(&mut state, PlayerId(0), false, &mut events);

        let first_powder = inject_serum_powder(&mut state, PlayerId(0), 0);
        let future_second_powder = state.players[0].library[0];
        state.objects.get_mut(&future_second_powder).unwrap().name = SERUM_POWDER_NAME.to_string();

        let waiting = use_serum_powder(&mut state, PlayerId(0), first_powder, &mut events)
            .expect("first use_serum_powder declare point");
        assert_eq!(
            bottom_phase_for(&waiting, PlayerId(0)),
            Some((
                1,
                PendingMulliganAction::UseSerumPowder {
                    object_id: first_powder
                }
            ))
        );

        let non_powder_card = *state.players[0]
            .hand
            .iter()
            .find(|&&id| id != first_powder)
            .unwrap();
        bottom(&mut state, PlayerId(0), vec![non_powder_card], &mut events)
            .expect("bottom resolves the first owed BottomCards obligation");

        assert_eq!(state.prepaid_mulligan_bottoms.get(&PlayerId(0)), Some(&1));
        assert_eq!(state.objects[&first_powder].zone, Zone::Exile);
        assert_eq!(state.players[0].hand.len(), 6);
        assert!(state.players[0].hand.contains(&future_second_powder));

        let second_powder = future_second_powder;
        let library_before_second_use = state.players[0].library.len();

        let waiting = use_serum_powder(&mut state, PlayerId(0), second_powder, &mut events)
            .expect("second use_serum_powder at the same count must succeed directly");

        assert_eq!(
            bottom_phase_for(&waiting, PlayerId(0)),
            None,
            "a second Serum Powder use at the same, already-paid mulligan_count must not recharge a BottomCards phase — got {:?}",
            waiting
        );
        assert_eq!(
            state.prepaid_mulligan_bottoms.get(&PlayerId(0)),
            Some(&1),
            "the ledger must remain unchanged by a zero-owed declare point"
        );
        assert_eq!(decision_count_for(&waiting, PlayerId(0)), Some(1));
        assert_eq!(
            state.players[0].library.len(),
            library_before_second_use - 6,
            "second Powder use should only draw 6 cards from the library, with no additional card silently bottomed"
        );
    }

    /// CR 103.5: Documents the owed==0 fast path explicitly — `Keep` with
    /// `mulligan_count = 0` never enters `BottomCards`.
    #[test]
    fn keep_with_zero_mulligans_resolves_immediately_no_bottom_phase() {
        let mut state = setup_with_libraries(20);
        let mut events = Vec::new();
        state.waiting_for = start_mulligan(&mut state, &mut events);

        let waiting = decide(&mut state, PlayerId(0), true, &mut events);

        assert_eq!(
            bottom_phase_for(&waiting, PlayerId(0)),
            None,
            "Keep with 0 mulligans must not enter BottomCards"
        );
        assert!(!pending_decision_players(&waiting).contains(&PlayerId(0)));
        assert!(pending_decision_players(&waiting).contains(&PlayerId(1)));
    }

    /// Hostile-fixture coverage: a `SelectCards` submission while the player's
    /// entry is still in `Declare` (nothing owed) must be rejected outright.
    #[test]
    fn select_cards_while_declare_phase_rejected() {
        let mut state = setup_with_libraries(20);
        let mut events = Vec::new();
        state.waiting_for = start_mulligan(&mut state, &mut events);

        assert_eq!(bottom_phase_for(&state.waiting_for, PlayerId(0)), None);

        let some_card = state.players[0].hand[0];
        let result = handle_mulligan_bottom(&mut state, PlayerId(0), vec![some_card], &mut events);
        assert!(
            result.is_err(),
            "SelectCards must be rejected while the player's entry is in Declare phase (nothing owed), got {:?}",
            result
        );
        assert_eq!(state.players[0].hand.len(), 7);
        assert!(state.players[0].hand.contains(&some_card));
    }

    /// CR 800.4a: A player who concedes while mid-`BottomCards { then:
    /// UseSerumPowder { object_id } }` (not just the simpler `then: Keep` case)
    /// must be pruned cleanly, with no panic or dangling reference to the
    /// earmarked Serum Powder object.
    #[test]
    fn concede_during_mulligan_use_serum_powder_bottoming_excludes_cleanly() {
        use crate::game::engine::apply;
        use crate::types::actions::GameAction;

        let mut state = setup_n_player_with_libraries(3, 30);
        let mut events = Vec::new();
        state.waiting_for = start_mulligan(&mut state, &mut events);

        decide(&mut state, PlayerId(0), false, &mut events);
        decide(&mut state, PlayerId(0), false, &mut events);
        let powder_id = inject_serum_powder(&mut state, PlayerId(0), 0);
        let waiting = use_serum_powder(&mut state, PlayerId(0), powder_id, &mut events)
            .expect("use_serum_powder declare point");
        assert_eq!(
            bottom_phase_for(&waiting, PlayerId(0)),
            Some((
                1,
                PendingMulliganAction::UseSerumPowder {
                    object_id: powder_id
                }
            ))
        );

        let result = apply(
            &mut state,
            PlayerId(0),
            GameAction::Concede {
                player_id: PlayerId(0),
            },
        );
        assert!(result.is_ok(), "concede must not panic, got {:?}", result);

        assert!(
            !pending_decision_players(&state.waiting_for).contains(&PlayerId(0)),
            "conceded P0 must be pruned from pending regardless of their phase, got {:?}",
            state.waiting_for
        );
        assert!(
            !state.prepaid_mulligan_bottoms.contains_key(&PlayerId(0)),
            "conceded P0's ledger row must be pruned"
        );

        decide(&mut state, PlayerId(1), true, &mut events);
        let waiting = decide(&mut state, PlayerId(2), true, &mut events);
        assert!(
            matches!(waiting, WaitingFor::Priority { .. }),
            "remaining players should complete the flow after P0's concession, got {:?}",
            waiting
        );
    }
}
