/**
 * Zustand store for multiplayer P2P draft state.
 *
 * Separate from `draftStore` (Quick Draft, single-player) because the
 * multiplayer draft lifecycle is fundamentally different:
 * - Host/guest asymmetry (host runs WASM, guest is stateless receiver)
 * - Lobby phase with seat management before draft starts
 * - Network events (disconnect, reconnect, kick, pause/resume)
 * - Pairing and match handoff after deckbuilding
 *
 * The store wraps `DraftPodHostAdapter` or `DraftPodGuestAdapter` and
 * projects their events into reactive Zustand state for the React UI.
 */

import { create, type StateCreator } from "zustand";

import type {
  DraftCardInstance,
  DraftPlayerView,
  PairingView,
  SeatPublicView,
  StandingEntry,
} from "../adapter/draft-adapter";
import type { EngineAdapter, GameAction, GameEvent, GameLogEntry, MatchScore, SubmitResult } from "../adapter/types";
import type { DraftCommanderLaunch, DraftMatchDeckPayload, DraftMatchLaunch, DraftMatchSettlement, DraftPauseReason } from "../network/draftProtocol";
import { MAX_MATERIALIZED_VIRTUAL_BASICS } from "../components/draft/workspace/types";
import type { DraftCardPlacement, DraftWorkspaceState } from "../components/draft/workspace/types";
import {
  appendWorkspaceInstanceToResolvedDestination,
  createDraftWorkspaceState,
  makeInteractiveVirtualBasicInstanceId,
  reconcileWorkspaceState,
  updateWorkspacePlacement,
} from "../components/draft/workspace/workspacePlacement";
import {
  addVirtualBasic,
  countProjectedNames,
  projectWorkspaceLandCounts,
  projectWorkspaceMainDeck,
  projectWorkspacePartition,
  removeVirtualBasic,
  type DraftWorkspacePartition,
} from "../components/draft/workspace/workspaceProjection";
import type { AISeatBinding } from "../game/controllers/aiController";
import { createGameLoopController, type GameLoopController } from "../game/controllers/gameLoopController";
import { processRemoteUpdate } from "../game/dispatch";
import type { P2PGuestAdapter, P2PHostAdapter } from "../adapter/p2p-adapter";
import type { SeatKind, SeatMutation } from "../multiplayer/seatTypes";
import type { HostResult, JoinResult } from "../network/connection";
import { debugLog } from "../game/debugLog";
import { reportStructuredActionRejection } from "../game/actionRejectionReporter";
import { resyncFromAdapterSafely } from "../game/staleStateWatchdog";
import { useGameStore } from "./gameStore";
import {
  DraftPodHostAdapter,
  type DraftPodHostConfig,
  type DraftPodHostEvent,
  type DraftPodHostStatus,
} from "../adapter/draftPodHostAdapter";
import {
  DraftPodGuestAdapter,
  type DraftPodGuestConfig,
  type DraftPodGuestEvent,
  type DraftPodGuestStatus,
} from "../adapter/draftPodGuestAdapter";
import type { DraftGuestRecoveryFailure } from "../adapter/p2p-draft-guest";
import {
  clearActiveDraftPod,
  clearActiveDraftGuest,
  clearActiveDraftGuestIfCurrent,
  clearDraftSettlementOutbox,
  loadDraftIntergameCommands,
  loadActiveDraftPod,
  inspectActiveDraftGuest,
  loadDraftGuestSession,
  loadDraftSettlementOutbox,
  saveActiveDraftPod,
  saveDraftIntergameCommands,
  saveDraftSettlementOutbox,
  type ActiveDraftPodMeta,
  type ActiveDraftPodPhase,
} from "../services/draftPersistence";
import { getEffectiveOffline } from "./connectivityStore";
import {
  commandAcknowledgement,
  consumeIntergamePermit,
  draftIntergameDigest,
  IntergameCommandController,
  type DraftIntergameCommand,
  type DraftIntergameCommandAck,
  type DraftIntergameCommandPayload,
} from "../services/intergameCommandLedger";
import type {
  DraftAutoPickPlacementHints,
  DraftPickDestination,
  DraftPickOutcome,
  DraftPickPlacementHint,
  PendingDraftPickIntent,
} from "./draftStore";
import { DRAFT_DECK_SESSION_KEY } from "./draftStore";
import { FORMAT_DEFAULTS } from "./multiplayerStore";

// ── Types ──────────────────────────────────────────────────────────────

export type DraftRole = "host" | "guest";

export const DRAFT_OFFLINE_ERROR = "offline.startUnavailable";

export type GuestDraftResumeOutcome = "resumed" | "absent" | "invalid" | "failed" | "offline" | "superseded";

/**
 * The pod SESSION's phase.
 *
 * Every member is the projection of an engine or adapter session status — the
 * union of the ranges of `phaseForDraftViewStatus`, `hostStatusToPhase` and
 * `guestStatusToPhase`. Nothing else belongs here.
 *
 * The Bo3 intergame window is deliberately NOT a member: the pod session is
 * `MatchInProgress` for the whole match, individual games included. See
 * {@link DraftPodScreen} and {@link draftPodScreen}.
 */
export type MultiplayerDraftPhase =
  | "idle"
  | "connecting"
  | "lobby"
  | "drafting"
  | "deckbuilding"
  | "pairing"
  | "matchInProgress"
  | "roundComplete"
  | "complete"
  | "error"
  | "kicked"
  | "hostLeft";

/** True only while a connected pod role has a live draft-session phase. */
export function isMultiplayerDraftPodLive(
  state: { role: DraftRole | null; phase: MultiplayerDraftPhase },
): boolean {
  return state.role !== null && (
    state.phase === "connecting"
    || state.phase === "lobby"
    || state.phase === "drafting"
    || state.phase === "deckbuilding"
    || state.phase === "pairing"
    || state.phase === "matchInProgress"
    || state.phase === "roundComplete"
  );
}

/**
 * The screen the pod UI shows.
 *
 * Every member but `betweenGames` is a `MultiplayerDraftPhase` — the projection
 * of the pod session's engine/adapter status. `betweenGames` is not a sibling of
 * `matchInProgress`: the pod session is `MatchInProgress` for the whole match,
 * individual games included, so the Bo3 intergame window is a refinement *within*
 * that phase, not an alternative to it. Keeping it out of `MultiplayerDraftPhase`
 * is what stops the five status writers (`statusChanged`, `viewUpdated`,
 * `draftStarted` on the host; `statusChanged`, `viewUpdated` on the guest) from
 * overwriting it — the clobber is no longer a rule to remember, it is a value
 * `tsc` refuses to accept.
 */
export type DraftPodScreen = MultiplayerDraftPhase | "betweenGames";

/**
 * Single authority for which pod screen the store's state calls for.
 *
 * The overlay is called for exactly while BOTH hold:
 *   1. the pod session is still in a match (`phase === "matchInProgress"`), and
 *   2. an intergame prompt is live (`sideboardPrompt !== null`).
 *
 * Conjunct 1 is released by every writer that moves the pod session on — round
 * boundary, tournament end, `Abandoned`, adapter error, `kicked`, `hostLeft`,
 * `leave`, `reset`. Conjunct 2 is released by the four writers that end the
 * window: `bo3GameStarted`, host `bo3GameStart`, guest `bo3GameStart`, and
 * `disposeMatchAdapter`.
 *
 * Conjunct 2 is an inference from a proxy, and the proxy's lifecycle has a hole:
 * nothing closes the window when the pod host's intergame orchestration
 * deadlocks. Do NOT "fix" that by latching a flag — a latch is what the phase
 * member was and what stranded the user. The deadlock's release is the viewer's,
 * via `DraftPodPage`'s local overlay dismissal, which suppresses the *rendering*
 * without touching this state.
 *
 * Keyed on `sideboardPrompt` alone, not `sideboardPrompt || playDrawPrompt`,
 * because `playDrawPrompt` is strictly nested inside it: the writers that set it
 * (`bo3ChoosePlayDraw`, host and guest) leave `sideboardPrompt` untouched, all
 * three writers that set `sideboardPrompt` null `playDrawPrompt`, and every
 * writer that clears one clears both. A `playDrawPrompt !== null` disjunct would
 * be unreachable, so no test could discriminate it. The nesting is pinned
 * directly instead — see the store tests.
 */
export function draftPodScreen(
  state: Pick<MultiplayerDraftState, "phase" | "sideboardPrompt">,
): DraftPodScreen {
  return state.phase === "matchInProgress" && state.sideboardPrompt !== null
    ? "betweenGames"
    : state.phase;
}

/**
 * Stable identity of the live intergame prompt, or `null` when none is live.
 *
 * Exists so a viewer's decision to hide the overlay can be scoped to the prompt
 * it was made about. Returning a primitive keeps it usable as a zustand selector
 * without `useShallow`.
 *
 * The third component is not redundant. One intergame window delivers TWO
 * prompts, and they carry the same `matchId` and the same `gameNumber`:
 * `transitionToPlayDraw` (`p2p-draft-host.ts`) sends the play/draw prompt with
 * `gameNumber: state.gameNumber`, and `bo3ChoosePlayDraw` sets `playDrawPrompt`
 * while leaving `sideboardPrompt` set. Without the discriminator a dismissal
 * made about sideboarding would carry straight over and suppress the play/draw
 * decision — a different decision, on a 10-second timer that auto-chooses
 * (`startPlayDrawTimer` -> `autoChoosePlayDraw`).
 *
 * Note that `playDrawPrompt`'s nesting inside `sideboardPrompt` has OPPOSITE
 * consequences at the two layers, and both are deliberate: `draftPodScreen`
 * keys on `sideboardPrompt` alone because the nesting means there is only one
 * screen answer, while this key must discriminate the prompt type precisely
 * because the nesting means the two prompts otherwise share an identity.
 */
export function intergamePromptKey(
  state: Pick<MultiplayerDraftState, "sideboardPrompt" | "playDrawPrompt">,
): string | null {
  return state.sideboardPrompt
    ? `${state.sideboardPrompt.matchId}#${state.sideboardPrompt.gameNumber}#${state.playDrawPrompt ? "pd" : "sb"}`
    : null;
}

export interface PairingInfo {
  round: number;
  table: number;
  opponentName: string;
  matchHostPeerId: string;
  matchId: string;
}

interface MultiplayerDraftState {
  role: DraftRole | null;
  phase: MultiplayerDraftPhase;
  roomCode: string | null;
  draftCode: string | null;
  seatIndex: number | null;
  view: DraftPlayerView | null;
  seats: SeatPublicView[];
  joined: number;
  total: number;
  paused: boolean;
  pauseReason: DraftPauseReason | null;
  pairing: PairingInfo | null;
  error: string | null;
  /** Recovery-only failure semantics, retained for an explicit retry CTA. */
  guestRecoveryFailure: DraftGuestRecoveryFailure | null;
  selectedCard: string | null;
  workspaceState: DraftWorkspaceState | null;
  pendingPickIntent: PendingDraftPickIntent | null;
  interactionGeneration: number;
  pickInteractionLocked: boolean;
  workspaceSyncError: string | null;
  mainDeck: string[];
  landCounts: Record<string, number>;
  timerRemainingMs: number | null;
  standings: StandingEntry[];
  currentRound: number;
  nextPairingRound: number;
  pairings: PairingView[];
  /** Full deck submitted during deckbuilding (mainDeck + lands). */
  submittedDeck: string[];
  submittedWorkspaceState: DraftWorkspaceState | null;
  submittedPartition: DraftWorkspacePartition | null;
  intergameWorkspaceState: DraftWorkspaceState | null;
  matchPairing: DraftMatchLaunch | null;
  matchAdapter: unknown | null;
  /**
   * The Commander pod's launch into ONE shared N-seat game.
   *
   * `commanderLaunch` is written to a NON-NULL value only from a
   * `commanderLaunch` event arm — the host's in `handleHostEvent` and the
   * guest's in `handleGuestEvent` — never directly by `launchCommanderGame` or
   * `joinCommanderGame`. Three writers clear it DELIBERATELY:
   * `disposeMatchAdapter`, but ONLY when a `matchAdapter` exists (its opening
   * guard); `cancelCommanderLaunch`; and `launchCommanderGame`'s failure arm
   * (the `set({ error })` in its catch). The last two must clear it explicitly
   * BECAUSE that guard excludes both paths — the store's `matchAdapter` is not
   * assigned until the success path, after `installMatchRuntime` returns.
   *
   * It is ALSO cleared wherever `initialState` is spread, which is every
   * session boundary and NOT only `leave`/`reset` — `hostDraft` and `joinDraft`
   * each spread it on their success and offline-error paths too. Do not read
   * the deliberate list above as exhaustive; grep `...initialState` for the
   * full set.
   */
  commanderLaunch: DraftCommanderLaunch | null;
  /** This client's own seat in the launched Commander game. */
  commanderSeat: number | null;
  /**
   * Guest: a `joinCommanderGame` is bringing its adapter up right now.
   *
   * The OBSERVABLE half of `commanderJoinInFlight`, which is module-local and
   * so cannot be selected from. The guest's join parks on `initializeGame()`
   * until every live seat has joined AND the host has called
   * `startPregameGame` — minutes, at a full table — and without this the Join
   * button is indistinguishable from a dead one for that whole span. The host
   * already had an observable for the same wait in `commanderLaunch`; this is
   * the guest's.
   *
   * A boolean rather than a narrowing type because the axis really is binary
   * and carries nothing: the room code, deck and game id all live on
   * `commanderLaunch`, which stays set for the whole join. Same shape as this
   * store's other in-flight flags (`paused`, `pickInteractionLocked`).
   */
  commanderJoinPending: boolean;
  /** Bo3: sideboard prompt state between games. */
  sideboardPrompt: {
    matchId: string;
    gameNumber: number;
    score: MatchScore;
    loserSeat: number | null;
    timerMs: number;
  } | null;
  /** Bo3: play/draw choice prompt. */
  playDrawPrompt: {
    matchId: string;
    gameNumber: number;
    score: MatchScore;
    timerMs: number;
  } | null;
  /** Bo3: whether this player has submitted their sideboard. */
  sideboardSubmitted: boolean;
}

