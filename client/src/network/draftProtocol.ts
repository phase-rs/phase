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
 *  12 — publish the engine-derived next pairing round on the player view
 *  13 — first-contact `draft_join` and `draft_reconnect` messages carry an
 *       exact draft protocol version; reconnect rejections became typed so
 *       only an invalidated capability is cleared from durable guest recovery.
 *  14 — deck submissions carry an immutable client-generated id and the
 *       host returns an explicit durable receipt.  This makes a reloaded
 *       participant's deck outbox idempotent instead of relying on a
 *       best-effort state update.
 *  15 — two-card pick steps: `draft_pick` carries `cardInstanceIds` (CR 903.13b)
 *  16 — commander-designation inputs on the player view, shipped together:
 *       `grantable_commander_filler` (CR 903.13e) and `draft_set_code`
 *       (CR 903.13f(3)). Both are capability, not parseability: a v15 host
 *       omits `draft_set_code`, and a v16 guest reading it as absent asks the
 *       engine for partners under the DEFAULT grant. Under that grant an
 *       ordinary mono-colored Commander Masters legend is not pairable, so a
 *       legal pairing silently REPLACES the player's first commander instead.
 *       Only this version number can refuse that pairing — the guest gate in
 *       `p2p-draft-guest.ts` is exact-equality.
 *  17 — `draft_submit_deck` carries the CR 903.3 commander designation:
 *       `commanders: string[]`, required, bounded 0..2 by `validateSubmitDeck`.
 *       A PARSEABILITY break, not a capability one — a v16 producer's payload
 *       is now REFUSED by the validator rather than silently accepted with the
 *       designation absent.
 *
 *       This is the SECOND required field added to this one message, and it is
 *       independent of v14's `submissionId`: v14 made the submission idempotent
 *       across reconnect, v17 makes it carry the designation. They compose
 *       rather than supersede — `validateSubmitDeck` is the single authority
 *       for the message and enforces `submissionId`, `mainDeck`, and
 *       `commanders` together, so neither refusal can be dropped by satisfying
 *       the other.
 *  18 — `pick_steps_per_pack` on the player and spectator views (CR 903.13b):
 *       the engine-derived count of pick STEPS a pack contains,
 *       `cards_per_pack.div_ceil(cards_per_pick)`. A CAPABILITY addition, not
 *       a parseability break — a v17 host simply omits the field, and a v18
 *       guest reading it as absent renders a progress bar whose denominator
 *       the session can never reach (14 pips for a Commander pack that drains
 *       in 7 steps), which is precisely the defect this field fixes. Only this
 *       version number refuses that pairing.
 */
export const DRAFT_PROTOCOL_VERSION = 18 as const;

/** Canonical multiset fingerprint: deck order is UI-only, card counts are not. */
export function deckSubmissionFingerprint(mainDeck: readonly string[]): string {
  const counts = new Map<string, number>();
  for (const card of mainDeck) counts.set(card, (counts.get(card) ?? 0) + 1);
  return JSON.stringify([...counts.entries()].sort(([left], [right]) => (
    left < right ? -1 : left > right ? 1 : 0
  )));
}

