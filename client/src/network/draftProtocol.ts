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
  DraftRarityGroupKind,
  DraftSourceView,
  PackDistribution,
  SeatPublicView,
  SharedStackDecisionRecord,
  SharedStackDecisionView,
  SharedStackPileDecision,
  SharedStackPileView,
  SharedStackRefusal,
  SharedStackView,
} from "../adapter/draft-adapter";
import type { DeckCardCount, MatchConfig, MatchScore } from "../adapter/types";
import {
  MAX_MATERIALIZED_VIRTUAL_BASICS,
  MAX_DRAFT_WORKSPACE_NETWORK_PLACEMENTS,
  isPlainRecord,
  validateWorkspaceState,
  type DraftWorkspaceState,
} from "../components/draft/workspace/types";
import { BASIC_LAND_NAMES } from "../constants/game";
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
 *       (CR 903.13f(3)) — both since replaced by their plural forms in v20.
 *       Both were capability, not parseability: a v15 host
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
 *  19 — per-pack booster shape on the player and spectator views, shipped
 *       together because a progress display reads all three as one contract:
 *       `pack_sizes` (cards in each booster, in pack order), `pack_set_codes`
 *       (the set filling each booster), and `pack_pick_steps` (CR 903.13b pick
 *       STEPS in each booster — the per-pack counterpart of v18's scalar
 *       `pick_steps_per_pack`). A multi-set draft opens a different set each
 *       round and those boosters differ in size, so every v18 field that
 *       described "the pack" now has a per-pack form.
 *
 *       A CAPABILITY addition, not a parseability break — a v18 host omits all
 *       three, and a v19 guest reading them as absent falls back to the v18
 *       scalars, which describe the CURRENT booster. That fallback is correct
 *       for the single-set drafts a v18 host can run and wrong only for the
 *       multi-set drafts it cannot, so the degradation is bounded. Only this
 *       version number refuses the pairing.
 *  20 — CR 903.13's deck-construction concessions became PLURAL on the player
 *       and spectator views, shipped together because both describe one
 *       question: `grantable_commander_fillers` replaces v16's
 *       `grantable_commander_filler` (CR 903.13e) and `draft_set_codes`
 *       replaces its `draft_set_code` (CR 903.13f(3)).
 *
 *       A RENAME, so a parseability break in both directions rather than a
 *       capability addition: a v19 host sends only the singular spellings and a
 *       v20 guest reads BOTH new fields as absent — no filler offered in the
 *       deckbuilder and no partner grant queried — while a v19 guest reading a
 *       v20 host's view does the same. Silently losing a grant the rules make
 *       is precisely the defect this version exists to fix, so it is refused at
 *       the pairing gate instead.
 *
 *       Plural because CR 903.13e/f condition each grant on what the draft
 *       CONTAINED and state their conditions independently: a draft opening
 *       Commander Masters and Battle for Baldur's Gate boosters concedes The
 *       Prismatic Piper AND Faceless One, and keeps the CR 903.13f(3) partner
 *       grant. Multi-set selection (v19) is what made that draft reachable, so
 *       the singular fields could no longer name the answer. The engine takes
 *       the union in `draft_set_concessions_for`; the client still never learns
 *       which sets grant what.
 *
 *  21 — complete, validated per-seat workspace snapshots and workspace pool
 *       metadata. This is additive and retains the v13–20 contract.
 *  22 — durable, session-bound participant leave handshake. `draft_leave`
 *       and its acknowledgement both carry the exact protocol version and
 *       capability token, so a guest clears recoverable state only after the
 *       host has durably revoked that exact seat.
 *  23 — `pick_selection_mode` on player views. A v22 peer lacks the
 *       engine-owned selection procedure and can render Commander Draft as a
 *       direct selection, so the first-contact gate must refuse the pairing.
 *  24 — the merged P2P contract requires both v23's `pick_selection_mode` on
 *       player views and `active_pack_count` on public seats: the engine-owned
 *       `0|1` presence signal for a seat's active pack. It never reveals a
 *       pack's cards or remaining-card count. The independently released
 *       `active_pack_count` contract had also claimed v23, so v24 explicitly
 *       rejects a v23 first contact rather than conflating the two shapes.
 *  25 — player views carry `launch_capability`, the engine-authorized
 *       post-draft multiplayer launch. A v24 peer lacks this procedure-owned
 *       capability and would otherwise infer from `DraftKind` or hide the
 *       launch entirely, so the exact first-contact gate refuses the pairing.
 *  26 — player views carry required `commanders_required`, the exact
 *       procedure-owned designation count. A v25 peer lacks it and would
 *       otherwise infer designation capability from `DraftKind`.
 *  27 — `draft_commander_launch`, the Host → Guest N-seat Commander game
 *       launch. A v26 peer has no arm for it and would silently drop the
 *       message, stranding that seat on the completed pod with no way to
 *       join the game every other seat is playing.
 *  28 — authoritative per-seat Auto Lands requests and correlated results,
 *       including explicit rejection so an older peer cannot leave a waiter
 *       pending after an otherwise exact handshake.
 *  29 — a Cube match launch run on the host's own seat names the original
 *       entries as its in-game booster source; a guest authority and public
 *       draft views never receive them.
 *       Older hosts would silently discard the in-game booster source.
 *  30 — shared-stack pile decisions. `draft_pile_decision` carries a pile
 *       index and a typed `"Take"`/`"Decline"`, and player views carry
 *       `shared_stack` (engine-published pile totals, the active seat's
 *       revealed prefix, and the per-pile per-decision legality the reducer
 *       enforces) plus the advisory `play_first_chooser`. A CAPABILITY
 *       addition with a silent-drop hazard, like 27: a v29 host has no arm
 *       for `draft_pile_decision` and would drop it, leaving the guest's turn
 *       hung with no error, and a v29 guest reading a v30 view would render a
 *       Winston pod with no piles. Both directions are refusable only at this
 *       gate. Other kinds are unaffected: `shared_stack` and
 *       `play_first_chooser` are absent from every non-Winston view, which is
 *       also why their TypeScript mirrors are optional.
 *
 *       AMENDED IN PLACE, NOT BUMPED. `shared_stack.history` — a bounded,
 *       oldest-first vector of `SharedStackDecisionRecord` (seat, pile,
 *       decision, pile_size; no card, deliberately and permanently) — was
 *       added after this entry was written. 30 is unreleased upstream
 *       (upstream is 29), so no peer has ever spoken a 30 without it and there
 *       is no version for a bump to separate. The Rust field carries
 *       `#[serde(default)]` for locally persisted snapshots; this TypeScript
 *       mirror is REQUIRED rather than optional, because `shared_stack` is
 *       engine-built on every frame and a client never constructs one. Other
 *       kinds stay unaffected for the reason above: no non-Winston view
 *       carries a `shared_stack` at all.
 *
 *       AMENDED IN PLACE AGAIN, for the same reason and on the same evidence
 *       (30 is unreleased upstream; upstream is 29, so no peer has ever spoken
 *       a 30 without these):
 *         • `shared_stack.forced_draw` — the card the VIEWER's own final-pile
 *           decline drew, or null. The one private field on a `SharedStackView`
 *           and the only one that names a card off the face-down stack; the
 *           engine sends it to that seat alone, never to the opponent and never
 *           to either spectator visibility.
 *         • `seats[*].drafted_card_count` — how many cards each seat has
 *           drafted. Unlike everything else in 30 this is NOT Winston-scoped:
 *           it rides every kind's public seat list. It is a count and never an
 *           identity, and it is already public in every kind (a pick-and-pass
 *           seat's total follows from the pick number; a shared stack's is
 *           visible across the table).
 *         • `DraftPlayerView.distribution` — how the procedure delivers
 *           boosters to seats. Like `drafted_card_count` this is NOT
 *           Winston-scoped: it is a required field on every kind's player
 *           view, which crosses this wire in `draft_welcome`,
 *           `draft_reconnect_ack`, `draft_state_update` and `draft_pick_ack`.
 *           Published for the same reason `launch_capability` is, and
 *           deliberately not status-gated, so a surface that outlives the
 *           drafting phase can still tell a pile pod from a passing one.
 *       Of these, only `SharedStackState::forced_draws` carries
 *       `#[serde(default)]` — it is the one that rides a persisted snapshot.
 *       `drafted_card_count` and `distribution` carry no serde attribute and
 *       need none: they live on view types the engine builds fresh for every
 *       frame and never deserializes from an older snapshot. All three
 *       TypeScript mirrors are REQUIRED rather than optional, for that same
 *       reason — a client never constructs one of these views.
 */