interface MultiplayerDraftActions {
  /** Dismiss the current phase-scoped error banner. */
  clearError: () => void;
  /** Host: create a new draft pod and start accepting guests. */
  /** `true` only after the current adapter initialized and remains owned. */
  hostDraft: (config: DraftPodHostConfig) => Promise<boolean>;
  /** Guest: join an existing draft pod by room code. */
  joinDraft: (config: DraftPodGuestConfig) => Promise<boolean>;
  /** Reconnect exclusively through the persisted capability, never `draft_join`. */
  resumeDraft: (options?: { routeToken?: number; signal?: AbortSignal }) => Promise<GuestDraftResumeOutcome>;
  /** Host: start the draft once the pod is ready. */
  startDraft: (botFillEmptySeats?: boolean) => Promise<void>;
  /** Both: submit a pick. */
  submitPick: (cardInstanceId: string, destination?: DraftPickDestination, placementHint?: DraftPickPlacementHint) => Promise<DraftPickOutcome>;
  /** Both: submit one complete engine-defined pick step. */
  submitPickStep: (cardInstanceIds: readonly string[], destination?: DraftPickDestination, placementHint?: DraftPickPlacementHint) => Promise<DraftPickOutcome>;
  /** Both: submit a pick using a drafted card's draft-time effect. */
  submitPickWithDraftEffect: (effectCardInstanceId: string, cardInstanceIds: readonly [string, string], destination?: DraftPickDestination, placementHint?: DraftPickPlacementHint) => Promise<DraftPickOutcome>;
  /** Both: select a card (UI highlight before confirming pick). */
  selectCard: (cardInstanceId: string | null) => void;
  /** Both: confirm the currently selected card as pick. */
  confirmPick: (destination?: DraftPickDestination, placementHint?: DraftPickPlacementHint) => Promise<DraftPickOutcome>;
  /** Both: pick a card from the current pack using a deterministic draft heuristic. */
  autoPickCard: (placementHints?: DraftAutoPickPlacementHints) => Promise<DraftPickOutcome>;
  setWorkspaceState: (next: DraftWorkspaceState) => void;
  setWorkspacePlacement: (instanceId: string, placement: DraftCardPlacement) => void;
  addBasicLand: (name: string) => void;
  removeBasicLand: (name: string) => void;
  retryWorkspaceSync: () => Promise<void>;
  setIntergameWorkspaceState: (next: DraftWorkspaceState) => void;
  /** Both: submit the built deck. */
  submitDeck: (commanders?: string[]) => Promise<void>;
  /** Host: kick a player from the pod. */
  kickPlayer: (seat: number, reason?: string) => void;
  /** Host: pause the draft. */
  requestPause: () => void;
  /** Host: resume the draft. */
  requestResume: () => void;
  /** Both: tear down the connection and reset state. Lifecycle callers retain
   * recovery; an explicit leave revokes it only after host acknowledgement. */
  leave: (preserveRecovery?: boolean) => Promise<void>;
  /** Reset store to initial state (without network cleanup). */
  reset: () => void;
  /** Both: start the match for the current pairing. */
  startMatch: () => Promise<string | null>;
  /**
   * CR 903.13a: host the completed Commander pod's ONE shared N-seat game.
   *
   * Opens a per-launch P2P room, brings a `P2PHostAdapter` up on it with the
   * engine-piloted seats already claimed, puts a `draft_commander_launch` on
   * every live human seat (the host's own included), and navigates to
   * `?mode=draft-match`. It computes no game state. The seat count comes from
   * `view.seats`, never from the literal 4 — CR 903.13 fixes no pod size
   * (CR 800.1 only requires more than two), so the pod's own seat list is the
   * authority, bounded by the transport's own six-seat ceiling.
   */
  launchCommanderGame: (navigate: (path: string) => void) => Promise<void>;
  /**
   * CR 903.13a: join the Commander game this seat was invited to.
   *
   * The mirror of `launchCommanderGame` on a guest. Dials the room named by
   * this client's own `commanderLaunch`, brings a `P2PGuestAdapter` up on the
   * deck that launch carries, installs the runtime under the SHARED game id and
   * navigates to `?mode=draft-match`. The seat is never derived here — it
   * arrives on the wire as `playerIdentity` and is recorded in `commanderSeat`.
   */
  joinCommanderGame: (navigate: (path: string) => void) => Promise<void>;
  /**
   * Host: abandon a launch that is still coming up.
   *
   * Aborts the in-flight launch, tears the room down through `terminateGame()`
   * so every connected guest is told rather than left reconnecting, and clears
   * the launch state. A no-op when no launch is in flight.
   */
  cancelCommanderLaunch: () => Promise<void>;
  /**
   * Both: end the pod session iff the game being left was the pod's LAST act.
   *
   * THE single gate for "does leaving this `mode=draft-match` game end the
   * pod?", shared by every affordance that leaves one. It resolves when the
   * teardown has settled, so a caller navigates after awaiting it.
   */
  endCommanderSession: () => Promise<void>;
  /** Both: report a match result back to the pod host. */
  reportMatchResult: (matchId: string, winnerSeat: number | null) => Promise<void>;
  /** Both: report the active game result using the current draft match pairing. */
  reportActiveMatchGameResult: (gameWinner: number | null) => Promise<void>;
  /** Settles a bound match concession for the authenticated game seat. */
  reportActiveMatchConcession: (concedingGamePlayer?: number) => Promise<void>;
  /** Host: advance to the next round (Casual mode). */
  advanceRound: () => void;
  /** Host: override a match result (Casual mode). */
  overrideMatchResult: (matchId: string, winnerSeat: number | null) => void;
  /** Host: replace a disconnected player with a bot (Casual mode). */
  replaceSeatWithBot: (seat: number) => void;
  /** Both: submit sideboard between Bo3 games. */
  submitSideboard: (matchId: string, mainDeck: string[], sideboard: Array<{ name: string; count: number }>) => void;
  /** Both: choose play or draw (loser of previous game). */
  choosePlayDraw: (matchId: string, playFirst: boolean) => void;
  /** Both: handle between-games prompt from match adapter. */
  handleBetweenGamesPrompt: (prompt: { matchId: string; gameNumber: number; score: MatchScore; loserSeat: number | null; timerMs: number }) => void;
  /** Durable ingress; raw sideboard/play-draw transport is intentionally not exposed. */
  submitIntergameCommand: (payload: DraftIntergameCommandPayload) => Promise<void>;
  /** Executes only a pod-issued authorization after re-checking its immutable ack. */
  submitAuthorized: (command: DraftIntergameCommand, acknowledgement: DraftIntergameCommandAck) => Promise<void>;
}

// ── Module-level adapter refs ──────────────────────────────────────────

let activeHostAdapter: DraftPodHostAdapter | null = null;
let activeGuestAdapter: DraftPodGuestAdapter | null = null;
let activeHostEventUnsub: (() => void) | null = null;
let activeGuestEventUnsub: (() => void) | null = null;
let activeHostAbort: AbortController | null = null;
/**
 * The Commander launch currently bringing its room up.
 *
 * Sits beside `activeHostAbort` because it is the same shape: a module-local
 * `AbortController` that discriminates a deliberate teardown from a failure.
 * `hostDraft` (below) is the complete precedent — it already carries all three
 * parts used here: the module-local controller, a deliberately silent catch for
 * a superseded attempt, and an identity-guarded `finally`. It differs only in
 * discriminating on adapter/epoch identity where this reads `signal.aborted`,
 * itself this file's idiom. Deliberately NOT modelled on
 * `resumeGuestDraftAttempt`, which is a dedupe/coalesce record rather than an
 * abort handle.
 *
 * Two jobs. It is the in-flight guard for `launchCommanderGame`. The button's
 * own `disabled` (`DraftPodPage`'s `launchDisabled`) cannot do that job alone:
 * it keys on `commanderLaunch`, which is not written until the host's own
 * seat-0 launch is delivered, so it does not cover the window from the press to
 * that write — which is the whole `hostRoom` round-trip.
 * And it is the only handle on the adapter while the host is parked on
 * `await roomFull` — `matchAdapter` is not `set` until after that await —
 * which is exactly the window `cancelCommanderLaunch` exists for.
 *
 * `adapter` is NULLABLE because the slot is claimed BEFORE the adapter exists.
 * It has to be: `hostRoom` does real PeerJS signalling, hundreds of
 * milliseconds to seconds, and a guard that only takes effect after the
 * constructor lets a second press through that whole window — opening a second
 * room and a second adapter, sending every live seat a second launch, and
 * leaking the first adapter. Claiming the slot first also makes that window
 * CANCELLABLE, which is what `cancelCommanderLaunch` acts on.
 *
 * `AbortController` rather than a stored reject callback, because the catch arm
 * has to tell a CANCEL apart from a FAILURE: a failure banners and disposes, a
 * cancel does neither (`cancelCommanderLaunch` owns teardown through
 * `terminateGame()`, which flushes `host_left` before closing — `dispose()`
 * would race that).
 */
let commanderLaunchInFlight: { adapter: P2PHostAdapter | null; abort: AbortController } | null = null;
/**
 * The Commander JOIN currently bringing its adapter up, on a guest.
 *
 * Same shape and the same two jobs as `commanderLaunchInFlight` above, for the
 * mirror-image path. The re-entry half is what matters at N players: a second
 * press opens a second `joinRoom`, the host's `handleNewGuest` hands it the
 * NEXT waiting seat, and the human who was going to take that seat is kicked
 * "Lobby full" while `roomFull` fires on a ghost. `startMatch`'s guest arm has
 * the same shape and is harmless only because a 1v1 match has no third player
 * to displace.
 *
 * Claimed BEFORE the first `await` for the same reason the launch's is: the
 * `await import()` plus a full `joinRoom` signalling round-trip is exactly the
 * window a double-press sails through.
 */
let commanderJoinInFlight: { abort: AbortController } | null = null;

/**
 * THE single authority for abandoning a Commander bring-up still in flight.
 *
 * Both handles above are module-local and are released ONLY by their owner's
 * `finally`, which cannot run while that owner is parked — the host on
 * `await roomFull`, the guest on `joinRoom`'s PeerJS round-trip. Aborting the
 * signal is what unparks them, and it is the ONLY thing that does:
 * `disposeMatchAdapter` cannot help, because its whole body is fenced on a
 * `matchAdapter` the store does not hold until the launch has already
 * succeeded.
 *
 * So every path that ends a pod session has to come through here. Left parked,
 * the launch's re-entry guard stays claimed for the lifetime of the tab and
 * silently refuses every later launch — a new pod, a pressed button, and
 * nothing happens, with no error to explain it — while the `P2PHostAdapter`,
 * its Peer and the registered room leak.
 *
 * The two roles need different amounts of help, which is why this is one
 * function and not two call sites:
 *   - the GUEST's join needs only the abort. `joinCommanderGame`'s own catch
 *     owns its cleanup, disposing the adapter or destroying the bare peer.
 *   - the HOST's launch needs the abort AND `terminateGame()`, never
 *     `dispose()`: guests already seated must be told rather than left burning
 *     the reconnect backoff against a Peer that is gone. With no sessions it
 *     degrades to a dispose, so it is correct on both sides of that line.
 *
 * Returns the host teardown so a caller that can await it does; the aborts
 * themselves are synchronous, so a synchronous caller (`reset`) still gets the
 * whole unparking effect without awaiting.
 */
function abandonCommanderBringUp(): Promise<void> {
  commanderJoinInFlight?.abort.abort();
  const handle = commanderLaunchInFlight;
  if (!handle) return Promise.resolve();
  handle.abort.abort();
  // The `.catch` is not decoration: `send` has no rejection handling, so a
  // rejecting `host_left` would otherwise reject this whole teardown.
  return handle.adapter?.terminateGame().catch(() => {}) ?? Promise.resolve();
}

let activeGuestAbort: AbortController | null = null;
let activeHostRouteAbortListener: { signal: AbortSignal; listener: () => void } | null = null;
let activeGuestRouteAbortListener: { signal: AbortSignal; listener: () => void } | null = null;
let activeHostPersistenceId: string | null = null;
let draftAdapterEpoch = 0;
let resumeGuestDraftAttempt: {
  routeToken: number;
  signal: AbortSignal | undefined;
  promise: Promise<GuestDraftResumeOutcome>;
} | null = null;
const disposedHostAdapters = new WeakSet<DraftPodHostAdapter>();
const disposedGuestAdapters = new WeakSet<DraftPodGuestAdapter>();
const retainedDraftSessionTeardowns = new Map<string, Promise<void>>();
let activeMatchController: GameLoopController | null = null;
const intergameControllers = new Map<string, IntergameCommandController>();
const DRAFT_MATCH_FORMAT_CONFIG = FORMAT_DEFAULTS.Limited;
/**
 * The largest Commander pod this TRANSPORT can host, not the largest the rules
 * or the engine allow. `P2PHostAdapter`'s constructor throws `P2P_PLAYER_COUNT`
 * outside 2..6, while the engine's Commander Draft format allows
 * `max_players: 8` (`crates/engine/src/types/format.rs`). Mirrored here rather
 * than imported because `p2p-adapter` exports no such constant and the launch
 * must refuse BEFORE it loads that module.
 */
export const COMMANDER_P2P_SEAT_CEILING = 6;
let lifecycleGeneration = 0;
let workspaceRevision = 0;
let exclusivePickToken: symbol | null = null;
let restoredWorkspace: { generation: number; state: DraftWorkspaceState | null } | null = null;
let pendingGuestPick: {
  generation: number;
  resolve: (view: DraftPlayerView | null) => void;
} | null = null;

function cloneWorkspace(state: DraftWorkspaceState): DraftWorkspaceState {
  return {
    ...state,
    placements: Object.fromEntries(
      Object.entries(state.placements).map(([instanceId, placement]) => [instanceId, { ...placement }]),
    ),
    virtualBasics: state.virtualBasics.map((basic) => ({ ...basic })),
  };
}

function beginDraftLifecycle(): number {
  lifecycleGeneration += 1;
  workspaceRevision = 0;
  exclusivePickToken = null;
  restoredWorkspace = null;
  pendingGuestPick?.resolve(null);
  pendingGuestPick = null;
  return lifecycleGeneration;
}

function workspaceFacades(workspace: DraftWorkspaceState, view: DraftPlayerView) {
  return {
    mainDeck: projectWorkspaceMainDeck(workspace, view.pool),
    landCounts: projectWorkspaceLandCounts(workspace),
  };
}

function activeWorkspaceAdapter(): DraftPodHostAdapter | DraftPodGuestAdapter | null {
  const role = useMultiplayerDraftStore.getState().role;
  return role === "host" ? activeHostAdapter : role === "guest" ? activeGuestAdapter : null;
}

function publishWorkspace(workspace: DraftWorkspaceState): Promise<void> {
  const adapter = activeWorkspaceAdapter();
  if (!adapter) return Promise.resolve();
  const generation = lifecycleGeneration;
  const revision = ++workspaceRevision;
  return adapter.updateWorkspace(workspace).then(
    () => {
      if (generation === lifecycleGeneration && revision === workspaceRevision
        && activeWorkspaceAdapter() === adapter) {
        useMultiplayerDraftStore.setState({ workspaceSyncError: null });
      }
    },
    (error: unknown) => {
      if (generation === lifecycleGeneration && revision === workspaceRevision
        && activeWorkspaceAdapter() === adapter) {
        useMultiplayerDraftStore.setState({
          workspaceSyncError: error instanceof Error ? error.message : String(error),
        });
      }
    },
  );
}

function installWorkspace(input: {
  view: DraftPlayerView;
  base: DraftWorkspaceState;
  publish: boolean;
  patch?: Partial<MultiplayerDraftState>;
}): DraftWorkspaceState {
  const workspace = reconcileWorkspaceState(input.base, input.view.pool);
  const current = useMultiplayerDraftStore.getState();
  const next = {
    ...input.patch,
    ...workspaceFacades(workspace, input.view),
    view: input.view,
    workspaceState: workspace,
  };
  useMultiplayerDraftStore.setState(
    next.phase !== undefined && next.phase !== current.phase
      ? { error: null, ...next }
      : next,
  );
  if (input.publish) void publishWorkspace(workspace);
  return workspace;
}

function applyDestination(
  workspace: DraftWorkspaceState,
  pool: DraftPlayerView["pool"],
  instanceIds: readonly string[],
  destination: DraftPickDestination,
  placementHint?: DraftPickPlacementHint,
): DraftWorkspaceState {
  let next = workspace;
  for (const instanceId of instanceIds) {
    const placement = next.placements[instanceId];
    if (!placement) continue;
    next = appendWorkspaceInstanceToResolvedDestination(next, pool, instanceId, {
      zone: destination,
      column: placementHint?.column ?? placement.column,
      row: placementHint?.row ?? placement.row,
    });
  }
  return next;
}

function poolMultiplicity(pool: DraftPlayerView["pool"]): Map<string, number> {
  const counts = new Map<string, number>();
  for (const card of pool) counts.set(card.instance_id, (counts.get(card.instance_id) ?? 0) + 1);
  return counts;
}

function exactAddedIds(
  before: ReadonlyMap<string, number>,
  after: ReadonlyMap<string, number>,
): string[] | null {
  const added: string[] = [];
  for (const instanceId of new Set([...before.keys(), ...after.keys()])) {
    const previous = before.get(instanceId) ?? 0;
    const current = after.get(instanceId) ?? 0;
    if (current === previous) continue;
    if (previous !== 0 || current !== 1) return null;
    added.push(instanceId);
  }
  return added;
}

type MultiplayerPickRequest =
  | { kind: "pick"; instanceIds: readonly string[]; destination: DraftPickDestination; placementHint?: DraftPickPlacementHint }
  | { kind: "draft-effect"; effectCardInstanceId: string; instanceIds: readonly [string, string]; destination: DraftPickDestination; placementHint?: DraftPickPlacementHint }
  | {
      kind: "auto-pick";
      instanceIds: readonly string[];
      destination: "deck";
      placementHints?: DraftAutoPickPlacementHints;
    };

