/**
 * Draft-specific persistence for P2P tournament sessions.
 *
 * Separate from `gamePersistence.ts` (which handles engine GameState)
 * because draft session data has a different shape and lifecycle:
 * - Host persists the full DraftSession JSON + seat tokens after every mutation (P2P-05)
 * - Guest persists the draft token at pod join time (P2P-04)
 *
 * Both use IndexedDB via idb-keyval for the same reasons as game persistence:
 * draft sessions can be large (8 players x 42 cards each = significant JSON).
 */

import { createStore, del, get, set } from "idb-keyval";

import type { DraftKind, DraftStatus, PoolInput } from "../adapter/draft-adapter";
import {
  isPlainRecord,
  validateWorkspaceState,
  type DraftWorkspaceState,
} from "../components/draft/workspace/types";
import type { DraftMatchBinding, DraftMatchLaunch, DraftMatchSettlement } from "../network/draftProtocol";
import { parseRoomCode } from "../network/connection";
import { ACTIVE_DRAFT_GUEST_KEY, ACTIVE_DRAFT_POD_KEY } from "../constants/storage";
import type { DraftIntergameCommand } from "./intergameCommandLedger";

export type { PoolInput } from "../adapter/draft-adapter";

// ── Types ──────────────────────────────────────────────────────────────

/**
 * Persisted snapshot of a P2P draft host session.
 *
 * Written after every authoritative mutation (guest join, pick, deck submit,
 * kick) so a crashed/reloaded host can restore the draft pod.
 */
export interface PersistedDraftHostSession {
  persistenceId: string;
  roomCode: string;
  kind: Exclude<DraftKind, "Quick">;
  podSize: number;
  hostDisplayName: string;
  tournamentFormat: "Swiss" | "SingleElimination";
  podPolicy: "Competitive" | "Casual";
  /** Seat index -> token. */
  seatTokens: Record<number, string>;
  /** Seat index -> display name. */
  seatNames: Record<number, string>;
  /** Tokens that were kicked — refused on reconnect. */
  kickedTokens: string[];
  /** Absolute reconnect deadline by seat. An absent field is legacy and cannot
   * safely establish a new recovery window. */
  reconnectDeadlines?: Record<number, number>;
  /** Seats whose reconnect grace elapsed while this host owned the pod. */
  expiredDisconnectedSeats?: number[];
  /** Whether StartDraft has been applied. */
  draftStarted: boolean;
  /** Host intent is separate from effective disconnect-induced pausing. */
  manualPause?: boolean;
  /** Draft code for display/identification. */
  draftCode: string;
  /** Serialized DraftSession JSON from draft-wasm. Null if draft hasn't started. */
  draftSessionJson: string | null;
  /** Pool source for re-initialization on resume (Set pool JSON or Cube list + settings). */
  poolInput: PoolInput;
  /** Pod-issued match capabilities, retained across host recovery. */
  matchBindings?: DraftMatchBinding[];
  /** Result receipts are written before the draft reducer observes them. */
  settlementOutbox?: DraftMatchSettlement[];
  settlementReceipts?: Array<{ matchId: string; receiptId: string; revision: number }>;
  /** Write-ahead Bo3 commands survive recovery without becoming replayable. */
  intergameCommands?: DraftIntergameCommand[];
  /** Host-owned intergame phase data; required to reject stale recovery writes. */
  bo3State?: Array<{
    matchId: string;
    seatA: number;
    seatB: number;
    submittedA: boolean;
    submittedB: boolean;
    loserSeat: number | null;
    gameNumber: number;
    score: { p0_wins: number; p1_wins: number; draws: number };
    /** Current deck partitions, used by the authoritative timeout default. */
    decks?: Array<{
      seat: number;
      main: Array<{ name: string; count: number }>;
      sideboard: Array<{ name: string; count: number }>;
    }>;
  }>;
  launchDigests?: Array<{ matchId: string; seat: number; digest: string }>;
  /** Full immutable launch records let a recovered host issue timeout commands
   * through the same launch-bound intergame ledger. */
  matchLaunches?: Array<{ matchId: string; seat: number; launch: DraftMatchLaunch }>;
  /** Durable idempotency ledger for participant deck-submission retries. */
  deckSubmissionReceipts?: Array<{
    seat: number;
    submissionId: string;
    payloadFingerprint: string;
  }>;
  /** Complete, validated workspace state per authoritative seat. */
  perSeatWorkspaceSnapshots?: Record<number, DraftWorkspaceState>;
}