export const DRAFT_PROTOCOL_VERSION = 30 as const;

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
   * In-game booster source. Original Cube entries, including duplicates, only
   * when the launch's engine runs on the host's own seat; null for a guest
   * authority, which must never learn the private multiset and so opens
   * ordinary set boosters.
   */
  booster_pack_pool?: string[] | null;
  /**
   * Every set whose draft boosters these decks' draft CONTAINED, supplied
   * verbatim from `DraftPlayerView.draft_set_codes`, populated by
   * `filter_for_player`.  CR 903.13f(3): a draft that contained Commander
   * Masters boosters grants the partner ability, for deckbuilding purposes, to
   * any card that can be a commander by itself whose color identity is one or
   * fewer colors.  A LIST because that rule asks about CONTAINMENT, so a
   * mixed-set draft must carry every set it contained rather than one chosen
   * representative.  Optional: absent or empty means no draft set is known,
   * which the engine reads as constructed play (no grant).
   */
  draft_set_codes?: string[] | null;
}

/**
 * CR 903.13a: every deck a completed Commander pod's launch needs, computed in
 * one pass over the pod's seat plan so each deck is synthesized exactly once.
 *
 * Purely a return type — nothing here travels on the wire. `draftSetCodes` is
 * deliberately absent: it belongs to the `DraftCommanderLaunch` MESSAGE, and
 * duplicating it here would give the launch two sources of truth.
 */