function intentForPick(request: MultiplayerPickRequest): PendingDraftPickIntent {
  switch (request.kind) {
    case "pick":
      return { kind: "pick", instanceIds: request.instanceIds, destination: request.destination, placementHint: request.placementHint };
    case "draft-effect":
      return { kind: "draft-effect", instanceIds: request.instanceIds, destination: request.destination, placementHint: request.placementHint };
    case "auto-pick":
      return { kind: "auto-pick", destination: "deck" };
  }
}

async function performPick(request: MultiplayerPickRequest): Promise<DraftPickOutcome> {
  if (exclusivePickToken) return { status: "ignored", reason: "busy" };
  if (request.kind === "pick" && (request.instanceIds.length === 0 || request.instanceIds.some((instanceId) => instanceId.length === 0))) {
    return { status: "rejected", reason: "invalid-request" };
  }
  if (request.kind === "auto-pick" && request.instanceIds.length === 0) {
    return { status: "rejected", reason: "invalid-request" };
  }
  if (request.kind === "draft-effect" && (request.instanceIds[0] === request.instanceIds[1]
    || request.instanceIds.some((instanceId) => instanceId.length === 0))) {
    return { status: "rejected", reason: "invalid-request" };
  }
  const state = useMultiplayerDraftStore.getState();
  const adapter = activeWorkspaceAdapter();
  if (!adapter || !state.view || !state.workspaceState) return { status: "rejected", reason: "invalid-request" };

  const token = Symbol("pick");
  exclusivePickToken = token;
  const generation = lifecycleGeneration;
  const before = poolMultiplicity(state.view.pool);
  const intent = intentForPick(request);
  useMultiplayerDraftStore.setState({ pendingPickIntent: intent, pickInteractionLocked: true });
  const isFresh = () => generation === lifecycleGeneration
    && exclusivePickToken === token
    && activeWorkspaceAdapter() === adapter;
  const cleanup = () => {
    if (exclusivePickToken !== token) return;
    exclusivePickToken = null;
    pendingGuestPick = null;
    useMultiplayerDraftStore.setState({ pendingPickIntent: null, pickInteractionLocked: false });
  };

  try {
    let acknowledgedView: DraftPlayerView | null;
    if (state.role === "host" && activeHostAdapter === adapter) {
      acknowledgedView = request.kind === "draft-effect"
        ? await adapter.submitPickWithDraftEffect(request.effectCardInstanceId, [...request.instanceIds])
        : await adapter.submitPick([...request.instanceIds]);
    } else if (state.role === "guest" && activeGuestAdapter === adapter) {
      const acknowledgement = new Promise<DraftPlayerView | null>((resolve) => {
        pendingGuestPick = { generation, resolve };
      });
      if (request.kind === "draft-effect") {
        await adapter.submitPickWithDraftEffect(request.effectCardInstanceId, [...request.instanceIds]);
      } else {
        await adapter.submitPick([...request.instanceIds]);
      }
      acknowledgedView = await acknowledgement;
    } else {
      acknowledgedView = null;
    }
    if (!isFresh()) return { status: "ignored", reason: "stale" };
    if (!acknowledgedView) {
      cleanup();
      return { status: "rejected", reason: "adapter" };
    }
    const added = exactAddedIds(before, poolMultiplicity(acknowledgedView.pool));
    const expected = request.instanceIds;
    const valid = added !== null
      && added.length === expected.length
      && expected.every((instanceId) => added.includes(instanceId));
    if (!valid) {
      cleanup();
      return { status: "rejected", reason: "unacknowledged" };
    }
    let workspace = reconcileWorkspaceState(state.workspaceState, acknowledgedView.pool);
    workspace = request.kind === "auto-pick"
      ? request.instanceIds.reduce(
        (next, instanceId) => applyDestination(
          next,
          acknowledgedView.pool,
          [instanceId],
          request.destination,
          request.placementHints?.[instanceId],
        ),
        workspace,
      )
      : applyDestination(
        workspace,
        acknowledgedView.pool,
        request.instanceIds,
        request.destination,
        request.placementHint,
      );
    exclusivePickToken = null;
    pendingGuestPick = null;
    installWorkspace({
      view: acknowledgedView,
      base: workspace,
      publish: true,
      patch: {
        phase: phaseForDraftViewStatus(acknowledgedView.status),
        selectedCard: null,
        pendingPickIntent: null,
        pickInteractionLocked: false,
      },
    });
    return { status: "acknowledged" };
  } catch {
    if (!isFresh()) return { status: "ignored", reason: "stale" };
    cleanup();
    return { status: "rejected", reason: "adapter" };
  }
}

interface DetachedDraftAdapters {
  host: DraftPodHostAdapter | null;
  guest: DraftPodGuestAdapter | null;
  hostPersistenceId: string | null;
}

function detachDraftAdapters(): DetachedDraftAdapters {
  const detached = {
    host: activeHostAdapter,
    guest: activeGuestAdapter,
    hostPersistenceId: activeHostPersistenceId,
  };
  activeHostAdapter = null;
  activeGuestAdapter = null;
  activeHostPersistenceId = null;
  activeHostAbort?.abort();
  activeGuestAbort?.abort();
  activeHostRouteAbortListener?.signal.removeEventListener("abort", activeHostRouteAbortListener.listener);
  activeGuestRouteAbortListener?.signal.removeEventListener("abort", activeGuestRouteAbortListener.listener);
  activeHostAbort = null;
  activeGuestAbort = null;
  activeHostRouteAbortListener = null;
  activeGuestRouteAbortListener = null;
  activeHostEventUnsub?.();
  activeGuestEventUnsub?.();
  activeHostEventUnsub = null;
  activeGuestEventUnsub = null;
  return detached;
}

async function disposeHostAdapter(adapter: DraftPodHostAdapter, preserveSession: boolean): Promise<void> {
  if (disposedHostAdapters.has(adapter)) return;
  disposedHostAdapters.add(adapter);
  await adapter.dispose({ preserveSession });
}

async function disposeGuestAdapter(adapter: DraftPodGuestAdapter, preserveRecovery = true): Promise<void> {
  if (disposedGuestAdapters.has(adapter)) return;
  disposedGuestAdapters.add(adapter);
  await adapter.dispose({ preserveRecovery });
}

async function disposeDetachedDraftAdapters(
  detached: DetachedDraftAdapters,
  preserveSession: boolean,
): Promise<void> {
  await Promise.allSettled([
    ...(detached.host ? [disposeHostAdapter(detached.host, preserveSession)] : []),
    ...(detached.guest ? [disposeGuestAdapter(detached.guest, preserveSession)] : []),
  ]);
}

/** Retains teardown after the active adapter ref has been detached on route abort. */
function retainDraftSessionTeardown(
  persistenceId: string | null,
  teardown: Promise<void>,
): void {
  if (!persistenceId) return;
  const previous = retainedDraftSessionTeardowns.get(persistenceId);
  const retained = previous
    ? previous.then(() => teardown, () => teardown)
    : teardown;
  retainedDraftSessionTeardowns.set(persistenceId, retained);
  void retained.then(
    () => {
      if (retainedDraftSessionTeardowns.get(persistenceId) === retained) {
        retainedDraftSessionTeardowns.delete(persistenceId);
      }
    },
    () => {
      if (retainedDraftSessionTeardowns.get(persistenceId) === retained) {
        retainedDraftSessionTeardowns.delete(persistenceId);
      }
    },
  );
}

/** Waits for the previous local owner of this persisted draft session to finish. */
async function claimDraftSessionOwner(persistenceId: string | undefined): Promise<void> {
  if (!persistenceId) return;
  await retainedDraftSessionTeardowns.get(persistenceId);
}

function lifecycleSignal(controller: AbortController): AbortSignal {
  return controller.signal;
}

function intergameAction(payload: DraftIntergameCommandPayload): GameAction {
  switch (payload.type) {
    case "SubmitSideboard":
      return { type: "SubmitSideboard", data: { main: payload.main, sideboard: payload.sideboard } };
    case "ChoosePlayDraw":
      return { type: "ChoosePlayDraw", data: { play_first: payload.playFirst } };
  }
}

const RARITY_SCORE: Record<string, number> = {
  mythic: 4,
  rare: 3,
  uncommon: 2,
  common: 1,
};

function preferredColors(pool: DraftCardInstance[]): Set<string> {
  const counts = new Map<string, number>();
  for (const card of pool) {
    for (const color of card.colors) {
      counts.set(color, (counts.get(color) ?? 0) + 1);
    }
  }

  return new Set(
    [...counts.entries()]
      .sort(([, a], [, b]) => b - a)
      .slice(0, 2)
      .map(([color]) => color),
  );
}

function curveScore(cmc: number, poolSize: number): number {
  if (poolSize < 5) {
    if (cmc <= 2) return 1;
    if (cmc >= 6) return -1;
    return 0;
  }

  if (cmc >= 2 && cmc <= 4) return 2;
  if (cmc >= 6) return -1;
  return 0;
}

function scoreDraftCard(card: DraftCardInstance, colors: Set<string>, poolSize: number): number {
  const rarityScore = (RARITY_SCORE[card.rarity.toLowerCase()] ?? 0) * 2;
  let colorScore = 0;
  if (card.colors.length === 0) {
    colorScore = 1;
  } else if (colors.size > 0) {
    colorScore = card.colors.some((color) => colors.has(color)) ? 3 : -1;
  }

  return rarityScore + colorScore + curveScore(card.cmc, poolSize);
}

/**
 * One whole CR 903.13b pick step: the top `view.required_pick_count` cards of
 * the current pack by the existing `scoreDraftCard` heuristic.
 *
 * The count is the engine's — never re-derived from `view.kind`, which cannot
 * see an odd pack's final one-card step. `Array.prototype.sort` is stable in
 * ES2019+ and sorts a COPY, so ties keep pack order and the N = 1 result is
 * identical to the previous first-best-wins linear scan. `preferredColors` and
 * the pool size are computed once, outside the comparator, exactly as before.
 *
 * Pick *quality* for a multi-card step is out of scope: these are the top N
 * independently scored cards, not a good pair.
 */
function chooseAutoPickCards(view: DraftPlayerView | null): string[] {
  const pack = view?.current_pack;
  if (!pack || pack.length === 0) return [];

  const colors = preferredColors(view.pool);
  const poolSize = view.pool.length;
  const scored = pack.map((card) => ({
    instanceId: card.instance_id,
    score: scoreDraftCard(card, colors, poolSize),
  }));
  scored.sort((a, b) => b.score - a.score);

  return scored.slice(0, view.required_pick_count).map((entry) => entry.instanceId);
}

function seatForLaunchGamePlayer(launch: DraftMatchLaunch, gamePlayer: number): number {
  switch (launch.type) {
    case "HumanHost":
      return gamePlayer === 0 ? launch.localSeat : launch.opponentSeat;
    case "HumanGuest":
      return gamePlayer === 1 ? launch.localSeat : launch.opponentSeat;
    case "Bot":
      return gamePlayer === 0 ? launch.localSeat : launch.botSeat;
  }
}

function winnerSeatForLaunch(launch: DraftMatchLaunch, gameWinner: number | null): number | null {
  return gameWinner === null ? null : seatForLaunchGamePlayer(launch, gameWinner);
}

function winnerSeatForGameResult(launch: DraftMatchLaunch, gameWinner: number | null): number | null {
  return gameWinner === null ? null : seatForLaunchGamePlayer(launch, gameWinner);
}

function localLaunchDeck(launch: DraftMatchLaunch) {
  switch (launch.type) {
    case "HumanHost":
    case "Bot":
      return launch.deckPayload.player;
    case "HumanGuest":
      return launch.localDeck;
  }
}

function nameMultiset(names: readonly string[]): Map<string, number> {
  const counts = new Map<string, number>();
  for (const name of names) counts.set(name, (counts.get(name) ?? 0) + 1);
  return counts;
}

function sameNameMultiset(left: readonly string[], right: readonly string[]): boolean {
  const leftCounts = nameMultiset(left);
  const rightCounts = nameMultiset(right);
  return leftCounts.size === rightCounts.size
    && [...leftCounts].every(([name, count]) => rightCounts.get(name) === count);
}

function disposeMatchController(): void {
  activeMatchController?.dispose();
  activeMatchController = null;
}

async function retryDraftSettlement(launch: DraftMatchLaunch, role: DraftRole): Promise<void> {
  if (launch.binding.matchAuthoritySeat !== launch.localSeat) return;
  const settlement = await loadDraftSettlementOutbox(launch.binding);
  if (!settlement) return;
  if (role === "host" && activeHostAdapter) {
    await activeHostAdapter.submitMatchSettlement(settlement);
    await clearDraftSettlementOutbox(launch.binding);
  } else if (role === "guest" && activeGuestAdapter) {
    activeGuestAdapter.sendMatchSettlement(settlement);
  }
}

/** The engine-side AI seat for a Bot draft match (the local player is game
 *  player 0, the bot is 1). Single authority shared by the live game-loop
 *  controller and the Resolve All batch drain, so the drain can never play
 *  the bot at a different difficulty than the controller driving it. */
export const DRAFT_BOT_AI_SEAT: AISeatBinding = { playerId: 1, difficulty: "Medium" };

async function installMatchRuntime(
  gameId: string,
  adapter: EngineAdapter,
  initResult: SubmitResult,
  controllerMode: "ai" | "online",
): Promise<void> {
  // Fetched after this match's engine is up, so the snapshot is
  // newest-by-construction under the global seq counter: it always passes the
  // commit gate, and it inherently drops any commit still in flight from the
  // previous game of a Bo3 (whose stamps are strictly lower).
  const snapshot = await adapter.getSnapshot();
  const initLogEntries: GameLogEntry[] = (initResult.log_entries ?? []).map((entry, i) => ({
    ...entry,
    seq: i,
  }));

  useGameStore.getState().commitEngineSnapshot(snapshot, {
    extraState: {
      gameId,
      gameMode: "draft-match",
      adapter,
      events: [] as GameEvent[],
      eventHistory: [] as GameEvent[],
      logHistory: initLogEntries,
      nextLogSeq: initLogEntries.length,
      stateHistory: [],
      turnCheckpoints: [],
      lobbyProgress: null,
    },
  });

  disposeMatchController();
  activeMatchController = createGameLoopController({
    mode: controllerMode,
    difficulty: DRAFT_BOT_AI_SEAT.difficulty,
    aiSeats: controllerMode === "ai" ? [DRAFT_BOT_AI_SEAT] : undefined,
    playerCount: 2,
  });
  activeMatchController.start();
}

function saveDraftPodProgress(phase: ActiveDraftPodPhase, view?: DraftPlayerView | null): void {
  const meta = loadActiveDraftPod();
  if (!meta) return;
  saveActiveDraftPod({
    ...meta,
    phase,
    pickCount: view?.pool.length ?? meta.pickCount,
    updatedAt: Date.now(),
  });
}

function updateActiveDraftPod(patch: Partial<ActiveDraftPodMeta>): void {
  const meta = loadActiveDraftPod();
  if (!meta) return;
  saveActiveDraftPod({ ...meta, ...patch, updatedAt: Date.now() });
}

function activePhaseForHostStatus(status: DraftPodHostStatus): ActiveDraftPodPhase | null {
  switch (status) {
    case "lobby":
    case "drafting":
    case "deckbuilding":
    case "pairing":
    case "matchInProgress":
    case "complete":
      return status;
    case "roundComplete":
      return "pairing";
    case "idle":
    case "connecting":
    case "error":
      return null;
  }
}

function phaseForDraftViewStatus(status: DraftPlayerView["status"]): MultiplayerDraftPhase {
  switch (status) {
    case "Lobby":
      return "lobby";
    case "Drafting":
    case "Paused":
      return "drafting";
    case "Deckbuilding":
      return "deckbuilding";
    case "Pairing":
      return "pairing";
    case "MatchInProgress":
      return "matchInProgress";
    case "RoundComplete":
      return "roundComplete";
    case "Complete":
      return "complete";
    case "Abandoned":
      return "error";
  }
}

function activePhaseForDraftViewStatus(status: DraftPlayerView["status"]): ActiveDraftPodPhase | null {
  switch (status) {
    case "Lobby":
      return "lobby";
    case "Drafting":
    case "Paused":
      return "drafting";
    case "Deckbuilding":
      return "deckbuilding";
    case "Pairing":
    case "RoundComplete":
      return "pairing";
    case "MatchInProgress":
      return "matchInProgress";
    case "Complete":
      return "complete";
    case "Abandoned":
      return null;
  }
}

