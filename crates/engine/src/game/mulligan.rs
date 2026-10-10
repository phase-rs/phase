use crate::game::ability_utils::build_resolved_from_def;
use crate::game::effects::resolve_ability_chain;
use crate::types::ability::AbilityKind;
use crate::types::actions::MulliganChoice;
use crate::types::card_type::CoreType;
use crate::types::events::GameEvent;
use crate::types::format::{DealOrder, FreeRevealMulligan, GameFormat, ZoneScope};
use crate::types::game_state::{
    GameState, MulliganBottomEntry, MulliganDecisionEntry, MulliganDecisionPhase,
    MulliganDeclaration, MulliganDeclarationKind, OpeningHandBottomReason, PendingBeginGameAbility,
    PendingMulliganAction, WaitingFor,
};
use crate::types::identifiers::ObjectId;
use crate::types::player::PlayerId;
use crate::types::zones::Zone;

use super::players::{is_alive, turn_order_index};
use super::turns;

/// CR 103.5: A player's starting hand size is normally seven cards.
const STARTING_HAND_SIZE: usize = 7;
/// CR 103.5 (final sentence): a player may take mulligans until their opening
/// hand would be zero cards. In a standard game that means at most 7 mulligans
/// (7→6→5→4→3→2→1→0; the 8th would be 0). CR 103.5c adds that in free-first
/// formats the first mulligan is uncounted, so the cap shifts up by one to 8
/// — the player may still be brought all the way down to a 0-card opening
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

    let deals: Vec<(PlayerId, usize)> = state
        .seat_order
        .iter()
        .map(|&player| (player, STARTING_HAND_SIZE))
        .collect();
    deal_hands(state, &deals, events);

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
            free_reveals_taken: 0,
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
        declared: Vec::new(),
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
///   at the new count. In a shared-library format the declaration is only
///   recorded in `declared` (CR 103.5) and carried out by `close_declare_round`.
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
/// When `pending` becomes empty, close the declare round if any declaration is
/// held, then advance to `finish_mulligans` — each player's bottoms are resolved
/// at their own declare point, so there is no separate batch bottoms phase.
pub fn handle_mulligan_decision(
    state: &mut GameState,
    player: PlayerId,
    choice: MulliganChoice,
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, String> {
    let free_first = free_first_mulligan(state);

    // Snapshot the current pending list (we own a clone because the engine
    // borrows `state.waiting_for` immutably during match dispatch).
    let WaitingFor::MulliganDecision {
        pending, declared, ..
    } = &state.waiting_for
    else {
        return Err("handle_mulligan_decision called outside MulliganDecision".to_string());
    };
    let mut pending = pending.clone();
    let mut declared = declared.clone();

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
    let reveals_taken = pending[idx].free_reveals_taken;

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
        MulliganChoice::Mulligan => match mulligan_timing(state) {
            MulliganTiming::Immediate => {
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
            MulliganTiming::Simultaneous => {
                // CR 103.5: record the declaration; the hand stays until every
                // player has declared.
                pending.remove(idx);
                declared.push(MulliganDeclaration {
                    player,
                    mulligan_count: current_count,
                    free_reveals_taken: reveals_taken,
                    kind: MulliganDeclarationKind::Regular,
                });
            }
        },
        MulliganChoice::FreeReveal => {
            if !free_reveal_offered(state, &pending[idx]) {
                return Err(format!(
                    "Player {:?} may not take a free reveal mulligan",
                    player
                ));
            }
            match mulligan_timing(state) {
                MulliganTiming::Immediate => {
                    redraw_after_free_reveal(state, player, events);
                    pending[idx].free_reveals_taken = reveals_taken.saturating_add(1);
                }
                MulliganTiming::Simultaneous => {
                    // CR 103.5: record the declaration; the reveal and redraw
                    // happen when the round closes.
                    pending.remove(idx);
                    declared.push(MulliganDeclaration {
                        player,
                        mulligan_count: current_count,
                        free_reveals_taken: reveals_taken,
                        kind: MulliganDeclarationKind::FreeReveal,
                    });
                }
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

    Ok(advance_after_decision(
        state, pending, declared, free_first, events,
    ))
}

/// CR 103.5 as modified by the Dandan free-reveal rule (`FreeRevealMulligan`
/// axis): whether the entry's seat may take the free reveal mulligan now.
/// Computed from the live hand, never stored, so `WaitingFor` leaks no hand
/// composition to the opponent.
pub(crate) fn free_reveal_offered(state: &GameState, entry: &MulliganDecisionEntry) -> bool {
    match state.format_config.format.free_reveal_mulligan() {
        FreeRevealMulligan::Unavailable => false,
        FreeRevealMulligan::WhenHandLacks {
            min_lands,
            min_nonlands,
        } => {
            let (lands, nonlands) = hand_land_split(state, entry.player);
            // `mulligan_count == 0` is "no regular mulligan taken yet": every
            // regular mulligan increments it and a free reveal never does.
            matches!(entry.phase, MulliganDecisionPhase::Declare)
                && entry.mulligan_count == 0
                && (lands < usize::from(min_lands) || nonlands < usize::from(min_nonlands))
        }
    }
}

/// CR 103.5b + Serum Powder Oracle text: every object in `player`'s hand named
/// "Serum Powder" (CR 201.2: name match is exact and case-insensitive).
pub(crate) fn serum_powders_in_hand(state: &GameState, player: PlayerId) -> Vec<ObjectId> {
    let Some(p) = state.players.iter().find(|p| p.id == player) else {
        return Vec::new();
    };
    p.hand
        .iter()
        .copied()
        .filter(|oid| {
            state
                .objects
                .get(oid)
                .is_some_and(|o| o.name.eq_ignore_ascii_case(SERUM_POWDER_NAME))
        })
        .collect()
}

/// The Serum Powders `seat`'s own pending entry may use now (CR 103.5b: "any
/// time you could mulligan"). The single seat-scoped authority for emitting
/// `MulliganChoice::UseSerumPowder`: the action names an object in one seat's
/// hand, so only a list built for that seat may carry it.
pub(crate) fn serum_powders_offered_to(state: &GameState, seat: PlayerId) -> Vec<ObjectId> {
    match &state.waiting_for {
        WaitingFor::MulliganDecision { pending, .. }
            if pending.iter().any(|entry| {
                entry.player == seat && matches!(entry.phase, MulliganDecisionPhase::Declare)
            }) =>
        {
            serum_powders_in_hand(state, seat)
        }
        _ => Vec::new(),
    }
}

/// Whether `seat`'s own pending entry may take the free reveal now. The single
/// seat-scoped authority for emitting `MulliganChoice::FreeReveal`: the action
/// names no seat, so only a list built for one seat may carry it.
pub(crate) fn free_reveal_offered_to(state: &GameState, seat: PlayerId) -> bool {
    match &state.waiting_for {
        WaitingFor::MulliganDecision { pending, .. } => pending
            .iter()
            .any(|entry| entry.player == seat && free_reveal_offered(state, entry)),
        _ => false,
    }
}

/// CR 103.5 as modified by the Dandan free-reveal rule: whether no hand dealt
/// from the shared pile list could avoid the reveal condition. A function of
/// the format axis and the registered pile only; absent or empty pile data
/// answers `false`.
pub fn free_reveal_futile_for(state: &GameState, seat: PlayerId) -> bool {
    let FreeRevealMulligan::WhenHandLacks {
        min_lands,
        min_nonlands,
    } = state.format_config.format.free_reveal_mulligan()
    else {
        return false;
    };
    let Some(pool) = state.deck_pool_of(seat) else {
        return false;
    };
    let (mut lands, mut nonlands) = (0usize, 0usize);
    for entry in pool.current_main.iter() {
        // CR 205.2a: land is a card type.
        if entry.card.card_type.core_types.contains(&CoreType::Land) {
            lands += entry.count as usize;
        } else {
            nonlands += entry.count as usize;
        }
    }
    lands + nonlands > 0 && !redraw_can_clear(min_lands, min_nonlands, lands, nonlands)
}

/// Whether a seven-card draw from `lands` lands and `nonlands` nonland cards
/// can hold at least `min_lands` lands and at least `min_nonlands` nonlands.
fn redraw_can_clear(min_lands: u8, min_nonlands: u8, lands: usize, nonlands: usize) -> bool {
    let (min_lands, min_nonlands) = (usize::from(min_lands), usize::from(min_nonlands));
    lands >= min_lands && nonlands >= min_nonlands && min_lands + min_nonlands <= STARTING_HAND_SIZE
}

/// (lands, nonland cards) in `player`'s hand. CR 205.2a: land is a card type.
fn hand_land_split(state: &GameState, player: PlayerId) -> (usize, usize) {
    let hand = state
        .players
        .iter()
        .find(|p| p.id == player)
        .map(|p| &p.hand);
    let lands = hand.map_or(0, |hand| {
        hand.iter()
            .filter(|id| {
                state
                    .objects
                    .get(id)
                    .is_some_and(|obj| obj.card_types.core_types.contains(&CoreType::Land))
            })
            .count()
    });
    (lands, hand.map_or(0, |hand| hand.len()) - lands)
}

/// CR 701.20a: show the hand to all players, by name only. The cards do not
/// move (CR 701.20b), and no object id is published: the hand is returned and
/// redealt before the end-of-action public-reveal hook reads event ids, which
/// would otherwise mark whichever objects now hold those ids as revealed.
fn reveal_hand(state: &GameState, player: PlayerId, events: &mut Vec<GameEvent>) {
    let card_names = state
        .players
        .iter()
        .find(|p| p.id == player)
        .into_iter()
        .flat_map(|p| p.hand.iter())
        .filter_map(|id| state.objects.get(id))
        .map(|obj| obj.name.clone())
        .collect();
    events.push(GameEvent::CardsRevealed {
        player,
        card_ids: Vec::new(),
        card_names,
    });
}

/// The free reveal where each player has their own library: reveal, shuffle
/// the hand back and redraw, leaving the count and bottoms ledger untouched.
fn redraw_after_free_reveal(state: &mut GameState, player: PlayerId, events: &mut Vec<GameEvent>) {
    reveal_hand(state, player, events);
    shuffle_hand_into_library(state, player, events);
    draw_n(state, player, STARTING_HAND_SIZE, events);
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
/// once every player is out of `pending`, carry out the held declarations
/// (`declared`) as one round and re-enter, or finish the mulligan flow.
/// Bottoming is resolved per-entry at each declare point, so there is no
/// separate batch bottoms phase.
pub(crate) fn advance_after_decision(
    state: &mut GameState,
    pending: Vec<MulliganDecisionEntry>,
    declared: Vec<MulliganDeclaration>,
    free_first: bool,
    events: &mut Vec<GameEvent>,
) -> WaitingFor {
    if !pending.is_empty() {
        return WaitingFor::MulliganDecision {
            pending,
            free_first_mulligan: free_first,
            declared,
        };
    }
    if declared.is_empty() {
        state.prepaid_mulligan_bottoms.clear();
        return finish_mulligans(state, events);
    }
    let pending = close_declare_round(state, declared, free_first, events);
    advance_after_decision(state, pending, Vec::new(), free_first, events)
}

/// CR 103.5: when a declared mulligan is carried out.
enum MulliganTiming {
    Immediate,
    Simultaneous,
}

/// CR 103.5: with a library per player the simultaneity of the redraws is
/// unobservable; a shared pile makes the return, shuffle and deal order matter.
fn mulligan_timing(state: &GameState) -> MulliganTiming {
    match state.format_config.format.shared_zones().library {
        ZoneScope::Shared => MulliganTiming::Simultaneous,
        ZoneScope::PerPlayer => MulliganTiming::Immediate,
    }
}

/// CR 103.5: once every player has declared, all the players who took a
/// mulligan do so at the same time: every hand goes back, the library is
/// shuffled, and the new hands are dealt. Returns the redrawers' fresh entries
/// in seat order.
fn close_declare_round(
    state: &mut GameState,
    declared: Vec<MulliganDeclaration>,
    free_first: bool,
    events: &mut Vec<GameEvent>,
) -> Vec<MulliganDecisionEntry> {
    let redrawers: Vec<MulliganDeclaration> = seat_walk_from_active(state)
        .into_iter()
        .filter_map(|player| declared.iter().find(|d| d.player == player).cloned())
        .collect();

    // CR 701.20a + CR 103.5: every free reveal is shown before any hand
    // returns, so no declarer's hand is seen after another's was shuffled away.
    for declaration in &redrawers {
        match declaration.kind {
            MulliganDeclarationKind::FreeReveal => reveal_hand(state, declaration.player, events),
            MulliganDeclarationKind::Regular => {}
        }
    }
    for declaration in &redrawers {
        return_hand_to_library(state, declaration.player, events);
    }
    // CR 701.24a: one shuffle per distinct library, however many seats share it.
    let mut holders: Vec<PlayerId> = Vec::new();
    for declaration in &redrawers {
        let holder = state.zone_storage_seat(Zone::Library, declaration.player);
        if !holders.contains(&holder) {
            holders.push(holder);
        }
    }
    for holder in holders {
        shuffle_library_of(state, holder);
    }

    for declaration in &redrawers {
        // CR 103.5: a fresh redraw voids any prior "already bottomed" credit.
        state.prepaid_mulligan_bottoms.insert(declaration.player, 0);
    }
    let deals: Vec<(PlayerId, usize)> = redrawers
        .iter()
        .map(|declaration| (declaration.player, STARTING_HAND_SIZE))
        .collect();
    deal_hands(state, &deals, events);

    let mut entries: Vec<MulliganDecisionEntry> = state
        .seat_order
        .iter()
        .filter_map(|&player| {
            redrawers
                .iter()
                .find(|d| d.player == player)
                .map(|d| MulliganDecisionEntry {
                    player,
                    mulligan_count: match d.kind {
                        MulliganDeclarationKind::Regular => d.mulligan_count + 1,
                        // The free reveal is not a regular mulligan: no count, no bottom.
                        MulliganDeclarationKind::FreeReveal => d.mulligan_count,
                    },
                    free_reveals_taken: match d.kind {
                        MulliganDeclarationKind::Regular => d.free_reveals_taken,
                        MulliganDeclarationKind::FreeReveal => {
                            d.free_reveals_taken.saturating_add(1)
                        }
                    },
                    phase: MulliganDecisionPhase::Declare,
                })
        })
        .collect();

    let mut idx = 0;
    while idx < entries.len() {
        if entries[idx].mulligan_count < max_mulligans_for(free_first) {
            idx += 1;
            continue;
        }
        // CR 103.5 final sentence + CR 103.5c: the last legal mulligan is an implicit Keep.
        let before = entries.len();
        resolve_declare_point(
            state,
            &mut entries,
            idx,
            free_first,
            PendingMulliganAction::Keep,
            events,
        )
        .expect("a Keep at the declare point performs no fallible action");
        if entries.len() == before {
            idx += 1;
        }
    }
    entries
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
        declared,
    } = &state.waiting_for
    else {
        return Err("handle_mulligan_bottom called outside MulliganDecision".to_string());
    };
    let free_first = *free_first_mulligan;
    let mut pending = pending.clone();
    let declared = declared.clone();

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

    Ok(advance_after_decision(
        state, pending, declared, free_first, events,
    ))
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
    // CR 103.5: A mulligan puts the owed number of those hand cards on the
    // bottom. Each selected object must therefore be distinct; this shared
    // validator also protects the Tiny Leaders format-extension bottoming path.
    // It mirrors `validate_keep_on_top_selection` / `validate_dig_selection`.
    let mut seen = std::collections::HashSet::new();
    for &card_id in cards {
        if !seen.insert(card_id) {
            return Err(format!("Duplicate card {:?} in bottom selection", card_id));
        }
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
                        ability: Box::new(build_resolved_from_def(ability, obj_id, player_id)),
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
    // The pregame conversation is complete before the phase driver crosses the
    // first turn boundary. Install its settled sentinel explicitly, including
    // the common case where there were no beginning-of-game abilities to drain.
    state.waiting_for = WaitingFor::Priority {
        player: state.priority_player,
    };
    turns::auto_advance(state, events)
}

/// TL:R 906.6a: Re-entry point after pruning an opening-hand bottom prompt.
pub(crate) fn enter_normal_mulligan_public(state: &GameState) -> WaitingFor {
    normal_mulligan_decision(state)
}

/// All players have kept. Start the game properly.
fn finish_mulligans(state: &mut GameState, events: &mut Vec<GameEvent>) -> WaitingFor {
    queue_begin_game_abilities(state);
    state.resolving_begin_game_abilities = true;
    resume_begin_game_abilities(state, events)
}

fn shuffle_hand_into_library(state: &mut GameState, player: PlayerId, events: &mut Vec<GameEvent>) {
    return_hand_to_library(state, player, events);
    shuffle_library_of(state, player);
}

fn return_hand_to_library(state: &mut GameState, player: PlayerId, events: &mut Vec<GameEvent>) {
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
    // pipeline under the `PregameProcedure` exempt cause; the caller shuffles
    // once afterwards.
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
}

/// CR 103.5 + CR 400.1 + CR 701.24a: shuffle the library `player` draws from,
/// which is the shared pile's holder's unless the format gives each player their own.
fn shuffle_library_of(state: &mut GameState, player: PlayerId) {
    let holder = state.zone_storage_seat(Zone::Library, player);
    let GameState { players, rng, .. } = state;
    let player_data = players
        .iter_mut()
        .find(|p| p.id == holder)
        .expect("player exists");
    crate::util::im_ext::shuffle_vector(&mut player_data.library, rng);
}

/// The living seats in `seat_order`, starting at the active player, so every
/// format (shared team turns included) deals and redraws in seat order.
fn seat_walk_from_active(state: &GameState) -> Vec<PlayerId> {
    let len = state.seat_order.len();
    let start = state
        .seat_order
        .iter()
        .position(|&id| id == state.active_player)
        .unwrap_or(0);
    (0..len)
        .map(|offset| state.seat_order[turn_order_index(start, offset, len, state.turn_direction)])
        .filter(|&player| is_alive(state, player))
        .collect()
}

/// CR 103.5 + the format's `DealOrder`: the recipient of each successive card,
/// seat by seat from the active player. Seats absent from `deals` or no longer
/// in the game receive nothing.
pub(crate) fn deal_sequence(state: &GameState, deals: &[(PlayerId, usize)]) -> Vec<PlayerId> {
    let ordered: Vec<(PlayerId, usize)> = seat_walk_from_active(state)
        .into_iter()
        .filter_map(|player| deals.iter().find(|(p, _)| *p == player).copied())
        .collect();
    match state.format_config.format.deal_order() {
        // CR 121.2c order (active player first) applied as the pregame default;
        // CR 103.5 sets no deal order.
        DealOrder::PlayerByPlayer => ordered
            .iter()
            .flat_map(|&(player, count)| std::iter::repeat_n(player, count))
            .collect(),
        DealOrder::Interleaved => {
            let rounds = ordered.iter().map(|&(_, count)| count).max().unwrap_or(0);
            (0..rounds)
                .flat_map(|round| {
                    ordered
                        .iter()
                        .filter(move |&&(_, count)| count > round)
                        .map(|&(player, _)| player)
                })
                .collect()
        }
    }
}

/// The one pregame deal: each recipient in `deal_sequence` order draws one
/// card, and a player's `CardsDrawn` follows their last card.
pub(crate) fn deal_hands(
    state: &mut GameState,
    deals: &[(PlayerId, usize)],
    events: &mut Vec<GameEvent>,
) {
    let sequence = deal_sequence(state, deals);
    for (slot, &recipient) in sequence.iter().enumerate() {
        draw_one(state, recipient, events);
        if sequence[slot + 1..].iter().all(|&p| p != recipient) {
            let count = deals
                .iter()
                .find(|(p, _)| *p == recipient)
                .map_or(0, |&(_, count)| count);
            events.push(GameEvent::CardsDrawn {
                player_id: recipient,
                count: count as u32,
            });
        }
    }
}

/// CR 121.1: draw the top card of the library `player` draws from. Returns
/// false when it is empty.
fn draw_one(state: &mut GameState, player: PlayerId, events: &mut Vec<GameEvent>) -> bool {
    // CR 103.5 + CR 121.1: the top of the player's library, which is the
    // shared pile's top in a shared-library format.
    let Some(&top_card) = state.library_of(player).front() else {
        return false;
    };
    // CR 103.5: pregame draw — route through the pipeline under the
    // `PregameProcedure` exempt cause.
    let req = crate::game::zone_pipeline::ZoneMoveRequest::pregame(top_card, Zone::Hand)
        .performed_by(player);
    crate::game::zone_pipeline::move_object(state, req, events);
    true
}

fn draw_n(state: &mut GameState, player_id: PlayerId, count: usize, events: &mut Vec<GameEvent>) {
    deal_hands(state, &[(player_id, count)], events);
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

    #[test]
    fn free_reveal_redraw_reveals_then_reshuffles_only_the_declarers_hand() {
        let mut state = setup_with_libraries(20);
        let mut events = Vec::new();
        let wf = start_mulligan(&mut state, &mut events);
        state.waiting_for = wf;
        let hand_of = |state: &GameState, seat: usize| -> Vec<ObjectId> {
            state.players[seat].hand.iter().copied().collect()
        };
        let (old_hand, other_hand) = (hand_of(&state, 0), hand_of(&state, 1));
        let old_names: Vec<String> = old_hand
            .iter()
            .map(|id| state.objects[id].name.clone())
            .collect();
        let library_len = state.players[0].library.len();

        events.clear();
        redraw_after_free_reveal(&mut state, PlayerId(0), &mut events);

        assert!(
            matches!(
                events.first(),
                Some(GameEvent::CardsRevealed { player, card_ids, card_names })
                    if *player == PlayerId(0) && card_ids.is_empty() && *card_names == old_names
            ),
            "the reveal comes first, by name only: {events:?}"
        );
        let new_hand = hand_of(&state, 0);
        assert_eq!(new_hand.len(), STARTING_HAND_SIZE);
        assert_ne!(new_hand, old_hand, "reach: the hand was redrawn");
        assert_eq!(state.players[0].library.len(), library_len);
        assert_eq!(hand_of(&state, 1), other_hand);
        assert!(!events.iter().any(|e| matches!(
            e,
            GameEvent::PlayerPerformedAction {
                action: crate::types::events::PlayerActionKind::ShuffledLibrary,
                ..
            }
        )));
    }

    #[test]
    fn free_reveal_is_refused_where_the_format_offers_none() {
        let mut state = setup_with_libraries(20);
        let mut events = Vec::new();
        let wf = start_mulligan(&mut state, &mut events);
        state.waiting_for = wf.clone();
        // Untyped test cards are all nonland: a (0, 7) hand that Dandan would offer.
        let entry = MulliganDecisionEntry {
            free_reveals_taken: 0,
            player: PlayerId(0),
            mulligan_count: 0,
            phase: MulliganDecisionPhase::Declare,
        };
        assert!(!free_reveal_offered(&state, &entry));
        assert!(handle_mulligan_decision(
            &mut state,
            PlayerId(0),
            MulliganChoice::FreeReveal,
            &mut events
        )
        .is_err());
        assert_eq!(state.waiting_for, wf);

        state.format_config = crate::types::format::FormatConfig::dandan();
        assert!(
            free_reveal_offered(&state, &entry),
            "reach: the axis is the difference"
        );
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

    fn dandan_with_pile(seats: u8, pile: usize) -> GameState {
        let mut state = setup_n_player_with_libraries(seats, 0);
        state.format_config.format = GameFormat::Dandan;
        for i in 0..pile {
            create_object(
                &mut state,
                CardId(i as u64),
                PlayerId(0),
                format!("Pile {i}"),
                Zone::Library,
            );
        }
        state
    }

    fn drawn_recipients(state: &GameState, events: &[GameEvent]) -> Vec<PlayerId> {
        events
            .iter()
            .filter_map(|e| match e {
                GameEvent::ZoneChanged {
                    object_id,
                    from: Some(Zone::Library),
                    to: Zone::Hand,
                    ..
                } => state
                    .players
                    .iter()
                    .find(|p| p.hand.contains(object_id))
                    .map(|p| p.id),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn deal_sequence_interleaves_from_the_active_player_when_the_format_says_so() {
        let (p0, p1) = (PlayerId(0), PlayerId(1));
        let alternating = |first: PlayerId, second: PlayerId, n: usize| -> Vec<PlayerId> {
            (0..n).flat_map(|_| [first, second]).collect()
        };

        let mut state = dandan_with_pile(2, 0);
        assert_eq!(
            deal_sequence(&state, &[(p0, 7), (p1, 7)]),
            alternating(p0, p1, 7)
        );
        assert_eq!(
            deal_sequence(&state, &[(p0, 7), (p1, 3)]),
            [p0, p1, p0, p1, p0, p1, p0, p0, p0, p0]
        );
        assert_eq!(deal_sequence(&state, &[(p1, 7)]), vec![p1; 7]);

        state.seat_order = vec![p1, p0];
        state.active_player = p1;
        assert_eq!(
            deal_sequence(&state, &[(p0, 7), (p1, 7)]),
            alternating(p1, p0, 7),
            "the active player deals first even when it is not the canonical seat"
        );

        let mut standard = setup_with_libraries(0);
        let expected: Vec<PlayerId> = [vec![p0; 7], vec![p1; 7]].concat();
        assert_eq!(deal_sequence(&standard, &[(p0, 7), (p1, 7)]), expected);
        standard.seat_order = vec![p1, p0];
        standard.active_player = p1;
        let expected: Vec<PlayerId> = [vec![p1; 7], vec![p0; 7]].concat();
        assert_eq!(deal_sequence(&standard, &[(p0, 7), (p1, 7)]), expected);
    }

    #[test]
    fn deal_hands_alternates_pile_cards_and_the_receiver_owns_them() {
        let (p0, p1) = (PlayerId(0), PlayerId(1));
        let mut state = dandan_with_pile(2, 16);
        let pile: Vec<ObjectId> = state.library_of(p0).iter().copied().collect();
        assert_eq!(pile.len(), 16, "reach: the pile is staged");
        let mut events = Vec::new();

        deal_hands(&mut state, &[(p0, 7), (p1, 7)], &mut events);

        let hand = |seat: PlayerId| -> Vec<ObjectId> {
            state.players[seat.0 as usize]
                .hand
                .iter()
                .copied()
                .collect()
        };
        assert_eq!(
            hand(p0),
            (0..14).step_by(2).map(|i| pile[i]).collect::<Vec<_>>()
        );
        assert_eq!(
            hand(p1),
            (1..14).step_by(2).map(|i| pile[i]).collect::<Vec<_>>()
        );
        for id in hand(p1) {
            assert_eq!(state.objects[&id].owner, p1, "the receiver owns the card");
        }
        let drawn_p0 = events
            .iter()
            .position(
                |e| matches!(e, GameEvent::CardsDrawn { player_id, count: 7 } if *player_id == p0),
            )
            .expect("P0 CardsDrawn");
        let last_move = events
            .iter()
            .rposition(|e| matches!(e, GameEvent::ZoneChanged { .. }))
            .expect("zone events");
        assert!(
            drawn_p0 < last_move,
            "P0's CardsDrawn precedes P1's last card"
        );
        assert!(
            matches!(events.last(), Some(GameEvent::CardsDrawn { player_id, count: 7 }) if *player_id == p1)
        );
    }

    #[test]
    fn standard_opening_deal_event_order_is_seat_by_seat() {
        let mut state = setup_with_libraries(20);
        let mut events = Vec::new();
        start_mulligan(&mut state, &mut events);

        let kinds: Vec<&str> = events
            .iter()
            .filter_map(|e| match e {
                GameEvent::ZoneChanged { .. } => Some("move"),
                GameEvent::CardsDrawn { player_id, .. } if *player_id == PlayerId(0) => Some("p0"),
                GameEvent::CardsDrawn { .. } => Some("p1"),
                _ => None,
            })
            .collect();
        let expected: Vec<&str> =
            [vec!["move"; 7], vec!["p0"], vec!["move"; 7], vec!["p1"]].concat();
        assert_eq!(kinds, expected);
    }

    #[test]
    fn shared_team_turn_opening_deal_walks_seats_from_the_starting_player() {
        for start in [1u8, 3] {
            let mut state = setup_n_player_with_libraries(4, 20);
            state.format_config = crate::types::format::FormatConfig::two_headed_giant();
            assert!(
                state.format_config.topology().has_shared_team_turns(),
                "reach: the shared-team-turn arm is the one under test"
            );
            state.active_player = PlayerId(start);
            state.seat_order.rotate_left(start as usize);
            let mut events = Vec::new();
            start_mulligan(&mut state, &mut events);

            let mut recipients = drawn_recipients(&state, &events);
            recipients.dedup();
            let expected: Vec<PlayerId> = (0..4).map(|i| PlayerId((start + i) % 4)).collect();
            assert_eq!(recipients, expected, "starting player P{start}");
        }
    }

    #[test]
    fn dandan_mulligan_is_held_until_every_player_has_declared() {
        let (p0, p1) = (PlayerId(0), PlayerId(1));
        let mut state = dandan_with_pile(2, 30);
        let mut events = Vec::new();
        state.waiting_for = start_mulligan(&mut state, &mut events);
        let p1_hand: Vec<ObjectId> = state.players[1].hand.iter().copied().collect();
        let pile_before: Vec<ObjectId> = state.library_of(p0).iter().copied().collect();

        events.clear();
        let waiting = decide(&mut state, p1, false, &mut events);

        assert!(!events
            .iter()
            .any(|e| matches!(e, GameEvent::ZoneChanged { .. })));
        assert_eq!(
            state.players[1].hand.iter().copied().collect::<Vec<_>>(),
            p1_hand
        );
        assert_eq!(
            state.library_of(p0).iter().copied().collect::<Vec<_>>(),
            pile_before
        );
        let WaitingFor::MulliganDecision {
            pending, declared, ..
        } = &waiting
        else {
            panic!("expected MulliganDecision, got {waiting:?}");
        };
        assert_eq!(
            pending.iter().map(|e| e.player).collect::<Vec<_>>(),
            vec![p0]
        );
        assert_eq!(
            declared,
            &vec![MulliganDeclaration {
                free_reveals_taken: 0,
                player: p1,
                mulligan_count: 0,
                kind: MulliganDeclarationKind::Regular,
            }]
        );
        assert_eq!(waiting.acting_players(), vec![p0]);

        events.clear();
        let waiting = decide(&mut state, p0, true, &mut events);
        assert_eq!(drawn_recipients(&state, &events), vec![p1; 7]);
        assert_eq!(decision_count_for(&waiting, p1), Some(1));
        assert!(matches!(
            &waiting,
            WaitingFor::MulliganDecision { declared, .. } if declared.is_empty()
        ));
    }

    #[test]
    fn dandan_close_returns_every_hand_before_the_deal_and_deals_active_player_first() {
        let (p0, p1) = (PlayerId(0), PlayerId(1));
        let mut state = dandan_with_pile(2, 40);
        let mut events = Vec::new();
        state.waiting_for = start_mulligan(&mut state, &mut events);

        decide(&mut state, p1, false, &mut events);
        events.clear();
        decide(&mut state, p0, false, &mut events);

        let moves: Vec<(Option<Zone>, Zone)> = events
            .iter()
            .filter_map(|e| match e {
                GameEvent::ZoneChanged { from, to, .. } => Some((*from, *to)),
                _ => None,
            })
            .collect();
        let expected: Vec<(Option<Zone>, Zone)> = [
            vec![(Some(Zone::Hand), Zone::Library); 14],
            vec![(Some(Zone::Library), Zone::Hand); 14],
        ]
        .concat();
        assert_eq!(moves, expected, "all hands return before any card is dealt");
        let alternating: Vec<PlayerId> = (0..7).flat_map(|_| [p0, p1]).collect();
        assert_eq!(drawn_recipients(&state, &events), alternating);
        assert!(!events.iter().any(|e| matches!(
            e,
            GameEvent::PlayerPerformedAction {
                action: crate::types::events::PlayerActionKind::ShuffledLibrary,
                ..
            }
        )));
        assert_eq!(decision_count_for(&state.waiting_for, p0), Some(1));
        assert_eq!(decision_count_for(&state.waiting_for, p1), Some(1));
    }

    #[test]
    fn dandan_round_waits_for_owed_bottoms_before_redrawing() {
        let (p0, p1) = (PlayerId(0), PlayerId(1));
        let mut state = dandan_with_pile(2, 40);
        let mut events = Vec::new();
        state.waiting_for = start_mulligan(&mut state, &mut events);
        decide(&mut state, p0, false, &mut events);
        decide(&mut state, p1, false, &mut events);
        assert_eq!(
            decision_count_for(&state.waiting_for, p1),
            Some(1),
            "reach: round one closed"
        );

        decide(&mut state, p1, false, &mut events);
        let p1_hand: Vec<ObjectId> = state.players[1].hand.iter().copied().collect();
        let waiting = decide(&mut state, p0, true, &mut events);
        assert_eq!(
            bottom_phase_for(&waiting, p0),
            Some((1, PendingMulliganAction::Keep))
        );
        assert_eq!(
            state.players[1].hand.iter().copied().collect::<Vec<_>>(),
            p1_hand
        );

        let bottomed = state.players[0].hand[0];
        let waiting = bottom(&mut state, p0, vec![bottomed], &mut events).expect("bottom");
        assert_ne!(
            state.players[1].hand.iter().copied().collect::<Vec<_>>(),
            p1_hand
        );
        assert_eq!(decision_count_for(&waiting, p1), Some(2));
        assert!(state.library_of(p0).contains(&bottomed));
        assert!(state.players.iter().all(|p| !p.hand.contains(&bottomed)));
    }

    #[test]
    fn dandan_cap_applies_an_implicit_keep_at_the_close() {
        let (p0, p1) = (PlayerId(0), PlayerId(1));
        for (count, at_cap) in [(6u8, true), (5, false)] {
            let mut state = dandan_with_pile(2, 40);
            let mut events = Vec::new();
            state.waiting_for = start_mulligan(&mut state, &mut events);
            let WaitingFor::MulliganDecision { pending, .. } = &mut state.waiting_for else {
                panic!("expected MulliganDecision");
            };
            pending[0].mulligan_count = count;

            decide(&mut state, p1, true, &mut events);
            let waiting = decide(&mut state, p0, false, &mut events);

            if at_cap {
                assert_eq!(
                    bottom_phase_for(&waiting, p0),
                    Some((7, PendingMulliganAction::Keep))
                );
            } else {
                assert_eq!(decision_count_for(&waiting, p0), Some(6));
                assert_eq!(bottom_phase_for(&waiting, p0), None);
            }
        }
    }

    #[test]
    fn dandan_elimination_with_a_held_declaration_still_closes_the_round() {
        use crate::game::engine::apply;
        use crate::types::actions::GameAction;

        for p0_mulligans in [true, false] {
            let mut state = dandan_with_pile(3, 60);
            let mut events = Vec::new();
            state.waiting_for = start_mulligan(&mut state, &mut events);
            let p0_hand: Vec<ObjectId> = state.players[0].hand.iter().copied().collect();

            decide(&mut state, PlayerId(0), !p0_mulligans, &mut events);
            decide(&mut state, PlayerId(1), true, &mut events);
            apply(
                &mut state,
                PlayerId(2),
                GameAction::Concede {
                    player_id: PlayerId(2),
                },
            )
            .expect("concede");

            if p0_mulligans {
                let hand: Vec<ObjectId> = state.players[0].hand.iter().copied().collect();
                assert_ne!(hand, p0_hand, "the held mulligan was carried out");
                assert_eq!(
                    decision_count_for(&state.waiting_for, PlayerId(0)),
                    Some(1),
                    "{:?}",
                    state.waiting_for
                );
            } else {
                assert!(
                    matches!(state.waiting_for, WaitingFor::Priority { .. }),
                    "{:?}",
                    state.waiting_for
                );
            }
        }
    }

    #[test]
    fn redraw_can_clear_is_a_count_rule_over_the_axis() {
        assert!(redraw_can_clear(2, 2, 2, 2));
        assert!(!redraw_can_clear(2, 2, 1, 70));
        assert!(!redraw_can_clear(2, 2, 70, 1));
        assert!(redraw_can_clear(2, 5, 2, 5));
        assert!(
            !redraw_can_clear(4, 4, 40, 40),
            "eight required cards do not fit a seven-card hand"
        );
        assert!(redraw_can_clear(0, 0, 0, 0));
    }
}