/**
 * Persisted guest token for draft reconnection.
 *
 * Saved at pod join time (P2P-04) so a guest whose tab crashes can
 * reopen and rejoin their seat.
 */
export interface PersistedDraftGuestSession {
  hostPeerId: string;
  draftToken: string;
  seatIndex: number;
  draftCode: string;
  /** Bound to the non-secret recovery pointer before reconnect is attempted. */
  roomCode: string;
  displayName: string;
  timestamp: number;
}

/** A participant-owned deck submission retained until the host acknowledges it. */
export interface PersistedDraftDeckSubmission {
  hostPeerId: string;
  /** Display identity captured at submit time; it can change from lobby pending. */
  draftCode: string;
  /** Stable pod route, unlike the draft code generated at draft start. */
  roomCode: string;
  /** Capability identity prevents reusing an outbox after a seat is reissued. */
  draftToken: string;
  submissionId: string;
  mainDeck: string[];
  /**
   * CR 903.3: the commander designation this submission carried. Persisted
   * because `draft_submit_deck` REQUIRES it (draft protocol 17) — a reconnect
   * replay that omitted it would be refused by `validateSubmitDeck` on the
   * host, and `draftPeerSession`'s decode `.catch` would drop the frame
   * silently rather than surfacing an error.
   */
  commanders: string[];
  timestamp: number;
}

/** A non-secret, validated route back to a guest's persisted capability. */
export interface ActiveDraftGuestMeta {
  roomCode: string;
  displayName: string;
  hostPeerId: string;
  timestamp: number;
}

/** Stable identity of a guest recovery locator read from local storage. */
export interface ActiveDraftGuestMetaCapture {
  roomCode: string;
  displayName: string;
  hostPeerId: string;
  timestamp: number;
}

/** Non-mutating classification for guest recovery callers. */
export type ActiveDraftGuestLoadResult =
  | { type: "absent" }
  | { type: "invalid"; capture: ActiveDraftGuestMetaCapture | null }
  | { type: "present"; meta: ActiveDraftGuestMeta; capture: ActiveDraftGuestMetaCapture };

export type ActiveDraftPodPhase =
  | "lobby"
  | "drafting"
  | "deckbuilding"
  | "pairing"
  | "matchInProgress"
  | "complete";

export interface ActiveDraftPodMeta {
  id: string;
  roomCode: string;
  kind: Exclude<DraftKind, "Quick">;
  podSize: number;
  hostDisplayName: string;
  tournamentFormat: "Swiss" | "SingleElimination";
  podPolicy: "Competitive" | "Casual";
  phase: ActiveDraftPodPhase;
  pickCount: number;
  updatedAt: number;
}

/** Stable identity captured before an asynchronous resume attempt. */
export interface ActiveDraftPodMetaCapture {
  id: string;
  roomCode: string;
  updatedAt: number;
}

export type ActiveDraftPodLoadResult =
  | { type: "absent" }
  | { type: "invalid"; capture: ActiveDraftPodMetaCapture | null }
  | { type: "present"; meta: ActiveDraftPodMeta; capture: ActiveDraftPodMetaCapture };

export type PersistedDraftHostSessionState = "live" | "terminal" | "invalid";

const PERSISTED_DRAFT_STATUS: Record<DraftStatus, PersistedDraftHostSessionState> = {
  Lobby: "live",
  Drafting: "live",
  Paused: "live",
  Deckbuilding: "live",
  Pairing: "live",
  MatchInProgress: "live",
  RoundComplete: "live",
  Complete: "terminal",
  Abandoned: "invalid",
};

// ── Store ──────────────────────────────────────────────────────────────