/**
 * Release the game-store runtime installed by `installMatchRuntime`, but only
 * while `adapter` is still the adapter sitting in `useGameStore`.
 *
 * The identity guard is the whole point. A later bring-up may already have
 * committed its own adapter under a new game id, and an unconditional clear
 * would blank that live game on behalf of a dead one.
 *
 * Documented exemption from the `commitEngineSnapshot` single-writer invariant:
 * this is a teardown clear, not a live-game commit. It has no snapshot to gate
 * on, and any subsequent live commit arrives newest-by-construction (a fresh
 * post-init fetch), so it cannot be resurrected by a stale pair.
 */
function clearInstalledGameRuntime(adapter: unknown): void {
  if (!adapter || useGameStore.getState().adapter !== adapter) return;
  useGameStore.setState({
    gameId: null,
    gameState: null,
    events: [],
    eventHistory: [],
    logHistory: [],
    nextLogSeq: 0,
    adapter: null,
    waitingFor: null,
    legalActions: [],
    autoPassRecommended: false,
    endContinuousEffectOffers: [],
    spellCosts: {},
    legalActionsByObject: {},
    activationBlockReasons: {},
    stateHistory: [],
    turnCheckpoints: [],
  });
}

/**
 * Dispose the active match adapter (P2PHostAdapter or P2PGuestAdapter).
 *
 * Documented exemption from the `commitEngineSnapshot` single-writer invariant:
 * see `clearInstalledGameRuntime`, which performs the game-store half.
 */
function disposeMatchAdapter(set: SetFn): void {
  const state = useMultiplayerDraftStore.getState();
  disposeMatchController();
  if (state.matchAdapter) {
    const adapter = state.matchAdapter as { dispose?: () => void };
    adapter.dispose?.();
    clearInstalledGameRuntime(state.matchAdapter);
    set({
      matchAdapter: null,
      matchPairing: null,
      commanderLaunch: null,
      commanderSeat: null,
      sideboardPrompt: null,
      playDrawPrompt: null,
      sideboardSubmitted: false,
    });
  }
}

// ── Initial state ──────────────────────────────────────────────────────

const initialState: MultiplayerDraftState = {
  role: null,
  phase: "idle",
  roomCode: null,
  draftCode: null,
  seatIndex: null,
  view: null,
  seats: [],
  joined: 0,
  total: 0,
  paused: false,
  pauseReason: null,
  pairing: null,
  error: null,
  guestRecoveryFailure: null,
  selectedCard: null,
  workspaceState: null,
  pendingPickIntent: null,
  interactionGeneration: 0,
  pickInteractionLocked: false,
  workspaceSyncError: null,
  mainDeck: [],
  landCounts: {},
  timerRemainingMs: null,
  standings: [],
  currentRound: 0,
  nextPairingRound: 1,
  pairings: [],
  submittedDeck: [],
  submittedWorkspaceState: null,
  submittedPartition: null,
  intergameWorkspaceState: null,
  matchPairing: null,
  matchAdapter: null,
  commanderLaunch: null,
  commanderSeat: null,
  commanderJoinPending: false,
  sideboardPrompt: null,
  playDrawPrompt: null,
  sideboardSubmitted: false,
};

/**
 * Single authority for how long a pod error lives.
 *
 * An error belongs to the phase it was raised in; a *change* of phase retires
 * it. Wrapping the store's setter — rather than writing `error: null` into each
 * success arm — gives one rule for both roles: guests write `phase` from many
 * arms of `handleGuestEvent` and cleared it at none, while the host had a
 * single ad-hoc supersession on `pairingsGenerated`.
 *
 * Two properties are load-bearing and neither is incidental:
 *
 * - It fires only when `phase` actually *changes*. `viewUpdated` writes `phase`
 *   on every broadcast — a pick, a seat connecting, a seat dropping — so clearing
 *   on the mere presence of the key would erase an error the user has not read.
 *   That form was considered and rejected when this banner was introduced.
 * - The incoming payload wins. `kicked` and `hostLeft` write `phase` and `error`
 *   in one `set()`; spreading the payload last preserves the reason they carry.
 *
 * Not covered, deliberately: a retry that does not change phase — `startMatch`
 * fails and succeeds while `phase` is already `matchInProgress`. Dismissal
 * (`clearError`) remains the clearing path for that case.
 *
 * The Bo3 intergame window is no longer a phase at all — it is derived by
 * `draftPodScreen` from (`phase`, `sideboardPrompt`), and the three enter sites
 * (`handleBetweenGamesPrompt` and the two `bo3SideboardPrompt` arms) write no
 * `phase`. Three consequences for this rule, all intended:
 *
 * - Entering the window is not a transition, so an unread error survives it.
 * - Neither is any `viewUpdated`/`statusChanged`/`draftStarted` broadcast
 *   *during* the window: the phase is already `matchInProgress`, so a seat
 *   dropping mid-window no longer retires the banner. That narrowing was this
 *   block's own complaint and is now gone.
 * - Nor is the Bo3 game *boundary*: `bo3GameStarted` and the two `bo3GameStart`
 *   arms write `phase: "matchInProgress"` into a state whose phase is already
 *   `matchInProgress`, so the equal-phase short-circuit below returns `next`
 *   unchanged. An error raised earlier in the match therefore rides into game 2
 *   until the pod phase actually moves. This is a deliberate delta, not a bug;
 *   `clearError` remains the user's dismissal path.
 *
 * Scope: this wraps the *initializer's* setter, so it covers every write made
 * inside this module. It is not zustand middleware and does not rebind
 * `api.setState`, so `useMultiplayerDraftStore.setState(…)` bypasses the rule.
 * That is fine today — production has no such call site (only tests do) — but a
 * future production write through `setState` would not be phase-scoped.
 */
function clearErrorOnPhaseChange(set: SetFn): SetFn {
  return (partial) =>
    set((state) => {
      const next = typeof partial === "function" ? partial(state) : partial;
      return next.phase === undefined || next.phase === state.phase
        ? next
        : { error: null, ...next };
    });
}

/** Applies {@link clearErrorOnPhaseChange} to the store's setter.
 *
 * Borrows the `create<T>()(middleware(initializer))` *shape* from `gameStore`,
 * but it is not zustand middleware: it wraps only the initializer's `set`, so
 * external `useMultiplayerDraftStore.setState(…)` calls are not phase-scoped.
 * No production code does that (tests do); see `clearErrorOnPhaseChange`. */
function phaseScopedError(
  initializer: (
    set: SetFn,
    get: () => MultiplayerDraftState & MultiplayerDraftActions,
  ) => MultiplayerDraftState & MultiplayerDraftActions,
): StateCreator<MultiplayerDraftState & MultiplayerDraftActions> {
  return (set, get) => initializer(clearErrorOnPhaseChange(set), get);
}

// ── Store ──────────────────────────────────────────────────────────────

export const useMultiplayerDraftStore = create<
  MultiplayerDraftState & MultiplayerDraftActions
