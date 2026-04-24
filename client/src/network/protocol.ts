import type { GameAction, GameEvent, GameState, LegalActionsResult, ManaCost } from "../adapter/types";
import type { SeatMutation, SeatView } from "../multiplayer/seatTypes";

/**
 * Wire-format projection of `LegalActionsResult`. Single source of truth for
 * the legal-action fields carried by `game_setup`, `state_update`, and
 * `reconnect_ack`. When `LegalActionsResult` grows a new field, this type
 * plus the two helpers below are the only places that need to change — the
 * message variants pick it up via intersection.
 *
 * `legalActions` (plural) is the wire name for what the adapter exposes as
 * `actions`; the rename is historical and preserved for backward
 * compatibility across builds already deployed in the wild.
 */
export interface LegalActionsWire {
  legalActions: GameAction[];
  autoPassRecommended?: boolean;
  legalActionsByObject?: Record<string, GameAction[]>;
  spellCosts?: Record<string, ManaCost>;
}

/** Host-side: project an engine `LegalActionsResult` onto the wire shape. */
export function legalActionsToWire(result: LegalActionsResult): LegalActionsWire {
  return {
    legalActions: result.actions,
    autoPassRecommended: result.autoPassRecommended,
    legalActionsByObject: result.legalActionsByObject,
    spellCosts: result.spellCosts,
  };
}

/** Guest-side: hydrate a wire payload into the adapter's `LegalActionsResult`. */
export function legalActionsFromWire(wire: LegalActionsWire): LegalActionsResult {
  return {
    actions: wire.legalActions,
    autoPassRecommended: wire.autoPassRecommended ?? false,
    legalActionsByObject: wire.legalActionsByObject,
    spellCosts: wire.spellCosts,
  };
}

/**
 * Wire-protocol version. Bumped whenever the binary wire format or the shape
 * of the first-contact messages (`game_setup` / `reconnect_ack`) changes in
 * a non-backward-compatible way. Carried on those two messages so a guest
 * connecting to a host running a different version can detect the mismatch
 * in-band and surface an actionable "refresh both windows" message instead
 * of silently corrupting state.
 *
 * Bumps to date:
 *   1 — pre-compression JSON-serialization era (no longer in production)
 *   2 — gzip + version-prefixed binary wire format (current)
 */
export const WIRE_PROTOCOL_VERSION = 2 as const;

export type P2PMessage =
  | { type: "guest_deck"; deckData: unknown; displayName?: string }
  | ({
      type: "game_setup";
      wireProtocolVersion: typeof WIRE_PROTOCOL_VERSION;
      assignedPlayerId: number;
      playerToken: string;
      state: GameState;
      events: GameEvent[];
      playerNames?: Record<number, string>;
    } & LegalActionsWire)
  | { type: "action"; senderPlayerId: number; action: GameAction }
  | ({
      type: "state_update";
      state: GameState;
      events: GameEvent[];
    } & LegalActionsWire)
  | { type: "action_rejected"; reason: string }
  | { type: "ping"; timestamp: number }
  | { type: "pong"; timestamp: number }
  | { type: "disconnect"; reason: string }
  | { type: "emote"; emote: string }
  | { type: "concede" }
  // Reconnect: guest presents prior token; host accepts (with fresh state) or rejects.
  | { type: "reconnect"; playerToken: string }
  | ({
      type: "reconnect_ack";
      wireProtocolVersion: typeof WIRE_PROTOCOL_VERSION;
      assignedPlayerId: number;
      state: GameState;
    } & LegalActionsWire)
  | { type: "reconnect_rejected"; reason: string }
  // Kick / forced removal (host → target).
  | { type: "kick"; reason: string; format?: string }
  // Host explicitly quit the game (host → all guests). Terminal: guests set
  // their `terminated` flag and skip the reconnect backoff that normally
  // fires on an unexpected connection drop. Distinct from the PeerSession
  // `disconnect` wire message because that one is a pure session-close
  // signal; `host_left` carries the game-level semantic that the room is
  // permanently gone and reconnect attempts would spin against a destroyed
  // Peer. Sent from `P2PHostAdapter.terminateGame()` only — component
  // unmount (StrictMode, tab close) goes through `dispose()` which does NOT
  // send this, since those cases may be transient and the reconnect loop is
  // correct behavior there.
  | { type: "host_left"; reason: string }
  // Lifecycle broadcasts (host → all remaining peers).
  | { type: "player_kicked"; playerId: number; reason: string }
  // Host chose "continue without them" OR guest self-conceded mid-game. Wire
  // variant kept distinct from `player_kicked` so clients can render correctly
  // (kick = host forcibly removed; conceded = player left or was continued past).
  | { type: "player_conceded"; playerId: number; reason: string }
  | { type: "player_disconnected"; playerId: number }
  | { type: "player_reconnected"; playerId: number }
  | { type: "game_paused"; reason: string }
  | { type: "game_resumed" }
  // Pre-game lobby progress (host → all peers in the lobby).
  | { type: "lobby_progress"; joined: number; total: number }
  | { type: "seat_mutate"; mutation: SeatMutation }
  | { type: "seat_snapshot"; view: SeatView };

