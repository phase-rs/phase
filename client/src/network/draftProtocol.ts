/**
 * P2P Draft Tournament protocol.
 *
 * Separate from the game protocol (`protocol.ts`) because draft is a
 * session layer above the engine: a tournament coordinator exchanges
 * draft-specific messages (picks, deck submissions, pairings) that have
 * no analog in the per-game wire format.
 *
 * Reuses the same binary wire encoding (gzip + version prefix) from
 * `protocol.ts` so both protocols share the same DataChannel transport
 * with identical compression semantics.
 *
 * The `DRAFT_PROTOCOL_VERSION` is independent of `WIRE_PROTOCOL_VERSION`
 * — a bump here means "the draft message shapes changed" without
 * implying any change to the game-level wire format.
 */

import type {
  DraftPlayerView,
  SeatPublicView,
} from "../adapter/draft-adapter";

// ── Protocol Version ───────────────────────────────────────────────────

/**
 * Draft protocol version. Bumped when message shapes change incompatibly.
 *
 * Bumps to date:
 *   1 — initial P2P draft tournament protocol
 */
export const DRAFT_PROTOCOL_VERSION = 1 as const;

// ── Message Types ──────────────────────────────────────────────────────

/**
 * Discriminated union of all draft-specific P2P messages.
 *
 * Flow:
 *   Guest → Host: `draft_join`, `draft_reconnect`, `draft_pick`, `draft_submit_deck`
 *   Host → Guest: `draft_welcome`, `draft_reconnect_ack`, `draft_reconnect_rejected`,
 *                 `draft_state_update`, `draft_pick_ack`, `draft_error`,
 *                 `draft_kicked`, `draft_pairing`, `draft_match_result`,
 *                 `draft_paused`, `draft_resumed`, `draft_lobby_update`,
 *                 `draft_host_left`
 */
export type DraftP2PMessage =
  // ── Guest → Host ───────────────────────────────────────────────────
  | {
      type: "draft_join";
      displayName: string;
    }
  | {
      type: "draft_reconnect";
      draftToken: string;
    }
  | {
      type: "draft_pick";
      cardInstanceId: string;
    }
  | {
      type: "draft_submit_deck";
      mainDeck: string[];
    }
  // ── Host → Guest ───────────────────────────────────────────────────
  | {
      type: "draft_welcome";
      draftProtocolVersion: typeof DRAFT_PROTOCOL_VERSION;
      /** Opaque token for reconnect — persisted by guest in IndexedDB. */
      draftToken: string;
      /** Seat index assigned to this guest (0-based). */
      seatIndex: number;
      /** Filtered view for this player. */
      view: DraftPlayerView;
      /** Draft code for display / persistence key. */
      draftCode: string;
    }
  | {
      type: "draft_reconnect_ack";
      draftProtocolVersion: typeof DRAFT_PROTOCOL_VERSION;
      seatIndex: number;
      view: DraftPlayerView;
      draftCode: string;
    }
  | {
      type: "draft_reconnect_rejected";
      reason: string;
    }
  | {
      type: "draft_state_update";
      view: DraftPlayerView;
    }
  | {
      type: "draft_pick_ack";
      view: DraftPlayerView;
    }
  | {
      type: "draft_error";
      reason: string;
    }
  | {
      type: "draft_kicked";
      reason: string;
    }
  | {
      type: "draft_pairing";
      round: number;
      table: number;
      opponentSeat: number;
      opponentName: string;
      /** PeerJS peer ID of the match host. Lower seat# hosts. */
      matchHostPeerId: string;
      matchId: string;
    }
  | {
      type: "draft_match_result";
      matchId: string;
      winnerSeat: number | null;
    }
  | {
      type: "draft_paused";
      reason: string;
    }
  | {
      type: "draft_resumed";
    }
  | {
      type: "draft_lobby_update";
      seats: SeatPublicView[];
      joined: number;
      total: number;
    }
  | {
      type: "draft_host_left";
      reason: string;
    };

// ── Validation ─────────────────────────────────────────────────────────

const VALID_DRAFT_TYPES = new Set([
  "draft_join",
  "draft_reconnect",
  "draft_pick",
  "draft_submit_deck",
  "draft_welcome",
  "draft_reconnect_ack",
  "draft_reconnect_rejected",
  "draft_state_update",
  "draft_pick_ack",
  "draft_error",
  "draft_kicked",
  "draft_pairing",
  "draft_match_result",
  "draft_paused",
  "draft_resumed",
  "draft_lobby_update",
  "draft_host_left",
]);

/** Validate a parsed object as a DraftP2PMessage. Throws on malformed data. */
export function validateDraftMessage(raw: unknown): DraftP2PMessage {
  if (typeof raw !== "object" || raw === null || !("type" in raw)) {
    throw new Error("Invalid draft message: missing type field");
  }
  const msg = raw as { type: string };
  if (!VALID_DRAFT_TYPES.has(msg.type)) {
    throw new Error(`Invalid draft message type: ${msg.type}`);
  }
  return raw as DraftP2PMessage;
}

// ── Wire Encoding (reuses game protocol's gzip format) ─────────────────

const FORMAT_RAW = 0x00;
const FORMAT_GZIP = 0x01;
const COMPRESSION_THRESHOLD = 256;

export async function encodeDraftWireMessage(msg: DraftP2PMessage): Promise<Uint8Array> {
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

export async function decodeDraftWireMessage(bytes: Uint8Array): Promise<DraftP2PMessage> {
  if (bytes.length < 1) throw new Error("empty draft wire message");
  const version = bytes[0];
  const payload = bytes.subarray(1);
  let json: string;
  if (version === FORMAT_RAW) {
    json = new TextDecoder().decode(payload);
  } else if (version === FORMAT_GZIP) {
    const stream = new Blob([payload]).stream().pipeThrough(new DecompressionStream("gzip"));
    json = await new Response(stream).text();
  } else {
    throw new Error(`unknown draft wire format version: 0x${version.toString(16)}`);
  }
  return validateDraftMessage(JSON.parse(json));
}