>()(phaseScopedError((set, get) => ({
  ...initialState,

  clearError: () => set({ error: null }),

  hostDraft: async (config) => {
    if (getEffectiveOffline()) {
      set({ error: DRAFT_OFFLINE_ERROR });
      return false;
    }
    const epoch = ++draftAdapterEpoch;
    const previous = detachDraftAdapters();
    const previousTeardown = disposeDetachedDraftAdapters(previous, true);
    retainDraftSessionTeardown(previous.hostPersistenceId, previousTeardown);
    if (previous.host || previous.guest) await previousTeardown;
    if (config.persistenceId) await claimDraftSessionOwner(config.persistenceId);
    if (epoch !== draftAdapterEpoch || config.signal?.aborted) return false;
    if (getEffectiveOffline()) {
      // Replacement teardown was authorized before connectivity changed. This
      // epoch now owns the detached lifecycle, so it must not leave the prior
      // role/phase live after declining to construct its successor.
      set({ ...initialState, error: DRAFT_OFFLINE_ERROR });
      return false;
    }

    const generation = beginDraftLifecycle();
    const adapter = new DraftPodHostAdapter();
    const controller = new AbortController();
    activeHostAdapter = adapter;
    activeHostAbort = controller;
    activeHostPersistenceId = config.persistenceId ?? null;

    activeHostEventUnsub = adapter.onEvent((event) => {
      if (generation === lifecycleGeneration && activeHostAdapter === adapter && epoch === draftAdapterEpoch) {
        handleHostEvent(event, set);
      }
    });

    const abortOwner = () => {
      controller.abort();
      if (activeHostAdapter !== adapter || epoch !== draftAdapterEpoch) return;
      ++draftAdapterEpoch;
      const detached = detachDraftAdapters();
      const teardown = disposeDetachedDraftAdapters(detached, true);
      retainDraftSessionTeardown(detached.hostPersistenceId, teardown);
      void teardown;
      set(initialState);
    };
    config.signal?.addEventListener("abort", abortOwner, { once: true });
    if (config.signal) activeHostRouteAbortListener = { signal: config.signal, listener: abortOwner };

    set({
      ...initialState,
      role: "host",
      phase: "connecting",
      seatIndex: 0,
      interactionGeneration: generation,
    });

    let initialized = false;
    try {
      await adapter.initialize({ ...config, signal: lifecycleSignal(controller) });
      initialized = true;
      if (activeHostAdapter !== adapter || epoch !== draftAdapterEpoch) return false;
      if (config.persistenceId) {
        const view = get().view;
        const phase = view ? activePhaseForDraftViewStatus(view.status) ?? "lobby" : "lobby";
        saveActiveDraftPod({
          id: config.persistenceId,
          roomCode: adapter.roomCode ?? config.preferredRoomCode ?? "",
          kind: config.kind,
          podSize: config.podSize,
          hostDisplayName: config.hostDisplayName,
          tournamentFormat: config.tournamentFormat,
          podPolicy: config.podPolicy,
          phase,
          pickCount: view?.pool.length ?? 0,
          updatedAt: Date.now(),
        });
      }
      return true;
    } catch {
      // The adapter reports the error while it is current. A late failure is
      // deliberately silent: its event gate was detached by the new owner.
    } finally {
      if (activeHostAdapter !== adapter || epoch !== draftAdapterEpoch) {
        config.signal?.removeEventListener("abort", abortOwner);
        await disposeHostAdapter(adapter, true);
      } else if (!initialized || adapter.status === "error") {
        activeHostAdapter = null;
        activeHostAbort = null;
        activeHostPersistenceId = null;
        activeHostEventUnsub?.();
        activeHostEventUnsub = null;
        config.signal?.removeEventListener("abort", abortOwner);
        activeHostRouteAbortListener = null;
        await disposeHostAdapter(adapter, true);
        if (epoch === draftAdapterEpoch && getEffectiveOffline()) {
          set({ ...initialState, error: DRAFT_OFFLINE_ERROR });
        }
      }
    }
    return false;
  },

  joinDraft: async (config) => {
    if (getEffectiveOffline()) {
      set({ error: DRAFT_OFFLINE_ERROR });
      return false;
    }
    const epoch = ++draftAdapterEpoch;
    const previous = detachDraftAdapters();
    const previousTeardown = disposeDetachedDraftAdapters(previous, true);
    retainDraftSessionTeardown(previous.hostPersistenceId, previousTeardown);
    if (previous.host || previous.guest) await previousTeardown;
    if (epoch !== draftAdapterEpoch || config.signal?.aborted) return false;
    if (getEffectiveOffline()) {
      // See hostDraft: this current replacement owns the already-detached
      // lifecycle and must publish an idle offline state rather than a phantom
      // connecting/lobby owner with no adapter.
      set({ ...initialState, error: DRAFT_OFFLINE_ERROR });
      return false;
    }

    const generation = beginDraftLifecycle();
    const adapter = new DraftPodGuestAdapter();
    const controller = new AbortController();
    activeGuestAdapter = adapter;
    activeGuestAbort = controller;

    activeGuestEventUnsub = adapter.onEvent((event) => {
      if (generation === lifecycleGeneration && activeGuestAdapter === adapter && epoch === draftAdapterEpoch) {
        handleGuestEvent(event, set);
      }
    });

    const abortOwner = () => {
      controller.abort();
      if (activeGuestAdapter !== adapter || epoch !== draftAdapterEpoch) return;
      ++draftAdapterEpoch;
      const detached = detachDraftAdapters();
      const teardown = disposeDetachedDraftAdapters(detached, true);
      retainDraftSessionTeardown(detached.hostPersistenceId, teardown);
      void teardown;
      set(initialState);
    };
    config.signal?.addEventListener("abort", abortOwner, { once: true });
    if (config.signal) activeGuestRouteAbortListener = { signal: config.signal, listener: abortOwner };

    set({
      ...initialState,
      role: "guest",
      phase: "connecting",
      interactionGeneration: generation,
    });

    let initialized = false;
    try {
      await adapter.initialize({ ...config, signal: lifecycleSignal(controller) });
      initialized = true;
      if (activeGuestAdapter === adapter && epoch === draftAdapterEpoch) return true;
    } catch {
      // See hostDraft: only the current owner is allowed to project errors.
    } finally {
      if (activeGuestAdapter !== adapter || epoch !== draftAdapterEpoch) {
        config.signal?.removeEventListener("abort", abortOwner);
        await disposeGuestAdapter(adapter);
      } else if (!initialized || adapter.status === "error") {
        activeGuestAdapter = null;
        activeGuestAbort = null;
        activeGuestEventUnsub?.();
        activeGuestEventUnsub = null;
        config.signal?.removeEventListener("abort", abortOwner);
        activeGuestRouteAbortListener = null;
        await disposeGuestAdapter(adapter);
        if (epoch === draftAdapterEpoch && getEffectiveOffline()) {
          set({ ...initialState, error: DRAFT_OFFLINE_ERROR });
        }
      }
    }
    return false;
  },

  resumeDraft: async (options = {}) => {
    if (getEffectiveOffline()) {
      set({ error: DRAFT_OFFLINE_ERROR });
      return "offline";
    }
    const routeToken = options.routeToken ?? 0;
    if (
      resumeGuestDraftAttempt
      && resumeGuestDraftAttempt.routeToken === routeToken
      && resumeGuestDraftAttempt.signal === options.signal
    ) {
      return resumeGuestDraftAttempt.promise;
    }

    const attempt: NonNullable<typeof resumeGuestDraftAttempt> = {
      routeToken,
      signal: options.signal,
      promise: Promise.resolve("superseded"),
    };
    const isCurrent = () => resumeGuestDraftAttempt === attempt && !options.signal?.aborted;
    attempt.promise = (async (): Promise<GuestDraftResumeOutcome> => {
      if (options.signal?.aborted) return "superseded";
      const active = inspectActiveDraftGuest();
      if (active.type === "absent") return "absent";
      if (active.type === "invalid") {
        if (active.capture) clearActiveDraftGuestIfCurrent(active.capture);
        else clearActiveDraftGuest();
        return "invalid";
      }
      const { meta: locator, capture } = active;
      const startingEpoch = draftAdapterEpoch;
      const locatorIsCurrent = () => {
        const current = inspectActiveDraftGuest();
        return current.type === "present"
          && current.capture.roomCode === capture.roomCode
          && current.capture.displayName === capture.displayName
          && current.capture.hostPeerId === capture.hostPeerId
          && current.capture.timestamp === capture.timestamp;
      };
        let session: Awaited<ReturnType<typeof loadDraftGuestSession>>;
      try {
        session = await loadDraftGuestSession(locator.hostPeerId, locator);
      } catch {
        if (!isCurrent() || draftAdapterEpoch !== startingEpoch || !locatorIsCurrent()) return "superseded";
        if (getEffectiveOffline()) {
          set({ error: DRAFT_OFFLINE_ERROR });
          return "offline";
        }
        return "failed";
      }
      if (!isCurrent() || draftAdapterEpoch !== startingEpoch || !locatorIsCurrent()) return "superseded";
      if (getEffectiveOffline()) {
        set({ error: DRAFT_OFFLINE_ERROR });
        return "offline";
      }
      if (!session) {
        clearActiveDraftGuestIfCurrent(capture);
        return "invalid";
      }

      if (!isCurrent() || draftAdapterEpoch !== startingEpoch || !locatorIsCurrent()) return "superseded";
      if (getEffectiveOffline()) {
        set({ error: DRAFT_OFFLINE_ERROR });
        return "offline";
      }
      const joined = await get().joinDraft({
        kind: "reconnect",
        roomCode: locator.roomCode,
        displayName: locator.displayName,
        hostPeerId: locator.hostPeerId,
        draftToken: session.draftToken,
        signal: options.signal,
      });
      if (!isCurrent()) return "superseded";
      if (joined) return "resumed";
      if (getEffectiveOffline()) {
        set({ error: DRAFT_OFFLINE_ERROR });
        return "offline";
      }
      if (get().guestRecoveryFailure?.kind === "invalid") {
        clearActiveDraftGuestIfCurrent(capture);
        return "invalid";
      }
      return "failed";
    })();
    resumeGuestDraftAttempt = attempt;
    try {
      return await attempt.promise;
    } finally {
      if (resumeGuestDraftAttempt === attempt) resumeGuestDraftAttempt = null;
    }
  },

  startDraft: async (botFillEmptySeats = true) => {
    if (!activeHostAdapter) return;
    if (getEffectiveOffline()) {
      set({ error: DRAFT_OFFLINE_ERROR });
      return;
    }
    await activeHostAdapter.startDraft(botFillEmptySeats);
  },

  submitPick: (cardInstanceId, destination = "deck", placementHint) => performPick({
    kind: "pick", instanceIds: [cardInstanceId], destination, placementHint,
  }),

  submitPickStep: (cardInstanceIds, destination = "deck", placementHint) => {
    const { view } = get();
    const selected = [...cardInstanceIds];
    const pack = view?.current_pack ?? [];
    if (
      !view
      || view.required_pick_count <= 0
      || selected.length !== view.required_pick_count
      || new Set(selected).size !== selected.length
      || selected.some((instanceId) => !pack.some((card) => card.instance_id === instanceId))
    ) {
      return Promise.resolve({ status: "rejected", reason: "invalid-request" });
    }
    return performPick({ kind: "pick", instanceIds: selected, destination, placementHint });
  },

  submitPickWithDraftEffect: (effectCardInstanceId, cardInstanceIds, destination = "deck", placementHint) => performPick({
    kind: "draft-effect", effectCardInstanceId, instanceIds: cardInstanceIds, destination, placementHint,
  }),

  selectCard: (cardInstanceId) => {
    if (get().pickInteractionLocked) return;
    set({ selectedCard: cardInstanceId });
  },

  confirmPick: (destination = "deck", placementHint) => {
    const { selectedCard } = get();
    if (!selectedCard) return Promise.resolve({ status: "rejected", reason: "invalid-request" });
    return performPick({ kind: "pick", instanceIds: [selectedCard], destination, placementHint });
  },

  autoPickCard: (placementHints) => {
    const { view } = get();
    const instanceIds = chooseAutoPickCards(view);
    if (instanceIds.length === 0) return Promise.resolve({ status: "rejected", reason: "invalid-request" });
    return performPick({
      kind: "auto-pick",
      instanceIds,
      destination: "deck",
      placementHints,
    });
  },

  setWorkspaceState: (next) => {
    const state = get();
    if (state.pickInteractionLocked || !state.view || !state.workspaceState) return;
    const reconciled = reconcileWorkspaceState(next, state.view.pool);
    if (reconciled === state.workspaceState) return;
    installWorkspace({ view: state.view, base: reconciled, publish: true });
  },

  setWorkspacePlacement: (instanceId, placement) => {
    const state = get();
    if (state.pickInteractionLocked || !state.view || !state.workspaceState) return;
    const next = updateWorkspacePlacement(state.workspaceState, state.view.pool, instanceId, placement);
    if (next === state.workspaceState) return;
    installWorkspace({ view: state.view, base: next, publish: true });
  },

  addBasicLand: (name) => {
    const state = get();
    if (state.pickInteractionLocked || !state.view || !state.workspaceState
      || state.workspaceState.virtualBasics.length >= MAX_MATERIALIZED_VIRTUAL_BASICS) return;
    const instanceId = makeInteractiveVirtualBasicInstanceId(state.workspaceState, state.view.pool);
    installWorkspace({
      view: state.view,
      base: addVirtualBasic(state.workspaceState, state.view.pool, { instanceId, name }),
      publish: true,
    });
  },

  removeBasicLand: (name) => {
    const state = get();
    if (state.pickInteractionLocked || !state.view || !state.workspaceState) return;
    const target = [...state.workspaceState.virtualBasics].reverse().find(
      (basic) => basic.name === name && state.workspaceState!.placements[basic.instanceId]?.zone === "deck",
    );
    if (!target) return;
    installWorkspace({
      view: state.view,
      base: removeVirtualBasic(state.workspaceState, target.instanceId),
      publish: true,
    });
  },

  retryWorkspaceSync: async () => {
    const state = get();
    if (!state.view || !state.workspaceState) return;
    await publishWorkspace(reconcileWorkspaceState(state.workspaceState, state.view.pool));
  },

  setIntergameWorkspaceState: (next) => {
    const state = get();
    if (draftPodScreen(state) !== "betweenGames" || !state.view || !state.intergameWorkspaceState) return;
    const workspace = reconcileWorkspaceState(next, state.view.pool);
    if (workspace === state.intergameWorkspaceState) return;
    set({ intergameWorkspaceState: workspace });
  },

  submitDeck: async (commanders = []) => {
    const { role, view, workspaceState } = get();
    if (!view || !workspaceState) return;
    const workspace = reconcileWorkspaceState(workspaceState, view.pool);
    const partition = projectWorkspacePartition(workspace, view.pool);

    if (role === "host" && activeHostAdapter) {
      const nextView = await activeHostAdapter.submitDeck(partition.mainDeck, commanders);
      installWorkspace({
        view: nextView,
        base: workspace,
        publish: false,
        patch: {
          submittedDeck: partition.mainDeck,
          submittedWorkspaceState: cloneWorkspace(workspace),
          submittedPartition: partition,
        },
      });
    } else if (role === "guest" && activeGuestAdapter) {
      await activeGuestAdapter.submitDeck(partition.mainDeck, commanders);
      set({
        submittedDeck: partition.mainDeck,
        submittedWorkspaceState: cloneWorkspace(workspace),
        submittedPartition: partition,
      });
    }
  },

  kickPlayer: (seat, reason) => {
    if (!activeHostAdapter) return;
    activeHostAdapter.kickPlayer(seat, reason);
  },

  requestPause: () => {
    if (!activeHostAdapter) return;
    activeHostAdapter.requestPause();
  },

  requestResume: () => {
    if (!activeHostAdapter) return;
    activeHostAdapter.requestResume();
  },

  launchCommanderGame: async (navigate) => {
    const { role, view, seatIndex, roomCode } = get();
    if (
      role !== "host" ||
      !view ||
      view.launch_capability !== "CommanderMultiplayer" ||
      view.status !== "Complete" ||
      seatIndex === null ||
      !activeHostAdapter
    ) {
      return;
    }
    // A second press while the first launch is still coming up would open a
    // SECOND room and a second adapter, put two `commanderLaunch` messages on
    // every live seat, and leak the first adapter. The button's own `disabled`
    // keys on `commanderLaunch`, which is not written until the launches have
    // been sent, so it leaves the whole `hostRoom` round-trip uncovered — the
    // guard belongs here as well, not instead.
    if (commanderLaunchInFlight) return;

    // CR 903.13a pods can seat more players than this TRANSPORT carries: the
    // engine's Commander Draft format allows eight (`max_pod_size`), while
    // `P2PHostAdapter` throws `P2P_PLAYER_COUNT` above six. Such a pod is
    // legal, its decks are real, and it still gets a game — a LOCAL one, with
    // every drafted deck and the other seats engine-piloted. This is not a
    // consolation prize invented here: it is what every Commander pod did
    // before the multiplayer launch existed, and dropping it turned a working
    // outcome into a permanently disabled button for 7- and 8-seat pods.
    //
    // Decided FIRST, before a room or an adapter is acquired, because the two
    // outcomes share nothing past this point: no P2P room, no seat mutations,
    // no launches on the wire, so none of the in-flight/cancel machinery below
    // applies and none of it is entered.
    if (view.seats.length > COMMANDER_P2P_SEAT_CEILING) {
      // `podCommanderDeckPayload`, NOT `commanderSeatDecks`: this needs ONE
      // game's payload in game-player order (local seat becomes player 0, the
      // rest ascending), which is a game-shaped ordering rule the host adapter
      // owns. `commanderSeatDecks` answers a different question — per-seat
      // decks split by who is live for a P2P launch — and has no meaning here,
      // where nobody is live and there is nothing to send.
      let payload: DraftMatchDeckPayload;
      try {
        payload = await activeHostAdapter.podCommanderDeckPayload(view, seatIndex);
      } catch (err) {
        // A refusal from draft-wasm reaches here: `get_bot_deck_inner` returns
        // `Err` when it cannot judge a bot deck's legality (no card database)
        // or when the deck it built is under the session's floor. Surface it
        // and do NOT navigate — the shape `startMatch`'s own catch uses.
        console.error("[multiplayerDraftStore] local Commander launch failed:", err);
        set({ error: err instanceof Error ? err.message : String(err) });
        return;
      }
      const localGameId = crypto.randomUUID();
      sessionStorage.setItem(`${DRAFT_DECK_SESSION_KEY}:${localGameId}`, JSON.stringify(payload));
      useGameStore.setState({ gameId: localGameId });
      // No `source=draft`/`draftId=`: those bind a game to a LOCAL Quick-Draft
      // run's bookkeeping, and a pod has neither a `DraftRun` nor active-quick-
      // draft meta. The pod is already `Complete`, so there is nothing to
      // report back to it. `commanderLaunch` is deliberately left NULL — no
      // launch went on any wire, there is no pod session to end, and
      // `endCommanderSession` must therefore leave this pod alone.
      navigate(
        `/game/${localGameId}?mode=ai&difficulty=${DRAFT_BOT_AI_SEAT.difficulty}` +
          `&format=CommanderDraft&players=${view.seats.length}&match=bo1`,
      );
      return;
    }

    const hostAdapter = activeHostAdapter;
    const localSeat = seatIndex;
    const gameId = crypto.randomUUID();
    // PER-LAUNCH, never a stable derivation of the pod's own code. A stable
    // code lets a guest still holding a PREVIOUS attempt's launch dial the room
    // a relaunch just opened — reopening the kick race from the stale-guest
    // side — and it cannot avoid `hostRoom`'s `unavailable-id` stall on a retry
    // while the signaling server still holds the previous registration. Shaped
    // after `P2PDraftHost`'s own derived match code; `preferredRoomCode`
    // bypasses `parseRoomCode`, so a derived code need not be five characters.
    const commanderRoomCode = `${roomCode ?? "draft"}-commander-${gameId.slice(0, 8)}`;

    const abort = new AbortController();
    // `host` is declared above the `try` because the catch's two cleanup arms
    // have to reach it. `handle` is claimed BEFORE the first `await` — the
    // re-entry guard above is only as good as the moment this assignment
    // happens, and everything from here to the constructor (a dynamic import
    // and a full `hostRoom` signalling round-trip) would otherwise be a window
    // a second press sails straight through.
    let host: HostResult | undefined;
    const handle: { adapter: P2PHostAdapter | null; abort: AbortController } = { adapter: null, abort };
    commanderLaunchInFlight = handle;
    try {
      // `startMatch`'s local precedent: the transport modules load through
      // `await import()` so the P2P bundle stays out of the pod's chunk. This
      // is a deliberate code-split, not a CLAUDE.md inline-import violation —
      // both modules' TYPE imports are static at file top.
      const [{ hostRoom }, { P2PHostAdapter }] = await Promise.all([
        import("../network/connection"),
        import("../adapter/p2p-adapter"),
      ]);

      // The room is LIVE before any seat is told about it.
      host = await hostRoom(abort.signal, { preferredRoomCode: commanderRoomCode });

      // RE-READ, and this is a CONTRACT rather than caution.
      // `P2PDraftHost.commanderSeatDecks`/`sendCommanderLaunches` require the
      // FRESHEST published view and say so in their own doc: reading it at call
      // time is explicitly the caller's responsibility, because
      // `handleGuestDisconnect` drops a `guestSessions` entry SYNCHRONOUSLY
      // while the engine's `connected` flag only reaches this store later. The
      // `view` destructured at the top of this function was read before a
      // dynamic import and a full `hostRoom` PeerJS round-trip — hundreds of
      // milliseconds to seconds — so by here it can be arbitrarily stale.
      //
      // The two reads may legitimately disagree, and the disagreement resolves
      // in favour of the FRESH one, both ways:
      //   - a seat live at the press and gone by now is classified
      //     engine-piloted (D2: a seat with no live connection at launch gets
      //     an engine pilot, not an invitation nothing can deliver). Against
      //     the stale read it is classified live, gets no engine seat,
      //     `sendToSeat` no-ops for it with no session, the seat stays
      //     `WaitingHuman`, `roomFull` never fires, and the host parks on
      //     "Waiting for players to join…" forever.
      //   - a seat that reconnected inside the window is invited rather than
      //     silently botted.
      //
      // `seats.length` cannot change across the window (a Complete pod's seat
      // list is fixed), so the ceiling refusal above stays valid; if that were
      // ever untrue the constructor's own `P2P_PLAYER_COUNT` throw lands in the
      // catch below as a banner, which is the safe direction.
      const launchView = get().view;
      // The pod itself can end inside that window: `leave`/`reset` null the
      // view. Both now abort this launch too, so the catch's `signal.aborted`
      // arm returns silently ahead of any banner — and a null view WITHOUT an
      // abort is a real failure that deserves one.
      if (!launchView) throw new Error("The pod ended before the Commander game could be launched.");

      // PURE — sends nothing, and synthesizes every seat's deck exactly once.
      const decks = await hostAdapter.commanderSeatDecks(launchView, localSeat);

      // INVARIANT, not a hope: a non-null `handle.adapter` means
      // `cancelCommanderLaunch` can reach the adapter. Rechecking here is what
      // establishes it — without this, a cancel landing during deck assembly
      // would still construct and initialize an adapter that nothing owns.
      //
      // No `await` between here and `handle.adapter = matchAdapter` below. The
      // cancel arm (the `abort.signal.aborted` branch of the catch, which
      // returns) and the failure arm below it (the `if (handle.adapter)`
      // cleanup, reached only when NOT aborted) both branch on `handle.adapter`
      // as an ownership invariant — null means this function still owns the
      // bare `HostResult`. An `await` inserted here yields control to a cancel
      // between the adapter existing and the handle knowing it, which leaks or
      // double-frees the Peer depending on which arm runs.
      abort.signal.throwIfAborted();

      const matchAdapter = new P2PHostAdapter(
        {
          player: decks.hostDeck,
          opponent: { main_deck: [], sideboard: [], commander: [], planar_deck: [], scheme_deck: [] },
          ai_decks: [],
          // CR 903.13f(3): the point at which the draft's set list ENTERS the
          // game pipeline under `mode=draft-match`. `DeckListPayload` is
          // module-private in `p2p-adapter`, so this literal is untyped and a
          // typo'd key is invisible to `tsc` — the store test asserts the field
          // instead. `?? null` narrows the view's OPTIONAL field onto the
          // wire's required-nullable one; it is not `?? []`, which would assert
          // "the draft contained zero sets" where the host knows the answer.
          draft_set_codes: launchView.draft_set_codes ?? null,
        },
        host.peer,
        host.onGuestConnected,
        launchView.seats.length,
        FORMAT_DEFAULTS.CommanderDraft,
        // ENGINE-OWNED: read the view's config, never construct one. A frontend
        // inventing a `MatchConfig` is what the superseded `&match=bo1` URL did
        // wrong.
        launchView.match_config,
        undefined /* gracePeriodMs */,
        undefined /* broker */,
        true /* ownsBroker */,
        undefined /* brokerGameCode */,
        // DELIBERATE divergence from PLAN.md D.3, which asked for
        // `{ gameId, roomCode }`. `startMatch` passes `undefined` here too, and
        // the record's only consumers are GameProvider's `p2p-host` RESUME
        // branch plus the adapter's own resume-record and engine-state
        // persistence — none of which `mode=draft-match` reads, because that
        // mode has no resume at all. Supplying it would only add a
        // `saveP2PHostSession` write per lifecycle event that nothing loads.
        undefined /* persistence */,
        // MUST stay undefined. `NativeP2PBridge` receives only
        // `hostDeck.player`, and `startPregameGameInner` returns through
        // `nativeBridge.start()` BEFORE the reconstruction that carries
        // `draft_set_codes` — so a desktop Commander host would silently drop
        // the CR 903.13f(3) partner grant for every seat while the wasm guest
        // gate still enforces it, leaving host and guest disagreeing about deck
        // legality on desktop only.
        undefined /* native */,
      );

      handle.adapter = matchAdapter;

      let resolveRoomFull!: () => void;
      const roomFull = new Promise<void>((resolve, reject) => {
        resolveRoomFull = resolve;
        // `cancelCommanderLaunch` aborts this signal to unpark the
        // launch; the catch below reads `signal.aborted` to tell that cancel
        // apart from a real failure.
        abort.signal.addEventListener("abort", () => reject(abort.signal.reason), { once: true });
      });
      // A cancel can settle this BEFORE the `await` below is reached; keep the
      // rejection observable without it becoming an UNHANDLED REJECTION. Same
      // reason, and the same one-liner, as the adapter's own `pregameReady`
      // gate (`P2PHostAdapter.resetPregameReady`, whose trailing
      // `void this.pregameReady.catch(() => {})` is the keep-alive).
      // DO NOT DROP THIS LINE.
      void roomFull.catch(() => {});

      // ATTACHED BEFORE `initialize()`. `applySeatMutation` emits `roomFull`
      // from INSIDE itself once it fills the last waiting seat, so a listener
      // attached after the AI mutations hangs an all-bot pod unconditionally.
      //
      // Exactly two arms. Under `mode=draft-match` `GameProvider` ADOPTS this
      // adapter and returns without subscribing — only its `isP2P` branch calls
      // `adapter.onEvent` — so this listener is the ONLY path by which a
      // `stateChanged` snapshot reaches the screen; without it the host's game
      // renders once and then freezes. `startMatch`'s
      // `GameOver`/`BetweenGamesSideboard`/`gameOver` arms are deliberately NOT
      // copied: they dereference `matchPairing`, which this launch leaves null,
      // and every `gameOver` emit is inside `P2PGuestAdapter`. A flat `if`
      // chain, so a further arm can be added by extension.
      matchAdapter.onEvent((event) => {
        if (event.type === "roomFull") {
          resolveRoomFull();
        }
        if (event.type === "stateChanged") {
          processRemoteUpdate(event.snapshot, event.events, event.logEntries).catch((err) => {
            // A rejected delivery is otherwise gone and the screen freezes on
            // the previous state — surface it and re-sync immediately.
            debugLog(`commander launch remote update failed: ${err instanceof Error ? err.message : String(err)}`);
            resyncFromAdapterSafely("delivery rejected");
          });
        }
      });

      await matchAdapter.initialize();

      // The engine-piloted seats claim their indices BEFORE anyone is invited.
      // A guest's `handleNewGuest` parks on `pregameReady`, which resolves at
      // the END of `initializeInner`, so a guest invited earlier takes
      // `firstWaitingSeat()` = 1 and the `SetKind` on that index then
      // invalidates its token and KICKS it.
      for (const { seat, deck } of decks.engineSeatDecks) {
        // `DraftDeckPayload` is structurally assignable to
        // `DeckChoice.DeckList.data`; naming the two engine types makes a
        // mistyped key a `tsc` error rather than a silent shape mismatch.
        const kind: SeatKind = {
          type: "Ai",
          data: {
            difficulty: DRAFT_BOT_AI_SEAT.difficulty,
            deck: { type: "DeckList", data: deck },
          },
        };
        const mutation: SeatMutation = { type: "SetKind", data: { seatIndex: seat, kind } };
        await matchAdapter.applySeatMutation(mutation);
      }

      // An abort only rejects the parked `roomFull` wait — it does not interrupt
      // an `await` that is already in flight. Re-read the signal here so a cancel
      // that landed during `initialize()` or the seat mutations stops the launch
      // BEFORE any guest is invited, rather than inviting everyone to a room
      // being torn down. Client idiom: `GameProvider`'s `setupP2P` rechecks with
      // `signal.throwIfAborted()` after each await. The throw lands in the catch
      // below, whose first statement returns silently on `signal.aborted`.
      abort.signal.throwIfAborted();

      // ONLY NOW is anyone invited. The host's own launch returns through
      // `sendToSeat`'s seat-0 local emit and lands in `handleHostEvent`.
      hostAdapter.sendCommanderLaunches(launchView, gameId, commanderRoomCode, decks);

      await roomFull;
      const initResult = await matchAdapter.startPregameGame();
      await installMatchRuntime(gameId, matchAdapter, initResult, "online");
      // The launch tail is a cancel window like any other: an abort rejects a
      // PARKED promise, it never interrupts an await already in flight, so a
      // cancel landing across the two awaits above would otherwise tear the
      // room down and then still navigate the host into it. The callees do
      // recheck their own disposal, but that is THEIR contract, not this
      // function's.
      abort.signal.throwIfAborted();
      // No `phase: "matchInProgress"`, unlike `startMatch`: the host stays on
      // `CompleteView` until it navigates, which is what lets step 4 render the
      // launch-in-flight state and its Cancel control.
      set({ matchAdapter, commanderSeat: 0 });
      navigate(`/game/${gameId}?mode=draft-match`);
    } catch (err) {
      // A CANCELLED launch is NOT a failure, and this discriminator must come
      // FIRST. `cancelCommanderLaunch` owns both the teardown (through
      // `terminateGame()`, which flushes the pending `host_left` sends that
      // `dispose()` would race) and the store state. Reporting here would
      // render the error banner `PodErrorBanner` reads — exactly what the user
      // just dismissed.
      if (abort.signal.aborted) {
        // A bare `HostResult` is the ONE resource `terminateGame()` can
        // never reach — it hangs off no adapter — so a launch cancelled before
        // the constructor has to release its own signalling registration.
        // Past the constructor, `handle.adapter` is non-null and the cancel
        // path owns the teardown; disposing here would race its `host_left`
        // flush.
        if (!handle.adapter) host?.destroy();
        return;
      }
      // The failure window straddles adapter creation, so cleanup has two arms.
      // `HostResult.destroy` is the documented sole authoritative teardown for
      // the signaling connection — never `host.peer.destroy()`. Leaving the room
      // registered makes the user's retry hit `unavailable-id`, which `hostRoom`
      // retries three times with 3s backoff and then REJECTS.
      // `terminateGame()`, not `dispose()`: a failure AFTER `roomFull` resolves
      // has live guest sessions, and `dispose()` closes them with no
      // `host_left`, so every guest burns the full reconnect backoff against a
      // Peer that is already gone. With no sessions it has nothing to flush and
      // degrades to a dispose, so it is correct on both sides of that line. The
      // `.catch` is not decoration: `send` has no rejection handling, so a
      // rejecting `host_left` would throw OUT of this catch block, skip the
      // error banner below and reject a `void`-ed call site — the user would
      // see a launch that silently did nothing.
      if (handle.adapter) await handle.adapter.terminateGame().catch(() => {});
      else host?.destroy();
      // A refusal from draft-wasm reaches here: `get_bot_deck_inner` returns
      // `Err` when it cannot judge a bot deck's legality (no card database) or
      // when the deck it built is under the session's floor. Surface it and do
      // NOT navigate — the same shape `startMatch`'s own catch uses.
      console.error("[multiplayerDraftStore] launchCommanderGame failed:", err);
      // The launch state goes with it. This catch covers the WHOLE try, so on
      // most of its paths — `hostRoom`, `commanderSeatDecks`, `initialize()`,
      // the seat mutations — `commanderLaunch` was never written and the clear
      // is a harmless no-op. It is load-bearing only past
      // `sendCommanderLaunches`, where the host's own seat-0 local emit has
      // already written it: `disposeMatchAdapter`'s clear cannot help there,
      // because its opening `matchAdapter` guard excludes this path —
      // `matchAdapter` is `set` only on the success path below. Left set, the
      // host would carry an error banner AND a permanently pending launch.
      set({
        error: err instanceof Error ? err.message : String(err),
        commanderLaunch: null,
        commanderSeat: null,
      });
    } finally {
      // Identity-guarded: an unconditional null would clear a SECOND launch's
      // handle when the first fails late.
      if (commanderLaunchInFlight === handle) commanderLaunchInFlight = null;
    }
  },

  joinCommanderGame: async (navigate) => {
    const launch = get().commanderLaunch;
    if (!launch) return;
    // A second press opens a SECOND `joinRoom`, which the host answers with the
    // NEXT waiting seat — kicking a later human "Lobby full" and firing
    // `roomFull` on a ghost seat. Claimed before the first `await`, because the
    // dynamic import plus the PeerJS round-trip below is the whole window.
    if (commanderJoinInFlight) return;

    // The signal has one LIVE use — `joinRoom` parks on it for the whole PeerJS
    // round-trip — and no driver: nothing calls `handle.abort.abort()`, because
    // the only Cancel affordance belongs to the host's `cancelCommanderLaunch`.
    // The `throwIfAborted()` and the `aborted` arm of the catch below are
    // therefore unreached today, and are kept so this handle stays symmetric
    // with `commanderLaunchInFlight` rather than diverging into a second shape.
    //
    // TRAP for whoever adds a guest-side cancel: aborting late is not enough.
    // By the time control reaches the `throwIfAborted()` below,
    // `installMatchRuntime` has already committed the engine snapshot into
    // `useGameStore` and started an `activeMatchController`, and the catch's
    // `dispose()` releases neither — `disposeMatchAdapter`, which would, is
    // fenced on a `matchAdapter` this path never sets. A guest cancel must tear
    // down the controller and the game store itself.
    const abort = new AbortController();
    const handle: { abort: AbortController } = { abort };
    commanderJoinInFlight = handle;
    // Published in the SAME statement sequence that claims the module handle,
    // so the two can never disagree about whether a join is running.
    set({ commanderJoinPending: true });
    // Declared above the `try` because the catch's two cleanup arms have to
    // reach them, exactly as `launchCommanderGame` declares its `host`.
    let join: JoinResult | undefined;
    let matchAdapter: P2PGuestAdapter | undefined;
    try {
      // The same deliberate code-split as `launchCommanderGame`: the P2P bundle
      // stays out of the pod's chunk. Both modules' TYPE imports are static at
      // file top, so this is not a CLAUDE.md inline-import violation.
      const [{ joinRoom }, { P2PGuestAdapter }] = await Promise.all([
        import("../network/connection"),
        import("../adapter/p2p-adapter"),
      ]);

      // `joinRoom` takes the cancellation signal as its SECOND parameter and
      // parks on it for the whole PeerJS round-trip — the longest span in the
      // join, and the one worth making cancellable.
      join = await joinRoom(launch.roomCode, abort.signal);

      matchAdapter = new P2PGuestAdapter(
        // Only this seat's own drafted deck. `launch.draftSetCodes` is NOT
        // plumbed in here: deck legality under CR 903.13f(3) is judged
        // host-side from the HOST's payload, and a guest's `deckData` never
        // carries a set list.
        { player: launch.localDeck },
        join.peer,
        join.conn.peer,
        join.conn,
        // NO tenth `matchConcedeBound` argument, deliberately unlike
        // `startMatch`'s 1v1 guest arm. That flag makes the guest send
        // `match_concede`, which this host refuses — it was constructed without
        // a `boundMatchConcede`, because whole-match settlement is a 1v1
        // primitive an N-player pod game has no meaning for. The host answers
        // with `action_failed` ("Whole-match concession is unavailable for this
        // game"), and the guest's `matchConcedeSent` latch then suppresses every
        // retry for the REST OF THE SESSION — it is reset only on a session
        // attach or the host-disconnect path, so a reconnect clears it. Bound,
        // the Concede button would be inert until one of those happened.
        //
        // That is the lesser reason. `boundMatchConcede` reports a PAIRWISE
        // match result and early-returns on the null `matchPairing` a Commander
        // launch leaves, so it has no meaning for an N-player pod game whatever
        // the latch does. Left unbound, the guest falls through to the
        // engine-level Concede instead (CR 104.3a; CR 800.4a).
      );

      // ATTACHED BEFORE `initialize()`, and this is the load-bearing ordering.
      // The adapter emits `playerIdentity` on BOTH of its setup paths, and both
      // settle the promise `initializeGame()` awaits moments later: `game_setup`
      // settles on the very next line, while `reconnect_ack` emits a
      // `stateChanged` in between and settles a few lines further down. Either
      // way the emit is inside the bring-up, so a listener attached after that
      // await misses it and the seat silently falls back to 0 — every guest then
      // rendering and acting as the HOST's seat. The `reconnect_ack` path also
      // shows why the `stateChanged` arm below is not optional: it delivers a
      // snapshot through this same listener on every reconnect.
      //
      // Under `mode=draft-match` `GameProvider` ADOPTS this adapter and returns
      // without subscribing, so this listener is also the ONLY path by which a
      // `stateChanged` snapshot reaches the screen; without it the guest's game
      // renders once and then freezes. `startMatch`'s guest arm's
      // `GameOver`/`gameOver` arms are deliberately NOT copied: they dereference
      // `matchPairing`, which a Commander launch leaves null.
      matchAdapter.onEvent((event) => {
        if (event.type === "playerIdentity") {
          // The wire is the ONLY truthful source of this seat. Human guests are
          // seated in CONNECTION order, so a pod-seat-3 player can legitimately
          // land on game seat 1 — which is why the launch payload carries no
          // seat index to derive it from.
          set({ commanderSeat: event.playerId });
        }
        if (event.type === "stateChanged") {
          processRemoteUpdate(event.snapshot, event.events, event.logEntries).catch((err) => {
            // A rejected delivery is otherwise gone and the screen freezes on
            // the previous state — surface it and re-sync immediately.
            debugLog(`commander join remote update failed: ${err instanceof Error ? err.message : String(err)}`);
            resyncFromAdapterSafely("delivery rejected");
          });
        }
      });

      await matchAdapter.initialize();
      const initResult = await matchAdapter.initializeGame();
      // The SHARED game id: every seat installs its runtime under the id the
      // host opened. Awaiting the whole bring-up BEFORE navigating is REQUIRED,
      // not stylistic — `GameProvider`'s `draft-match` branch is passive, it
      // asserts the runtime is already installed and bails to `onNoDeck`
      // otherwise. It is also what puts `commanderSeat` in the store before
      // `setupDraftMatchAvatars` reads it.
      await installMatchRuntime(launch.gameId, matchAdapter, initResult, "online");
      abort.signal.throwIfAborted();
      // `matchAdapter` in the store is load-bearing, not bookkeeping:
      // `disposeMatchAdapter`'s whole body is fenced on it, so a guest that
      // never set it would leak its Peer and its listeners on `leave`.
      set({ matchAdapter });
      navigate(`/game/${launch.gameId}?mode=draft-match`);
    } catch (err) {
      // The adapter never reached the store on this path, so
      // `disposeMatchAdapter` can never reach it either: this is the only place
      // its Peer and listeners can be released. Before the constructor, the
      // bare `JoinResult` owns the Peer instead — the mirror of the launch's
      // `handle.adapter` / `host` split.
      if (matchAdapter) {
        matchAdapter.dispose();
        // `installMatchRuntime` may ALREADY have committed this adapter into
        // `useGameStore` before `throwIfAborted()` fired — it awaits a snapshot
        // fetch, which is exactly the window a cancel lands in. `set({
        // matchAdapter })` never ran on this path and `disposeMatchAdapter`'s
        // whole body is fenced on that field, so this is the only place the
        // committed runtime can be released. Left behind it is a DISPOSED
        // adapter sitting under a live `draft-match` game id that no later
        // `leave`/`reset`/`endCommanderSession` can reach.
        clearInstalledGameRuntime(matchAdapter);
      } else {
        join?.destroyPeer();
      }
      // A cancelled join is a user action, not a failure. `commanderLaunch`
      // deliberately stays set on BOTH arms: the invitation is still open and
      // the seat can still be taken.
      if (abort.signal.aborted) return;
      console.error("[multiplayerDraftStore] joinCommanderGame failed:", err);
      set({ error: err instanceof Error ? err.message : String(err) });
    } finally {
      // Identity-guarded, as the launch's is: an unconditional null would clear
      // a SECOND join's handle when the first fails late — and would likewise
      // tell the UI no join is running while one still is.
      if (commanderJoinInFlight === handle) {
        commanderJoinInFlight = null;
        set({ commanderJoinPending: false });
      }
    }
  },

  cancelCommanderLaunch: async () => {
    const handle = commanderLaunchInFlight;
    // Reachable whenever the UI offers Cancel after the launch has already
    // settled. It must not throw, must not raise a banner, and must not clear
    // state a later launch owns.
    if (!handle) return;

    // Unparks the launch and tears its room down — see
    // `abandonCommanderBringUp`, which `leave` and `reset` share so there is
    // exactly one implementation of "abandon a bring-up" rather than three.
    // Its `await roomFull` rejects, its catch reads `signal.aborted` and
    // returns silently, and its own identity-guarded `finally` releases the
    // module handle.
    await abandonCommanderBringUp();
    // `disposeMatchAdapter` is NOT the escape hatch here — its body is fenced on
    // a `matchAdapter` the store only holds on the success path, so on a cancel
    // it is a no-op. Without this write the host keeps a launch that no longer
    // exists, and any waiting state keyed on that field would spin on it
    // forever.
    set({ commanderLaunch: null, commanderSeat: null });
  },

  endCommanderSession: async () => {
    // `mode=draft-match` covers TWO flows, and only one of them ends the pod.
    //
    // A pairwise pod match (`startMatch`) is mid-tournament: the pod must
    // survive it so the next round can be paired, and every "back to pod"
    // affordance has always been a bare navigate for that reason.
    //
    // A Commander launch (`launchCommanderGame` / `joinCommanderGame`) is the
    // pod's LAST act — one shared N-seat game for the whole pod — so its
    // transport and the pod session end together. Without the `leave()` the
    // player returns to a `CompleteView` holding a live adapter and a
    // `commanderLaunch` for a game that is already over, which renders the
    // waiting state (and its Cancel) forever against a `commanderLaunchInFlight`
    // handle its own `finally` already released: Launch disabled, Cancel inert,
    // waiting text permanent. A guest is offered Join for a game that has ended.
    //
    // DO NOT DELETE THE CONDITION. It does not look redundant by accident — the
    // two flows are indistinguishable from any call site's props, and making the
    // `leave()` unconditional (the shape this fix was first specified as) tears
    // down a LIVE pod tournament every time any player finishes a round.
    // `GamePage.commanderTeardown.test.tsx` fails against exactly that edit.
    //
    // `commanderLaunch` is the discriminator, and the choice is deliberate over
    // the equivalent `matchPairing === null`. The two agree today: the field is
    // written non-null ONLY by the `commanderLaunch` arms of `handleHostEvent`
    // and `handleGuestEvent`, `matchPairing` ONLY by their `matchStart` arms,
    // and neither flow writes the other's field. They diverge on a future third
    // `draft-match` flow — a positive "this IS a Commander game" then fails safe
    // by doing nothing, while an absence test would silently start tearing that
    // flow down.
    //
    // Read through `get()` at call time, never from a render: the decision
    // belongs to the press.
    if (!get().commanderLaunch) return;
    // `leave` clears `commanderLaunch` through `disposeMatchAdapter`, and a
    // caller that navigates before it settles renders one frame of the stale
    // waiting state — which is why this resolves rather than returning void.
    await get().leave(false);
  },

  startMatch: async () => {
    const { matchPairing, matchAdapter } = get();
    if (!matchPairing) return null;
    const gameId = `draft-match-${matchPairing.matchId}`;
    if (matchAdapter) return gameId;

    try {
      if (matchPairing.type === "HumanHost") {
        // Lower seat# hosts the match (D-09).
        const [{ hostRoom }, { P2PHostAdapter }] = await Promise.all([
          import("../network/connection"),
          import("../adapter/p2p-adapter"),
        ]);

        const host = await hostRoom(undefined, {
          preferredRoomCode: matchPairing.matchRoomCode,
        });

        const matchAdapter = new P2PHostAdapter(
          matchPairing.deckPayload,
          host.peer,
          host.onGuestConnected,
          2, // 1v1 match
          DRAFT_MATCH_FORMAT_CONFIG,
          matchPairing.matchConfig,
          undefined,
          undefined,
          undefined,
          undefined,
          undefined,
          undefined,
          {
            onConcede: (concedingGamePlayer) => get().reportActiveMatchConcession(concedingGamePlayer),
          },
        );

        let resolveRoomFull!: () => void;
        const roomFull = new Promise<void>((resolve) => {
          resolveRoomFull = resolve;
        });
        matchAdapter.onEvent((event) => {
          if (event.type === "roomFull") {
            resolveRoomFull();
          }
          if (event.type === "stateChanged") {
            processRemoteUpdate(event.snapshot, event.events, event.logEntries).catch((err) => {
              // A rejected delivery is otherwise gone and the screen freezes
              // on the previous state — surface it and re-sync immediately.
              debugLog(`draft-match remote update failed: ${err instanceof Error ? err.message : String(err)}`);
              resyncFromAdapterSafely("delivery rejected");
            });
          }
          if (event.type === "stateChanged") {
            const wf = event.snapshot.state?.waiting_for;
            if (!wf) return;

            if (wf.type === "GameOver") {
              // Match is complete — report result to pod host
              const winnerSeat = winnerSeatForLaunch(matchPairing, wf.data.winner);
              void get().reportMatchResult(matchPairing.matchId, winnerSeat);
            } else if (wf.type === "BetweenGamesSideboard") {
              // Between games in Bo3 — bridge to draft pod host for sideboard orchestration.
              const score = wf.data.score;
              const gameNumber = wf.data.game_number;
              // Determine loser: the player whose wins are fewer
              const loserSeat = score.p0_wins > score.p1_wins
                ? matchPairing.opponentSeat
                : score.p1_wins > score.p0_wins
                  ? matchPairing.localSeat
                  : null; // draw
              if (activeHostAdapter) {
                activeHostAdapter.handleMatchBetweenGames(
                  matchPairing.matchId,
                  gameNumber,
                  score,
                  loserSeat,
                  matchPairing.localSeat,
                  matchPairing.opponentSeat,
                );
              } else {
                activeGuestAdapter?.handleMatchBetweenGames(
                  matchPairing.matchId,
                  gameNumber,
                  score,
                  loserSeat,
                );
              }
              // Also transition the host's own UI to betweenGames
              get().handleBetweenGamesPrompt({
                matchId: matchPairing.matchId,
                gameNumber,
                score,
                loserSeat,
                timerMs: 0, // Host determines timer internally via podPolicy
              });
            }
          }
          if (event.type === "gameOver") {
            // Connection-level failure — report as match loss
            const winnerSeat = winnerSeatForLaunch(matchPairing, event.winner);
            void get().reportMatchResult(matchPairing.matchId, winnerSeat);
          }
        });

        await matchAdapter.initialize();
        await roomFull;
        const initResult = await matchAdapter.startPregameGame();
        await installMatchRuntime(gameId, matchAdapter, initResult, "online");
        set({ matchAdapter, phase: "matchInProgress" });
        return gameId;
      } else if (matchPairing.type === "HumanGuest") {
        // Higher seat# joins as guest.
        const [{ joinRoom }, { P2PGuestAdapter }] = await Promise.all([
          import("../network/connection"),
          import("../adapter/p2p-adapter"),
        ]);

        const { conn, peer } = await joinRoom(matchPairing.matchRoomCode);

        const matchAdapter = new P2PGuestAdapter(
          {
            player: matchPairing.localDeck,
          },
          peer,
          conn.peer,
          conn,
          undefined,
          undefined,
          undefined,
          undefined,
          undefined,
          true,
        );

        matchAdapter.onEvent((event) => {
          if (event.type === "stateChanged") {
            processRemoteUpdate(event.snapshot, event.events, event.logEntries).catch((err) => {
              // A rejected delivery is otherwise gone and the screen freezes
              // on the previous state — surface it and re-sync immediately.
              debugLog(`draft-match remote update failed: ${err instanceof Error ? err.message : String(err)}`);
              resyncFromAdapterSafely("delivery rejected");
            });
          }
          if (event.type === "stateChanged") {
            const wf = event.snapshot.state?.waiting_for;
            if (!wf) return;

            if (wf.type === "GameOver") {
              // Guest reports as backup (host's report is authoritative)
              const winnerSeat = winnerSeatForLaunch(matchPairing, wf.data.winner);
              void get().reportMatchResult(matchPairing.matchId, winnerSeat);
            }
            // BetweenGamesSideboard: guest receives sideboard prompt via draft pod channel
            // (handled by bo3SideboardPrompt event from P2PDraftGuest), not here.
          }
          if (event.type === "gameOver") {
            // Connection failure — report as match loss
            const winnerSeat = winnerSeatForLaunch(matchPairing, event.winner);
            void get().reportMatchResult(matchPairing.matchId, winnerSeat);
          }
        });

        await matchAdapter.initialize();
        const initResult = await matchAdapter.initializeGame();
        await installMatchRuntime(gameId, matchAdapter, initResult, "online");
        set({ matchAdapter, phase: "matchInProgress" });
        return gameId;
      } else {
        const { WasmAdapter } = await import("../adapter/wasm-adapter");
        const matchAdapter = new WasmAdapter();
        // #7920: a bot match installs no transport-side whole-match concede,
        // so the menu's Concede was refused as unbound. Bind the capability
        // to a plain game-level Concede for the local seat (game player 0 —
        // the DRAFT_BOT_AI_SEAT authority): CR 104.3a ends the game as a
        // loss, the game-over screen's existing pod effect settles the match,
        // and "Back to pod" returns to the standings with any next round
        // intact.
        matchAdapter.bindMatchConcede(() => {
          void useGameStore
            .getState()
            .dispatch({ type: "Concede", data: { player_id: 0 } })
            .catch((err) => {
              console.error("[multiplayerDraftStore] bot-match concede failed:", err);
            });
        });
        await matchAdapter.initialize();
        const initResult = await matchAdapter.initializeGame(
          matchPairing.deckPayload,
          DRAFT_MATCH_FORMAT_CONFIG,
          2,
          matchPairing.matchConfig,
        );
        await installMatchRuntime(gameId, matchAdapter, initResult, "ai");
        set({ matchAdapter, phase: "matchInProgress" });
        return gameId;
      }
    } catch (err) {
      console.error("[multiplayerDraftStore] startMatch failed:", err);
      set({ error: err instanceof Error ? err.message : String(err) });
      return null;
    }
  },

  reportMatchResult: (matchId, winnerSeat) => {
    const { role, matchPairing } = get();
    if (!matchPairing || matchPairing.matchId !== matchId) return Promise.resolve();
    if (matchPairing.binding.matchAuthoritySeat !== matchPairing.localSeat) return Promise.resolve();
    return (async () => {
      const existing = await loadDraftSettlementOutbox(matchPairing.binding);
      const settlement: DraftMatchSettlement = existing ?? {
        binding: matchPairing.binding,
        receiptId: crypto.randomUUID(),
        winnerSeat,
      };
      await saveDraftSettlementOutbox(settlement);
      if (role === "host" && activeHostAdapter) {
        await activeHostAdapter.submitMatchSettlement(settlement);
        await clearDraftSettlementOutbox(matchPairing.binding);
      } else if (role === "guest" && activeGuestAdapter) {
        activeGuestAdapter.sendMatchSettlement(settlement);
      }
    })();
  },

  reportActiveMatchGameResult: async (gameWinner) => {
    const { matchPairing, reportMatchResult } = get();
    if (!matchPairing) return;
    await reportMatchResult(
      matchPairing.matchId,
      winnerSeatForGameResult(matchPairing, gameWinner),
    );
  },

  reportActiveMatchConcession: async (concedingGamePlayer = 0) => {
    const { matchPairing, reportMatchResult } = get();
    if (!matchPairing) return;
    await reportMatchResult(
      matchPairing.matchId,
      seatForLaunchGamePlayer(matchPairing, concedingGamePlayer === 0 ? 1 : 0),
    );
  },

  advanceRound: () => {
    if (!activeHostAdapter) return;
    void activeHostAdapter.advanceRound();
  },

  overrideMatchResult: (matchId, winnerSeat) => {
    if (!activeHostAdapter) return;
    void activeHostAdapter.overrideMatchResult(matchId, winnerSeat);
  },

  replaceSeatWithBot: (seat) => {
    if (!activeHostAdapter) return;
    void activeHostAdapter.replaceSeatWithBot(seat);
  },

  submitSideboard: (matchId, mainDeck, sideboard) => {
    const launch = get().matchPairing;
    if (!launch || launch.matchId !== matchId) return;
    const submittedNames = [
      ...mainDeck,
      ...sideboard.flatMap(({ name, count }) => (
        Number.isSafeInteger(count) && count >= 0 ? Array<string>(count).fill(name) : []
      )),
    ];
    const registered = localLaunchDeck(launch);
    if (!sameNameMultiset(submittedNames, [...registered.main_deck, ...registered.sideboard])) {
      set({ error: "Sideboard submission does not match the registered match pool" });
      return;
    }
    void get().submitIntergameCommand({
      type: "SubmitSideboard",
      main: countProjectedNames(mainDeck),
      sideboard,
    });
  },

  choosePlayDraw: (matchId, playFirst) => {
    void matchId;
    void get().submitIntergameCommand({ type: "ChoosePlayDraw", playFirst });
  },

  submitIntergameCommand: async (payload) => {
    const state = get();
    const launch = state.matchPairing;
    const gameNumber = state.sideboardPrompt?.gameNumber ?? state.playDrawPrompt?.gameNumber;
    if (!launch || gameNumber === undefined || state.seatIndex === null) return;
    let controller = intergameControllers.get(launch.matchId);
    if (!controller) {
      controller = new IntergameCommandController(await loadDraftIntergameCommands(launch.matchId));
      controller.recover();
      intergameControllers.set(launch.matchId, controller);
    }
    const command = controller.hold({
      commandId: crypto.randomUUID(), matchId: launch.matchId, gameNumber,
      seat: state.seatIndex, payload, launchPayload: launch, launchDigest: draftIntergameDigest(launch),
    });
    await saveDraftIntergameCommands(launch.matchId, controller.snapshot());
    if (state.role === "host") activeHostAdapter?.submitAuthorized(state.seatIndex, command);
    else activeGuestAdapter?.submitAuthorized(command);
    if (payload.type === "SubmitSideboard") set({ sideboardSubmitted: true });
  },

  submitAuthorized: async (command, acknowledgement) => {
    const state = get();
    const launch = state.matchPairing;
    if (!launch || command.matchId !== launch.matchId || command.launchDigest !== draftIntergameDigest(launch)) return;
    let controller = intergameControllers.get(command.matchId);
    if (!controller) {
      controller = new IntergameCommandController(await loadDraftIntergameCommands(command.matchId));
      controller.recover();
      intergameControllers.set(command.matchId, controller);
    }
    const known = controller.snapshot().find((candidate) => candidate.commandId === command.commandId);
    if (known?.status === "Pending") controller.authorize(command.commandId, acknowledgement);
    else if (!known && command.status === "Authorized") {
      controller = new IntergameCommandController([...controller.snapshot(), command]);
      intergameControllers.set(command.matchId, controller);
    }
    const permit = controller.begin(command.commandId, acknowledgement);
    if (!permit || !consumeIntergamePermit(permit, acknowledgement)) return;
    const adapter = state.matchAdapter as EngineAdapter | null;
    if (!adapter) return;
    // Re-check immediately before crossing the host, guest, native, or WASM sink.
    const actor = launch.type === "HumanGuest" ? 1 : 0;
    try {
      await adapter.submitAction(intergameAction(command.payload), actor);
    } catch (err) {
      if (reportStructuredActionRejection(err) !== "not-structured") {
        const message = err instanceof Error ? err.message : String(err);
        controller.reject(command.commandId, acknowledgement, message);
        await saveDraftIntergameCommands(command.matchId, controller.snapshot());
        if (command.payload.type === "SubmitSideboard") set({ sideboardSubmitted: false });
        return;
      }
      throw err;
    }
    const receipted = controller.receipt(command.commandId, acknowledgement, crypto.randomUUID());
    if (!receipted) return;
    await saveDraftIntergameCommands(command.matchId, controller.snapshot());
    if (state.role === "host") {
      activeHostAdapter?.submitAuthorized(state.seatIndex!, receipted);
    } else {
      activeGuestAdapter?.acknowledgeAuthorized(commandAcknowledgement(receipted), receipted.receiptId!);
    }
  },

  handleBetweenGamesPrompt: (prompt) => {
    const state = get();
    const source = state.intergameWorkspaceState ?? state.submittedWorkspaceState;
    const launch = state.matchPairing;
    const view = state.view;
    let intergameWorkspaceState: DraftWorkspaceState | null = null;
    let error = state.error;
    if (source && launch && view) {
      const candidate = reconcileWorkspaceState(cloneWorkspace(source), view.pool);
      const partition = projectWorkspacePartition(candidate, view.pool);
      const registered = localLaunchDeck(launch);
      if (sameNameMultiset(
        [...partition.mainDeck, ...partition.sideboard],
        [...registered.main_deck, ...registered.sideboard],
      )) {
        intergameWorkspaceState = candidate;
      } else {
        error = "Submitted deck partition does not match the registered match pool";
      }
    }
    set({
      phase: "matchInProgress",
      intergameWorkspaceState,
      error,
      sideboardPrompt: {
        matchId: prompt.matchId,
        gameNumber: prompt.gameNumber,
        score: prompt.score,
        loserSeat: prompt.loserSeat,
        timerMs: prompt.timerMs,
      },
      sideboardSubmitted: false,
      playDrawPrompt: null,
      timerRemainingMs: prompt.timerMs > 0 ? prompt.timerMs : null,
    });
  },

  leave: async (preserveRecovery = false) => {
    // FIRST, and before the pod adapters go: a Commander launch or join parked
    // on its own await outlives everything below it. `disposeMatchAdapter` is
    // fenced on a `matchAdapter` that does not exist until the launch has
    // succeeded, and nothing else releases the module-local in-flight handles —
    // so without this the launch guard stays claimed and every later Commander
    // launch in this tab is silently refused. Aborting before the pod adapters
    // are disposed also stops the launch reaching `sendCommanderLaunches` on a
    // session that is about to be torn down.
    await abandonCommanderBringUp();

    const host = activeHostAdapter;
    const guest = activeGuestAdapter;

    // An explicit guest leave is host-acknowledged. Until that completes, the
    // live adapter remains the recovery owner; tearing down the lifecycle here
    // would discard the session that must reconnect after a dropped ACK.
    if (host) {
      await host.dispose({ preserveSession: preserveRecovery });
    }
    if (guest) {
      await guest.dispose({ preserveRecovery });
    }

    beginDraftLifecycle();
    // The pod session has now ended, so its game transport can follow it.
    disposeMatchAdapter(set);

    if (activeHostAdapter === host) {
      activeHostAdapter = null;
      if (!preserveRecovery) {
        clearActiveDraftPod();
      }
    }
    if (activeGuestAdapter === guest) {
      activeGuestAdapter = null;
    }
    set({ ...initialState, interactionGeneration: lifecycleGeneration });
  },

  reset: () => {
    // Same obligation as `leave`, and the aborts inside are synchronous, so a
    // synchronous `reset` still unparks both bring-ups. Only the host's
    // `terminateGame()` flush is left to settle on its own — `void`, because
    // `reset` cannot await and a dropped rejection here would be unhandled.
    void abandonCommanderBringUp();
    beginDraftLifecycle();
    disposeMatchAdapter(set);
    set({ ...initialState, interactionGeneration: lifecycleGeneration });
  },
})));