const DRAFT_HOST_PREFIX = "phase-draft-host:";
const DRAFT_GUEST_PREFIX = "phase-draft-guest:";
const DRAFT_SETTLEMENT_PREFIX = "phase-draft-settlement:";
const DRAFT_INTERGAME_PREFIX = "phase-draft-intergame:";
const DRAFT_DECK_SUBMISSION_PREFIX = "phase-draft-deck-submission:";
const HOST_SESSION_TTL_MS = 24 * 60 * 60 * 1000;
/** Guest token TTL — 4 hours matches the game session TTL. */
const GUEST_SESSION_TTL_MS = 4 * 60 * 60 * 1000;

let _store: ReturnType<typeof createStore> | undefined;

export function getDraftStore(): ReturnType<typeof createStore> {
  if (!_store) {
    _store = createStore("phase-draft-session", "phase-draft-session");
  }
  return _store;
}

// ── Host Persistence ───────────────────────────────────────────────────

export async function saveDraftHostSession(
  id: string,
  session: PersistedDraftHostSession,
): Promise<void> {
  try {
    await set(DRAFT_HOST_PREFIX + id, session, getDraftStore());
  } catch (err) {
    console.warn("[saveDraftHostSession] IDB write failed:", err);
    throw err;
  }
}

export async function loadDraftHostSession(
  id: string,
): Promise<PersistedDraftHostSession | null> {
  try {
    const s = await get<PersistedDraftHostSession>(
      DRAFT_HOST_PREFIX + id,
      getDraftStore(),
    );
    if (!s) return null;
    if (!isPersistedDraftHostSession(s)) return null;

    const snapshots: Record<number, DraftWorkspaceState> = {};
    if (s.perSeatWorkspaceSnapshots !== undefined) {
      if (!isPlainRecord(s.perSeatWorkspaceSnapshots)) return null;
      const rawSnapshots = s.perSeatWorkspaceSnapshots as unknown as Record<PropertyKey, unknown>;
      for (const key of Reflect.ownKeys(rawSnapshots)) {
        if (
          typeof key !== "string"
          || !Object.prototype.propertyIsEnumerable.call(rawSnapshots, key)
        ) {
          return null;
        }
        const seat = Number(key);
        if (!Number.isSafeInteger(seat) || seat < 0 || String(seat) !== key) return null;
        const snapshot = validateWorkspaceState(rawSnapshots[key]);
        if ("error" in snapshot) return null;
        snapshots[seat] = snapshot;
      }
    }

    return { ...s, perSeatWorkspaceSnapshots: snapshots };
  } catch {
    return null;
  }
}

export async function clearDraftHostSession(id: string): Promise<void> {
  try {
    await del(DRAFT_HOST_PREFIX + id, getDraftStore());
  } catch { /* best-effort */ }
}

// ── Active Host Meta ──────────────────────────────────────────────────

export function saveActiveDraftPod(meta: ActiveDraftPodMeta): void {
  localStorage.setItem(ACTIVE_DRAFT_POD_KEY, JSON.stringify(meta));
}

export function loadActiveDraftPod(): ActiveDraftPodMeta | null {
  const result = inspectActiveDraftPod();
  return result.type === "present" ? result.meta : null;
}

/**
 * Reads active-host metadata without mutating it. Resume owns deletion because
 * the record can change while IndexedDB is being read.
 */
export function inspectActiveDraftPod(): ActiveDraftPodLoadResult {
  try {
    const raw = localStorage.getItem(ACTIVE_DRAFT_POD_KEY);
    if (!raw) return { type: "absent" };
    const value: unknown = JSON.parse(raw);
    const capture = activeDraftPodCapture(value);
    if (!isActiveDraftPodMeta(value) || Date.now() - value.updatedAt > HOST_SESSION_TTL_MS) {
      return { type: "invalid", capture };
    }
    return { type: "present", meta: value, capture: activeDraftPodCapture(value)! };
  } catch {
    return { type: "invalid", capture: null };
  }
}

export function clearActiveDraftPod(): void {
  localStorage.removeItem(ACTIVE_DRAFT_POD_KEY);
}

// ── Active Guest Meta ─────────────────────────────────────────────────

/**
 * The room code and display name are enough to reconnect, but never confer a
 * seat. The opaque draft token stays in IndexedDB under the expected host id.
 */