/** The host's reason for declining a first-contact draft connection. */
export type DraftReconnectRejectionKind =
  | "ProtocolMismatch"
  | "Kicked"
  | "UnknownToken"
  | "NoReconnectWindow";

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
  /**
   * Set code of the draft these decks were built from, supplied verbatim from
   * `DraftPlayerView.draft_set_code` (draft-core/src/view.rs:302, populated by
   * `filter_for_player` at :569).  CR 903.13f(3): a draft that contained
   * Commander Masters boosters grants the partner ability, for deckbuilding
   * purposes, to any card that can be a commander by itself whose color
   * identity is one or fewer colors.  Optional: absent means no draft set code
   * is known, which the engine reads as constructed play (no grant).
   */
  draft_set_code?: string | null;
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
      draftProtocolVersion: typeof DRAFT_PROTOCOL_VERSION;
    }
  | {
      type: "draft_reconnect";
      draftToken: string;
      draftProtocolVersion: typeof DRAFT_PROTOCOL_VERSION;
    }
  | {
      type: "draft_pick";
      /** One whole CR 903.13b pick step: every card this seat drafts now. */
      cardInstanceIds: string[];
    }
  | {
      type: "draft_pick_with_draft_effect";
      effectCardInstanceId: string;
      cardInstanceIds: string[];
    }
  | {
      type: "draft_submit_deck";
      /** Stable across reconnect/reload retries of this exact payload. */
      submissionId: string;
      mainDeck: string[];
      /**
       * CR 903.3: the card names this seat designates as its commander(s).
       * CR 903.1 scopes the designation to the Commander variant, so `[]` is
       * the correct and meaningful value for every non-Commander kind, not a
       * missing field.
       */
      commanders: string[];
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
      kind: DraftReconnectRejectionKind;
      reason: string;
    }
  | {
      type: "draft_state_update";
      view: DraftPlayerView;
    }
  | {
      /** The submission is in the host's durable receipt ledger. */
      type: "draft_deck_submit_ack";
      submissionId: string;
      view: DraftPlayerView;
    }
  | {
      type: "draft_pick_ack";
      view: DraftPlayerView;
    }
  | {
      type: "draft_error";
      reason: string;
      /** Present only when this rejects one durable deck-submission command. */
      submissionId?: string;
      /** A retryable failure retains the guest outbox; rejection frees it. */
      submissionDisposition?: "Rejected" | "Retryable";
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
  "draft_deck_submit_ack",
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

/**
 * The largest `DraftProcedure.cards_per_pick` over every kind — the
 * session-free half of a `Pick` payload's bound. The EXACT per-session count is
 * owned by the engine's `apply_pick_inner`; what is bounded here is what a
 * message alone can state.
 */
// @sync-with: crates/draft-core/src/types.rs
const MAX_CARDS_PER_PICK = 2;

/**
 * CR 702.124g: "no partner ability or combination of partner abilities can
 * ever let a player have more than two commanders." The session-free half of
 * a `SubmitDeck` payload's bound — what is bounded here is what a message
 * ALONE can state. Whether a designation is REQUIRED (CR 903.3) and whether
 * the named cards are actually in the deck (CR 702.124h) are session-dependent
 * and belong to the engine's `validate_limited_deck`, never here.
 *
 * The floor is 0: CR 903.1 puts the commander designation inside the Commander
 * variant, so a deck outside it has none and an empty designation is the
 * correct, meaningful value for every non-Commander draft kind.
 */
// @sync-with: crates/draft-core/src/types.rs
const MAX_COMMANDER_DESIGNATIONS = 2;

function requireDraftCardInstanceId(value: unknown, field: string, context: string): string {
  if (
    typeof value !== "string"
    || value.length === 0
    || value.length > MAX_DRAFT_CARD_INSTANCE_ID_LENGTH
  ) {
    throw new Error(`Invalid ${context}: ${field} must be a bounded string`);
  }
  return value;
}

function validatePick(raw: Record<string, unknown>): DraftP2PMessage {
  // A `Pick` is legitimately length 1 — for all four CR 905.1a kinds, and for
  // a Commander pod's odd-pack final step (CR 903.13b) — so the bound is a
  // RANGE. Do not copy `validateDraftEffectPick`'s `[0] === [1]` distinctness
  // check: that form is correct only under its `length !== 2` early return.
  if (
    !Array.isArray(raw.cardInstanceIds)
    || raw.cardInstanceIds.length === 0
    || raw.cardInstanceIds.length > MAX_CARDS_PER_PICK
  ) {
    throw new Error(
      `Invalid draft pick: cardInstanceIds must hold 1..${MAX_CARDS_PER_PICK} cards`,
    );
  }
  const cardInstanceIds = raw.cardInstanceIds.map((cardId, index) =>
    requireDraftCardInstanceId(cardId, `cardInstanceIds[${index}]`, "draft pick"),
  );
  if (new Set(cardInstanceIds).size !== cardInstanceIds.length) {
    throw new Error("Invalid draft pick: cardInstanceIds must be distinct");
  }
  return { ...raw, type: "draft_pick", cardInstanceIds } as DraftP2PMessage;
}

function validateSubmitDeck(raw: Record<string, unknown>): DraftP2PMessage {
  // v14 (idempotency) and v17 (CR 903.3 designation) both added a REQUIRED
  // field to this one message, blind to each other. They are orthogonal, so
  // this validator enforces both rather than either superseding the other:
  // dropping the `submissionId` guard would let a reconnect retry lose its
  // durable receipt, and dropping the `commanders` guard would let a
  // pre-v17 payload through with the designation absent.
  requireDraftCardInstanceId(raw.submissionId, "submissionId", "deck submission");
  if (
    !Array.isArray(raw.mainDeck)
    || !raw.mainDeck.every((card) => typeof card === "string")
  ) {
    throw new Error("Invalid draft deck submission");
  }
  // The bound is a RANGE with a floor of ZERO, and the floor is the part that
  // must not be copied from `validatePick`: a pick step always takes at least
  // one card, but CR 903.1 puts the commander designation inside the Commander
  // variant, so a Premier / Traditional / Sealed pod legitimately designates
  // none. `[].map(...)` never invokes its callback, so this condition is the
  // ONLY thing that decides the empty case.
  if (
    !Array.isArray(raw.commanders)
    || raw.commanders.length > MAX_COMMANDER_DESIGNATIONS
  ) {
    throw new Error(
      `Invalid deck submission: commanders must hold 0..${MAX_COMMANDER_DESIGNATIONS} cards`,
    );
  }
  const commanders = raw.commanders.map((name, index) =>
    requireDraftCardInstanceId(name, `commanders[${index}]`, "deck submission"),
  );
  // NO distinctness check, in EITHER landed form. CR 702.124h designates two
  // legendary CARDS, and `validate_limited_deck`'s step-5 multiset guard exists
  // precisely because two copies of one name can be legal input — the
  // CR 903.13e filler case is exactly that. Copy neither
  // `validateDraftEffectPick`'s `[0] === [1]` nor `validatePick`'s `new Set(...)`.
  //
  // `mainDeck`'s guard above is deliberately a TYPE guard only (an array of
  // strings), with no entry-count cap. A deck-SIZE refusal stays with the
  // engine: `draftPeerSession`'s decode `.catch` drops a validator throw, so
  // raising a size refusal here would convert an engine-loud refusal (which
  // reaches the guest as `draft_error`) into a wire-silent one.
  return { ...raw, type: "draft_submit_deck", commanders } as DraftP2PMessage;
}

function validateDraftEffectPick(raw: Record<string, unknown>): DraftP2PMessage {
  const effectCardInstanceId = requireDraftCardInstanceId(
    raw.effectCardInstanceId,
    "effectCardInstanceId",
    "draft-effect pick",
  );
  if (!Array.isArray(raw.cardInstanceIds) || raw.cardInstanceIds.length !== 2) {
    throw new Error("Invalid draft-effect pick: cardInstanceIds must contain exactly two cards");
  }
  const cardInstanceIds = raw.cardInstanceIds.map((cardId, index) =>
    requireDraftCardInstanceId(cardId, `cardInstanceIds[${index}]`, "draft-effect pick"),
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
  if (msg.type === "draft_pick") {
    return validatePick(raw as Record<string, unknown>);
  }
  if (msg.type === "draft_submit_deck") {
    return validateSubmitDeck(raw as Record<string, unknown>);
  }
  if (msg.type === "draft_deck_submit_ack") {
    const acknowledgement = raw as Record<string, unknown>;
    requireDraftCardInstanceId(acknowledgement.submissionId, "submissionId", "deck acknowledgement");
    if (typeof acknowledgement.view !== "object" || acknowledgement.view === null) {
      throw new Error("Invalid draft deck acknowledgement");
    }
    return {
      ...acknowledgement,
      view: normalizeDraftPlayerView(acknowledgement.view),
    } as DraftP2PMessage;
  }
  if (msg.type === "draft_error") {
    const error = raw as Record<string, unknown>;
    if (error.submissionDisposition !== undefined) {
      if (error.submissionDisposition !== "Rejected" && error.submissionDisposition !== "Retryable") {
        throw new Error("Invalid draft submission error disposition");
      }
      requireDraftCardInstanceId(error.submissionId, "submissionId", "deck submission error");
    }
    return error as DraftP2PMessage;
  }
  if (msg.type === "draft_reconnect_rejected") {
    const rejection = raw as Record<string, unknown>;
    // Pre-v13 frames carried only a string reason. They are never a capability-revocation
    // signal: normalize it to the safe terminal outcome that preserves the
    // guest's recovery records and asks the user to refresh.
    if (rejection.kind === undefined && typeof rejection.reason === "string") {
      return {
        ...rejection,
        type: "draft_reconnect_rejected",
        kind: "ProtocolMismatch",
      } as DraftP2PMessage;
    }
    if (
      (rejection.kind !== "ProtocolMismatch"
        && rejection.kind !== "Kicked"
        && rejection.kind !== "UnknownToken"
        && rejection.kind !== "NoReconnectWindow")
      || typeof rejection.reason !== "string"
    ) {
      throw new Error("Invalid draft reconnect rejection");
    }
    return rejection as DraftP2PMessage;
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
