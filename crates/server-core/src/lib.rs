pub mod ai_seats_wire_guard;
pub mod client_hello_guard;
pub mod client_message_wire_guard;
pub mod deck_resolve;
pub mod draft_action_payload_guard;
pub mod draft_session;
pub mod draft_wire_guard;
pub mod emote_guard;
pub mod filter;
pub mod game_action_payload_guard;
pub mod game_reconnect_guard;
pub mod game_state_snapshot_wire_guard;
#[cfg(test)]
mod harness;
pub mod legacy_deck_guard;
pub mod legacy_join_guard;
pub mod lobby;
pub mod lobby_subscriber_wire_guard;
pub mod p2p_backup_guard;
pub mod persist;
pub mod protocol;
pub mod reconnect;
pub mod seat_mutation_wire_guard;
pub mod session;
pub mod spectator_wire_guard;
pub mod starter_decks;
pub mod takeback;

pub use ai_seats_wire_guard::guard_create_ai_seats;
pub use client_hello_guard::guard_client_hello;
pub use client_message_wire_guard::{
    guard_broker_projection_inbound, guard_client_message_before_dispatch,
};
pub use deck_resolve::resolve_deck;
pub use draft_action_payload_guard::guard_draft_action_payload;
pub use draft_session::{generate_draft_code, DraftSession, DraftSessionManager};
pub use draft_wire_guard::{
    guard_create_draft_with_settings, guard_draft_action, guard_join_draft_with_password,
    guard_reconnect_draft,
};
pub use emote_guard::guard_emote;
pub use filter::{filter_events_for_player, filter_state_for_player};
pub use game_reconnect_guard::guard_game_reconnect;
pub use game_state_snapshot_wire_guard::{
    guard_game_state_for_broadcast, guard_state_snapshot_broadcast, StateSnapshotParts,
    MAX_SNAPSHOT_EVENTS, MAX_SNAPSHOT_LEGAL_ACTIONS, MAX_SNAPSHOT_LOG_ENTRIES,
    MAX_SNAPSHOT_OBJECTS,
};
pub use legacy_deck_guard::guard_legacy_deck;
pub use legacy_join_guard::guard_legacy_join_game;
pub use lobby::LobbyManager;
pub use lobby_subscriber_wire_guard::{guard_lobby_subscriber_capacity, MAX_LOBBY_SUBSCRIBERS};
pub use p2p_backup_guard::{
    guard_p2p_backup, guard_p2p_backup_overwrite, redact_p2p_backup_snapshot_secrets,
    validate_p2p_backup_host_peer_id, MAX_P2P_SNAPSHOT_LEN,
};
pub use persist::{restored_draft_lobby_register_request, PersistedLobbyMeta, PersistedSession};
pub use protocol::{
    AiSeatRequest, ClientMessage, DeckChoice, DeckData, LobbyGame, PlayerSlotInfo, SeatKind,
    SeatMutation, SeatView, ServerMessage,
};
pub use reconnect::ReconnectManager;
pub use seat_mutation_wire_guard::guard_seat_mutation;
pub use session::{
    acting_player, acting_players, generate_game_code, generate_player_token, is_acting,
    BroadcastSnapshot, SessionManager,
};
pub use spectator_wire_guard::{
    guard_draft_spectator_capacity, guard_game_spectator_capacity, guard_spectate_draft,
    guard_spectator_join, MAX_DRAFT_SPECTATORS_PER_DRAFT, MAX_GAME_SPECTATORS_PER_GAME,
};
pub use takeback::{PendingTakeback, TakebackOutcome, MAX_TAKEBACK_HISTORY};