export function saveActiveDraftGuest(meta: Omit<ActiveDraftGuestMeta, "timestamp">): void {
  const roomCode = parseRoomCode(meta.roomCode);
  const displayName = meta.displayName.trim();
  if (!roomCode || !displayName || !meta.hostPeerId.trim()) return;
  localStorage.setItem(ACTIVE_DRAFT_GUEST_KEY, JSON.stringify({
    roomCode,
    displayName,
    hostPeerId: meta.hostPeerId,
    timestamp: Date.now(),
  }));
}

function activeDraftGuestCapture(value: unknown): ActiveDraftGuestMetaCapture | null {
  if (!isActiveDraftGuestMeta(value)) return null;
  return {
    roomCode: value.roomCode,
    displayName: value.displayName,
    hostPeerId: value.hostPeerId,
    timestamp: value.timestamp,
  };
}

/** Inspects the guest recovery locator without removing malformed or expired data. */
export function inspectActiveDraftGuest(): ActiveDraftGuestLoadResult {
  try {
    const raw = localStorage.getItem(ACTIVE_DRAFT_GUEST_KEY);
    if (!raw) return { type: "absent" };
    const value: unknown = JSON.parse(raw);
    if (!isActiveDraftGuestMeta(value)) return { type: "invalid", capture: null };
    const capture = activeDraftGuestCapture(value);
    if (!capture || Date.now() - capture.timestamp > GUEST_SESSION_TTL_MS) {
      return { type: "invalid", capture };
    }
    return { type: "present", meta: value, capture };
  } catch {
    return { type: "invalid", capture: null };
  }
}

export function loadActiveDraftGuest(): ActiveDraftGuestMeta | null {
  const active = inspectActiveDraftGuest();
  if (active.type === "present") return active.meta;
  if (active.type === "invalid") clearActiveDraftGuest();
  return null;
}

/** Clears guest metadata only when it still matches a previously inspected locator. */
export function clearActiveDraftGuestIfCurrent(capture: ActiveDraftGuestMetaCapture): void {
  try {
    const raw = localStorage.getItem(ACTIVE_DRAFT_GUEST_KEY);
    if (!raw) return;
    const currentCapture = activeDraftGuestCapture(JSON.parse(raw));
    if (
      currentCapture?.roomCode === capture.roomCode
      && currentCapture.displayName === capture.displayName
      && currentCapture.hostPeerId === capture.hostPeerId
      && currentCapture.timestamp === capture.timestamp
    ) {
      clearActiveDraftGuest();
    }
  } catch {
    // A malformed replacement is not evidence that this caller owns it.
  }
}

export function clearActiveDraftGuest(): void {
  localStorage.removeItem(ACTIVE_DRAFT_GUEST_KEY);
}

/** Do not erase a newer guest locator when an older adapter is disposed. */
export function clearActiveDraftGuestForHost(hostPeerId: string): void {
  const current = loadActiveDraftGuest();
  if (current?.hostPeerId === hostPeerId) clearActiveDraftGuest();
}

/** Clears stale metadata only when it is still the record this caller read. */
export function clearActiveDraftPodIfCurrent(capture: ActiveDraftPodMetaCapture): void {
  try {
    const raw = localStorage.getItem(ACTIVE_DRAFT_POD_KEY);
    if (!raw) return;
    const current: unknown = JSON.parse(raw);
    const currentCapture = activeDraftPodCapture(current);
    if (
      currentCapture?.id === capture.id &&
      currentCapture.roomCode === capture.roomCode &&
      currentCapture.updatedAt === capture.updatedAt
    ) {
      clearActiveDraftPod();
    }
  } catch {
    // A malformed replacement is not evidence that this caller owns it.
  }
}

/**
 * The host snapshot, not the UI progress cache, decides whether a pod can be
 * resumed. A lobby has not created a WASM session yet, so its null JSON is a
 * valid live state only before `draftStarted`.
 */
