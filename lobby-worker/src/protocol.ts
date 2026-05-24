// Hand-written mirror of the LobbyOnly subset of
// crates/server-core/src/protocol.rs.
//
// ⚠️ STUB ONLY. This mirror is exactly the cross-adapter duplication /
// drift hazard that the WASM-shared-crate approach exists to eliminate
// (.planning/lobby-failover-federation-plan.md §4a, §6c). It lives here only
// to validate the Cloudflare plumbing. When the real `lobby-broker` crate is
// compiled to WASM and loaded into the DO, delete this file.

/** MUST equal `PROTOCOL_VERSION` in crates/server-core/src/protocol.rs.
 *  The client hard-rejects the handshake on any mismatch. */
export const PROTOCOL_VERSION = 6;

/** Wire shape of a lobby row (snake_case — protocol.rs has no rename_all on
 *  `LobbyGame`). Only the fields the lobby listing needs are populated. */
export interface LobbyGame {
  game_code: string;
  host_name: string;
  created_at: number;
  has_password: boolean;
  host_version: string;
  host_build_commit: string;
  current_players: number;
  max_players: number;
  format: string | null;
  room_name: string | null;
  is_p2p: boolean;
  is_sandbox: boolean;
}

/** Per-socket state, persisted via `ws.serializeAttachment` so it survives
 *  Durable Object hibernation (the attachment is restored on wake). */
export interface SocketState {
  /** True once the socket has sent `SubscribeLobby`. */
  subscribed: boolean;
  /** The host's build commit from its `ClientHello` — stamped onto
   *  `LobbyGame.host_build_commit` so guest/host build-compat gating works. */
  buildCommit: string;
  /** game_code this socket registered, if it is a host. Removed on close. */
  ownedGameCode?: string;
}

/** Secret/per-room data never broadcast in `LobbyGame` (peer id, password,
 *  and the configs echoed back to a joining guest). */
export interface RoomSecret {
  hostPeerId: string;
  password: string | null;
  formatConfig: unknown | null;
  matchConfig: unknown;
}
