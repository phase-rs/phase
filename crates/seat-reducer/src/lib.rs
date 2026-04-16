pub mod types;

use types::{ReducerCtx, SeatDelta, SeatError, SeatKind, SeatMutation, SeatState};

/// Apply a seat mutation to the current state.
///
/// Phase 1 implements only the `Start` arm. `SetKind` and `Remove` return
/// `SeatError::InvalidTransition` as placeholders until Phase 2.
pub fn apply(
    state: &mut SeatState,
    mutation: SeatMutation,
    ctx: &ReducerCtx,
) -> Result<SeatDelta, SeatError> {
    if !state.is_pregame() {
        return Err(SeatError::GameStarted);
    }

    match mutation {
        SeatMutation::Start => apply_start(state, ctx),
        SeatMutation::SetKind { seat_index, kind } => apply_set_kind(state, seat_index, kind, ctx),
        SeatMutation::Remove { seat_index } => apply_remove(state, seat_index),
    }
}

fn apply_start(state: &mut SeatState, _ctx: &ReducerCtx) -> Result<SeatDelta, SeatError> {
    if !state.is_full() {
        return Err(SeatError::NotFull);
    }

    state.game_started = true;
    Ok(SeatDelta {
        now_started: true,
        ..SeatDelta::empty()
    })
}

fn apply_set_kind(
    state: &mut SeatState,
    seat_index: u8,
    kind: SeatKind,
    ctx: &ReducerCtx,
) -> Result<SeatDelta, SeatError> {
    let seat = seat_index as usize;
    if seat == 0 || seat >= state.seats.len() {
        return Err(SeatError::SeatImmutable);
    }

    let current = state.seats[seat].clone();
    if current == kind {
        return Ok(SeatDelta::empty());
    }

    let mut delta = SeatDelta::empty();
    delta.mutated_seats.push(seat_index);

    match (current, kind.clone()) {
        (SeatKind::HostHuman, _) => Err(SeatError::SeatImmutable),
        (SeatKind::WaitingHuman, SeatKind::JoinedHuman)
        | (SeatKind::Ai { .. }, SeatKind::JoinedHuman)
        | (SeatKind::JoinedHuman, SeatKind::HostHuman)
        | (SeatKind::WaitingHuman, SeatKind::HostHuman)
        | (SeatKind::Ai { .. }, SeatKind::HostHuman) => Err(SeatError::InvalidTransition),
        (SeatKind::WaitingHuman, SeatKind::WaitingHuman)
        | (SeatKind::JoinedHuman, SeatKind::JoinedHuman) => Ok(delta),
        (SeatKind::WaitingHuman, SeatKind::Ai { difficulty, deck }) => {
            let resolved = ctx
                .deck_resolver
                .resolve(&deck)
                .map_err(SeatError::DeckResolutionFailed)?;
            state.seats[seat] = kind;
            state.tokens[seat].clear();
            delta.new_ai.push((seat_index, difficulty, resolved));
            Ok(delta)
        }
        (SeatKind::Ai { .. }, SeatKind::WaitingHuman) => {
            state.seats[seat] = SeatKind::WaitingHuman;
            state.tokens[seat].clear();
            delta.removed_ai.push(seat_index);
            Ok(delta)
        }
        (SeatKind::Ai { .. }, SeatKind::Ai { difficulty, deck }) => {
            let resolved = ctx
                .deck_resolver
                .resolve(&deck)
                .map_err(SeatError::DeckResolutionFailed)?;
            state.seats[seat] = kind;
            state.tokens[seat].clear();
            delta.removed_ai.push(seat_index);
            delta.new_ai.push((seat_index, difficulty, resolved));
            Ok(delta)
        }
        (SeatKind::JoinedHuman, SeatKind::WaitingHuman) => {
            delta.invalidated_tokens.push(state.tokens[seat].clone());
            state.tokens[seat].clear();
            state.seats[seat] = SeatKind::WaitingHuman;
            Ok(delta)
        }
        (SeatKind::JoinedHuman, SeatKind::Ai { difficulty, deck }) => {
            let resolved = ctx
                .deck_resolver
                .resolve(&deck)
                .map_err(SeatError::DeckResolutionFailed)?;
            delta.invalidated_tokens.push(state.tokens[seat].clone());
            state.tokens[seat].clear();
            state.seats[seat] = kind;
            delta.new_ai.push((seat_index, difficulty, resolved));
            Ok(delta)
        }
    }
}

fn apply_remove(state: &mut SeatState, seat_index: u8) -> Result<SeatDelta, SeatError> {
    let seat = seat_index as usize;
    if seat == 0 || seat >= state.seats.len() {
        return Err(SeatError::SeatImmutable);
    }
    if state.seats.len() <= state.format.min_players as usize {
        return Err(SeatError::BelowFormatMin);
    }

    match &state.seats[seat] {
        SeatKind::JoinedHuman => return Err(SeatError::SeatClaimed),
        SeatKind::HostHuman => return Err(SeatError::SeatImmutable),
        SeatKind::Ai { .. } | SeatKind::WaitingHuman => {}
    }

    let removed_kind = state.seats.remove(seat);
    state.tokens.remove(seat);

    let mut delta = SeatDelta::empty();
    delta.mutated_seats = (seat_index..state.seats.len() as u8).collect();
    if matches!(removed_kind, SeatKind::Ai { .. }) {
        delta.removed_ai.push(seat_index);
    }

    delta.renumbering = Some(types::Renumbering {
        removed_index: seat_index,
        remapping: (seat_index + 1..=state.seats.len() as u8)
            .map(|old| (old, old - 1))
            .collect(),
    });

    Ok(delta)
}

#[cfg(test)]
mod tests;
