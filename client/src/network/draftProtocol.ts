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
import type { DeckCardCount, MatchConfig, MatchScore } from "../adapter/types";
import type {
  DraftIntergameCommand,
  DraftIntergameCommandAck,
} from "../services/intergameCommandLedger";

// ── Protocol Version ───────────────────────────────────────────────────

/**
 * Draft protocol version. Bumped when message shapes change incompatibly.
 *
 * Bumps to date:
 *   1 — initial P2P draft tournament protocol
 *   2 — add timer sync, match start, round advance messages (Phase 57)
 *   3 — add Bo3 sideboard and game-level result messages (Phase 58)
 *   4 — add deck-carrying tournament match launch descriptors
 *   5 — bind match settlement to a durable pod-issued capability
 *   6 — durable authorized Bo3 intergame command ledger
 *   7 — forward authenticated match-host between-games observations
 *   8 — add Sealed event kind and deckbuilding-first start flow
 *   9 — add engine-owned limited-pool presentation groups
 *  10 — add authenticated draft-effect pick actions
 *  11 — instance-addressable pool entries (`instance_ids`) + engine rarity axis
 */
export const DRAFT_PROTOCOL_VERSION = 11 as const;

/**
 * Typed reason for a draft pause, used over the wire and on the i18n key path.
 *
 * Wire shape mirrors the Rust `DraftPauseReason` enum (default PascalCase
 * serde). The TS i18n key path also uses PascalCase
 * (`pauseReason.PlayerDisconnected`) so wire = lookup with no boundary
 * conversion.
 */
export type DraftPauseReason =
  | "PlayerDisconnected"
  | "PausedByHost"
  | "DisconnectGraceExpired";

export const DraftPauseReason = {
  PlayerDisconnected: "PlayerDisconnected" as const,
  PausedByHost: "PausedByHost" as const,
  DisconnectGraceExpired: "DisconnectGraceExpired" as const,
};

export interface DraftDeckPayload {
  main_deck: string[];
  sideboard: string[];
  commander: string[];
}

export interface DraftMatchDeckPayload {
  player: DraftDeckPayload;
  opponent: DraftDeckPayload;
  ai_decks: DraftDeckPayload[];
}

/**
 * Pod-issued capability for exactly one tournament match authority.  The
 * random lease and nonce are intentionally opaque: a match result is valid
 * only when it echoes the complete binding issued for its current round.
 */
export interface DraftMatchBinding {
  podId: string;
  matchId: string;
  round: number;
  sessionKey: string;
  lease: string;
  nonce: string;
  revision: number;
  matchAuthoritySeat: number;
}

export interface DraftMatchSettlement {
  binding: DraftMatchBinding;
  receiptId: string;
  winnerSeat: number | null;
}

export type DraftMatchLaunch =
  | {
      type: "HumanHost";
      matchId: string;
      matchRoomCode: string;
      round: number;
      localSeat: number;
      opponentSeat: number;
      opponentName: string;
      matchHostPeerId: string;
      deckPayload: DraftMatchDeckPayload;
      matchConfig: MatchConfig;
      binding: DraftMatchBinding;
    }
  | {
      type: "HumanGuest";
      matchId: string;
      matchRoomCode: string;
      round: number;
      localSeat: number;
      opponentSeat: number;
      opponentName: string;
      matchHostPeerId: string;
      localDeck: DraftDeckPayload;
      matchConfig: MatchConfig;
      binding: DraftMatchBinding;
    }
  | {
      type: "Bot";
      matchId: string;
      round: number;
      localSeat: number;
      botSeat: number;
      botName: string;
      deckPayload: DraftMatchDeckPayload;
      matchConfig: MatchConfig;
      binding: DraftMatchBinding;
    };

// ── Message Types ──────────────────────────────────────────────────────