// ── Event handlers ─────────────────────────────────────────────────────

function hostStatusToPhase(status: DraftPodHostStatus): MultiplayerDraftPhase {
  switch (status) {
    case "idle":
      return "idle";
    case "connecting":
      return "connecting";
    case "lobby":
      return "lobby";
    case "drafting":
      return "drafting";
    case "deckbuilding":
      return "deckbuilding";
    case "pairing":
      return "pairing";
    case "matchInProgress":
      return "matchInProgress";
    case "roundComplete":
      return "roundComplete";
    case "complete":
      return "complete";
    case "error":
      return "error";
  }
}

function guestStatusToPhase(status: DraftPodGuestStatus): MultiplayerDraftPhase {
  switch (status) {
    case "idle":
      return "idle";
    case "connecting":
      return "connecting";
    case "lobby":
      return "lobby";
    case "drafting":
      return "drafting";
    case "deckbuilding":
      return "deckbuilding";
    case "matchInProgress":
      return "matchInProgress";
    case "complete":
      return "complete";
    case "kicked":
      return "kicked";
    case "hostLeft":
      return "hostLeft";
    case "error":
      return "error";
  }
}

type SetFn = (
  partial:
    | Partial<MultiplayerDraftState>
    | ((state: MultiplayerDraftState) => Partial<MultiplayerDraftState>),
) => void;

