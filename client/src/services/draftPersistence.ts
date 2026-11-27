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

import { createStore, del, get, promisifyRequest, set } from "idb-keyval";

import { DRAFT_KINDS } from "../adapter/draft-adapter";
import type { DraftKind, DraftStatus, PoolInput } from "../adapter/draft-adapter";
import {
  isPlainRecord,
  validateWorkspaceState,
  type DraftWorkspaceState,
} from "../components/draft/workspace/types";
import type { DraftMatchBinding, DraftMatchLaunch, DraftMatchSettlement } from "../network/draftProtocol";
import { parseRoomCode } from "../network/connection";
import { parseWebSocketUrl } from "../config/multiplayerServer";
import { ACTIVE_DRAFT_GUEST_KEY, ACTIVE_DRAFT_POD_KEY } from "../constants/storage";
import {
  runCrossTabTransaction,
  type CrossTabResource,
  type CrossTabTxnFailure,
  type CrossTabTxnResult,
} from "./crossTabTransaction";
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
  /**
   * Non-secret name this private record shares with its public pointer.
   * Absent on a record written before records carried one; never written empty.
   */
  capabilityId?: string;
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
  companion?: string | null;
  timestamp: number;
}

/** A non-secret, validated route back to a guest's persisted capability. */
export interface ActiveDraftGuestMeta {
  roomCode: string;
  displayName: string;
  hostPeerId: string;
  timestamp: number;
  /**
   * Non-secret name of the private record this pointer leads to. Absent on a
   * pointer written before records carried one; never written empty.
   */
  capabilityId?: string;
}

/** Stable identity of a guest recovery locator read from local storage. */
export interface ActiveDraftGuestMetaCapture {
  roomCode: string;
  displayName: string;
  hostPeerId: string;
  timestamp: number;
  /** Null for a pointer without a name. */
  capabilityId: string | null;
}

/**
 * One guest's durable recovery capability: the secret host-issued token and
 * the non-secret name its private record and public pointer share. The name is
 * null only for records written before records carried one.
 */
export interface DraftGuestCapability {
  readonly hostPeerId: string;
  readonly draftToken: string;
  readonly roomCode: string;
  readonly displayName: string;
  readonly capabilityId: string | null;
}

/** The fields that decide which stored private record a token owns. */
export type DraftGuestCapabilityOwner = Omit<DraftGuestCapability, "capabilityId">;

/** The fields that decide which stored deck outbox a token owns. */
export type DraftDeckSubmissionOwner = Pick<DraftGuestCapability, "hostPeerId" | "roomCode" | "draftToken">;

/** Non-mutating classification for guest recovery callers. */
export type ActiveDraftGuestLoadResult =
  | { type: "absent" }
  | { type: "invalid"; capture: ActiveDraftGuestMetaCapture | null }
  | { type: "present"; meta: ActiveDraftGuestMeta; capture: ActiveDraftGuestMetaCapture };

/** Which side of a server-hosted draft a native seat took when it was opened. */
export type NativeDraftParticipant = "host" | "guest";

/**
 * The secret half of a native draft seat's recovery capability, kept in
 * IndexedDB under its own name so no other seat's write can replace it.
 */
export interface NativeDraftCapabilityRecord {
  capabilityId: string;
  /** Canonical, as `canonicalNativeDraftEndpoint` returns it. */
  endpoint: string;
  draftCode: string;
  playerToken: string;
  seatIndex: number;
  participant: NativeDraftParticipant;
  timestamp: number;
}

/**
 * The non-secret route back to a native seat's private record. It shares the
 * guest pointer's key, told apart by its `transport` tag, and never carries
 * the seat token.
 */
export interface ActiveNativeDraftPointer {
  transport: "native";
  capabilityId: string;
  endpoint: string;
  draftCode: string;
  seatIndex: number;
  participant: NativeDraftParticipant;
  timestamp: number;
}

/** The exact stored text of a native pointer, as a reader saw it. */
export interface NativeDraftPointerCapture {
  raw: string;
}

/** Non-mutating classification of the native pointer. */
export type ActiveNativeDraftLoadResult =
  | { type: "absent" }
  | { type: "invalid"; capture: NativeDraftPointerCapture }
  | { type: "present"; pointer: ActiveNativeDraftPointer; capture: NativeDraftPointerCapture };

/** Non-mutating classification of the private record a native pointer names. */
export type NativeDraftCapabilityLoadResult =
  | { type: "absent" }
  | { type: "invalid" }
  | { type: "mismatch" }
  | { type: "present"; record: NativeDraftCapabilityRecord }
  | { type: "readFailure"; error: unknown };

/** What an exact clear of one native capability matches on. */
export interface NativeDraftCapabilityHold {
  readonly capabilityId: string;
  readonly endpoint: string;
  readonly draftCode: string;
  readonly playerToken: string;
}

export type ActiveDraftPodPhase =
  | "lobby"
  | "drafting"
  | "review"
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

/**
 * Host recovery must keep absence, malformed storage, terminal history and an
 * IndexedDB failure distinct.  In particular, a read failure keeps the saved
 * record for a source-bound retry and does not authorize locator cleanup.
 * The loader reads a saved draft session only for its status; whether the
 * draft engine can restore it is the engine's own check. The loader returns
 * the parsed session and keeps no unparsed copy.
 */
export type DraftHostSessionLoadResult =
  | { type: "absent" }
  | { type: "invalid" }
  | { type: "terminal"; session: PersistedDraftHostSession }
  | { type: "admitted"; session: PersistedDraftHostSession }
  | { type: "readFailure"; error: unknown };