export interface CommanderSeatDecks {
  hostDeck: DraftDeckPayload;
  /**
   * Every LIVE human seat's own deck, including `localSeat`.
   *
   * `localSeat`'s entry is the SAME OBJECT as `hostDeck`, never a second
   * synthesis of it. That is what lets the sender address every recipient from
   * this one list without re-reading the draft session — the exactly-once
   * export invariant would otherwise force a second `exportDraftSession()`.
   */
  liveSeatDecks: Array<{ seat: number; deck: DraftDeckPayload }>;
  engineSeatDecks: Array<{ seat: number; deck: DraftDeckPayload }>;
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

/**
 * A completed Commander pod's launch into ONE shared N-seat game.
 *
 * A SIBLING of `DraftMatchLaunch`, never a fourth arm of it: every
 * `DraftMatchLaunch` arm is pairwise/tournament-shaped (`matchId`, `round`,
 * `binding`, `localSeat`/`opponentSeat`) and flows into tournament settlement,
 * none of which an N-seat Commander launch has.
 */
export interface DraftCommanderLaunch {
  /** Shared game id: host and every guest install the runtime under the same id. */
  gameId: string;
  /** PeerJS room code the host is hosting the Commander game on. */
  roomCode: string;
  /** This seat's own drafted, submitted deck. */
  localDeck: DraftDeckPayload;
  /** Total seats in the game — pod seats, humans and engine-piloted alike. */
  playerCount: number;
  /**
   * CR 903.13f(3): every set the draft contained. REQUIRED but nullable,
   * deliberately unlike the optional `draft_set_codes` on `DraftMatchDeckPayload`
   * — the host always constructs this field.
   *
   * Carry the host's own value through; do not substitute a literal. `null` is
   * this wire's declared "no sets" value, so it is what an absent list is
   * spelled as here. That is a contract-vocabulary choice, NOT a rules one: the
   * engine reads `null`, `undefined` and `[]` identically as the empty array,
   * i.e. constructed play (`engine_wasm.d.ts`; `deserialize_draft_set_codes`
   * maps `Absent => Vec::new()`). Substituting `[]` would not change the grant —
   * it would just assert "the draft contained zero sets" where the host
   * already knows the answer.
   */
  draftSetCodes: string[] | null;
}

// ── Message Types ──────────────────────────────────────────────────────

/**
 * Discriminated union of all draft-specific P2P messages.
 *
 * Flow:
 *   Guest → Host: `draft_join`, `draft_reconnect`, `draft_pick`, `draft_pick_with_draft_effect`, `draft_pile_decision`, `draft_submit_deck`,
 *                 `draft_request_advance`, `draft_workspace_update`, `draft_suggest_lands`, `draft_leave`
 *   Host → Guest: `draft_welcome`, `draft_reconnect_ack`, `draft_reconnect_rejected`,
 *                 `draft_state_update`, `draft_pick_ack`, `draft_suggest_lands_result`, `draft_suggest_lands_rejected`, `draft_error`,
 *                 `draft_kicked`, `draft_pairing`, `draft_match_result`,
 *                 `draft_paused`, `draft_resumed`, `draft_lobby_update`,
 *                 `draft_host_left`, `draft_timer_sync`, `draft_match_start`,
 *                 `draft_commander_launch`, `draft_leave_ack`
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
      /**
       * One whole shared-stack turn decision. Names NO cards: a decline adds
       * nothing to any pool, which is exactly why the engine publishes
       * `shared_stack.decisions` as the acknowledgement detector.
       */
      type: "draft_pile_decision";
      /**
       * The pile the guest believes it is deciding on. An
       * optimistic-concurrency check against the ENGINE's cursor, not a
       * selector: a duplicate frame naming a stale pile is refused
       * `PileNotActive` rather than silently applied to the next pile.
       */
      pile: number;
      decision: SharedStackPileDecision;
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
  | {
      type: "draft_workspace_update";
      workspaceState: DraftWorkspaceState;
    }
  | {
      /** The host derives spells and seat from its retained authoritative state. */
      type: "draft_suggest_lands";
      requestId: string;
    }
  | {
      /** Explicit participant exit, bound to the currently authenticated seat. */
      type: "draft_leave";
      draftProtocolVersion: typeof DRAFT_PROTOCOL_VERSION;
      draftToken: string;
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
      workspaceState: DraftWorkspaceState | null;
    }
  | {
      type: "draft_reconnect_ack";
      draftProtocolVersion: typeof DRAFT_PROTOCOL_VERSION;
      seatIndex: number;
      view: DraftPlayerView;
      draftCode: string;
      workspaceState: DraftWorkspaceState | null;
    }
  | {
      type: "draft_reconnect_rejected";
      kind: DraftReconnectRejectionKind;
      reason: string;
    }
  | {
      /** Sent only after the host has durably revoked this exact seat. */
      type: "draft_leave_ack";
      draftProtocolVersion: typeof DRAFT_PROTOCOL_VERSION;
      draftToken: string;
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
      type: "draft_suggest_lands_result";
      requestId: string;
      lands: Record<string, number>;
    }
  | {
      type: "draft_suggest_lands_rejected";
      requestId: string;
      reason: string;
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
  | {
      /** Host → Guest: instructs the seat to join the pod's shared Commander game. */
      type: "draft_commander_launch";
      launch: DraftCommanderLaunch;
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
  "draft_pile_decision",
  "draft_submit_deck",
  "draft_workspace_update",
  "draft_suggest_lands",
  "draft_leave",
  "draft_welcome",
  "draft_reconnect_ack",
  "draft_reconnect_rejected",
  "draft_leave_ack",
  "draft_state_update",
  "draft_deck_submit_ack",
  "draft_pick_ack",
  "draft_suggest_lands_result",
  "draft_suggest_lands_rejected",
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
  "draft_commander_launch",
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
const MAX_DRAFT_SUGGESTION_REASON_LENGTH = 1024;

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

/**
 * The largest `PackDistribution::SharedStackPiles::pile_count` over every kind
 * — the session-free half of a pile decision's bound, mirroring the engine's
 * `MAX_SHARED_STACK_PILES`.
 *
 * A ceiling, not the live pile count: the EXACT pile a decision may name is
 * the engine's cursor, and `shared_stack::refusal_for` is the single authority
 * that judges it. What is bounded here is what a message ALONE can state.
 *
 * Pinned against the Rust constant by `scripts/check-protocol-version.mjs`,
 * which reads BOTH literals and compares them. That is a real cross-language
 * pin and it is the only one: this module's own test asserts pile 3 rejected
 * and pile 2 accepted, which pins the constant against ITSELF and would pass at
 * any value the two sides happened to share. The engine's figure is derived
 * from the procedure table, so a future 4-pile row moves it with every Rust
 * test still green — and without the script's comparison the transport would
 * silently refuse legal pile-3 decisions.
 */
// @sync-with: crates/draft-core/src/types.rs `MAX_SHARED_STACK_PILES`
const MAX_SHARED_STACK_PILES = 3;

/**
 * Every shared-stack decision a message may name.
 *
 * A tuple the type is checked against, so a decision added to
 * `SharedStackPileDecision` cannot leave this bound silently narrow — the same
 * device `DRAFT_KINDS` uses, for the same reason.
 */
// @sync-with: crates/draft-core/src/types.rs `SharedStackPileDecision::ALL`
const SHARED_STACK_PILE_DECISIONS: readonly SharedStackPileDecision[] = ["Take", "Decline"];

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

function requireExactOwnKeyRecord(
  raw: unknown,
  expectedKeys: readonly string[],
  context: string,
): Record<string, unknown> {
  if (!isPlainRecord(raw)) {
    throw new Error(`Invalid ${context}: must be a plain record`);
  }
  const keys = Reflect.ownKeys(raw);
  if (
    keys.length !== expectedKeys.length
    || !expectedKeys.every((expected) => Object.prototype.hasOwnProperty.call(raw, expected))
    || !keys.every((key) =>
      typeof key === "string"
      && expectedKeys.includes(key)
      && Object.prototype.propertyIsEnumerable.call(raw, key))
  ) {
    throw new Error(`Invalid ${context}: invalid fields`);
  }
  return raw;
}

function validateSuggestedLands(raw: unknown): Record<string, number> {
  if (!isPlainRecord(raw)) {
    throw new Error("Invalid land suggestion result: lands must be a plain record");
  }
  const keys = Reflect.ownKeys(raw);
  if (keys.length > BASIC_LAND_NAMES.size) {
    throw new Error("Invalid land suggestion result: too many land entries");
  }
  const lands: Record<string, number> = {};
  for (const key of keys) {
    if (
      typeof key !== "string"
      || !Object.prototype.propertyIsEnumerable.call(raw, key)
      || !BASIC_LAND_NAMES.has(key)
    ) {
      throw new Error("Invalid land suggestion result: land names must be canonical basics");
    }
    const count = raw[key];
    if (
      typeof count !== "number"
      || !Number.isSafeInteger(count)
      || count < 0
      || count > MAX_MATERIALIZED_VIRTUAL_BASICS
    ) {
      throw new Error("Invalid land suggestion result: land counts must be bounded nonnegative integers");
    }
    lands[key] = count;
  }
  return lands;
}

function validateSuggestLandsMessage(raw: unknown, type: string): DraftP2PMessage {
  if (type === "draft_suggest_lands") {
    const request = requireExactOwnKeyRecord(raw, ["type", "requestId"], "land suggestion request");
    return {
      type: "draft_suggest_lands",
      requestId: requireDraftCardInstanceId(request.requestId, "requestId", "land suggestion request"),
    };
  }
  if (type === "draft_suggest_lands_result") {
    const result = requireExactOwnKeyRecord(raw, ["type", "requestId", "lands"], "land suggestion result");
    return {
      type: "draft_suggest_lands_result",
      requestId: requireDraftCardInstanceId(result.requestId, "requestId", "land suggestion result"),
      lands: validateSuggestedLands(result.lands),
    };
  }
  const rejection = requireExactOwnKeyRecord(raw, ["type", "requestId", "reason"], "land suggestion rejection");
  if (
    typeof rejection.reason !== "string"
    || rejection.reason.length === 0
    || rejection.reason.length > MAX_DRAFT_SUGGESTION_REASON_LENGTH
  ) {
    throw new Error("Invalid land suggestion rejection: reason must be a bounded string");
  }
  return {
    type: "draft_suggest_lands_rejected",
    requestId: requireDraftCardInstanceId(rejection.requestId, "requestId", "land suggestion rejection"),
    reason: rejection.reason,
  };
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

/**
 * Bound an untrusted `draft_pile_decision` payload.
 *
 * A TRANSPORT bound, not a legality check, and the distinction is the whole
 * point: this function receives only the message and can never consult the
 * session, so the EXACT check is `shared_stack::refusal_for`'s, inside the
 * engine. All that is refused here is a payload no session could make
 * meaningful — a pile index outside the mirrored ceiling, or a decision that
 * is not one of the two the axis defines.
 */
function validatePileDecision(raw: Record<string, unknown>): DraftP2PMessage {
  if (
    typeof raw.pile !== "number"
    || !Number.isInteger(raw.pile)
    || raw.pile < 0
    || raw.pile >= MAX_SHARED_STACK_PILES
  ) {
    throw new Error(
      `Invalid pile decision: pile must be an integer in 0..${MAX_SHARED_STACK_PILES - 1}`,
    );
  }
  if (
    typeof raw.decision !== "string"
    || !SHARED_STACK_PILE_DECISIONS.some((decision) => decision === raw.decision)
  ) {
    throw new Error(
      `Invalid pile decision: decision must be one of ${SHARED_STACK_PILE_DECISIONS.join(", ")}`,
    );
  }
  return { ...raw, type: "draft_pile_decision" } as DraftP2PMessage;
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

function normalizeSeatPublicView(raw: unknown, position: number): SeatPublicView {
  if (typeof raw !== "object" || raw === null) {
    throw new Error("Invalid draft message: malformed public seat");
  }
  const seat = raw as Record<string, unknown>;
  // `seat_index` IS the position, exactly as a pile's `index` is: both engine
  // builders write `seat_index: i as u8` off `.enumerate()`
  // (`view.rs`), and `buildSeatPublicViews` counts `i` to `podSize`.
  //
  // This matters more than the pile rule, because `seat_index` is the address
  // the CLIENT resolves seats through -- `seats.find(s => s.seat_index === n)`
  // names whose turn it is, keys the React rows, tests for the local seat, and
  // picks the kick target. Two seats sharing an index makes them answer to one
  // address: the active player renders under a fallback name while another
  // seat's name is drawn twice, and one lobby row's kick hits both.
  if (seat.seat_index !== position) {
    throw new Error(
      `Invalid draft message: seats[${position}].seat_index must equal its position`,
    );
  }
  if (
    !Number.isInteger(seat.active_pack_count)
    || (seat.active_pack_count !== 0 && seat.active_pack_count !== 1)
  ) {
    throw new Error("Invalid draft message: active_pack_count must be an integer 0 or 1");
  }
  // Bounded only as a non-negative integer, unlike `active_pack_count`'s exact
  // 0-or-1: this is a real count with no product ceiling. A cube shared-stack
  // pod's per-seat total rises with the host's own `cards_per_pack`, so any
  // fixed bound written here would be a second, wrong authority on pool size.
  if (!Number.isInteger(seat.drafted_card_count) || (seat.drafted_card_count as number) < 0) {
    throw new Error("Invalid draft message: drafted_card_count must be a non-negative integer");
  }
  // A CLOSED SET, like `refusal` in `normalizeSharedStackDecisionView`. The
  // dashboard now indexes a total `Record` with this value, so an out-of-union
  // variant renders an EMPTY status rather than the raw key text it used to --
  // failing silently instead of visibly. Unreachable while both transports
  // refuse a protocol mismatch, which is exactly why it should be refused here
  // rather than left to a renderer to absorb.
  if (!PICK_STATUSES.includes(seat.pick_status as SeatPublicView["pick_status"])) {
    throw new Error("Invalid draft message: pick_status must be a known status");
  }
  return {
    ...seat,
    seat_index: position,
    active_pack_count: seat.active_pack_count,
    drafted_card_count: seat.drafted_card_count,
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

const VALID_DRAFT_RARITY_GROUP_KINDS = new Set<DraftRarityGroupKind>([
  "mythic",
  "rare",
  "uncommon",
  "common",
  "rarity_other",
]);

function validateWorkspaceCapabilities(raw: unknown): Record<string, unknown> {
  if (typeof raw !== "object" || raw === null || Array.isArray(raw)) {
    throw new Error("Invalid draft message: workspace_capabilities must be an object");
  }
  const capabilities = raw as Record<string, unknown>;
  if (!("rarity_group_order" in capabilities)) {
    throw new Error("Invalid draft message: workspace_capabilities requires rarity_group_order");
  }
  const order = capabilities.rarity_group_order;
  if (
    order !== null
    && (!Array.isArray(order)
      || !order.every((kind) =>
        typeof kind === "string"
        && VALID_DRAFT_RARITY_GROUP_KINDS.has(kind as DraftRarityGroupKind)))
  ) {
    throw new Error("Invalid draft message: rarity_group_order is malformed");
  }
  return capabilities;
}

function validateWorkspaceRowClassification(raw: unknown): Record<string, unknown> {
  if (typeof raw !== "object" || raw === null || Array.isArray(raw)) {
    throw new Error("Invalid draft message: workspace_row_classification must be an object");
  }
  const rows = raw as Record<string, unknown>;
  for (const field of ["creature_instance_ids", "noncreature_instance_ids"] as const) {
    if (!(field in rows)) {
      throw new Error(`Invalid draft message: workspace_row_classification requires ${field}`);
    }
    if (!Array.isArray(rows[field]) || !rows[field].every((id) => typeof id === "string")) {
      throw new Error(`Invalid draft message: ${field} must be a string array`);
    }
  }
  return rows;
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
    workspace_capabilities: "workspace_capabilities" in groups
      ? validateWorkspaceCapabilities(groups.workspace_capabilities)
      : { rarity_group_order: null },
    workspace_row_classification: "workspace_row_classification" in groups
      ? validateWorkspaceRowClassification(groups.workspace_row_classification)
      : { creature_instance_ids: [], noncreature_instance_ids: [] },
  };
}

/**
 * Accept only the redacted source view shape. In particular this projection
 * never spreads a host-provided Chaos layout, because doing so would make an
 * `assignments` matrix observable to a guest even if no UI rendered it.
 */
function normalizeDraftSourceView(raw: unknown): DraftSourceView | undefined {
  if (raw === undefined || raw === null) return undefined;
  if (typeof raw !== "object") throw new Error("Invalid draft message: malformed source view");
  const source = raw as Record<string, unknown>;
  if (source.type === "Cube") {
    if (typeof source.data !== "object" || source.data === null) {
      throw new Error("Invalid draft message: malformed cube source view");
    }
    const data = source.data as Record<string, unknown>;
    if (typeof data.id !== "string" || typeof data.name !== "string") {
      throw new Error("Invalid draft message: malformed cube source view");
    }
    return { type: "Cube", data: { id: data.id, name: data.name } };
  }
  if (source.type !== "Set" || typeof source.data !== "object" || source.data === null) {
    throw new Error("Invalid draft message: malformed set source view");
  }
  const data = source.data as Record<string, unknown>;
  if (typeof data.layout !== "object" || data.layout === null) {
    throw new Error("Invalid draft message: malformed set layout view");
  }
  const layout = data.layout as Record<string, unknown>;
  if (typeof layout.UniformByRound === "object" && layout.UniformByRound !== null) {
    const uniform = layout.UniformByRound as Record<string, unknown>;
    if (!Array.isArray(uniform.codes) || !uniform.codes.every((code) => typeof code === "string")) {
      throw new Error("Invalid draft message: malformed uniform source view");
    }
    return { type: "Set", data: { layout: { UniformByRound: { codes: [...uniform.codes] } } } };
  }
  if (typeof layout.Chaos !== "object" || layout.Chaos === null) {
    throw new Error("Invalid draft message: malformed Chaos source view");
  }
  const chaos = layout.Chaos as Record<string, unknown>;
  const optionalString = (value: unknown): string | null => {
    if (value === null) return null;
    if (typeof value === "string") return value;
    throw new Error("Invalid draft message: malformed Chaos source view");
  };
  const optionalStringArray = (value: unknown): string[] | null => {
    if (value === null) return null;
    if (Array.isArray(value) && value.every((code) => typeof code === "string")) return [...value];
    throw new Error("Invalid draft message: malformed Chaos source view");
  };
  if (!Array.isArray(chaos.candidate_codes) || !chaos.candidate_codes.every((code) => typeof code === "string")) {
    throw new Error("Invalid draft message: malformed Chaos source view");
  }
  return {
    type: "Set",
    data: {
      layout: {
        Chaos: {
          candidate_codes: [...chaos.candidate_codes],
          current_pack_code: optionalString(chaos.current_pack_code),
          completed_own_pack_codes: optionalStringArray(chaos.completed_own_pack_codes),
          actual_set_codes: optionalStringArray(chaos.actual_set_codes),
        },
      },
    },
  };
}

// ---------------------------------------------------------------------------
// v30 shared-stack wire validation.
//
// Everything below exists because `normalizeDraftPlayerView` used to validate a
// handful of named fields and then spread the rest of the frame through
// `as unknown as DraftPlayerView`. A cast is not a check: `distribution`,
// `shared_stack` and `play_first_chooser` crossed the transport boundary
// entirely unvalidated, so a malformed or hostile peer frame reached the store
// and the renderer with its declared TypeScript shape and none of its content.
// The draft protocol is compared for EXACT equality at the handshake
// (`p2p-draft-guest.ts`), so a v30 peer that omits a v30 field is malformed
// rather than old, and these validators are strict accordingly.
//
// They validate SHAPE and closed sets, never legality: `legality` entries carry
// the engine's verdict and are checked for being well-formed, not for being
// right. Nothing here recomputes a rule.
// ---------------------------------------------------------------------------

const PICK_STATUSES: readonly SeatPublicView["pick_status"][] = [
  "Pending",
  "Picked",
  "Waiting",
  "TimedOut",
  "NotDrafting",
];

const SHARED_STACK_DECISIONS: readonly SharedStackPileDecision[] = ["Take", "Decline"];
const SHARED_STACK_REFUSALS: readonly SharedStackRefusal[] = [
  "PileNotActive",
  "PileEmpty",
  "NoGuaranteedCard",
];

function requireObject(raw: unknown, context: string): Record<string, unknown> {
  if (typeof raw !== "object" || raw === null || Array.isArray(raw)) {
    throw new Error(`Invalid draft message: malformed ${context}`);
  }
  return raw as Record<string, unknown>;
}

/**
 * A non-negative integer. The engine's `usize` counts -- `total_cards`,
 * `main_stack_remaining`, a pile's `total`, a record's `pile_size` -- are this
 * shape, and deliberately have no ceiling: a cube pod's card count is the
 * host's to choose, so any bound written here would be a second, wrong
 * authority on pool size.
 */
function requireCount(record: Record<string, unknown>, field: string, context: string): number {
  const value = record[field];
  if (typeof value !== "number" || !Number.isInteger(value) || value < 0) {
    throw new Error(
      `Invalid draft message: ${context}.${field} must be a non-negative integer`,
    );
  }
  return value;
}

/**
 * An integer inside `[min, max]`, for the fields the engine declares as sized.
 *
 * Seat and pile ADDRESSES are `u8` in `draft-core` (`SharedStackView.active_seat`
 * and `.active_pile`, `SharedStackPileView.index`, `SharedStackDecisionRecord`'s
 * `seat` and `pile`); `decisions` is a `u32`. A frame carrying a value outside
 * those ranges is one the engine could not have produced, so it is refused here
 * rather than trusted as frontend state.
 */
function requireBoundedInt(
  record: Record<string, unknown>,
  field: string,
  context: string,
  min: number,
  max: number,
): number {
  const value = record[field];
  if (typeof value !== "number" || !Number.isInteger(value) || value < min || value > max) {
    throw new Error(
      `Invalid draft message: ${context}.${field} must be an integer in [${min}, ${max}]`,
    );
  }
  return value;
}

const U8_MAX = 255;
const U32_MAX = 4294967295;

/**
 * A field the engine ALWAYS serializes, refused when the frame omits it.
 *
 * `history` and `forced_draw` carry no `skip_serializing_if` in
 * `draft-core::view`, so every real frame has both -- `forced_draw` as `null`
 * when the viewer drew nothing. Defaulting them on absence turned a truncated
 * or hostile frame into a plausible-looking one: a missing `history` read as
 * "no decisions yet" rather than as a frame that should never have been
 * accepted.
 */
function requirePresent(record: Record<string, unknown>, field: string, context: string): unknown {
  if (!(field in record)) {
    throw new Error(`Invalid draft message: ${context}.${field} is required`);
  }
  return record[field];
}

function normalizePackDistribution(raw: unknown): PackDistribution {
  if (raw === "PickAndPass" || raw === "AllAtOnce") return raw;
  const tagged = requireObject(raw, "distribution");
  const shared = tagged.SharedStackPiles;
  if (shared === undefined || Object.keys(tagged).length !== 1) {
    throw new Error("Invalid draft message: distribution must be a known pack distribution");
  }
  const piles = requireObject(shared, "distribution.SharedStackPiles");
  // NONZERO. `shared_stack::piles_needed` refuses a zero pile count outright --
  // "a shared-stack distribution must deal at least one pile" -- so a frame
  // declaring zero describes a session the reducer would never have built.
  const pile_count = requireBoundedInt(
    piles,
    "pile_count",
    "distribution.SharedStackPiles",
    1,
    U8_MAX,
  );
  return { SharedStackPiles: { pile_count } };
}

function normalizeSharedStackDecisionView(raw: unknown, context: string): SharedStackDecisionView {
  const entry = requireObject(raw, context);
  if (!SHARED_STACK_DECISIONS.includes(entry.decision as SharedStackPileDecision)) {
    throw new Error(`Invalid draft message: ${context}.decision must be Take or Decline`);
  }
  if (
    entry.refusal !== null
    && !SHARED_STACK_REFUSALS.includes(entry.refusal as SharedStackRefusal)
  ) {
    throw new Error(`Invalid draft message: ${context}.refusal must be null or a known refusal`);
  }
  return {
    decision: entry.decision as SharedStackPileDecision,
    refusal: entry.refusal as SharedStackRefusal | null,
  };
}

/**
 * A drafted card, checked for the two fields every consumer dereferences.
 *
 * Deliberately NOT a full field-by-field check of `DraftCardInstance`: `pool`
 * and `current_pack` have never been validated here either, so a complete one
 * belongs in a change that covers all four rather than half of them. What this
 * does buy is that the two v30 card-bearing payloads cannot deliver entries the
 * pile table and the forced-draw notice will read `undefined` off -- `key` and
 * the rendered name both come from these.
 */
function normalizeDraftCardInstance(raw: unknown, context: string): unknown {
  const card = requireObject(raw, context);
  if (typeof card.instance_id !== "string" || card.instance_id.length === 0) {
    throw new Error(`Invalid draft message: ${context}.instance_id must be a non-empty string`);
  }
  if (typeof card.name !== "string") {
    throw new Error(`Invalid draft message: ${context}.name must be a string`);
  }
  return card;
}

function normalizeSharedStackPileView(raw: unknown, position: number): SharedStackPileView {
  const context = `shared_stack.piles[${position}]`;
  const pile = requireObject(raw, context);
  // `shared_stack_view` builds this vector with `.enumerate()` and
  // `let index = i as u8`, so a pile's index IS its position -- always, on every
  // path. A frame carrying a duplicate or shuffled index is describing a vector
  // the engine never produced, and the client addresses piles BY index, so a
  // duplicate would make two piles answer to the same address.
  if (pile.index !== position) {
    throw new Error(`Invalid draft message: ${context}.index must equal its position`);
  }
  const total = requireCount(pile, "total", context);
  const revealed = normalizeArrayField<unknown>(pile, "revealed")
    .map((card, i) => normalizeDraftCardInstance(card, `${context}.revealed[${i}]`)) as
    SharedStackPileView["revealed"];
  // The engine publishes a PREFIX of the pile, so more revealed cards than the
  // pile is tall is a frame that contradicts itself. Not a legality check --
  // this compares the frame against itself, and nothing here re-derives which
  // cards a viewer may see.
  if (revealed.length > total) {
    throw new Error(`Invalid draft message: ${context}.revealed is longer than the pile`);
  }
  if (!Array.isArray(pile.legality)) {
    throw new Error(`Invalid draft message: ${context}.legality must be an array`);
  }
  return {
    index: position,
    total,
    revealed,
    legality: pile.legality.map((entry, i) =>
      normalizeSharedStackDecisionView(entry, `${context}.legality[${i}]`)),
  };
}

function normalizeSharedStackDecisionRecord(
  raw: unknown,
  index: number,
  seatCount: number,
  pileCount: number,
): SharedStackDecisionRecord {
  const context = `shared_stack.history[${index}]`;
  const record = requireObject(raw, context);
  if (!SHARED_STACK_DECISIONS.includes(record.decision as SharedStackPileDecision)) {
    throw new Error(`Invalid draft message: ${context}.decision must be Take or Decline`);
  }
  return {
    seat: requireBoundedInt(record, "seat", context, 0, seatCount - 1),
    pile: requireBoundedInt(record, "pile", context, 0, pileCount - 1),
    decision: record.decision as SharedStackPileDecision,
    pile_size: requireCount(record, "pile_size", context),
  };
}

/**
 * @param declaredPileCount the pile count the frame's own `distribution` names,
 *   or `null` when the distribution is not a shared stack. The two are checked
 *   against each other rather than each in isolation.
 */
function normalizeSharedStackView(
  raw: unknown,
  declaredPileCount: number | null,
  seatCount: number,
): SharedStackView | null {
  if (raw === null || raw === undefined) return null;
  const stack = requireObject(raw, "shared_stack");
  if (!Array.isArray(stack.piles)) {
    throw new Error("Invalid draft message: shared_stack.piles must be an array");
  }
  // CROSS-FIELD. `piles_needed(pile_count)` returns `usize::from(pile_count)`,
  // so the reducer's pile vector is exactly as long as the distribution says --
  // a frame where the two disagree is describing a session that cannot exist,
  // and each half looks fine on its own.
  //
  // `null` cannot reach here: the caller refuses a `shared_stack` paired with
  // any other distribution before calling. That refusal is what makes this
  // check un-evadable -- without it, declaring `PickAndPass` alongside a
  // shared stack skipped straight past this and still rendered the pile table,
  // so a one-word edit defeated the whole guard.
  if (declaredPileCount !== null && stack.piles.length !== declaredPileCount) {
    throw new Error(
      "Invalid draft message: shared_stack.piles length must equal the declared pile_count",
    );
  }
  // The published length, which the cross-check above has already tied to the
  // declared count -- so bounding a reference by either is bounding it by both.
  const pileCount = stack.piles.length;
  // REQUIRED, not defaulted: both are unconditionally serialized by the engine,
  // so a frame omitting either is malformed rather than sparse.
  const rawHistory = requirePresent(stack, "history", "shared_stack");
  const rawForced = requirePresent(stack, "forced_draw", "shared_stack");
  if (!Array.isArray(rawHistory)) {
    throw new Error("Invalid draft message: shared_stack.history must be an array");
  }
  const forced = rawForced === null
    ? null
    : normalizeDraftCardInstance(rawForced, "shared_stack.forced_draw");
  return {
    main_stack_remaining: requireCount(stack, "main_stack_remaining", "shared_stack"),
    total_cards: requireCount(stack, "total_cards", "shared_stack"),
    // AGAINST THIS FRAME'S CARDINALITIES, not merely against `u8`. The engine
    // copies the live seat and cursor out of a session that HAS those seats and
    // those piles (`shared_stack_view`), so a two-seat, one-pile frame naming
    // seat 2 or pile 1 is one it could not have emitted -- and every such value
    // fits a `u8` perfectly well, which is why the integer bound alone admitted
    // them.
    active_seat: requireBoundedInt(stack, "active_seat", "shared_stack", 0, seatCount - 1),
    active_pile: requireBoundedInt(stack, "active_pile", "shared_stack", 0, pileCount - 1),
    piles: stack.piles.map(normalizeSharedStackPileView),
    decisions: requireBoundedInt(stack, "decisions", "shared_stack", 0, U32_MAX),
    history: rawHistory.map((record, i) =>
      normalizeSharedStackDecisionRecord(record, i, seatCount, pileCount)),
    forced_draw: forced as SharedStackView["forced_draw"],
  };
}

/** The seat that chooses play/draw, or `null` when nobody has been designated. */
function normalizePlayFirstChooser(raw: unknown, seatCount: number): number | null {
  if (raw === null || raw === undefined) return null;
  // A SEAT INDEX, so it is bounded by the seats this frame actually carries.
  // `play_first_chooser` is produced only for a shared-stack session of exactly
  // two seats, as `(starting_seat + 1) % 2` -- so the engine's own range is
  // {0, 1}. Bounding by the frame's seat count is the general form of that and
  // does not hard-code the two-seat rule here.
  if (typeof raw !== "number" || !Number.isInteger(raw) || raw < 0 || raw >= seatCount) {
    throw new Error("Invalid draft message: play_first_chooser must be null or a seat index");
  }
  return raw;
}

function normalizeDraftPlayerView(raw: unknown): DraftPlayerView {
  if (raw === undefined) {
    throw new Error("Invalid draft message: launch_capability must be a known capability");
  }
  if (typeof raw !== "object" || raw === null) {
    throw new Error("Invalid draft message: malformed player view");
  }
  const view = raw as Record<string, unknown>;
  if (
    view.launch_capability !== "None"
    && view.launch_capability !== "CommanderMultiplayer"
  ) {
    throw new Error("Invalid draft message: launch_capability must be a known capability");
  }
  if (
    typeof view.commanders_required !== "number"
    || !Number.isInteger(view.commanders_required)
    || view.commanders_required < 0
    || view.commanders_required > 255
  ) {
    throw new Error("Invalid draft message: commanders_required must be a u8 count");
  }
  const distribution = normalizePackDistribution(view.distribution);
  const declaredPileCount = typeof distribution === "object"
    ? distribution.SharedStackPiles.pile_count
    : null;
  // SEATS FIRST, because the shared-stack references are validated against how
  // many there are. Normalizing the stack before the seats meant the seat count
  // was simply unavailable, which is why `active_seat` could only ever be
  // checked against `u8`.
  const seats = normalizeArrayField(view, "seats").map(normalizeSeatPublicView);
  // The seat count is the new authority for those references, and unlike
  // `pile_count` it carries no ceiling of its own -- `normalizeArrayField`
  // caps nothing. Keeping the `u8` bound as well means a frame publishing 400
  // seats still cannot name seat 300, a value the engine's own `u8` field could
  // not hold and which the previous bound refused.
  const seatBound = Math.min(seats.length, U8_MAX + 1);
  // A LIVE PILE TURN IMPLIES THE DISTRIBUTION THAT DEALS PILES.
  // `shared_stack_view_for` returns `Some` only for a session that HAS a shared
  // stack and is drafting, so the engine cannot pair one with any other
  // distribution. Refusing the pairing here is also what stops the pile-count
  // cross-check below being sidestepped: the client renders the pile table on
  // `view.shared_stack` alone, so a frame declaring `PickAndPass` beside an
  // arbitrary pile vector used to render while skipping every shared-stack
  // rule.
  if (view.shared_stack !== undefined && view.shared_stack !== null && declaredPileCount === null) {
    throw new Error(
      "Invalid draft message: shared_stack requires a SharedStackPiles distribution",
    );
  }
  const pool_groups = normalizePoolGroups(view.pool_groups);
  const source = normalizeDraftSourceView(view.source);
  // A v28 peer may still send this former public-view field. Do not preserve
  // it through a permissive object spread: the host-only source belongs only
  // to the local WASM session and launch payloads, never a participant frame.
  const { booster_pack_pool: _boosterPackPool, ...publicView } = view;
  return {
    ...publicView,
    ...(pool_groups !== undefined ? { pool_groups } : {}),
    ...(source !== undefined ? { source } : {}),
    draft_effects: normalizeArrayField(view, "draft_effects"),
    seats,
    // The v30 fields, validated rather than spread. Listed AFTER the spread so
    // the checked value is the one that survives -- a raw `view.distribution`
    // riding the spread would otherwise win.
    //
    // `distribution` is required: it is non-optional on `DraftPlayerView`, and
    // the draft protocol is compared for EXACT equality at the handshake, so a
    // peer that omits it is malformed rather than old.
    distribution,
    // The two OPTIONAL fields keep their absence rather than being materialized
    // as `null`. Same conditional-spread idiom as `pool_groups` and `source`
    // above, and for a concrete reason: turning an absent field into a present
    // null changes the object's shape, which a wire round-trip can see.
    //
    // The stack is checked AGAINST the distribution, not merely beside it: the
    // declared pile count and the published pile vector have to agree, and
    // neither half looks wrong on its own.
    ...(view.shared_stack !== undefined
      ? { shared_stack: normalizeSharedStackView(view.shared_stack, declaredPileCount, seatBound) }
      : {}),
    ...(view.play_first_chooser !== undefined
      ? { play_first_chooser: normalizePlayFirstChooser(view.play_first_chooser, seatBound) }
      : {}),
  } as unknown as DraftPlayerView;
}

function requireWorkspaceState(
  raw: Record<string, unknown>,
  nullable: false,
): DraftWorkspaceState;
function requireWorkspaceState(
  raw: Record<string, unknown>,
  nullable: true,
): DraftWorkspaceState | null;
function requireWorkspaceState(
  raw: Record<string, unknown>,
  nullable: boolean,
): DraftWorkspaceState | null {
  if (!("workspaceState" in raw)) {
    throw new Error("Invalid draft message: missing workspaceState");
  }
  if (raw.workspaceState === null && nullable) return null;
  const validated = validateWorkspaceState(raw.workspaceState, {
    maxPlacementCount: MAX_DRAFT_WORKSPACE_NETWORK_PLACEMENTS,
  });
  if ("error" in validated) {
    throw new Error(`Invalid draft message: ${validated.error}`);
  }
  return validated;
}

/**
 * The `Array.isArray` + element-type idiom `validateSubmitDeck` uses, lifted
 * because a commander launch needs it for four arrays. Module-private, matching
 * the same un-exported guard in `adapter/format-config-shape.ts` and
 * `services/scryfall.ts` rather than reaching across layers for one.
 */
function isStringArray(value: unknown): value is string[] {
  return Array.isArray(value) && value.every((entry) => typeof entry === "string");
}

function validateCommanderLaunch(raw: Record<string, unknown>): DraftP2PMessage {
  if (typeof raw.launch !== "object" || raw.launch === null) {
    throw new Error("Invalid commander launch: missing launch");
  }
  const launch = raw.launch as Record<string, unknown>;
  if (typeof launch.gameId !== "string" || launch.gameId.length === 0) {
    throw new Error("Invalid commander launch: gameId must be a non-empty string");
  }
  // An empty room code becomes `joinRoom("")` on the guest, so it is rejected
  // here for the same reason `draft_leave` rejects an empty draft token.
  if (typeof launch.roomCode !== "string" || launch.roomCode.length === 0) {
    throw new Error("Invalid commander launch: roomCode must be a non-empty string");
  }
  // No upper bound: the ceiling is enforced where the game adapter is built
  // (`P2PHostAdapter` refuses > 6; the engine's Commander Draft format allows
  // 8), never by the message.
  if (
    typeof launch.playerCount !== "number"
    || !Number.isInteger(launch.playerCount)
    || launch.playerCount <= 0
  ) {
    throw new Error("Invalid commander launch: playerCount must be a positive integer");
  }
  if (typeof launch.localDeck !== "object" || launch.localDeck === null) {
    throw new Error("Invalid commander launch: missing localDeck");
  }
  const localDeck = launch.localDeck as Record<string, unknown>;
  if (
    !isStringArray(localDeck.main_deck)
    || !isStringArray(localDeck.sideboard)
    || !isStringArray(localDeck.commander)
  ) {
    throw new Error("Invalid commander launch: malformed localDeck");
  }
  // Required but NULLABLE, mirroring `requireWorkspaceState`: an absent key is
  // rejected because the type carries no `?`, while `null` is the meaningful
  // "no draft set is known, so constructed play" value the engine already reads.
  if (!("draftSetCodes" in launch)) {
    throw new Error("Invalid commander launch: missing draftSetCodes");
  }
  if (launch.draftSetCodes !== null && !isStringArray(launch.draftSetCodes)) {
    throw new Error("Invalid commander launch: draftSetCodes must be null or a string array");
  }
  return raw as DraftP2PMessage;
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
  if (msg.type === "draft_leave" || msg.type === "draft_leave_ack") {
    const leave = raw as Record<string, unknown>;
    if (
      leave.draftProtocolVersion !== DRAFT_PROTOCOL_VERSION
      || typeof leave.draftToken !== "string"
      || leave.draftToken.length === 0
    ) {
      throw new Error("Invalid draft leave message");
    }
    return leave as DraftP2PMessage;
  }
  if (msg.type === "draft_pick_with_draft_effect") {
    return validateDraftEffectPick(raw as Record<string, unknown>);
  }
  if (msg.type === "draft_pick") {
    return validatePick(raw as Record<string, unknown>);
  }
  if (msg.type === "draft_pile_decision") {
    return validatePileDecision(raw as Record<string, unknown>);
  }
  if (msg.type === "draft_submit_deck") {
    return validateSubmitDeck(raw as Record<string, unknown>);
  }
  if (msg.type === "draft_commander_launch") {
    return validateCommanderLaunch(raw as Record<string, unknown>);
  }
  if (
    msg.type === "draft_suggest_lands"
    || msg.type === "draft_suggest_lands_result"
    || msg.type === "draft_suggest_lands_rejected"
  ) {
    return validateSuggestLandsMessage(raw, msg.type);
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
  if (msg.type === "draft_workspace_update") {
    const update = raw as Record<string, unknown>;
    if ("seat" in update || "seatIndex" in update) {
      throw new Error("Invalid draft message: workspace update must not include a seat");
    }
    return {
      type: "draft_workspace_update",
      workspaceState: requireWorkspaceState(update, false),
    };
  }
  const viewMessage = raw as { type: string; view?: unknown; seats?: unknown };
  if (["draft_welcome", "draft_reconnect_ack", "draft_state_update", "draft_pick_ack"].includes(msg.type)) {
    const workspaceState = msg.type === "draft_welcome" || msg.type === "draft_reconnect_ack"
      ? requireWorkspaceState(raw as Record<string, unknown>, true)
      : undefined;
    return {
      ...viewMessage,
      view: normalizeDraftPlayerView(viewMessage.view),
      ...(workspaceState !== undefined ? { workspaceState } : {}),
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