/**
 * Discriminated union of all draft-specific P2P messages.
 *
 * Flow:
 *   Guest → Host: `draft_join`, `draft_reconnect`, `draft_pick`, `draft_pick_with_draft_effect`, `draft_submit_deck`,
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
      type: "draft_pick_with_draft_effect";
      effectCardInstanceId: string;
      cardInstanceIds: string[];
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
      /** Match-authority seat → pod host: authenticated result settlement. */
      type: "draft_match_settlement";
      settlement: DraftMatchSettlement;
    }
  | {
      /** Pod host → match-authority seat: durable exact-once receipt. */
      type: "draft_match_settlement_ack";
      matchId: string;
      receiptId: string;
      revision: number;
    }
  | {
      type: "draft_paused";
      reason: DraftPauseReason;
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
      launch: DraftMatchLaunch;
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
      /** Match host → pod host: authenticated observation of an engine between-games state. */
      type: "draft_bo3_between_games";
      matchId: string;
      gameNumber: number;
      score: MatchScore;
      loserSeat: number | null;
    }
  | {
      /** Guest → Host: player submits their sideboarded deck for the next game. */
      type: "draft_bo3_sideboard_submit";
      matchId: string;
      mainDeck: string[];
      sideboard: DeckCardCount[];
    }
  | {
      /** Participant → pod: a durable, still-held intergame command. */
      type: "draft_bo3_intergame_command";
      command: DraftIntergameCommand;
    }
  | {
      /** Pod → participant: the exact held command is now executable. */
      type: "draft_bo3_intergame_authorized";
      command: DraftIntergameCommand;
      acknowledgement: DraftIntergameCommandAck;
    }
  | {
      /** Participant → pod: the authorized command reached its local sink. */
      type: "draft_bo3_intergame_receipt";
      acknowledgement: DraftIntergameCommandAck;
      receiptId: string;
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
  "draft_pick_with_draft_effect",
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
  "draft_match_settlement",
  "draft_match_settlement_ack",
  "draft_paused",
  "draft_resumed",
  "draft_lobby_update",
  "draft_host_left",
  "draft_timer_sync",
  "draft_request_advance",
  "draft_match_start",
  "draft_bo3_sideboard_prompt",
  "draft_bo3_between_games",
  "draft_bo3_sideboard_submit",
  "draft_bo3_intergame_command",
  "draft_bo3_intergame_authorized",
  "draft_bo3_intergame_receipt",
  "draft_bo3_play_draw_prompt",
  "draft_bo3_play_draw_choice",
  "draft_bo3_game_start",
  "draft_bo3_score_update",
  "draft_bo3_match_complete",
]);

const MAX_DRAFT_CARD_INSTANCE_ID_LENGTH = 256;

function requireDraftCardInstanceId(value: unknown, field: string): string {
  if (
    typeof value !== "string"
    || value.length === 0
    || value.length > MAX_DRAFT_CARD_INSTANCE_ID_LENGTH
  ) {
    throw new Error(`Invalid draft-effect pick: ${field} must be a bounded string`);
  }
  return value;
}

function validateDraftEffectPick(raw: Record<string, unknown>): DraftP2PMessage {
  const effectCardInstanceId = requireDraftCardInstanceId(
    raw.effectCardInstanceId,
    "effectCardInstanceId",
  );
  if (!Array.isArray(raw.cardInstanceIds) || raw.cardInstanceIds.length !== 2) {
    throw new Error("Invalid draft-effect pick: cardInstanceIds must contain exactly two cards");
  }
  const cardInstanceIds = raw.cardInstanceIds.map((cardId, index) =>
    requireDraftCardInstanceId(cardId, `cardInstanceIds[${index}]`),
  );
  if (cardInstanceIds[0] === cardInstanceIds[1]) {
    throw new Error("Invalid draft-effect pick: cardInstanceIds must be distinct");
  }
  return {
    ...raw,
    type: "draft_pick_with_draft_effect",
    effectCardInstanceId,
    cardInstanceIds,
  } as DraftP2PMessage;
}

function normalizeArrayField<T>(record: Record<string, unknown>, field: string): T[] {
  if (!(field in record)) return [];
  const value = record[field];
  if (!Array.isArray(value)) {
    throw new Error(`Invalid draft message: ${field} must be an array`);
  }
  return value as T[];
}

function normalizeSeatPublicView(raw: unknown): SeatPublicView {
  if (typeof raw !== "object" || raw === null) {
    throw new Error("Invalid draft message: malformed public seat");
  }
  const seat = raw as Record<string, unknown>;
  return {
    ...seat,
    face_up_draft_cards: normalizeArrayField(seat, "face_up_draft_cards"),
  } as SeatPublicView;
}

