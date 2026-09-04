import type {
  EndContinuousEffectOffer,
  GameAction,
  GameEvent,
  GameLogEntry,
  GameState,
  LegalActionsResult,
  ManaCost,
  ObjectId,
  ObjectAction,
  ActionRejection,
} from "../adapter/types";
import type { InteractionSubmission, ViewerInteraction } from "../adapter/generated/interaction";
import type { SeatMutation, SeatView } from "../multiplayer/seatTypes";
import type { P2PAuthorityStamp, P2PSessionKey } from "../services/p2pSession";
import type { P2PTerminalResult } from "../services/p2pTerminalResult";
import { decodeJsonEnvelope, encodeJsonEnvelope } from "./wireEnvelope";

/**
 * The stable session identity is carried on reconnect so a resumed host can
 * accept a legitimate guest while replacing its previous host incarnation.
 * Every host-originated frame is stamped by P2PHostAdapter; the optional
 * shape keeps first-contact guest_deck messages intentionally credentialless.
 */
export interface P2PAuthorityWire {
  authority?: P2PAuthorityStamp;
}

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
  endContinuousEffectOffers?: EndContinuousEffectOffer[];
  manaPaymentShortcutActions?: GameAction[];
  legalActionsByObject?: Record<string, ObjectAction[]>;
  spellCosts?: Record<string, ManaCost>;
  viewerInteraction?: ViewerInteraction;
}

/** Host-side: project an engine `LegalActionsResult` onto the wire shape. */
export function legalActionsToWire(result: LegalActionsResult): LegalActionsWire {
  return {
    legalActions: result.actions,
    autoPassRecommended: result.autoPassRecommended,
    endContinuousEffectOffers: result.endContinuousEffectOffers ?? [],
    manaPaymentShortcutActions: result.manaPaymentShortcutActions ?? [],
    legalActionsByObject: result.legalActionsByObject,
    spellCosts: result.spellCosts,
    viewerInteraction: result.viewerInteraction,
  };
}

