pub mod deck_resolve;
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
pub mod session;
pub mod starter_decks;

pub use deck_resolve::resolve_deck;
pub use draft_session::{generate_draft_code, DraftSession, DraftSessionManager};
pub use draft_wire_guard::{
    guard_create_draft_with_settings, guard_join_draft_with_password, guard_reconnect_draft,
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
pub use session::{
    acting_player, acting_players, generate_game_code, generate_player_token, is_acting,
    SessionManager,
};