const PERSISTED_DRAFT_STATUS: Record<DraftStatus, PersistedDraftHostSessionState> = {
  Lobby: "live",
  Drafting: "live",
  Review: "live",
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
const NATIVE_DRAFT_CAPABILITY_PREFIX = "phase-native-draft-capability:";
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
): Promise<DraftHostSessionLoadResult> {
  try {
    const raw = await get<unknown>(
      DRAFT_HOST_PREFIX + id,
      getDraftStore(),
    );
    // IndexedDB deletion is represented by a nullish read.  Falsy values are
    // still values: they must pass through the structural classifier so a
    // corrupt `false`, `0`, or empty string cannot take the absent/guest
    // fallback route by truthiness.
    if (raw === undefined || raw === null) return { type: "absent" };
    if (!isPersistedDraftHostSession(raw)) return { type: "invalid" };
    const s = raw;

    const snapshots: Record<number, DraftWorkspaceState> = {};
    if (s.perSeatWorkspaceSnapshots !== undefined) {
      if (!isPlainRecord(s.perSeatWorkspaceSnapshots)) return { type: "invalid" };
      const rawSnapshots = s.perSeatWorkspaceSnapshots as unknown as Record<PropertyKey, unknown>;
      for (const key of Reflect.ownKeys(rawSnapshots)) {
        if (
          typeof key !== "string"
          || !Object.prototype.propertyIsEnumerable.call(rawSnapshots, key)
        ) {
          return { type: "invalid" };
        }
        const seat = Number(key);
        if (!Number.isSafeInteger(seat) || seat < 0 || String(seat) !== key) return { type: "invalid" };
        const snapshot = validateWorkspaceState(rawSnapshots[key]);
        if ("error" in snapshot) return { type: "invalid" };
        snapshots[seat] = snapshot;
      }
    }

    const session = { ...s, perSeatWorkspaceSnapshots: snapshots };

    const state = persistedDraftHostSessionState(session);
    if (state === "invalid") return { type: "invalid" };
    if (state === "terminal") return { type: "terminal", session };
    return { type: "admitted", session };
  } catch (error) {
    return { type: "readFailure", error };
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

/** What a guest seat publishes as its pointer; the write stamps the time. */
export type ActiveDraftGuestPublication = Omit<ActiveDraftGuestMeta, "timestamp" | "capabilityId"> & {
  capabilityId: string;
};

/** The pointer a publication writes, stamped now; null for one that does not validate. */
function activeDraftGuestPointer(meta: ActiveDraftGuestPublication): ActiveDraftGuestMeta | null {
  const roomCode = parseRoomCode(meta.roomCode);
  const displayName = meta.displayName.trim();
  if (!roomCode || !displayName || !meta.hostPeerId.trim() || !isNonEmptyString(meta.capabilityId)) {
    return null;
  }
  return {
    roomCode,
    displayName,
    hostPeerId: meta.hostPeerId,
    timestamp: Date.now(),
    capabilityId: meta.capabilityId,
  };
}

/**
 * The room code and display name are enough to reconnect, but never confer a
 * seat. The opaque draft token stays in IndexedDB under the expected host id.
 * Writes over whatever is stored. Settles once a native seat's record this
 * pointer replaces is dealt with.
 */
function publishGuestBody(meta: ActiveDraftGuestPublication): Promise<void> {
  const written = activeDraftGuestPointer(meta);
  return written === null ? Promise.resolve() : replaceActiveDraftPointer(JSON.stringify(written));
}

/**
 * A guest seat's publication, written at the call outside the recovery-pointer
 * transaction, so another tab's pointer step is not ordered against it.
 * Otherwise as `ActiveDraftPointerTxn.publishGuest`.
 */
export function saveActiveDraftGuest(meta: ActiveDraftGuestPublication): Promise<void> {
  return publishGuestBody(meta);
}

/**
 * Writes a guest seat's pointer only while the stored pointer is still
 * `standsOn`, read and written in one synchronous step, and returns the
 * capture of what it wrote; null when it wrote nothing. The text it replaces
 * is a guest pointer, which names no native record. Throws as the write does.
 */
function republishGuestBody(
  standsOn: ActiveDraftGuestMetaCapture,
  meta: ActiveDraftGuestPublication,
): ActiveDraftGuestMetaCapture | null {
  const written = activeDraftGuestPointer(meta);
  if (written === null) return null;
  const settled = replaceActiveDraftPointerIfCurrent(JSON.stringify(written), (stored) => {
    const current = storedGuestCapture(stored);
    return current !== null && sameActiveDraftGuestCapture(current, standsOn);
  });
  return settled === null ? null : activeDraftGuestCapture(written);
}

/**
 * A guest seat's republication, compared and written at the call outside the
 * recovery-pointer transaction, so another tab's pointer step is not ordered
 * against it. Otherwise as `ActiveDraftPointerTxn.republishGuest`.
 */
export function republishActiveDraftGuest(
  standsOn: ActiveDraftGuestMetaCapture,
  meta: ActiveDraftGuestPublication,
): ActiveDraftGuestMetaCapture | null {
  return republishGuestBody(standsOn, meta);
}

/** Whether two guest captures are the same stored pointer, capability included. */
export function sameActiveDraftGuestCapture(
  left: ActiveDraftGuestMetaCapture,
  right: ActiveDraftGuestMetaCapture,
): boolean {
  return left.roomCode === right.roomCode
    && left.displayName === right.displayName
    && left.hostPeerId === right.hostPeerId
    && left.timestamp === right.timestamp
    && left.capabilityId === right.capabilityId;
}

/** The guest capture of a stored pointer's text: null for none, unparseable or tagged text. */
function storedGuestCapture(raw: string | null): ActiveDraftGuestMetaCapture | null {
  if (raw === null) return null;
  try {
    return activeDraftGuestCapture(JSON.parse(raw));
  } catch {
    return null;
  }
}

function activeDraftGuestCapture(value: unknown): ActiveDraftGuestMetaCapture | null {
  if (!isActiveDraftGuestMeta(value)) return null;
  return {
    roomCode: value.roomCode,
    displayName: value.displayName,
    hostPeerId: value.hostPeerId,
    timestamp: value.timestamp,
    capabilityId: value.capabilityId ?? null,
  };
}

/** Inspects the guest recovery locator without removing malformed or expired data. */
export function inspectActiveDraftGuest(): ActiveDraftGuestLoadResult {
  try {
    const raw = localStorage.getItem(ACTIVE_DRAFT_GUEST_KEY);
    if (!raw) return { type: "absent" };
    const value: unknown = JSON.parse(raw);
    // A tagged record belongs to its transport's reader, never to this one.
    if (isRecord(value) && hasTransportTag(value)) return { type: "absent" };
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

/** Reads the guest recovery locator; malformed or expired storage is left as it is. */
export function loadActiveDraftGuest(): ActiveDraftGuestMeta | null {
  const active = inspectActiveDraftGuest();
  return active.type === "present" ? active.meta : null;
}

/** Clears guest metadata only when it still matches a previously inspected locator. */
function clearGuestIfCurrentBody(capture: ActiveDraftGuestMetaCapture): void {
  try {
    const raw = localStorage.getItem(ACTIVE_DRAFT_GUEST_KEY);
    if (!raw) return;
    const currentCapture = activeDraftGuestCapture(JSON.parse(raw));
    if (currentCapture !== null && sameActiveDraftGuestCapture(currentCapture, capture)) {
      localStorage.removeItem(ACTIVE_DRAFT_GUEST_KEY);
    }
  } catch {
    // A malformed replacement is not evidence that this caller owns it.
  }
}

/**
 * Clears guest metadata only when it still matches a previously inspected
 * locator, compared and removed inside the recovery-pointer transaction. A
 * transaction that cannot run leaves the pointer.
 */
export async function clearActiveDraftGuestIfCurrent(capture: ActiveDraftGuestMetaCapture): Promise<void> {
  await withActiveDraftPointer((pointer) => pointer.clearGuestIfCurrent(capture));
}

/**
 * Removes a stored pointer that reads as no guest pointer at all (no capture
 * can name it), and nothing else: a pointer any reader can use stays.
 */
function clearUnreadableGuestBody(): void {
  const active = inspectActiveDraftGuest();
  if (active.type !== "invalid" || active.capture !== null) return;
  try {
    if (localStorage.getItem(ACTIVE_DRAFT_GUEST_KEY) === null) return;
    localStorage.removeItem(ACTIVE_DRAFT_GUEST_KEY);
  } catch {
    // Unavailable storage is not evidence that the stored text is unreadable.
  }
}

/**
 * Clears a stored guest pointer that cannot be read as one, inspected and
 * removed inside the recovery-pointer transaction, so a pointer another writer
 * stored after the caller's own inspection stays. A transaction that cannot
 * run leaves the pointer.
 */
export async function clearActiveDraftGuest(): Promise<void> {
  await withActiveDraftPointer((pointer) => pointer.clearUnreadableGuest());
}

/** Do not erase a newer pod's host locator when an older pod is ended. */
export function clearActiveDraftPodFor(persistenceId: string): void {
  const current = inspectActiveDraftPod();
  if (current.type !== "absent" && current.capture?.id === persistenceId) clearActiveDraftPod();
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
    isDraftKind(value.kind) &&
    isPositiveInteger(value.podSize) &&
    typeof value.hostDisplayName === "string" &&
    (value.tournamentFormat === "Swiss" || value.tournamentFormat === "SingleElimination") &&
    (value.podPolicy === "Competitive" || value.podPolicy === "Casual") &&
    (value.phase === "lobby" || value.phase === "drafting" || value.phase === "review" || value.phase === "deckbuilding" ||
      value.phase === "pairing" || value.phase === "matchInProgress" || value.phase === "complete") &&
    isNonnegativeInteger(value.pickCount) &&
    isPositiveFiniteNumber(value.updatedAt)
  );
}

function isActiveDraftGuestMeta(value: unknown): value is ActiveDraftGuestMeta {
  if (!isRecord(value) || hasTransportTag(value)) return false;
  return isCanonicalRoomCode(value.roomCode)
    && isNonEmptyString(value.displayName)
    && isNonEmptyString(value.hostPeerId)
    && isPositiveFiniteNumber(value.timestamp)
    && isOptionalCapabilityId(value.capabilityId);
}

function isOptionalCapabilityId(value: unknown): value is string | undefined {
  return value === undefined || isNonEmptyString(value);
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

/**
 * The kinds a persisted pod may carry, DERIVED from `DRAFT_KINDS` rather than
 * restated beside it.
 *
 * `"Quick"` is excluded because a Quick draft is single-player against bots and
 * is never persisted as a P2P host session; that exclusion is expressed once,
 * here, as a filter over the authority.
 */
const PERSISTABLE_DRAFT_KINDS: readonly Exclude<DraftKind, "Quick">[] = DRAFT_KINDS.filter(
  (kind): kind is Exclude<DraftKind, "Quick"> => kind !== "Quick",
);

/**
 * IndexedDB is untrusted, so the kind is validated rather than asserted.
 *
 * Folds `PERSISTABLE_DRAFT_KINDS` instead of enumerating the kinds inline:
 * TypeScript never checks a type-guard BODY against the union in its
 * `value is …` clause, so a hand-written chain here would keep compiling —
 * and keep refusing — after `DRAFT_KINDS` grew. The symptom is silent: a
 * persisted host session of the new kind is classified corrupt and discarded
 * on resume, with no error raised anywhere.
 */
function isDraftKind(value: unknown): value is Exclude<DraftKind, "Quick"> {
  return PERSISTABLE_DRAFT_KINDS.some((kind) => kind === value);
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

/** An untagged pointer is a P2P guest's; any own `transport` key names another reader. */
function hasTransportTag(value: Record<string, unknown>): boolean {
  return Object.prototype.hasOwnProperty.call(value, "transport");
}

// ── Guest Persistence ──────────────────────────────────────────────────

export async function saveDraftGuestSession(
  hostPeerId: string,
  data: {
    draftToken: string;
    seatIndex: number;
    draftCode: string;
    roomCode: string;
    displayName: string;
    capabilityId: string;
  },
): Promise<void> {
  const roomCode = parseRoomCode(data.roomCode);
  const displayName = data.displayName.trim();
  if (!roomCode) throw new Error("Invalid draft room code");
  if (!displayName) throw new Error("Draft display name is required");
  if (!isNonEmptyString(data.capabilityId)) throw new Error("Draft recovery capability is required");
  const session: PersistedDraftGuestSession = {
    hostPeerId,
    draftToken: data.draftToken,
    seatIndex: data.seatIndex,
    draftCode: data.draftCode,
    roomCode,
    displayName,
    timestamp: Date.now(),
    capabilityId: data.capabilityId,
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
  identity?: Pick<ActiveDraftGuestMeta, "roomCode" | "displayName" | "capabilityId">,
): Promise<PersistedDraftGuestSession | null> {
  try {
    const session = await get<PersistedDraftGuestSession>(
      DRAFT_GUEST_PREFIX + hostPeerId,
      getDraftStore(),
    );
    if (!session || !isPersistedDraftGuestSession(session) || session.hostPeerId !== hostPeerId) return null;
    // An expired record stays; the next admission at this key replaces it.
    if (Date.now() - session.timestamp > GUEST_SESSION_TTL_MS) return null;
    if (identity && (session.roomCode !== identity.roomCode || session.displayName !== identity.displayName)) {
      return null;
    }
    // A pointer that names a capability binds only the record carrying that name.
    if (identity?.capabilityId !== undefined && session.capabilityId !== identity.capabilityId) return null;
    return session;
  } catch {
    return null;
  }
}

/**
 * Whether a stored private record is a live record of this owner's token,
 * room and display name. Expired and malformed records are owned by nobody.
 */
function ownsDraftGuestRecord(value: unknown, owner: DraftGuestCapabilityOwner): value is PersistedDraftGuestSession {
  return isPersistedDraftGuestSession(value)
    && Date.now() - value.timestamp <= GUEST_SESSION_TTL_MS
    && value.hostPeerId === owner.hostPeerId
    && value.draftToken === owner.draftToken
    && value.roomCode === parseRoomCode(owner.roomCode)
    && value.displayName === owner.displayName.trim();
}

/**
 * Reads the capability name the stored private record gives this token, or
 * null when no live record at the owner's key belongs to it. Read only.
 */
export async function loadDraftGuestCapability(
  owner: DraftGuestCapabilityOwner,
): Promise<DraftGuestCapability | null> {
  try {
    const record = await get<unknown>(DRAFT_GUEST_PREFIX + owner.hostPeerId, getDraftStore());
    if (!ownsDraftGuestRecord(record, owner)) return null;
    return {
      hostPeerId: owner.hostPeerId,
      draftToken: owner.draftToken,
      roomCode: owner.roomCode,
      displayName: owner.displayName,
      capabilityId: record.capabilityId ?? null,
    };
  } catch {
    return null;
  }
}

/**
 * Deletes the record at `key` only if `owns` holds for the value read in the
 * same read-write transaction, so a record written in between is never erased.
 * Any failure, including IndexedDB being unavailable, keeps the record.
 */
async function removeDraftRecordIf(
  key: string,
  owns: (value: unknown) => boolean,
): Promise<"removed" | "kept"> {
  try {
    return await getDraftStore()("readwrite", (store) => new Promise<"removed" | "kept">((resolve, reject) => {
      const read = store.get(key);
      read.onerror = () => reject(read.error);
      read.onsuccess = () => {
        let owned: boolean;
        try {
          owned = owns(read.result);
        } catch (error) {
          reject(error);
          return;
        }
        if (!owned) {
          resolve("kept");
          return;
        }
        store.delete(key);
        promisifyRequest(store.transaction).then(() => resolve("removed"), reject);
      };
    }));
  } catch {
    return "kept";
  }
}

/**
 * Removes a guest's private record and public pointer only where each
 * provably belongs to this capability; malformed, expired and ambiguous
 * storage stays.
 */
export async function clearDraftGuestRecovery(capability: DraftGuestCapability): Promise<void> {
  const removal = await removeDraftRecordIf(
    DRAFT_GUEST_PREFIX + capability.hostPeerId,
    (value) => ownsDraftGuestRecord(value, capability)
      && (value.capabilityId ?? null) === capability.capabilityId,
  );
  // The pointer is read and removed inside one recovery-pointer transaction.
  await withActiveDraftPointer((pointer) => {
    const active = inspectActiveDraftGuest();
    if (active.type !== "present") return;
    const { capture } = active;
    if (
      capture.hostPeerId !== capability.hostPeerId
      || capture.roomCode !== parseRoomCode(capability.roomCode)
      || capture.displayName !== capability.displayName.trim()
    ) return;
    const namesThisCapability = capture.capabilityId !== null
      ? capture.capabilityId === capability.capabilityId
      // An unnamed pointer leads to whatever record sits at its key, so it is
      // this capability's only when this clear has just removed its unnamed record.
      : capability.capabilityId === null && removal === "removed";
    if (namesThisCapability) pointer.clearGuestIfCurrent(capture);
  });
}

// ── Recovery-Pointer Transaction ──────────────────────────────────────

/**
 * The recovery pointer's generation counter, a non-secret integer kept under
 * this key in `localStorage` and in the draft store, beside the records the
 * pointer leads to, so clearing that store resets both.
 */
const ACTIVE_DRAFT_POINTER_GENERATION_KEY = "phase-active-draft-pointer-generation";

const ACTIVE_DRAFT_POINTER_RESOURCE: CrossTabResource = {
  lockName: "phase-active-draft-pointer",
  localGenerationKey: ACTIVE_DRAFT_POINTER_GENERATION_KEY,
  generationStore: getDraftStore,
  generationKey: ACTIVE_DRAFT_POINTER_GENERATION_KEY,
};

/**
 * The recovery pointer's operations, valid only inside the one body they were
 * handed to: each throws, and acts on nothing, once that body has returned.
 * Each compares and writes exactly as its exported counterpart describes.
 */
export interface ActiveDraftPointerTxn {
  publishNative(pointer: ActiveNativeDraftPointer): Promise<void>;
  republishNative(capture: NativeDraftPointerCapture, pointer: ActiveNativeDraftPointer): NativeDraftRepublication;
  publishGuest(meta: ActiveDraftGuestPublication): Promise<void>;
  republishGuest(
    standsOn: ActiveDraftGuestMetaCapture,
    meta: ActiveDraftGuestPublication,
  ): ActiveDraftGuestMetaCapture | null;
  clearGuestIfCurrent(capture: ActiveDraftGuestMetaCapture): void;
  clearNativeIfCurrent(capture: NativeDraftPointerCapture): boolean;
  clearUnreadableGuest(): void;
}

export type ActiveDraftPointerTxnResult<T> = CrossTabTxnResult<T, CrossTabTxnFailure>;

/** A recovery-pointer publication was refused before its step ran. */
export class ActiveDraftPointerUnavailableError extends Error {
  readonly reason: CrossTabTxnFailure;
  constructor(reason: CrossTabTxnFailure) {
    super(`Recovery pointer unavailable: ${reason}`);
    this.name = "ActiveDraftPointerUnavailableError";
    this.reason = reason;
  }
}

/**
 * Runs `body` inside the cross-tab recovery-pointer transaction: under its
 * exclusive lock, once this tab's storage shows every earlier step any tab
 * committed, so another tab's step lands wholly before `body` or wholly after
 * it. `body` is synchronous, and it reaches the pointer only through the
 * capability it is handed; it never calls an exported pointer function, whose
 * own lock request would wait on this one. A promise `body` returns is not
 * awaited under the lock. A transaction that cannot be confirmed in its bound
 * skips `body`, and a throw from `body` rejects. With no lock manager `body`
 * runs unguarded at the call, and nothing orders it against another tab.
 */
export async function withActiveDraftPointer<T>(
  body: (pointer: ActiveDraftPointerTxn) => T,
): Promise<ActiveDraftPointerTxnResult<T>> {
  const result = await runCrossTabTransaction(ACTIVE_DRAFT_POINTER_RESOURCE, () => {
    let open = true;
    const scoped = <A extends unknown[], R>(operation: (...args: A) => R) => (...args: A): R => {
      if (!open) throw new Error("Recovery-pointer transaction already ended");
      return operation(...args);
    };
    const pointer: ActiveDraftPointerTxn = {
      publishNative: scoped(publishNativeBody),
      republishNative: scoped(republishNativeBody),
      publishGuest: scoped(publishGuestBody),
      republishGuest: scoped(republishGuestBody),
      clearGuestIfCurrent: scoped(clearGuestIfCurrentBody),
      clearNativeIfCurrent: scoped(clearNativeIfCurrentBody),
      clearUnreadableGuest: scoped(clearUnreadableGuestBody),
    };
    try {
      // Boxed, so a removal the body started settles outside the lock.
      return { value: body(pointer) };
    } finally {
      open = false;
    }
  }, "run-unguarded");
  return result.status === "committed" ? { status: "committed", value: result.value.value } : result;
}

/** The value of a step that ran; a step that could not run rejects. */
function committedPointerStep<T>(result: ActiveDraftPointerTxnResult<T>): T {
  if (result.status === "skipped") throw new ActiveDraftPointerUnavailableError(result.reason);
  return result.value;
}

// ── Native Draft Capability ───────────────────────────────────────────

/**
 * The one form a native draft endpoint is stored and compared in: a ws/wss
 * address `parseWebSocketUrl` accepts, with no userinfo and no query, as its
 * serialized `href`. Null for anything else. Each output maps to itself.
 */
export function canonicalNativeDraftEndpoint(value: string): string | null {
  const url = parseWebSocketUrl(value.trim());
  // The query is tested on `href` rather than `search`, which is "" for a bare
  // trailing "?" that still serializes. With any fragment already refused, a
  // literal "?" in the serialized form can only be the query delimiter, since
  // a "?" in the path or userinfo percent-encodes to %3F.
  if (!url || url.username !== "" || url.password !== "" || url.href.includes("?")) return null;
  return url.href;
}

function isCanonicalNativeEndpoint(value: unknown): value is string {
  return typeof value === "string" && canonicalNativeDraftEndpoint(value) === value;
}

function isNativeDraftParticipant(value: unknown): value is NativeDraftParticipant {
  return value === "host" || value === "guest";
}

function isNativeDraftCapabilityRecord(value: unknown): value is NativeDraftCapabilityRecord {
  return isRecord(value)
    && isNonEmptyString(value.capabilityId)
    && isCanonicalNativeEndpoint(value.endpoint)
    && isNonEmptyString(value.draftCode)
    && isNonEmptyString(value.playerToken)
    && isNonnegativeInteger(value.seatIndex)
    && isNativeDraftParticipant(value.participant)
    && isPositiveFiniteNumber(value.timestamp);
}

function isActiveNativeDraftPointer(value: unknown): value is ActiveNativeDraftPointer {
  return isRecord(value)
    && value.transport === "native"
    && isNonEmptyString(value.capabilityId)
    && isCanonicalNativeEndpoint(value.endpoint)
    && isNonEmptyString(value.draftCode)
    && isNonnegativeInteger(value.seatIndex)
    && isNativeDraftParticipant(value.participant)
    && isPositiveFiniteNumber(value.timestamp);
}

/**
 * Commits a native seat's secret half and returns the pointer that leads to
 * it, without writing that pointer: only the installed owner publishes it.
 * Throws on invalid input or a failed write.
 */
export async function saveNativeDraftCapability(
  input: Omit<NativeDraftCapabilityRecord, "timestamp">,
): Promise<ActiveNativeDraftPointer> {
  const record: NativeDraftCapabilityRecord = {
    capabilityId: input.capabilityId,
    endpoint: input.endpoint,
    draftCode: input.draftCode,
    playerToken: input.playerToken,
    seatIndex: input.seatIndex,
    participant: input.participant,
    timestamp: Date.now(),
  };
  if (!isNativeDraftCapabilityRecord(record)) throw new Error("Invalid native draft recovery capability");
  await set(NATIVE_DRAFT_CAPABILITY_PREFIX + record.capabilityId, record, getDraftStore());
  return {
    transport: "native",
    capabilityId: record.capabilityId,
    endpoint: record.endpoint,
    draftCode: record.draftCode,
    seatIndex: record.seatIndex,
    participant: record.participant,
    timestamp: record.timestamp,
  };
}

/**
 * The capability a stored pointer's text names, read as a native pointer:
 * null for any text that is not one or names none. A malformed or expired
 * native pointer still names its capability.
 */
function nativePointerCapabilityId(raw: string | null): string | null {
  if (raw === null) return null;
  let value: unknown;
  try {
    value = JSON.parse(raw);
  } catch {
    return null;
  }
  return isRecord(value) && value.transport === "native" && isNonEmptyString(value.capabilityId)
    ? value.capabilityId
    : null;
}

/**
 * Removes a native seat's private record once no pointer leads to it. The
 * stored pointer is read in the same step as the delete, so a pointer that
 * names this capability again by then, its own resume's, keeps the record.
 * Unavailable storage keeps it.
 */
async function removeUnnamedNativeDraftCapability(capabilityId: string): Promise<void> {
  await removeDraftRecordIf(
    NATIVE_DRAFT_CAPABILITY_PREFIX + capabilityId,
    (value) => value !== undefined
      && nativePointerCapabilityId(localStorage.getItem(ACTIVE_DRAFT_GUEST_KEY)) !== capabilityId,
  );
}

/**
 * The one write of the tab's recovery pointer, over `stored`, the text it
 * replaces; throws as the write does. A native pointer it replaces leaves its
 * seat's private record with no route back, so the returned promise settles
 * once that record is removed or kept.
 */
function writeActiveDraftPointerOver(stored: string | null, text: string): Promise<void> {
  const replaced = nativePointerCapabilityId(stored);
  localStorage.setItem(ACTIVE_DRAFT_GUEST_KEY, text);
  return replaced === null ? Promise.resolve() : removeUnnamedNativeDraftCapability(replaced);
}

/** Writes the tab's one recovery pointer over whatever is stored. */
function replaceActiveDraftPointer(text: string): Promise<void> {
  let stored: string | null;
  try {
    stored = localStorage.getItem(ACTIVE_DRAFT_GUEST_KEY);
  } catch {
    stored = null;
  }
  return writeActiveDraftPointerOver(stored, text);
}

/**
 * Writes the tab's one recovery pointer only when `isCurrent` holds for the
 * stored text, reading, deciding and writing in one synchronous step; null
 * when it wrote nothing. A read it cannot make throws, as the write does: it
 * is no evidence that the stored pointer is current.
 */
function replaceActiveDraftPointerIfCurrent(
  text: string,
  isCurrent: (stored: string | null) => boolean,
): Promise<void> | null {
  const stored = localStorage.getItem(ACTIVE_DRAFT_GUEST_KEY);
  if (!isCurrent(stored)) return null;
  return writeActiveDraftPointerOver(stored, text);
}

/** A native pointer's stored text, its fields in their stored order. */
function nativePointerText(pointer: ActiveNativeDraftPointer): string {
  return JSON.stringify({
    transport: "native",
    capabilityId: pointer.capabilityId,
    endpoint: pointer.endpoint,
    draftCode: pointer.draftCode,
    seatIndex: pointer.seatIndex,
    participant: pointer.participant,
    timestamp: pointer.timestamp,
  });
}

/**
 * Writes a native pointer over whatever is stored; one that does not validate
 * is not written. Settles once a native record the pointer replaces is dealt with.
 */
function publishNativeBody(pointer: ActiveNativeDraftPointer): Promise<void> {
  if (!isActiveNativeDraftPointer(pointer)) return Promise.resolve();
  return replaceActiveDraftPointer(nativePointerText(pointer));
}

/**
 * Writes a native pointer over whatever is stored, inside the recovery-pointer
 * transaction; one that does not validate is not written. Settles once a
 * native record the pointer replaces is dealt with, after the transaction
 * ends. Rejects with `ActiveDraftPointerUnavailableError`, writing nothing,
 * when the transaction cannot run, and as the write does.
 */
export async function publishActiveNativeDraft(pointer: ActiveNativeDraftPointer): Promise<void> {
  const removal = committedPointerStep(await withActiveDraftPointer((txn) => txn.publishNative(pointer)));
  await removal;
}

/**
 * Inspects the native pointer without mutating it. Anything not tagged
 * `native` is absent to this reader; a tagged record that is malformed or
 * expired is invalid.
 */
export function inspectActiveNativeDraft(): ActiveNativeDraftLoadResult {
  let raw: string | null;
  try {
    raw = localStorage.getItem(ACTIVE_DRAFT_GUEST_KEY);
  } catch {
    return { type: "absent" };
  }
  if (raw === null) return { type: "absent" };
  let value: unknown;
  try {
    value = JSON.parse(raw);
  } catch {
    return { type: "absent" };
  }
  if (!isRecord(value) || !hasTransportTag(value) || value.transport !== "native") return { type: "absent" };
  const capture: NativeDraftPointerCapture = { raw };
  if (!isActiveNativeDraftPointer(value) || Date.now() - value.timestamp > GUEST_SESSION_TTL_MS) {
    return { type: "invalid", capture };
  }
  return {
    type: "present",
    pointer: {
      transport: "native",
      capabilityId: value.capabilityId,
      endpoint: value.endpoint,
      draftCode: value.draftCode,
      seatIndex: value.seatIndex,
      participant: value.participant,
      timestamp: value.timestamp,
    },
    capture,
  };
}

/** Reads the private record a native pointer names. Never mutates. */
export async function loadNativeDraftCapability(
  pointer: ActiveNativeDraftPointer,
): Promise<NativeDraftCapabilityLoadResult> {
  let value: unknown;
  try {
    value = await get<unknown>(NATIVE_DRAFT_CAPABILITY_PREFIX + pointer.capabilityId, getDraftStore());
  } catch (error) {
    return { type: "readFailure", error };
  }
  if (value === undefined) return { type: "absent" };
  if (!isNativeDraftCapabilityRecord(value) || Date.now() - value.timestamp > GUEST_SESSION_TTL_MS) {
    return { type: "invalid" };
  }
  if (
    value.capabilityId !== pointer.capabilityId
    || value.endpoint !== pointer.endpoint
    || value.draftCode !== pointer.draftCode
    || value.seatIndex !== pointer.seatIndex
    || value.participant !== pointer.participant
  ) {
    return { type: "mismatch" };
  }
  return { type: "present", record: value };
}

/**
 * What a native republication did. Either way `settled` settles once the
 * private records it may remove are dealt with.
 */
export type NativeDraftRepublication =
  | { readonly status: "published"; readonly settled: Promise<void> }
  | { readonly status: "superseded"; readonly settled: Promise<void> };

/**
 * Writes `pointer` only while the stored text is still `capture`, read and
 * written in one synchronous step. Otherwise it writes and removes nothing of
 * the stored pointer's, and removes this capability's record only once no
 * pointer names it. Throws on a pointer that does not validate or names
 * another capability than the capture, and as the read or the write does.
 */
function republishNativeBody(
  capture: NativeDraftPointerCapture,
  pointer: ActiveNativeDraftPointer,
): NativeDraftRepublication {
  if (!isActiveNativeDraftPointer(pointer) || nativePointerCapabilityId(capture.raw) !== pointer.capabilityId) {
    throw new Error("Invalid native draft recovery pointer");
  }
  const settled = replaceActiveDraftPointerIfCurrent(nativePointerText(pointer), (stored) => stored === capture.raw);
  return settled === null
    ? { status: "superseded", settled: removeUnnamedNativeDraftCapability(pointer.capabilityId) }
    : { status: "published", settled };
}

/**
 * A native republication compared and written inside the recovery-pointer
 * transaction; otherwise as `ActiveDraftPointerTxn.republishNative`. Rejects
 * with `ActiveDraftPointerUnavailableError`, writing nothing, when the
 * transaction cannot run, and on what the republication throws.
 */
export async function republishActiveNativeDraft(
  capture: NativeDraftPointerCapture,
  pointer: ActiveNativeDraftPointer,
): Promise<NativeDraftRepublication> {
  return committedPointerStep(await withActiveDraftPointer((txn) => txn.republishNative(capture, pointer)));
}

/**
 * Removes the native pointer only when its stored text is still the captured
 * text, and says whether it did.
 */
function clearNativeIfCurrentBody(capture: NativeDraftPointerCapture): boolean {
  try {
    if (localStorage.getItem(ACTIVE_DRAFT_GUEST_KEY) !== capture.raw) return false;
    localStorage.removeItem(ACTIVE_DRAFT_GUEST_KEY);
    return true;
  } catch {
    // Unavailable storage is not evidence that this caller owns the pointer.
    return false;
  }
}

/**
 * Removes the native pointer only when its stored text is still the captured
 * text, compared and removed inside the recovery-pointer transaction, and
 * says whether it did. A transaction that cannot run removes nothing.
 */
export async function clearActiveNativeDraftIfCurrent(capture: NativeDraftPointerCapture): Promise<boolean> {
  const cleared = await withActiveDraftPointer((pointer) => pointer.clearNativeIfCurrent(capture));
  return cleared.status === "committed" && cleared.value;
}

/**
 * Clears a native pointer a resume cannot use (expired, malformed, or leading
 * to an absent, invalid or mismatched record) when its stored text is still
 * the captured text, and then the private record it names, which no pointer
 * leads to any more.
 */
export async function clearUnusableNativeDraft(capture: NativeDraftPointerCapture): Promise<void> {
  if (!(await clearActiveNativeDraftIfCurrent(capture))) return;
  const capabilityId = nativePointerCapabilityId(capture.raw);
  if (capabilityId !== null) await removeUnnamedNativeDraftCapability(capabilityId);
}

/**
 * The guest's exact-clear rule for a native capability: its private record
 * goes only when it is a live record of this token and name, and a pointer
 * naming this capability goes whatever record sits behind it. Malformed,
 * expired and another token's records stay.
 */
export async function clearNativeDraftCapability(hold: NativeDraftCapabilityHold): Promise<void> {
  await removeDraftRecordIf(
    NATIVE_DRAFT_CAPABILITY_PREFIX + hold.capabilityId,
    (value) => isNativeDraftCapabilityRecord(value)
      && Date.now() - value.timestamp <= GUEST_SESSION_TTL_MS
      && value.capabilityId === hold.capabilityId
      && value.playerToken === hold.playerToken
      && value.endpoint === hold.endpoint
      && value.draftCode === hold.draftCode,
  );
  // The pointer is read and removed inside one recovery-pointer transaction.
  await withActiveDraftPointer((txn) => {
    const active = inspectActiveNativeDraft();
    if (active.type !== "present") return;
    const { pointer, capture } = active;
    if (
      pointer.capabilityId === hold.capabilityId
      && pointer.endpoint === hold.endpoint
      && pointer.draftCode === hold.draftCode
    ) {
      txn.clearNativeIfCurrent(capture);
    }
  });
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
    ...(submission.companion !== undefined ? { companion: submission.companion } : {}),
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
    const value = await get<unknown>(
      `${DRAFT_DECK_SUBMISSION_PREFIX}${hostPeerId}`,
      getDraftStore(),
    );
    if (!isPersistedDraftDeckSubmission(value) || value.hostPeerId !== hostPeerId) return null;
    if (identity && (value.roomCode !== roomCode || value.draftToken !== identity.draftToken)) {
      return null;
    }
    return value;
  } catch {
    return null;
  }
}

/**
 * Removes the outbox only when it is this owner's and, given a submission id,
 * only that submission; anything else at the key, malformed storage included,
 * stays, and so does everything when IndexedDB is unavailable.
 */
export async function clearDraftDeckSubmission(
  owner: DraftDeckSubmissionOwner,
  submissionId?: string,
): Promise<void> {
  await removeDraftRecordIf(
    `${DRAFT_DECK_SUBMISSION_PREFIX}${owner.hostPeerId}`,
    (value) => isPersistedDraftDeckSubmission(value)
      && value.hostPeerId === owner.hostPeerId
      && value.roomCode === parseRoomCode(owner.roomCode)
      && value.draftToken === owner.draftToken
      && (submissionId === undefined || value.submissionId === submissionId),
  );
}

function isPersistedDraftDeckSubmission(value: unknown): value is PersistedDraftDeckSubmission {
  return isRecord(value)
    && typeof value.hostPeerId === "string"
    && isNonEmptyString(value.draftCode)
    && isCanonicalRoomCode(value.roomCode)
    && isNonEmptyString(value.draftToken) && isNonEmptyString(value.submissionId)
    && Array.isArray(value.mainDeck) && value.mainDeck.every((card) => typeof card === "string")
    // A record written before the designation existed cannot be replayed:
    // the host would refuse it. Discarding it is the fail-safe answer — the
    // guest simply builds a fresh submission.
    && Array.isArray(value.commanders)
    && value.commanders.every((card) => typeof card === "string");
}

function isPersistedDraftGuestSession(value: unknown): value is PersistedDraftGuestSession {
  if (!isRecord(value)) return false;
  return isNonEmptyString(value.hostPeerId)
    && isNonEmptyString(value.draftToken)
    && isNonnegativeInteger(value.seatIndex)
    && typeof value.draftCode === "string"
    && isCanonicalRoomCode(value.roomCode)
    && isNonEmptyString(value.displayName)
    && isPositiveFiniteNumber(value.timestamp)
    && isOptionalCapabilityId(value.capabilityId);
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