const VALID_TYPES = new Set([
  "guest_deck",
  "game_setup",
  "action",
  "state_update",
  "action_rejected",
  "ping",
  "pong",
  "disconnect",
  "emote",
  "concede",
  "reconnect",
  "reconnect_ack",
  "reconnect_rejected",
  "kick",
  "host_left",
  "player_kicked",
  "player_conceded",
  "player_disconnected",
  "player_reconnected",
  "game_paused",
  "game_resumed",
  "lobby_progress",
  "seat_mutate",
  "seat_snapshot",
]);

/** Validate an already-parsed object as a P2PMessage. Throws on malformed data. */
export function validateMessage(raw: unknown): P2PMessage {
  if (typeof raw !== "object" || raw === null || !("type" in raw)) {
    throw new Error("Invalid message: missing type field");
  }
  const msg = raw as { type: string };
  if (!VALID_TYPES.has(msg.type)) {
    throw new Error(`Invalid message type: ${msg.type}`);
  }
  return raw as P2PMessage;
}

// ── Wire-Format Encoding ─────────────────────────────────────────────────
// The P2P DataChannel carries gzipped JSON with a 1-byte version prefix:
//   [0x00][raw JSON]       — tiny messages where gzip would inflate
//   [0x01][gzip(JSON)]     — state_update, game_setup, etc.
// Messages smaller than COMPRESSION_THRESHOLD skip compression because gzip's
// ~20-byte header would inflate sub-100-byte payloads. Ping/pong and small
// control messages take the raw path; state broadcasts take the gzip path.

const FORMAT_RAW = 0x00;
const FORMAT_GZIP = 0x01;
const COMPRESSION_THRESHOLD = 256;

export async function encodeWireMessage(msg: P2PMessage): Promise<Uint8Array> {
  const json = JSON.stringify(msg);
  const jsonBytes = new TextEncoder().encode(json);
  if (jsonBytes.length < COMPRESSION_THRESHOLD) {
    const out = new Uint8Array(1 + jsonBytes.length);
    out[0] = FORMAT_RAW;
    out.set(jsonBytes, 1);
    return out;
  }
  const stream = new Blob([jsonBytes]).stream().pipeThrough(new CompressionStream("gzip"));
  const gzipped = new Uint8Array(await new Response(stream).arrayBuffer());
  const out = new Uint8Array(1 + gzipped.length);
  out[0] = FORMAT_GZIP;
  out.set(gzipped, 1);
  return out;
}

export async function decodeWireMessage(bytes: Uint8Array): Promise<P2PMessage> {
  if (bytes.length < 1) throw new Error("empty wire message");
  const version = bytes[0];
  const payload = bytes.subarray(1);
  let json: string;
  if (version === FORMAT_RAW) {
    json = new TextDecoder().decode(payload);
  } else if (version === FORMAT_GZIP) {
    const stream = new Blob([payload]).stream().pipeThrough(new DecompressionStream("gzip"));
    json = await new Response(stream).text();
  } else {
    throw new Error(`unknown wire format version: 0x${version.toString(16)}`);
  }
  return validateMessage(JSON.parse(json));
}