export function persistedDraftHostSessionState(
  session: PersistedDraftHostSession,
): PersistedDraftHostSessionState {
  if (typeof session.draftStarted !== "boolean") return "invalid";
  if (!session.draftStarted) {
    return session.draftSessionJson === null ? "live" : "invalid";
  }
  if (typeof session.draftSessionJson !== "string") return "invalid";

  try {
    const value: unknown = JSON.parse(session.draftSessionJson);
    if (!isRecord(value) || typeof value.status !== "string") return "invalid";
    if (!(value.status in PERSISTED_DRAFT_STATUS)) return "invalid";
    return PERSISTED_DRAFT_STATUS[value.status as DraftStatus];
  } catch {
    return "invalid";
  }
}

function isActiveDraftPodMeta(value: unknown): value is ActiveDraftPodMeta {
  if (!isRecord(value)) return false;
  return (
    typeof value.id === "string" && value.id.length > 0 &&
    isCanonicalRoomCode(value.roomCode) &&
    (value.kind === "Premier" || value.kind === "Traditional" || value.kind === "Sealed" || value.kind === "CommanderDraft") &&
    isPositiveInteger(value.podSize) &&
    typeof value.hostDisplayName === "string" &&
    (value.tournamentFormat === "Swiss" || value.tournamentFormat === "SingleElimination") &&
    (value.podPolicy === "Competitive" || value.podPolicy === "Casual") &&
    (value.phase === "lobby" || value.phase === "drafting" || value.phase === "deckbuilding" ||
      value.phase === "pairing" || value.phase === "matchInProgress" || value.phase === "complete") &&
    isNonnegativeInteger(value.pickCount) &&
    isPositiveFiniteNumber(value.updatedAt)
  );
}

function isActiveDraftGuestMeta(value: unknown): value is ActiveDraftGuestMeta {
  if (!isRecord(value)) return false;
  return isCanonicalRoomCode(value.roomCode)
    && isNonEmptyString(value.displayName)
    && isNonEmptyString(value.hostPeerId)
    && isPositiveFiniteNumber(value.timestamp);
}

/**
 * IndexedDB is a user-controlled boundary. Validate every persisted field the
 * recovery path reads before rebuilding host configuration or passing the
 * snapshot into the P2P host; a type assertion here would turn bad storage
 * into a resume-time exception.
 */
function isPersistedDraftHostSession(value: unknown): value is PersistedDraftHostSession {
  if (!isRecord(value)) return false;
  return (
    isNonEmptyString(value.persistenceId) &&
    isCanonicalRoomCode(value.roomCode) &&
    isDraftKind(value.kind) &&
    isPositiveInteger(value.podSize) &&
    isNonEmptyString(value.hostDisplayName) &&
    (value.tournamentFormat === "Swiss" || value.tournamentFormat === "SingleElimination") &&
    (value.podPolicy === "Competitive" || value.podPolicy === "Casual") &&
    isSeatStringRecord(value.seatTokens) &&
    isSeatStringRecord(value.seatNames) &&
    Array.isArray(value.kickedTokens) && value.kickedTokens.every((token) => typeof token === "string") &&
    (value.reconnectDeadlines === undefined || isSeatDeadlineRecord(value.reconnectDeadlines)) &&
    (value.expiredDisconnectedSeats === undefined ||
      (Array.isArray(value.expiredDisconnectedSeats) && value.expiredDisconnectedSeats.every(isNonnegativeInteger))) &&
    typeof value.draftStarted === "boolean" &&
    (value.manualPause === undefined || typeof value.manualPause === "boolean") &&
    // A live pre-draft lobby has no generated draft code yet; the host uses
    // the room code as its seed fallback until StartDraft assigns one.
    typeof value.draftCode === "string" &&
    (value.draftSessionJson === null || typeof value.draftSessionJson === "string") &&
    isPoolInput(value.poolInput) &&
    isOptionalRecordArray(value.matchBindings) &&
    isOptionalRecordArray(value.settlementOutbox) &&
    isOptionalRecordArray(value.settlementReceipts) &&
    isOptionalRecordArray(value.intergameCommands) &&
    isOptionalRecordArray(value.deckSubmissionReceipts) &&
    isOptionalRecordArray(value.bo3State) &&
    isOptionalRecordArray(value.launchDigests) &&
    isOptionalRecordArray(value.matchLaunches)
  );
}

function isDraftKind(value: unknown): value is Exclude<DraftKind, "Quick"> {
  return value === "Premier" || value === "Traditional" || value === "Sealed" || value === "CommanderDraft";
}