/** Guest-side: hydrate a wire payload into the adapter's `LegalActionsResult`. */
export function legalActionsFromWire(wire: LegalActionsWire): LegalActionsResult {
  return {
    actions: wire.legalActions,
    autoPassRecommended: wire.autoPassRecommended ?? false,
    endContinuousEffectOffers: wire.endContinuousEffectOffers ?? [],
    manaPaymentShortcutActions: wire.manaPaymentShortcutActions ?? [],
    legalActionsByObject: wire.legalActionsByObject,
    spellCosts: wire.spellCosts,
    viewerInteraction: wire.viewerInteraction,
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
 * A host → guest mismatch is enforced in exactly ONE place:
 * `P2PGuestAdapter.handleHostMessage` (`adapter/p2p-adapter.ts`).
 * `validateMessage` below deliberately does NOT check it. A throw from there
 * propagates out of `decodeWireMessage` into the `catch` in `peer.ts`, which
 * warns and drops the frame — so the host's `game_setup` never reaches the
 * adapter, the setup promise never settles and the guest hangs on the
 * connecting screen with no layer able to tell the user. Only the adapter
 * holds the state needed to perform the response.
 *
 * The guest → host direction has its own single site: current guests must stamp
 * this version on `guest_deck` / `reconnect`, and `P2PHostAdapter`'s
 * first-contact gate rejects a missing or unequal value before it allocates a
 * seat or adopts reconnect state.
 *
 * Bumps to date:
 *  40 — DerivedViews.room_half_identities publishes both halves of every
 *       battlefield Room in printed order, resolved through the COPIED halves
 *       for a permanent that copies a Room (CR 709.5b + CR 707.2). The unlock
 *       special action's offer names the half it would unlock and shows that
 *       half's cost (CR 709.5e) from this map: an enter-as-copy recipient
 *       carries neither on its own printed card (its `back_face` is empty and
 *       its printed name is its own), and printed order is engine work
 *       besides — `room::live_face_door` reads `modal_back_face`, the
 *       CR 709.5d mapping. Face-down permanents are absent (CR 708.2a). The
 *       field is additive behind a serde default, but this client renders the
 *       map directly rather than deriving halves from raw state; a v39 host
 *       would silently label every offered door "Tap for Mana" again, which is
 *       the defect this bump exists to fix. Since game_setup and reconnect_ack
 *       carry GameState, first contact rejects the version skew rather than
 *       allowing that capability loss.
 *  39 — DerivedViews.storm_count publishes the engine-owned number of copies a
 *       current Storm trigger will create, or a newly cast Storm spell would
 *       create. The field is additive, but this client renders the scalar
 *       directly; a v38 host would silently omit the Storm HUD status. Since
 *       game_setup and reconnect_ack carry GameState, first contact rejects
 *       the version skew rather than allowing that capability loss.
 *  38 — Casting permissions gained a typed lifetime. Two parts, with different
 *       compatibility consequences:
 *       (a) `CastingPermission::ExileWithAltAbilityCost` gained `duration`
 *           and `source_id`; `::ExileWithAltCost` gained `source_id` beside the
 *           `duration` it already carried. `source_id` is the granting
 *           permanent whose presence bounds the stated lifetime. Additive
 *           behind serde defaults, like entry 34's `prepared_copy_source`.
 *       (b) `Duration` gained the `WhileControllingHost` ("for as long as
 *           you control ~") and `WhileHostOnBattlefield` ("for as long as ~
 *           remains on the battlefield") variants (CR 611.2b). Each new tag
 *           is a PARSE BREAK in the v38 -> v37 direction, in the same shape
 *           as the full-game bump at entry 46 (new tag emitted, old peer
 *           cannot parse it; the break runs ONE way, unlike entry 32's
 *           two-way break): a v37 peer deserializing a snapshot that contains
 *           either new tag fails on an unknown variant. There is no serde
 *           default that can rescue it, which is why the handshake must
 *           refuse the pairing rather than degrade.
 *  37 — FormatConfig gained default_deck_copy_limit, the resolved per-format
 *       deck-copy ceiling (CR 100.2a / CR 100.2b / CR 903.5b) max_deck_copies
 *       and the deck-compatibility admission path now both read. The field
 *       is optional and still parses on an older peer, but silently falls
 *       back to the fail-closed UpTo(1) singleton cap instead of the
 *       format's real declared limit — a silent capability loss like 24, not
 *       a parse break. game_setup and reconnect_ack both carry the full
 *       GameState, so this P2P track is broken by the same change as the
 *       full-game PROTOCOL_VERSION track (see
 *       crates/lobby-broker/src/protocol.rs entry 48) and must bump in
 *       lockstep with it.
 *  36 — Resolution-time optional fixed sacrifice payments add a typed
 *       replacement-resumable continuation to GameState.
 *  35 — QuantityRef.Aggregate and QuantityRef.TrackedSetAggregate were
 *       replaced in GameState snapshots by the canonical
 *       QuantityRef.PropertyAggregate tag with a validated source object. New
 *       peers migrate both legacy input tags, but a v34 peer cannot parse the
 *       canonical tag emitted by v35, so P2P first contact rejects the skew.
 *  34 — GameState gained serialized cast-occurrence provenance and prepared-copy links.
 *  33 — Resolution-time optional PayCost(OneOf) branch choice added a
 *       serialized WaitingFor/GameAction pair.
 *  32 — FormatConfig.deck_size changed from a bare u16 to the adjacently
 *       tagged DeckSizeRule enum (Minimum(u16) / Exactly(u16)), because
 *       CR 903.13f(1) makes Commander Draft a command-zone format with a
 *       minimum rather than an exact size, and GameFormat gained a
 *       CommanderDraft variant (CR 903.13a). A PARSE bump like 16, not a
 *       silent capability loss like 24: FormatConfig::deck_size carries
 *       neither a serde default nor a deserialize_with, so a v31 peer's
 *       "deck_size": 60 cannot deserialize against the adjacently tagged enum
 *       and a v32 peer's {"type":"Minimum","data":60} cannot deserialize
 *       against a v31 u16 — the break is unconditional, runs in BOTH
 *       directions, and hits every format's snapshot, not just Commander
 *       Draft's. GameState.format_config's serde default does NOT rescue it:
 *       a field-level default applies only when the key is ABSENT, and an old
 *       peer sends the key present with the old inner shape. The
 *       GameFormat::CommanderDraft variant is the second and narrower half —
 *       it breaks only when that variant is actually serialized.
 *  31 — Action and mana-payment-preview rejections carry engine-owned,
 *       viewer-filtered ActionRejection DTOs. First-contact versioning keeps
 *       legacy peers from treating a typed rejection as a transport string.
 *  30 — ManaRestriction.CannotCastSpellFromZone adds a serialized
 *       GameState/ManaUnit restriction used by Karolina Dean. Older peers
 *       cannot deserialize that externally tagged enum variant.
 *  29 — WaitingFor.ChooseObjectsSelection publishes min and optional max
 *       bounds. A v28 peer silently ignores the additive fields and offers
 *       out-of-range selections, so refuse the capability mismatch during
 *       host/guest first contact.
 *  28 — PayCostKind::TapCreatures changed from { aggregate } to a required
 *       { mode } (Fixed/VariableX/Aggregate) — the fix that also unlocks
 *       the u32::MAX X-sentinel tap-cost form (Glacian, Powerstone Engineer
 *       + 8 sibling cards, #7799). mode carries no serde default, so a
 *       GameState snapshot paused mid-TapCreatures payment under the old
 *       aggregate shape now fails to deserialize instead of risking a
 *       silent fixed/aggregate misclassification. game_setup and
 *       reconnect_ack both carry the full GameState, so this P2P track is
 *       broken by the same change as the full-game PROTOCOL_VERSION track
 *       (see crates/lobby-broker/src/protocol.rs entry 37) and must bump
 *       in lockstep with it.
 *  27 — WaitingFor.ChooseDungeon.options changed from DungeonId[] to
 *       DungeonPreview[], and ChooseDungeonRoom dropped option_names, gained a
 *       required dungeon_name, and changed options from number[] to
 *       RoomPreview[], so each option carries the room's printed name and
 *       room-ability text (CR 309.4b-c). A PARSE bump like 16, not a silent
 *       capability loss like 24: none of the new fields carry a serde default,
 *       so a v26 peer cannot deserialize a dungeon-choice snapshot at all.
 *       DerivedViews.dungeon_rooms rides along in the same bump — it IS
 *       optional and would parse on a v26 peer, but this client deleted its
 *       dungeon_progress room-index derivation, so a v26 host would leave a
 *       v27 guest with no dungeon badge.
 *  26 — DerivedViews.current_target_kind publishes the engine's CR 115.1
 *       classification of the live target announcement. The field is optional
 *       and parses on a v24 peer, so the loss is silent: this client deleted
 *       inferTargetNoun, so a v24 host would leave a v25 guest naming no
 *       target at all. The handshake is the only place to refuse it.
 *  23 — WaitingFor::AlternativeCastChoice.alternative_additional_cost_description
 *       changed from a string to a typed Emerge-sacrifice descriptor. Older
 *       clients would receive an object where their modal expects display text.
 *  22 — LegalActionsWire.viewerInteraction carries attachmentViews: the engine's
 *       membership list for each host's attachment fan. It parses on a v21 peer
 *       as an empty map, so the loss is silent — a guest paired with a v21 host
 *       would simply find every attachment fan gone.
 *  21 — LegalActionsWire.viewerInteraction carries the loop-shortcut preview,
 *       and the state snapshot carries WaitingFor::LoopShortcut.declaration.
 *       Both are optional and parse on a v20 peer; the loss is silent, so the
 *       handshake is the only place the pairing can be refused.
 *  24 — DerivedViews.legend_candidate_identities carries the engine-authored
 *       copy identity required to label legend-rule choices. The field is
 *       additive but the client no longer infers this identity from raw state,
 *       so accepting a v23 peer would silently remove every choice option.
 *  20 — Serialized player-action completion provenance and modal continuations.
 *  19 — Added an action_noop acknowledgement for accepted transport no-ops.
 *  18 — DebugCardEntries added a serialized, private resolution frame for
 *       multi-card sandbox battlefield entries that pause for replacement or
 *       as-enters choices. Old peers cannot deserialize that GameState shape.
 *  16 — PayableResource::ManaGeneric changed from { per_x } to
 *       { base_cost: ManaCost } (#6410) — a GameState payload field type
 *       change, and base_cost intentionally carries no serde default (a
 *       missing base_cost must fail deserialization, not silently resolve
 *       to a zero-cost payment), so old and new peers can't parse each
 *       other's serialized snapshots.
 *   1 — pre-compression JSON-serialization era (no longer in production)
 *   2 — gzip + version-prefixed binary wire format
 *   3 — Planechase state and action payloads in game_setup/reconnect snapshots
 *   4 — Archenemy derived view and scheme deck payloads
 *   5 — CardPredicateGuessMade game event shape
 *  13 — Actor-scoped priority-passing settings and filtered per-player state.
 *  17 — Sacrificial-mana source selection action and waiting-state snapshots.
 *  12 — Connive exact subject snapshots and resident paused post-replacement
 *       drains changed P2P GameState snapshots.
 *  11 — Serialized GameState trigger provenance and paused logical zone-change owners.
 *  10 — Dedicated companion deck slot and typed companion-reveal choices.
 *   9 — Meld pair and attacking-entry choices after mana-payment preview variants.
 *   8 — Mana-payment preview request/response variants.
 *   7 — PrecastCopyShortcut action and its two WaitingFor variants.
 *  17 — Bound draft-match concession request. A Traditional-draft guest
 *       asks its match authority to settle the match; it must not send a
 *       game-level concession through the ordinary P2P path.
 *   6 — Mulligan bottoming folded into a MulliganDecisionPhase::BottomCards
 *       sub-phase on WaitingFor::MulliganDecision; the MulliganBottomCards
 *       variant was removed
 */
export const WIRE_PROTOCOL_VERSION = 41 as const;

export type P2PMessage = P2PAuthorityWire & (
  | {
      type: "guest_deck";
      deckData: unknown;
      displayName?: string;
      reservationToken?: string;
      wireProtocolVersion: typeof WIRE_PROTOCOL_VERSION;
    }
  | ({
      type: "game_setup";
      wireProtocolVersion: typeof WIRE_PROTOCOL_VERSION;
      assignedPlayerId: number;
      playerToken: string;
      revision?: number;
      state: GameState;
      events: GameEvent[];
      playerNames?: Record<number, string>;
    } & LegalActionsWire)
  | { type: "action"; senderPlayerId: number; action: GameAction }
  | { type: "interaction"; senderPlayerId: number; submission: InteractionSubmission }
  | { type: "preview_mana_payment"; requestId: number; action: GameAction }
  | ({
      type: "state_update";
      revision?: number;
      state: GameState;
      events: GameEvent[];
      logEntries?: GameLogEntry[];
    } & LegalActionsWire)
  | { type: "action_rejected"; rejection: ActionRejection }
  | { type: "action_failed"; message: string }
  | { type: "action_noop" }
  | { type: "mana_payment_preview"; requestId: number; sourceIds: ObjectId[] }
  | { type: "mana_payment_preview_rejected"; requestId: number; rejection: ActionRejection }
  | { type: "mana_payment_preview_failed"; requestId: number; message: string }
  | { type: "ping"; timestamp: number }
  | { type: "pong"; timestamp: number }
  | { type: "disconnect"; reason: string }
  | { type: "emote"; emote: string }
  | { type: "concede" }
  /** Protected by a draft-installed match capability on the host. */
  | { type: "match_concede" }
  // Reconnect: guest presents prior token; host accepts (with fresh state) or rejects.
  | {
      type: "reconnect";
      playerToken: string;
      sessionKey?: P2PSessionKey;
      wireProtocolVersion: typeof WIRE_PROTOCOL_VERSION;
    }
  | ({
      type: "reconnect_ack";
      wireProtocolVersion: typeof WIRE_PROTOCOL_VERSION;
      assignedPlayerId: number;
      revision?: number;
      state: GameState;
      playerNames?: Record<number, string>;
    } & LegalActionsWire)
  | {
      type: "reconnect_rejected";
      reason: string;
      reasonCode?: "first_message_invalid" | "wire_protocol_version_required" | "wire_protocol_mismatch" | "malformed_authority";
      hostWireProtocolVersion?: number;
      guestWireProtocolVersion?: number;
    }
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
  /** Recipient-scoped terminal commitment. It is host-originated and
   * lease-bound; guests pin the first valid terminal id and never reconnect
   * after accepting the commitment for their filtered state. */
  | { type: "terminal_result"; result: P2PTerminalResult }
  /** Native server AI became unable to advance the authoritative session.
   * The host sends this only after the final state revision it depends on. */
  | { type: "ai_driver_fault"; id: number; revision: number; message: string }
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
  | { type: "seat_snapshot"; view: SeatView }
);

const VALID_TYPES = new Set([
  "guest_deck",
  "game_setup",
  "action",
  "interaction",
  "preview_mana_payment",
  "state_update",
  "action_rejected",
  "action_failed",
  "action_noop",
  "mana_payment_preview",
  "mana_payment_preview_rejected",
  "mana_payment_preview_failed",
  "ping",
  "pong",
  "disconnect",
  "emote",
  "concede",
  "match_concede",
  "reconnect",
  "reconnect_ack",
  "reconnect_rejected",
  "kick",
  "host_left",
  "terminal_result",
  "ai_driver_fault",
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

export async function encodeWireMessage(msg: P2PMessage): Promise<Uint8Array> {
  return encodeJsonEnvelope(JSON.stringify(msg));
}

export async function decodeWireMessage(bytes: Uint8Array): Promise<P2PMessage> {
  const json = await decodeJsonEnvelope(bytes);
  return validateMessage(JSON.parse(json));
}