function installEventView(view: DraftPlayerView): void {
  if (exclusivePickToken) return;
  const state = useMultiplayerDraftStore.getState();
  const restored = restoredWorkspace?.generation === lifecycleGeneration ? restoredWorkspace : null;
  restoredWorkspace = null;
  const base = restored?.state ?? state.workspaceState ?? createDraftWorkspaceState();
  const workspace = reconcileWorkspaceState(base, view.pool);
  const publish = restored !== null
    ? (restored.state === null ? view.pool.length > 0 : workspace !== base)
    : workspace !== state.workspaceState;
  installWorkspace({
    view,
    base: workspace,
    publish,
    patch: {
      phase: phaseForDraftViewStatus(view.status),
      timerRemainingMs: view.timer_remaining_ms ?? null,
      standings: view.standings ?? [],
      currentRound: view.current_round ?? 0,
      pairings: view.pairings ?? [],
    },
  });
}

function handleHostEvent(event: DraftPodHostEvent, set: SetFn): void {
  switch (event.type) {
    case "workspaceRestored":
      restoredWorkspace = {
        generation: lifecycleGeneration,
        state: event.workspaceState ? cloneWorkspace(event.workspaceState) : null,
      };
      break;
    case "statusChanged":
      set({ phase: hostStatusToPhase(event.status) });
      {
        const activePhase = activePhaseForHostStatus(event.status);
        if (activePhase) saveDraftPodProgress(activePhase);
      }
      break;
    case "roomCreated":
      set({ roomCode: event.roomCode });
      updateActiveDraftPod({ roomCode: event.roomCode });
      break;
    case "viewUpdated":
      installEventView(event.view);
      {
        const activePhase = activePhaseForDraftViewStatus(event.view.status);
        if (activePhase) saveDraftPodProgress(activePhase, event.view);
      }
      break;
    case "lobbyUpdate":
      set({ joined: event.joined, total: event.total, seats: event.seats });
      break;
    case "lobbyFull":
      break;
    case "draftStarted": {
      const activePhase = activePhaseForDraftViewStatus(event.view.status);
      installEventView(event.view);
      if (activePhase) saveDraftPodProgress(activePhase, event.view);
      break;
    }
    case "draftComplete":
      set({ phase: "deckbuilding" });
      saveDraftPodProgress("deckbuilding");
      break;
    case "allDecksSubmitted":
      // Shape B: a documented no-op. This event fires for EVERY pod kind, so it
      // cannot know where the pod went — a `PostDraftPlay::CompleteImmediately`
      // pod is already `Complete` (draft-core session.rs:902), and writing
      // "pairing" here overwrote the reducer's own answer. The `viewUpdated`
      // that follows establishes BOTH: the phase via `phaseForDraftViewStatus`
      // and the persisted record via `activePhaseForDraftViewStatus`.
      //
      // The arm itself stays for the reader, not for the compiler: this
      // `switch` returns `void` and has no `default`/`assertNever`, so dropping
      // the case would still compile. Written out, the no-op is a decision on
      // the record; deleted, it reads as an event nobody considered.
      break;
    case "draftPaused":
      set({ paused: true, pauseReason: event.reason });
      break;
    case "draftResumed":
      set({ paused: false, pauseReason: null });
      break;
    case "pairingsGenerated":
      // No `error: null` here. `clearErrorOnPhaseChange` retires the banner one
      // step earlier, when `roundAdvanced` / `statusChanged("pairing")` moves the
      // phase into `pairing` — off `roundComplete` at a round boundary, off
      // `deckbuilding` at round 0 — and it does the same for guests, which this
      // host-only arm never could.
      set({
        phase: "matchInProgress",
        currentRound: event.round,
        pairings: event.pairings,
      });
      saveDraftPodProgress("matchInProgress");
      break;
    case "matchStart":
      set({ matchPairing: event.launch, phase: "matchInProgress" });
      void retryDraftSettlement(event.launch, "host");
      break;
    case "commanderLaunch":
      // One of the field's two non-null writers; the guest's arm in
      // `handleGuestEvent` is the other, and the field's own doc carries the
      // full enumeration of writers and clearers. The host receives its OWN
      // launch the same way every other live seat does — `sendCommanderLaunches`
      // includes the local seat, `sendToSeat`'s seat-0 arm turns that into a
      // local emit, and `draftPodHostAdapter` re-emits it here. So
      // `launchCommanderGame` must never `set` this field directly.
      //
      // Deliberately UNLIKE `matchStart` above: NO pod phase is written. A
      // Commander launch does not move the pod, which stays `Complete`, and the
      // host must stay on `CompleteView` so step 4's launch-in-flight state and
      // D7's Cancel can render. Writing "matchInProgress" here would overwrite
      // the reducer's own answer — the bug `allDecksSubmitted` documents.
      set({ commanderLaunch: event.launch });
      break;
    case "roundAdvanced":
      disposeMatchAdapter(set);
      // `currentRound` is engine-owned: `viewUpdated` and `pairingsGenerated`
      // both write it from engine state moments later.
      set({ phase: "pairing" });
      saveDraftPodProgress("pairing");
      break;
    case "roundComplete":
      disposeMatchAdapter(set);
      break;
    case "matchResultReceived":
      // Informational — standings update comes via viewUpdated
      break;
    case "timerExpired":
      break;
    case "error":
      set({ error: event.message });
      break;
    // Seat events are informational — the lobby update carries the authoritative seat list
    case "seatJoined":
    case "seatReconnected":
    case "seatDisconnected":
    case "seatKicked":
    case "pickReceived":
    case "deckSubmitted":
      break;
    case "bo3SideboardPromptSent":
      // Host UI transition handled by the stateChanged bridge in startMatch.
      break;
    case "bo3BothSideboardsSubmitted":
      // Informational — play/draw prompt or game start follows automatically.
      break;
    case "bo3GameStarted":
      set({ phase: "matchInProgress", sideboardPrompt: null, playDrawPrompt: null, sideboardSubmitted: false });
      saveDraftPodProgress("matchInProgress");
      break;
    case "bo3SideboardPrompt":
      useMultiplayerDraftStore.getState().handleBetweenGamesPrompt(event);
      break;
    case "bo3ChoosePlayDraw":
      set({
        playDrawPrompt: {
          matchId: event.matchId,
          gameNumber: event.gameNumber,
          score: event.score,
          timerMs: event.timerMs,
        },
        timerRemainingMs: event.timerMs > 0 ? event.timerMs : null,
      });
      break;
    case "bo3GameStart":
      set({
        phase: "matchInProgress",
        sideboardPrompt: null,
        playDrawPrompt: null,
        sideboardSubmitted: false,
      });
      break;
    case "bo3AuthorizedCommand":
      void useMultiplayerDraftStore.getState().submitAuthorized(event.command, event.acknowledgement);
      break;
  }
}