function isPoolInput(value: unknown): value is PoolInput {
  if (!isRecord(value) || !isRecord(value.data)) return false;
  if (value.type === "Set") {
    // Two spellings reach here. The live one is a `SetPackSequence`: the
    // distinct pools plus the pack-ordered sequence naming which fills each
    // booster. A pod persisted before multi-set pods existed carries one
    // serialized pool under `set_pool_json`; draft-wasm still promotes that to
    // the one-element sequence it meant, so a host mid-lobby across the upgrade
    // resumes instead of having its snapshot discarded as corrupt.
    if (isSetPackSequence(value.data)) return true;
    return typeof value.data.set_pool_json === "string" && isJsonRecord(value.data.set_pool_json);
  }
  if (value.type === "Chaos") {
    return (
      Array.isArray(value.data.pools) &&
      value.data.pools.every(isRecord) &&
      Array.isArray(value.data.candidate_codes) &&
      value.data.candidate_codes.length > 0 &&
      value.data.candidate_codes.every(isNonEmptyString)
    );
  }
  if (value.type !== "Cube") return false;
  const settings = value.data.cube_draft_settings;
  return (
    isNonEmptyString(value.data.cube_list_text) &&
    isNonEmptyString(value.data.cube_name) &&
    isRecord(settings) &&
    isPositiveInteger(settings.pod_size) &&
    isPositiveInteger(settings.pack_count) &&
    isPositiveInteger(settings.cards_per_pack) &&
    isPositiveInteger(settings.min_deck_size) &&
    isRecord(settings.addable_cards) &&
    (settings.addable_cards.policy === "StandardBasics" ||
      settings.addable_cards.policy === "CustomOnly" ||
      settings.addable_cards.policy === "StandardBasicsPlusCustom") &&
    Array.isArray(settings.addable_cards.custom) &&
    settings.addable_cards.custom.every((card) => typeof card === "string")
  );
}

/**
 * A pack sequence carries one pool object per distinct set and one set code per
 * booster. The sequence must be non-empty — a pod that named no booster has no
 * pool at all — and every entry a string, since draft-wasm resolves each
 * against the supplied pools by name.
 */
function isSetPackSequence(data: Record<string, unknown>): boolean {
  return (
    Array.isArray(data.pools) &&
    data.pools.every(isRecord) &&
    Array.isArray(data.sequence) &&
    data.sequence.length > 0 &&
    data.sequence.every((code) => typeof code === "string")
  );
}

function isSeatStringRecord(value: unknown): value is Record<number, string> {
  return isRecord(value) && Object.entries(value).every(([seat, token]) => isPositiveSeat(seat) && typeof token === "string");
}

function isSeatDeadlineRecord(value: unknown): value is Record<number, number> {
  return isRecord(value) && Object.entries(value).every(
    ([seat, deadline]) => isPositiveSeat(seat) && isPositiveFiniteNumber(deadline),
  );
}

function isPositiveSeat(value: string): boolean {
  const seat = Number(value);
  return Number.isInteger(seat) && seat >= 0 && String(seat) === value;
}

function isOptionalRecordArray(value: unknown): boolean {
  return value === undefined || (Array.isArray(value) && value.every(isRecord));
}

function isNonEmptyString(value: unknown): value is string {
  return typeof value === "string" && value.trim().length > 0;
}

function isPositiveInteger(value: unknown): value is number {
  return typeof value === "number" && Number.isInteger(value) && value > 0;
}

function isNonnegativeInteger(value: unknown): value is number {
  return typeof value === "number" && Number.isInteger(value) && value >= 0;
}

function isPositiveFiniteNumber(value: unknown): value is number {
  return typeof value === "number" && Number.isFinite(value) && value > 0;
}

function isJsonRecord(value: string): boolean {
  try {
    return isRecord(JSON.parse(value));
  } catch {
    return false;
  }
}

function activeDraftPodCapture(value: unknown): ActiveDraftPodMetaCapture | null {
  if (!isRecord(value) || typeof value.id !== "string" || !isCanonicalRoomCode(value.roomCode) ||
    !isPositiveFiniteNumber(value.updatedAt)) {
    return null;
  }
  return { id: value.id, roomCode: value.roomCode, updatedAt: value.updatedAt };
}

