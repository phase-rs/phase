pub mod ai_seats_wire_guard;
pub mod deck_resolve;
pub mod draft_action_payload_guard;
pub mod draft_session;
pub mod draft_wire_guard;
pub mod emote_guard;
pub mod filter;
pub mod game_reconnect_guard;
#[cfg(test)]
mod harness;
pub mod legacy_deck_guard;
pub mod lobby;
pub mod lookup_join_guard;
pub mod persist;
pub mod protocol;
pub mod reconnect;
pub mod seat_mutation_wire_guard;
pub mod session;
pub mod spectator_wire_guard;
pub mod starter_decks;

pub use ai_seats_wire_guard::guard_create_ai_seats;
pub use deck_resolve::resolve_deck;
pub use draft_action_payload_guard::guard_draft_action_payload;
pub use draft_session::{generate_draft_code, DraftSession, DraftSessionManager};
pub use draft_wire_guard::{
    guard_create_draft_with_settings, guard_draft_action, guard_join_draft_with_password,
    guard_reconnect_draft,
};
pub use emote_guard::guard_emote;
pub use filter::filter_state_for_player;
pub use game_reconnect_guard::guard_game_reconnect;
pub use legacy_deck_guard::guard_legacy_deck;
pub use lobby::LobbyManager;
pub use lookup_join_guard::guard_lookup_join_target;
pub use persist::{PersistedLobbyMeta, PersistedSession};
pub use protocol::{
    AiSeatRequest, ClientMessage, DeckChoice, DeckData, LobbyGame, PlayerSlotInfo, SeatKind,
    SeatMutation, SeatView, ServerMessage,
};
pub use reconnect::ReconnectManager;
pub use seat_mutation_wire_guard::guard_seat_mutation;
pub use session::{
    acting_player, acting_players, generate_game_code, generate_player_token, is_acting,
    SessionManager,
};
pub use spectator_wire_guard::{guard_spectate_draft, guard_spectator_join};