function handleGuestEvent(event: DraftPodGuestEvent, set: SetFn): void {
  switch (event.type) {
    case "workspaceRestored":
      restoredWorkspace = {
        generation: lifecycleGeneration,
        state: event.workspaceState ? cloneWorkspace(event.workspaceState) : null,
      };
      break;
    case "statusChanged":
      set({ phase: guestStatusToPhase(event.status) });
      break;
    case "joined":
      set({
        seatIndex: event.seatIndex,
        draftCode: event.draftCode,
        phase: "lobby",
      });
      break;
    case "reconnected":
      set({ seatIndex: event.seatIndex });
      break;
    case "viewUpdated":
      installEventView(event.view);
      break;
    case "pickAcknowledged":
      if (pendingGuestPick?.generation === lifecycleGeneration) {
        const pending = pendingGuestPick;
        pendingGuestPick = null;
        pending.resolve(event.view);
      }
      break;
    case "lobbyUpdate":
      set({ seats: event.seats, joined: event.joined, total: event.total });
      break;
    case "draftPaused":
      set({ paused: true, pauseReason: event.reason });
      break;
    case "draftResumed":
      set({ paused: false, pauseReason: null });
      break;
    case "pairing":
      set({
        pairing: {
          round: event.round,
          table: event.table,
          opponentName: event.opponentName,
          matchHostPeerId: event.matchHostPeerId,
          matchId: event.matchId,
        },
      });
      break;
    case "matchResult":
      break;
    case "timerSync":
      set({ timerRemainingMs: event.remainingMs });
      break;
    case "matchStart":
      set({
        matchPairing: event.launch,
        phase: "matchInProgress",
      });
      void retryDraftSettlement(event.launch, "guest");
      break;
    case "commanderLaunch":
      // The mirror of `handleHostEvent`'s arm, and the reason the pod's guests
      // saw nothing at all: this event travelled the whole wire and then fell
      // out of a switch that had no arm for it.
      //
      // Deliberately UNLIKE `matchStart` above on both axes. NO pod phase is
      // written — the pod stays `complete`, which is the view the guest's join
      // affordance renders from — and this does NOT join the game.
      // Joining is the user's decision, made through `joinCommanderGame`.
      set({ commanderLaunch: event.launch });
      break;
    case "matchSettlementAcknowledged": {
      const binding = useMultiplayerDraftStore.getState().matchPairing?.binding;
      if (binding?.matchId === event.matchId && binding.revision === event.revision) {
        void clearDraftSettlementOutbox(binding);
      }
      break;
    }
    case "kicked":
      set({ phase: "kicked", error: event.reason });
      break;
    case "hostLeft":
      set({ phase: "hostLeft", error: event.reason });
      break;
    case "error":
      if (pendingGuestPick?.generation === lifecycleGeneration) {
        const pending = pendingGuestPick;
        pendingGuestPick = null;
        pending.resolve(null);
      }
      set({ error: event.message });
      break;
    case "reconnecting":
      break;
    case "reconnectFailed":
      set({ error: event.failure.message, guestRecoveryFailure: event.failure });
      break;
    case "bo3SideboardPrompt":
      useMultiplayerDraftStore.getState().handleBetweenGamesPrompt(event);
      break;
    case "bo3ChoosePlayDraw":
      set({
        playDrawPrompt: {
          matchId: event.matchId,
          gameNumber: event.gameNumber,
          score: event.score,
          timerMs: event.timerMs,
        },
        timerRemainingMs: event.timerMs > 0 ? event.timerMs : null,
      });
      break;
    case "bo3GameStart":
      set({
        phase: "matchInProgress",
        sideboardPrompt: null,
        playDrawPrompt: null,
        sideboardSubmitted: false,
      });
      break;
    case "bo3AuthorizedCommand":
      void useMultiplayerDraftStore.getState().submitAuthorized(event.command, event.acknowledgement);
      break;
    case "bo3ScoreUpdate":
      // Informational — standings update comes via viewUpdated
      break;
  }
}