function isCanonicalRoomCode(value: unknown): value is string {
  return typeof value === "string" && parseRoomCode(value) === value;
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null;
}

// ── Guest Persistence ──────────────────────────────────────────────────

export async function saveDraftGuestSession(
  hostPeerId: string,
  data: { draftToken: string; seatIndex: number; draftCode: string; roomCode: string; displayName: string },
): Promise<void> {
  const roomCode = parseRoomCode(data.roomCode);
  const displayName = data.displayName.trim();
  if (!roomCode) throw new Error("Invalid draft room code");
  if (!displayName) throw new Error("Draft display name is required");
  const session: PersistedDraftGuestSession = {
    hostPeerId,
    draftToken: data.draftToken,
    seatIndex: data.seatIndex,
    draftCode: data.draftCode,
    roomCode,
    displayName,
    timestamp: Date.now(),
  };
  try {
    await set(DRAFT_GUEST_PREFIX + hostPeerId, session, getDraftStore());
  } catch (err) {
    console.warn("[saveDraftGuestSession] IDB write failed:", err);
    throw err;
  }
}

export async function loadDraftGuestSession(
  hostPeerId: string,
  identity?: Pick<ActiveDraftGuestMeta, "roomCode" | "displayName">,
): Promise<PersistedDraftGuestSession | null> {
  try {
    const session = await get<PersistedDraftGuestSession>(
      DRAFT_GUEST_PREFIX + hostPeerId,
      getDraftStore(),
    );
    if (!session || !isPersistedDraftGuestSession(session) || session.hostPeerId !== hostPeerId) return null;
    if (Date.now() - session.timestamp > GUEST_SESSION_TTL_MS) {
      await clearDraftGuestSession(hostPeerId);
      return null;
    }
    if (identity && (session.roomCode !== identity.roomCode || session.displayName !== identity.displayName)) {
      return null;
    }
    return session;
  } catch {
    return null;
  }
}

export async function clearDraftGuestSession(hostPeerId: string): Promise<void> {
  try {
    await del(DRAFT_GUEST_PREFIX + hostPeerId, getDraftStore());
  } catch { /* best-effort */ }
}

/** Clears both halves of one guest's recovery identity. */
export async function clearDraftGuestRecovery(hostPeerId: string): Promise<void> {
  await clearDraftGuestSession(hostPeerId);
  clearActiveDraftGuestForHost(hostPeerId);
}

/**
 * Writes the deck command before it enters the transport.  The key is owned by
 * the participant (not the pod), so a host reload or a dropped DataChannel
 * cannot turn an accepted deck into a lost UI submission.
 */
export async function saveDraftDeckSubmission(
  hostPeerId: string,
  submission: Omit<PersistedDraftDeckSubmission, "hostPeerId" | "timestamp">,
): Promise<void> {
  const roomCode = parseRoomCode(submission.roomCode);
  if (!roomCode) throw new Error("Invalid draft room code");
  const value: PersistedDraftDeckSubmission = {
    hostPeerId,
    draftCode: submission.draftCode,
    roomCode,
    draftToken: submission.draftToken,
    submissionId: submission.submissionId,
    mainDeck: [...submission.mainDeck],
    commanders: [...submission.commanders],
    timestamp: Date.now(),
  };
  await set(`${DRAFT_DECK_SUBMISSION_PREFIX}${hostPeerId}`, value, getDraftStore());
}

export async function loadDraftDeckSubmission(
  hostPeerId: string,
  identity?: Pick<PersistedDraftDeckSubmission, "roomCode" | "draftToken">,
): Promise<PersistedDraftDeckSubmission | null> {
  try {
    const roomCode = identity && parseRoomCode(identity.roomCode);
    if (identity && !roomCode) return null;
    const value = await get<PersistedDraftDeckSubmission>(
      `${DRAFT_DECK_SUBMISSION_PREFIX}${hostPeerId}`,
      getDraftStore(),
    );
    if (!value || value.hostPeerId !== hostPeerId || !isNonEmptyString(value.draftCode)
      || !isCanonicalRoomCode(value.roomCode)
      || !isNonEmptyString(value.draftToken) || !isNonEmptyString(value.submissionId)
      || !Array.isArray(value.mainDeck) || !value.mainDeck.every((card) => typeof card === "string")
      // A record written before the designation existed cannot be replayed:
      // the host would refuse it. Discarding it is the fail-safe answer — the
      // guest simply builds a fresh submission.
      || !Array.isArray(value.commanders)
      || !value.commanders.every((card) => typeof card === "string")) {
      return null;
    }
    if (identity && (value.roomCode !== roomCode || value.draftToken !== identity.draftToken)) {
      return null;
    }
    return value;
  } catch {
    return null;
  }
}

