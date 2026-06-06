//! Reservation token ownership checks.
//!
//! Tokens returned by `reserve_seat` are bound to the connection that received
//! them. Other clients must not release or consume tokens they do not hold.

use crate::lobby::LobbyManager;

/// Error returned when a client attempts to operate on another connection's
/// reservation token.
pub const NOT_OWNED_RESERVATION: &str =
    "You may only release or use a seat reservation issued to this connection";

/// Whether `reservations` contains an entry for `(game_code, token)`.
pub fn conn_holds_reservation(
    reservations: &[(String, String)],
    game_code: &str,
    token: &str,
) -> bool {
    reservations
        .iter()
        .any(|(code, held)| code == game_code && held == token)
}

/// Outcome of attempting to release a reservation the caller claims to hold.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReservationRelease {
    /// Token was held and successfully released from the lobby.
    Released,
    /// Caller does not hold this token on their connection.
    NotHeld,
    /// Caller held the token but it was already gone from the lobby (expired).
    NotFound,
}

/// Release `token` for `game_code` only when the caller holds it in
/// `reservations`. Prunes the matching entry from `reservations` on success.
pub fn release_owned_reservation(
    lobby: &mut LobbyManager,
    reservations: &mut Vec<(String, String)>,
    game_code: &str,
    token: &str,
) -> ReservationRelease {
    if !conn_holds_reservation(reservations, game_code, token) {
        return ReservationRelease::NotHeld;
    }
    if lobby.release_reservation(game_code, token) {
        reservations.retain(|(code, t)| code != game_code || t != token);
        ReservationRelease::Released
    } else {
        ReservationRelease::NotFound
    }
}

/// Outcome of attempting to consume a reservation the caller claims to hold.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReservationConsume {
    /// Token was held and consumed (seat counted toward occupancy).
    Consumed,
    /// Caller does not hold this token on their connection.
    NotHeld,
    /// Caller held the token but it was already gone from the lobby (expired).
    NotFound,
}

/// Consume `token` for `game_code` only when the caller holds it in
/// `reservations`. Prunes the matching entry from `reservations` on success.
pub fn consume_owned_reservation(
    lobby: &mut LobbyManager,
    reservations: &mut Vec<(String, String)>,
    game_code: &str,
    token: &str,
) -> ReservationConsume {
    if !conn_holds_reservation(reservations, game_code, token) {
        return ReservationConsume::NotHeld;
    }
    if lobby.consume_reservation(game_code, token) {
        reservations.retain(|(code, t)| code != game_code || t != token);
        ReservationConsume::Consumed
    } else {
        ReservationConsume::NotFound
    }
}