/** v10 → v11: an old-shape entry carries no `instance_ids`. Upgrade it to the
 * representative id — the one instance the old wire shape can address. A
 * collapsed multi-copy entry from a v10 message therefore addresses only its
 * representative; the other copies' ids were never serialized and cannot be
 * reconstructed here (re-deriving them would make this normalizer a second
 * classification authority). */
function normalizePoolEntry(raw: unknown): Record<string, unknown> {
  if (typeof raw !== "object" || raw === null) {
    throw new Error("Invalid draft message: malformed pool entry");
  }
  const entry = raw as Record<string, unknown>;
  if (Array.isArray(entry.instance_ids)) return entry;
  const card = entry.card as { instance_id?: unknown } | undefined;
  const id = typeof card?.instance_id === "string" ? [card.instance_id] : [];
  return { ...entry, instance_ids: id };
}

function normalizePoolGroup(raw: unknown): Record<string, unknown> {
  if (typeof raw !== "object" || raw === null) {
    throw new Error("Invalid draft message: malformed pool group");
  }
  const group = raw as Record<string, unknown>;
  return {
    ...group,
    cards: normalizeArrayField(group, "cards").map(normalizePoolEntry),
  };
}

/** v10 → v11: fill the missing rarity axis (empty — the old host never
 * classified it) and upgrade every group entry. */
function normalizePoolGroups(raw: unknown): Record<string, unknown> | undefined {
  if (raw === undefined || raw === null) return undefined;
  if (typeof raw !== "object") {
    throw new Error("Invalid draft message: malformed pool groups");
  }
  const groups = raw as Record<string, unknown>;
  return {
    ...groups,
    color_groups: normalizeArrayField(groups, "color_groups").map(normalizePoolGroup),
    type_groups: normalizeArrayField(groups, "type_groups").map(normalizePoolGroup),
    cmc_groups: normalizeArrayField(groups, "cmc_groups").map(normalizePoolGroup),
    rarity_groups: normalizeArrayField(groups, "rarity_groups").map(normalizePoolGroup),
    type_filter_options: normalizeArrayField(groups, "type_filter_options"),
    color_filter_options: normalizeArrayField(groups, "color_filter_options"),
  };
}

function normalizeDraftPlayerView(raw: unknown): DraftPlayerView {
  if (raw === undefined) {
    return { draft_effects: [], seats: [] } as unknown as DraftPlayerView;
  }
  if (typeof raw !== "object" || raw === null) {
    throw new Error("Invalid draft message: malformed player view");
  }
  const view = raw as Record<string, unknown>;
  const pool_groups = normalizePoolGroups(view.pool_groups);
  return {
    ...view,
    ...(pool_groups !== undefined ? { pool_groups } : {}),
    draft_effects: normalizeArrayField(view, "draft_effects"),
    seats: normalizeArrayField(view, "seats").map(normalizeSeatPublicView),
  } as unknown as DraftPlayerView;
}

/** Validate a parsed object as a DraftP2PMessage. Throws on malformed data. */
export function validateDraftMessage(raw: unknown): DraftP2PMessage {
  if (typeof raw !== "object" || raw === null || !("type" in raw)) {
    throw new Error("Invalid draft message: missing type field");
  }
  const msg = raw as { type: string };
  if (!VALID_DRAFT_TYPES.has(msg.type)) {
    throw new Error(`Invalid draft message type: ${msg.type}`);
  }
  if (msg.type === "draft_pick_with_draft_effect") {
    return validateDraftEffectPick(raw as Record<string, unknown>);
  }
  const viewMessage = raw as { type: string; view?: unknown; seats?: unknown };
  if (["draft_welcome", "draft_reconnect_ack", "draft_state_update", "draft_pick_ack"].includes(msg.type)) {
    return {
      ...viewMessage,
      view: normalizeDraftPlayerView(viewMessage.view),
    } as DraftP2PMessage;
  }
  if (msg.type === "draft_lobby_update") {
    const lobby = raw as Record<string, unknown>;
    return {
      ...viewMessage,
      seats: normalizeArrayField(lobby, "seats").map(normalizeSeatPublicView),
    } as DraftP2PMessage;
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