export async function clearDraftDeckSubmission(hostPeerId: string, submissionId?: string): Promise<void> {
  try {
    if (submissionId) {
      const current = await loadDraftDeckSubmission(hostPeerId);
      if (current?.submissionId !== submissionId) return;
    }
    await del(`${DRAFT_DECK_SUBMISSION_PREFIX}${hostPeerId}`, getDraftStore());
  } catch {
    /* Retain safely when IndexedDB is unavailable. */
  }
}

function isPersistedDraftGuestSession(value: unknown): value is PersistedDraftGuestSession {
  if (!isRecord(value)) return false;
  return isNonEmptyString(value.hostPeerId)
    && isNonEmptyString(value.draftToken)
    && isNonnegativeInteger(value.seatIndex)
    && typeof value.draftCode === "string"
    && isCanonicalRoomCode(value.roomCode)
    && isNonEmptyString(value.displayName)
    && isPositiveFiniteNumber(value.timestamp);
}

/** A participant-owned outbox survives a reload until the pod host acks it. */
export async function saveDraftSettlementOutbox(settlement: DraftMatchSettlement): Promise<void> {
  try {
    await set(draftSettlementKey(settlement.binding), settlement, getDraftStore());
  } catch (err) {
    console.warn("[draftPersistence] settlement outbox write failed:", err);
  }
}

export async function loadDraftSettlementOutbox(
  binding: DraftMatchBinding,
): Promise<DraftMatchSettlement | null> {
  try {
    const settlement = await get<DraftMatchSettlement>(draftSettlementKey(binding), getDraftStore());
    return settlement && sameSettlementBinding(settlement.binding, binding) ? settlement : null;
  } catch {
    return null;
  }
}

export async function clearDraftSettlementOutbox(binding: DraftMatchBinding): Promise<void> {
  try {
    await del(draftSettlementKey(binding), getDraftStore());
  } catch {
    /* best-effort; retry remains safe */
  }
}

/** Participant-owned journal retained until every command is receipted. */
export async function saveDraftIntergameCommands(
  matchId: string,
  commands: DraftIntergameCommand[],
): Promise<void> {
  try {
    await set(DRAFT_INTERGAME_PREFIX + matchId, commands, getDraftStore());
  } catch (err) {
    console.warn("[draftPersistence] intergame command write failed:", err);
  }
}

export async function loadDraftIntergameCommands(matchId: string): Promise<DraftIntergameCommand[]> {
  try {
    return await get<DraftIntergameCommand[]>(DRAFT_INTERGAME_PREFIX + matchId, getDraftStore()) ?? [];
  } catch {
    return [];
  }
}

export async function clearDraftIntergameCommands(matchId: string): Promise<void> {
  try {
    await del(DRAFT_INTERGAME_PREFIX + matchId, getDraftStore());
  } catch {
    /* best-effort */
  }
}

function draftSettlementKey(binding: DraftMatchBinding): string {
  return `${DRAFT_SETTLEMENT_PREFIX}${binding.podId}:${binding.matchId}`;
}

/** Legacy or cross-round outboxes are never eligible for settlement replay. */
function sameSettlementBinding(left: DraftMatchBinding, right: DraftMatchBinding): boolean {
  return left.podId === right.podId
    && left.matchId === right.matchId
    && left.round === right.round
    && left.sessionKey === right.sessionKey
    && left.lease === right.lease
    && left.nonce === right.nonce
    && left.revision === right.revision
    && left.matchAuthoritySeat === right.matchAuthoritySeat;
}
