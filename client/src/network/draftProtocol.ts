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
import type { DeckCardCount, MatchScore } from "../adapter/types";

// ── Protocol Version ───────────────────────────────────────────────────

/**
 * Draft protocol version. Bumped when message shapes change incompatibly.
 *
 * Bumps to date:
 *   1 — initial P2P draft tournament protocol
 *   2 — add timer sync, match start, round advance messages (Phase 57)
 *   3 — add Bo3 sideboard and game-level result messages (Phase 58)
 */
export const DRAFT_PROTOCOL_VERSION = 3 as const;

// ── Message Types ──────────────────────────────────────────────────────

/**
 * Discriminated union of all draft-specific P2P messages.
 *
 * Flow:
 *   Guest → Host: `draft_join`, `draft_reconnect`, `draft_pick`, `draft_submit_deck`,
 *                 `draft_request_advance`
 *   Host → Guest: `draft_welcome`, `draft_reconnect_ack`, `draft_reconnect_rejected`,
 *                 `draft_state_update`, `draft_pick_ack`, `draft_error`,
 *                 `draft_kicked`, `draft_pairing`, `draft_match_result`,
 *                 `draft_paused`, `draft_resumed`, `draft_lobby_update`,
 *                 `draft_host_left`, `draft_timer_sync`, `draft_match_start`
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
    }
  | {
      /** Host → Guest: lightweight timer tick with host-authoritative remaining time. */
      type: "draft_timer_sync";
      /** Milliseconds remaining for the current pick. Host-authoritative. */
      remainingMs: number;
    }
  | {
      /** Host UI only: trigger manual round advance in Casual mode. */
      type: "draft_request_advance";
    }
  | {
      /** Host → Guest: instructs player to start their match for this round. */
      type: "draft_match_start";
      matchId: string;
      round: number;
      opponentSeat: number;
      opponentName: string;
      /** PeerJS peer ID of the player who hosts the game match (lower seat# hosts). */
      matchHostPeerId: string;
      /** Whether this recipient is the match host (lower seat# hosts). */
      isMatchHost: boolean;
    }
  // ── Bo3 (Traditional Draft) Messages ────────────────────────��────────
  | {
      /** Host → Both: prompt players to sideboard between games in a Bo3 match. */
      type: "draft_bo3_sideboard_prompt";
      matchId: string;
      gameNumber: number;
      score: MatchScore;
      /** Seat index of the loser (who gets play/draw choice), or null if draw. */
      loserSeat: number | null;
      /** Sideboard timer duration in ms (0 = no timer). */
      timerMs: number;
    }
  | {
      /** Guest → Host: player submits their sideboarded deck for the next game. */
      type: "draft_bo3_sideboard_submit";
      matchId: string;
      mainDeck: string[];
      sideboard: DeckCardCount[];
    }
  | {
      /** Host → Guest: prompt the loser to choose play or draw for the next game. */
      type: "draft_bo3_play_draw_prompt";
      matchId: string;
      gameNumber: number;
      score: MatchScore;
      /** Play/draw timer duration in ms (0 = no timer). */
      timerMs: number;
    }
  | {
      /** Guest → Host: loser's play/draw choice for the next game. */
      type: "draft_bo3_play_draw_choice";
      matchId: string;
      playFirst: boolean;
    }
  | {
      /** Host → Both: signal that the next game is starting. */
      type: "draft_bo3_game_start";
      matchId: string;
      gameNumber: number;
      firstPlayerSeat: number;
    }
  | {
      /** Host → All: broadcast updated Bo3 score to pod for standings display. */
      type: "draft_bo3_score_update";
      matchId: string;
      scoreA: number;
      scoreB: number;
    }
  | {
      /** Host → Both: the Bo3 match is complete (one player reached 2 wins). */
      type: "draft_bo3_match_complete";
      matchId: string;
      winnerSeat: number;
      finalScoreA: number;
      finalScoreB: number;
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
  "draft_timer_sync",
  "draft_request_advance",
  "draft_match_start",
  "draft_bo3_sideboard_prompt",
  "draft_bo3_sideboard_submit",
  "draft_bo3_play_draw_prompt",
  "draft_bo3_play_draw_choice",
  "draft_bo3_game_start",
  "draft_bo3_score_update",
  "draft_bo3_match_complete",
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
