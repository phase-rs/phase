import type { BatchResolveResult, EngineAdapter, EngineSnapshot, GameAction, GameEvent, GameLogEntry, GameState, WaitingFor } from "../adapter/types";
import { AdapterError, AdapterErrorCode } from "../adapter/types";
import { attemptStateRehydrate, isEnginePanic, notifyEngineLost, routePanic } from "./engineRecovery";
import { normalizeEvents } from "../animation/eventNormalizer";
import { SPECTATOR_PLAYER_ID } from "../constants/game";
import { getPlayerId } from "../hooks/usePlayerId";
import type { AnimationStep } from "../animation/types";
import { audioManager } from "../audio/AudioManager";
import { MAX_UNDO_HISTORY, UNDOABLE_ACTIONS } from "../constants/game";
import { debugLog } from "./debugLog";
import { flashInGameRolls } from "./diceContest";
import i18n from "../i18n";
import { useAnimationStore } from "../stores/animationStore";
import { useAppNotificationStore } from "../stores/appToastStore";
import {
  isMultiplayerMode,
  useGameStore,
  saveAuthoritativeGame,
  saveCheckpoints,
} from "../stores/gameStore";
import { getOpponentDisplayName } from "../stores/multiplayerStore";
import { usePreferencesStore } from "../stores/preferencesStore";
import { useUiStore } from "../stores/uiStore";
import { pressureMultiplier, stackPressureFromLength, STACK_PRESSURE_ELEVATED } from "../utils/stackPressure";
import { effectiveStackPressure, recordStackResolutions } from "../utils/stackThroughput";
import { applySpellPaymentPreference } from "./castPaymentMode";

/**
 * Event types whose SFX is deferred to the card slam onImpact callback
 * in AnimationOverlay, so sound aligns with the visual impact moment.
 */
const SLAM_DEFERRED_SFX = new Set(["DamageDealt", "GroupedDamageFlurry"]);

/** Schedule SFX for each animation step, offset to sync with visual timing. */
function scheduleSfxForSteps(steps: AnimationStep[], multiplier: number): void {
  let offset = 0;
  for (const step of steps) {
    // Filter out slam-deferred events — their SFX fires at impact time instead
    const immediate = step.effects.filter((e) => !e.displayOnly && !SLAM_DEFERRED_SFX.has(e.event.type));
    if (immediate.length > 0) {
      if (offset === 0) {
        audioManager.playSfxForStep(immediate);
      } else {
        const delay = offset;
        setTimeout(() => audioManager.playSfxForStep(immediate), delay);
      }
    }
    offset += step.duration * multiplier;
  }
}

/**
 * Module-level position snapshot for AnimationOverlay position lookups.
 */
export let currentSnapshot = useAnimationStore.getState().captureSnapshot();

interface PendingLocalAction {
  kind: "local";
  action: GameAction;
  actor: number;
  session: BoundGameSession | null;
  /** WaitingFor object that prompted this local action. */
  waitingFor: WaitingFor | null;
  resolve: () => void;
  reject: (err: unknown) => void;
}

interface PendingRemoteUpdate {
  kind: "remote";
  snapshot: EngineSnapshot;
  events: GameEvent[];
  logEntries?: GameLogEntry[];
  resolve: () => void;
  reject: (err: unknown) => void;
}

type PendingWork = PendingLocalAction | PendingRemoteUpdate;

type BoundGameSession = {
  adapter: EngineAdapter;
  generation: number;
};

type GameSessionPreferenceAction = Extract<
  GameAction,
  { type: "SetPhaseStops" } | { type: "SetPriorityPassingMode" }
>;

/** Module-level mutex — replaces useRef from the hook version. */
let isAnimating = false;

/** Unified queue for local actions and remote state updates. */
const pendingQueue: PendingWork[] = [];

/**
 * Identifies the game state for which the current dispatch pipeline is valid.
 * Restoring a saved game replaces the engine state wholesale, so work queued
 * for the old state must neither run nor release a newer dispatch's mutex.
 */
let dispatchGeneration = 0;

/**
 * The local action currently being processed (set while inside processAction),
 * paired with the seat and WaitingFor object it was issued against. Used with
 * pendingQueue to deduplicate rapid double-clicks.
 *
 * Actor preserves the #459 cross-seat priority case. WaitingFor preserves the
 * #1513 doubled-trigger case where two structurally identical choices are
 * responses to different engine prompts.
 */
let inFlightLocalAction: {
  action: GameAction;
  actor: number;
  session: BoundGameSession | null;
  waitingFor: WaitingFor | null;
} | null = null;

function isCurrentDispatchGeneration(generation: number): boolean {
  return generation === dispatchGeneration;
}

function isBoundGameSessionCurrent(session: BoundGameSession | null): boolean {
  if (!session) return true;
  const game = useGameStore.getState();
  return (
    game.adapter === session.adapter
    && game.gameSessionGeneration === session.generation
    && game.gameState !== null
  );
}

function isDispatchContextCurrent(
  generation: number,
  session: BoundGameSession | null,
): boolean {
  return isCurrentDispatchGeneration(generation) && isBoundGameSessionCurrent(session);
}

function sameBoundGameSession(
  a: BoundGameSession | null,
  b: BoundGameSession | null,
): boolean {
  return a?.adapter === b?.adapter && a?.generation === b?.generation;
}

/** Discard dispatch work that belongs to the game state being replaced. */
function abandonDispatchesForStateRestore(): void {
  dispatchGeneration += 1;
  inFlightLocalAction = null;
  isAnimating = false;
  while (pendingQueue.length > 0) {
    pendingQueue.shift()!.resolve();
  }
}

function releaseDispatchMutex(generation: number): void {
  if (!isCurrentDispatchGeneration(generation)) return;

  if (pendingQueue.length > 0) {
    processQueue(generation).catch(() => {
      if (isCurrentDispatchGeneration(generation)) isAnimating = false;
    });
  } else {
    isAnimating = false;
  }
}

/** Structural equality for GameAction — action objects are small plain JSON. */
function actionsEqual(a: GameAction, b: GameAction): boolean {
  return JSON.stringify(a) === JSON.stringify(b);
}

function waitingForActorMatches(
  waitingFor: WaitingFor | null,
  gameState: GameState | null,
  actor: number,
): boolean {
  if (!waitingFor || !("data" in waitingFor)) return false;
  const data = waitingFor.data;
  if (typeof data !== "object" || data === null) return false;
  const fields = data as Record<string, unknown>;

  if (waitingFor.type === "Priority") {
    return fields.player === actor || gameState?.priority_player === actor;
  }
  if (fields.player === actor) return true;

  const pending = fields.pending;
  return (
    Array.isArray(pending) &&
    pending.some((entry) => {
      if (typeof entry !== "object" || entry === null) return false;
      return (entry as Record<string, unknown>).player === actor;
    })
  );
}

function queuedLocalActionStillApplies(next: PendingLocalAction): boolean {
  if (!isBoundGameSessionCurrent(next.session)) return false;
  if (
    next.action.type === "SetPhaseStops"
    || next.action.type === "SetPriorityPassingMode"
  ) {
    return true;
  }
  const { gameState, legalActions, waitingFor } = useGameStore.getState();
  if (Object.is(next.waitingFor, waitingFor)) return true;
  if (!waitingForActorMatches(waitingFor, gameState, next.actor)) return false;
  if (legalActions.some((action) => actionsEqual(action, next.action))) return true;
  return (
    next.action.type === "PassPriority" &&
    waitingFor?.type === "Priority" &&
    gameState != null
  );
}

function isStateLost(err: unknown): boolean {
  return err instanceof AdapterError && err.code === AdapterErrorCode.STATE_LOST;
}

/**
 * Legacy adapter failure for an engine worker that has already been classified
 * as unrecoverably unresponsive. Current worker watchdogs do not reject at 60s;
 * they notify the UI and keep the request alive so the user can continue
 * waiting for a late response.
 */
function isEngineUnresponsive(err: unknown): boolean {
  return err instanceof AdapterError && err.code === AdapterErrorCode.ENGINE_UNRESPONSIVE;
}

/**
 * A benign actor-authorization rejection: the click landed in the same tick
 * that priority/turn shifted, so the engine correctly refused the now-stale
 * action (CR 117 priority / CR 500 turn structure). Nothing mutated engine
 * state, so dispatch treats it as a no-op rather than propagating an error to
 * the many fire-and-forget UI `dispatchAction(...)` call sites (which would
 * otherwise surface as an `unhandledrejection` and pollute crash telemetry).
 */
function isStaleAction(err: unknown): boolean {
  return err instanceof AdapterError && err.code === AdapterErrorCode.STALE_ACTION;
}

function actionErrorMessage(err: unknown): string {
  if (err instanceof Error && err.message) return err.message;
  if (typeof err === "string" && err.length > 0) return err;
  return i18n.t("actionError.unknownEngineError");
}

function actionLabel(action: GameAction): string {
  if (action.type === "ChooseTarget" && action.data.target === null) {
    return i18n.t("actionError.skipTarget");
  }
  return i18n.t("actionError.genericAction");
}

function shouldShowActionError(err: unknown): boolean {
  return !isStateLost(err) && !isEnginePanic(err) && !isEngineUnresponsive(err) && !isStaleAction(err);
}

function showActionError(action: GameAction, err: unknown): void {
  if (!shouldShowActionError(err)) return;
  useAppNotificationStore.getState().showNotification({
    title: i18n.t("actionError.title", { action: actionLabel(action) }),
    description: actionErrorMessage(err),
  });
}

async function processAction(
  action: GameAction,
  actor: number,
  generation: number,
  session: BoundGameSession | null,
): Promise<void> {
  if (!isDispatchContextCurrent(generation, session)) return;
  const { adapter, gameState } = useGameStore.getState();
  if (!adapter || !gameState) {
    debugLog("processAction called with no adapter or gameState");
    throw new Error("Game not initialized");
  }

  // 1. Capture snapshot before WASM call
  const snapshot = useAnimationStore.getState().captureSnapshot();
  currentSnapshot = snapshot;

  // 2. Save undo history if applicable. Three conditions must hold:
  //    a) Action is unrevealed-information (UNDOABLE_ACTIONS).
  //    b) Single-player — rewinding one client desyncs multiplayer.
  //    c) Stack is currently empty. Checkpoints exist only at stack-empty
  //       boundaries so undo always lands before the activation/trigger
  //       sequence that put things on the stack, never mid-resolution.
  const { gameMode } = useGameStore.getState();
  const shouldSaveHistory =
    UNDOABLE_ACTIONS.has(action.type) &&
    !isMultiplayerMode(gameMode) &&
    gameState.stack.length === 0;

  // 3. Call WASM — get events without updating state yet.
  // `actor` is the authenticated seat ID of whoever initiated this dispatch
  // (local human from `getPlayerId()`, or an AI seat from `aiController`).
  // The engine's guard rejects any action whose actor doesn't match the
  // authorized submitter — so passing the local human's ID during the AI's
  // turn correctly fails instead of silently applying as the AI.
  // If the engine reports STATE_LOST (thread-local cleared between calls —
  // PWA update desync, worker restart, etc.), transparently rehydrate from
  // the store snapshot and retry once. Safe because submitAction fails
  // before mutating any engine state when the cell is None.
  let result;
  try {
    result = await adapter.submitAction(action, actor);
  } catch (err) {
    if (!isDispatchContextCurrent(generation, session)) return;
    // Stale click after a priority/turn shift: the engine's actor-auth guard
    // correctly rejected it. Nothing changed engine-side, so drop it as a
    // no-op instead of letting a benign race escape as an unhandled rejection.
    if (isStaleAction(err)) {
      debugLog(`processAction: stale action ${action.type} (actor-auth rejected) — ignoring`, "warn");
      return;
    }
    // Engine panic: re-running the same action against the same state is
    // guaranteed to re-panic (the previous "ai-getAction-retry" / similar
    // failure modes were caused by exactly this loop). Surface the captured
    // panic message immediately instead of attempting recovery.
    if (isEnginePanic(err)) {
      // Try rehydrate — if the panic was in a side path and engine state
      // survived, downgrade to a non-fatal toast and let the user keep
      // playing. Only the true state-loss path triggers the blocking modal.
      await routePanic("submitAction-panic", err.panic);
      throw err;
    }
    // Worker wedged on submitAction: surface recovery and rethrow so the
    // dispatch mutex is released. Do NOT rehydrate — the worker is the thing
    // that's hung, so restoreState through it would hang too.
    if (isEngineUnresponsive(err)) {
      notifyEngineLost("submitAction-timeout");
      throw err;
    }
    if (!isStateLost(err)) throw err;
    debugLog(`processAction: STATE_LOST on ${action.type}; attempting rehydrate`, "warn");
    const recovered = await attemptStateRehydrate();
    if (!isDispatchContextCurrent(generation, session)) return;
    if (!recovered) {
      notifyEngineLost("submitAction");
      throw err;
    }
    // Recovery reported success but the underlying worker restoreState is
    // fire-and-forget from the adapter (void return, async worker). If the
    // restore silently failed — e.g., MULTIPLAYER_MODE refused it, the worker
    // crashed mid-restore — this retry will throw STATE_LOST again. Catch
    // that explicitly and surface via Layer 3 rather than letting the error
    // escape uncaught.
    try {
      result = await adapter.submitAction(action, actor);
    } catch (retryErr) {
      if (!isDispatchContextCurrent(generation, session)) return;
      // Prefer the captured panic message over the bare retry tag — that's
      // the "diagnostic: submitAction-retry" the user reported, which told
      // them nothing actionable.
      if (isEnginePanic(retryErr)) {
        await routePanic("submitAction-retry-panic", retryErr.panic);
      } else {
        notifyEngineLost("submitAction-retry");
      }
      throw retryErr;
    }
  }
  if (!isDispatchContextCurrent(generation, session)) return;
  const events: GameEvent[] = result.events;

  // 3b. Fetch the state AND its legal actions as ONE atomic snapshot, and persist
  //     before animations so a mid-animation page reload (e.g. PWA service-worker
  //     update) doesn't lose the latest state.
  //
  // This single fetch is the fix for the observed softlock. The old flow read
  // `getState()` here and `getLegalActions()` again *after* the animation window
  // (step 8), pairing values from two different engine versions: any advance
  // during the animation produced e.g. `waiting_for = Priority` alongside
  // `DecideOptionalEffect` legal actions, so the UI rendered Resolve/Resolve All
  // while the engine waited on an optional-effect choice whose modal never
  // appeared. The pair is now captured together and committed together.
  //
  // Recover from STATE_LOST here too — a worker restart could happen between
  // submitAction and this fetch. Critically: if recovery fails, do NOT call
  // saveGame — earlier revisions silently wrote a default empty GameState to
  // IDB on null, corrupting the checkpoint we now rely on for Layer 3 reload.
  let snapshotResult: EngineSnapshot;
  try {
    snapshotResult = await adapter.getSnapshot();
  } catch (err) {
    if (!isDispatchContextCurrent(generation, session)) return;
    if (isEnginePanic(err)) {
      await routePanic("getSnapshot-panic", err.panic);
      throw err;
    }
    if (isEngineUnresponsive(err)) {
      notifyEngineLost("getSnapshot-timeout");
      throw err;
    }
    if (!isStateLost(err)) throw err;
    debugLog("processAction: STATE_LOST on getSnapshot; attempting rehydrate", "warn");
    const recovered = await attemptStateRehydrate();
    if (!isDispatchContextCurrent(generation, session)) return;
    if (!recovered) {
      notifyEngineLost("getSnapshot");
      throw err;
    }
    try {
      snapshotResult = await adapter.getSnapshot();
    } catch (retryErr) {
      if (!isDispatchContextCurrent(generation, session)) return;
      if (isEnginePanic(retryErr)) {
        notifyEngineLost("getSnapshot-retry-panic", retryErr.panic);
      } else {
        notifyEngineLost("getSnapshot-retry");
      }
      throw retryErr;
    }
  }
  if (!isDispatchContextCurrent(generation, session)) return;
  const newState = snapshotResult.state;
  const { gameId } = useGameStore.getState();
  if (gameId) void saveAuthoritativeGame(gameId, adapter, newState);

  // 3c. Feed the throughput tracker: count stack entries that left the stack
  //     this action (resolved, countered, or otherwise removed), id-diffed so a
  //     resolution that spawns replacement triggers still counts even when net
  //     stack length is unchanged. Drives rate-based pacing for the
  //     low-depth-high-churn loops the depth signal can't see (Exquisite Blood +
  //     Sanguine Bond and friends). Single-dispatch resolves one item per pass;
  //     the batch path feeds its own gross count below.
  const nextStackIds = new Set(newState.stack.map((e) => e.id));
  const resolvedCount = gameState.stack.reduce(
    (n, e) => (nextStackIds.has(e.id) ? n : n + 1),
    0,
  );
  if (resolvedCount > 0) recordStackResolutions(resolvedCount);

  // 4. Checkpoint: save pre-action state on turn boundaries for debug restore
  const turnEvent = events.find((e) => e.type === "TurnStarted");
  if (turnEvent) {
    const prev = useGameStore.getState();
    const updated = [...prev.turnCheckpoints, gameState].slice(-MAX_UNDO_HISTORY);
    useGameStore.setState({ turnCheckpoints: updated });
    if (prev.gameId) saveCheckpoints(prev.gameId, updated);
  }

  // 5. Flash turn banner directly (bypasses animation queue for reliability)
  if (turnEvent && "data" in turnEvent) {
    const turnPlayerId = (turnEvent.data as { player_id: number }).player_id;
    const myId = getPlayerId();
    let bannerText: string;
    if (turnPlayerId === myId) {
      bannerText = "YOUR TURN";
    } else {
      const oppName = getOpponentDisplayName(turnPlayerId);
      bannerText = `${oppName.toUpperCase()}'S TURN`;
    }
    // CR 500: per-player turn count (skipped turns excluded). Engine increments
    // turns_taken before TurnStarted fires, so newState already has the value.
    const turnNumber = newState.players[turnPlayerId]?.turns_taken ?? 1;
    useUiStore.getState().flashTurnBanner(bannerText, turnNumber);
  }

  // 5b. Surface in-game dice/coin rolls out-of-band (DiceRollOverlay), the same
  // way the turn banner bypasses the animation queue. These events are marked
  // NON_VISUAL so normalizeEvents skips them below.
  flashInGameRolls(events);

  // 6. Normalize events into animation steps
  const pacingMultipliers = usePreferencesStore.getState().pacingMultipliers;
  const steps = normalizeEvents(events, { pacingMultipliers });

  // 7. Play animations (unless instant — multiplier === 0). Fold in stack
  //    pressure so per-resolution timing collapses under depth OR recent churn —
  //    without this the single-dispatch path animated every oscillation cycle at
  //    full speed (it previously read only the user speed preference).
  const multiplier =
    usePreferencesStore.getState().animationSpeedMultiplier *
    pressureMultiplier(effectiveStackPressure(newState.stack.length));

  if (steps.length > 0 && multiplier > 0) {
    useAnimationStore.getState().setAnimationNewState(newState);
    useAnimationStore.getState().enqueueSteps(steps);

    // Schedule SFX synced with each step's visual timing
    scheduleSfxForSteps(steps, multiplier);

    // Wait for total animation duration
    const totalDuration = steps.reduce(
      (sum, step) => sum + step.duration * multiplier,
      0,
    );
    await new Promise<void>((resolve) => setTimeout(resolve, totalDuration));
  } else if (steps.length > 0) {
    // Instant speed: fire all SFX immediately
    for (const step of steps) {
      audioManager.playSfxForStep(step.effects);
    }
  }

  // 8. Commit the snapshot captured in step 3b — the pair, together.
  //
  // There is deliberately NO second engine fetch here. Re-reading legal actions
  // after the animation window is what created the mixed-epoch pair in the first
  // place. Recovery for a state lost *during* the animation window is now lazy:
  // the next engine call classifies and recovers, exactly as it already does for
  // every other window between calls.
  //
  // The commit is revision-gated, so if a newer commit landed mid-animation
  // (a `gameStore.dispatch` from a modal, a remote update, an AI-loop advance),
  // THIS older pair is dropped rather than clobbering it.
  if (!isDispatchContextCurrent(generation, session)) return;
  const store = useGameStore.getState();
  const stateHistory = shouldSaveHistory
    ? [...store.stateHistory, gameState].slice(-MAX_UNDO_HISTORY)
    : undefined;
  store.commitEngineSnapshot(snapshotResult, {
    events,
    logEntries: result.log_entries ?? [],
    stateHistory,
  });

  // Play victory/defeat stinger on GameOver
  const gameOverEvent = events.find((e) => e.type === "GameOver");
  if (gameOverEvent && gameOverEvent.type === "GameOver") {
    const winner = (gameOverEvent.data as { winner: number | null }).winner;
    if (winner === null) {
      // Draw — just fade out
      audioManager.stopMusic(2.0);
    } else {
      const myId = getPlayerId();
      audioManager.playStinger(winner === myId ? "victory" : "defeat");
    }
  }
}

async function processQueue(generation: number): Promise<void> {
  while (isCurrentDispatchGeneration(generation) && pendingQueue.length > 0) {
    const next = pendingQueue.shift()!;
    try {
      if (next.kind === "local") {
        if (!queuedLocalActionStillApplies(next)) {
          debugLog(`dropping stale queued action ${next.action.type}: waitingFor changed`);
          next.resolve();
          continue;
        }
        inFlightLocalAction = {
          action: next.action,
          actor: next.actor,
          session: next.session,
          waitingFor: next.waitingFor,
        };
        try {
          await processAction(next.action, next.actor, generation, next.session);
        } finally {
          if (isCurrentDispatchGeneration(generation)) inFlightLocalAction = null;
        }
      } else {
        await processRemoteUpdateInner(next.snapshot, next.events, next.logEntries, generation);
      }
      next.resolve();
    } catch (err) {
      if (!isCurrentDispatchGeneration(generation)) {
        next.resolve();
        return;
      }
      debugLog(`processQueue error (${next.kind}): ${err instanceof Error ? err.message : String(err)}`);
      if (next.kind === "local") {
        showActionError(next.action, err);
      }
      next.reject(err);
      // If processAction escalated to Layer 3 (notifyEngineLost already
      // fired), drain the rest of the queue with the same error. Without
      // this, each remaining item would attempt its own recovery, each
      // one failing and re-firing notifyEngineLost — the modal is
      // de-duped but the log becomes noisy and we waste cycles on doomed
      // rehydrates. User is about to reload; nothing in this queue is
      // going to succeed.
      if (isStateLost(err) || isEnginePanic(err) || isEngineUnresponsive(err)) {
        // Drain on ENGINE_PANIC / ENGINE_UNRESPONSIVE too: each queued action
        // would otherwise hit its own catch + (no-op) recovery + re-throw,
        // doubling the noise for an unrecoverable failure. The first item
        // already fired notifyEngineLost (captured panic, or the timeout
        // recovery prompt) — a wedged worker won't service the rest either.
        while (pendingQueue.length > 0) {
          const stale = pendingQueue.shift()!;
          stale.reject(err);
        }
        break;
      }
    }
  }
  if (isCurrentDispatchGeneration(generation)) isAnimating = false;
}

/**
 * Standalone dispatch function with snapshot-animate-update flow.
 *
 * Flow per dispatch:
 * 1. Mutex gate — queue if already animating
 * 2. Capture snapshot of all card positions
 * 3. Call WASM via adapter.submitAction
 * 4. Normalize events into AnimationSteps
 * 5. Play animations (unless speed is 'instant')
 * 6. Update game state in gameStore
 * 7. Release mutex, process next queued action
 */
/**
 * Dispatch `action` on behalf of `actor`. `actor` defaults to the local
 * human's seat (`getPlayerId()`); the AI controller overrides it with the
 * AI seat's PlayerId so the engine accepts the action as coming from that
 * seat instead of the human.
 *
 * The engine itself enforces `actor === authorized_submitter(state)`, so a
 * misrouted action fails cleanly rather than silently applying as the
 * wrong player.
 */
async function dispatchActionInternal(
  action: GameAction,
  actor: number,
  session: BoundGameSession | null,
): Promise<void> {
  if (!isBoundGameSessionCurrent(session)) return;
  const { gameMode } = useGameStore.getState();
  if (gameMode === "spectate" || actor === SPECTATOR_PLAYER_ID) {
    return;
  }

  const submittedAction = actor === getPlayerId() ? applySpellPaymentPreference(action) : action;
  // Snapshot the prompt object that caused this action. The same action from
  // the same actor is a duplicate only while it answers the same prompt.
  const currentWaitingFor = useGameStore.getState().waitingFor;

  if (isAnimating) {
    // Same action + same actor + same prompt is a duplicate. A changed prompt
    // is a new decision even when the payload is structurally identical.
    if (
      inFlightLocalAction &&
      inFlightLocalAction.actor === actor &&
      sameBoundGameSession(inFlightLocalAction.session, session) &&
      actionsEqual(inFlightLocalAction.action, submittedAction) &&
      Object.is(inFlightLocalAction.waitingFor, currentWaitingFor)
    ) {
      return;
    }
    for (const pending of pendingQueue) {
      if (
        pending.kind === "local" &&
        pending.actor === actor &&
        sameBoundGameSession(pending.session, session) &&
        actionsEqual(pending.action, submittedAction) &&
        Object.is(pending.waitingFor, currentWaitingFor)
      ) {
        return;
      }
    }
    debugLog(`dispatch queued (mutex held): ${submittedAction.type}, queue=${pendingQueue.length}`, "warn");
    return new Promise<void>((resolve, reject) => {
      pendingQueue.push({
        kind: "local",
        action: submittedAction,
        actor,
        session,
        waitingFor: currentWaitingFor,
        resolve,
        reject,
      });
    });
  }

  const generation = dispatchGeneration;
  isAnimating = true;
  inFlightLocalAction = {
    action: submittedAction,
    actor,
    session,
    waitingFor: currentWaitingFor,
  };
  try {
    await processAction(submittedAction, actor, generation, session);
  } catch (e) {
    if (!isDispatchContextCurrent(generation, session)) return;
    debugLog(`dispatch error for ${submittedAction.type}: ${e instanceof Error ? e.message : String(e)}`);
    showActionError(submittedAction, e);
    throw e;
  } finally {
    if (isCurrentDispatchGeneration(generation)) inFlightLocalAction = null;
    releaseDispatchMutex(generation);
  }
}

export function dispatchAction(
  action: GameAction,
  actor: number = getPlayerId(),
): Promise<void> {
  return dispatchActionInternal(action, actor, null);
}

/** Dispatch a standing preference only while its captured game lifecycle is
 * still current. A late response from a disposed or resumed session is dropped
 * before snapshot fetch/commit, so it cannot overwrite the replacement game. */
export function dispatchActionForGameSession(
  action: GameSessionPreferenceAction,
  adapter: EngineAdapter,
  generation: number,
  actor: number = getPlayerId(),
): Promise<void> {
  return dispatchActionInternal(action, actor, { adapter, generation });
}

/**
 * Inner implementation for remote state updates — runs the animation pipeline.
 */
async function processRemoteUpdateInner(
  snapshot: EngineSnapshot,
  events: GameEvent[],
  logEntries: GameLogEntry[] = [],
  generation: number,
): Promise<void> {
  if (!isCurrentDispatchGeneration(generation)) return;
  const state = snapshot.state;

  // 1. Capture positions before updating state (for lookups during animation)
  currentSnapshot = useAnimationStore.getState().captureSnapshot();

  // 2. Flash turn banner
  const turnEvent = events.find((e) => e.type === "TurnStarted");
  if (turnEvent && "data" in turnEvent) {
    const turnPlayerId = (turnEvent.data as { player_id: number }).player_id;
    const myId = getPlayerId();
    let bannerText: string;
    if (turnPlayerId === myId) {
      bannerText = "YOUR TURN";
    } else {
      const oppName = getOpponentDisplayName(turnPlayerId);
      bannerText = `${oppName.toUpperCase()}'S TURN`;
    }
    // CR 500: per-player turn count from the post-update state.
    const turnNumber = state.players[turnPlayerId]?.turns_taken ?? 1;
    useUiStore.getState().flashTurnBanner(bannerText, turnNumber);
  }

  // 3. Normalize events into animation steps
  const pacingMultipliers = usePreferencesStore.getState().pacingMultipliers;
  const steps = normalizeEvents(events, { pacingMultipliers });

  // 4. Play animations (unless instant — multiplier === 0)
  const multiplier = usePreferencesStore.getState().animationSpeedMultiplier;

  if (steps.length > 0 && multiplier > 0) {
    useAnimationStore.getState().setAnimationNewState(state);
    useAnimationStore.getState().enqueueSteps(steps);
    scheduleSfxForSteps(steps, multiplier);

    const totalDuration = steps.reduce(
      (sum, step) => sum + step.duration * multiplier,
      0,
    );
    await new Promise<void>((resolve) => setTimeout(resolve, totalDuration));
  } else if (steps.length > 0) {
    for (const step of steps) {
      audioManager.playSfxForStep(step.effects);
    }
  }

  // 5. Commit the pair after animations complete — revision-gated, so a remote
  //    update that was superseded while its animation played is dropped rather
  //    than clobbering the newer state.
  if (!isCurrentDispatchGeneration(generation)) return;
  useGameStore.getState().commitEngineSnapshot(snapshot, { events, logEntries });

  // 6. Play victory/defeat stinger on GameOver
  const gameOverEvent = events.find((e) => e.type === "GameOver");
  if (gameOverEvent && gameOverEvent.type === "GameOver") {
    const winner = (gameOverEvent.data as { winner: number | null }).winner;
    if (winner === null) {
      audioManager.stopMusic(2.0);
    } else {
      const myId = getPlayerId();
      audioManager.playStinger(winner === myId ? "victory" : "defeat");
    }
  }
}

/**
 * Process an incoming remote state update (opponent's action in multiplayer/P2P).
 * Shares the animation mutex with dispatchAction so remote updates queue behind
 * local actions and vice versa — no overlapping animations.
 */
export async function processRemoteUpdate(
  snapshot: EngineSnapshot,
  events: GameEvent[],
  logEntries?: GameLogEntry[],
): Promise<void> {
  if (isAnimating) {
    return new Promise<void>((resolve, reject) => {
      pendingQueue.push({ kind: "remote", snapshot, events, logEntries, resolve, reject });
    });
  }

  const generation = dispatchGeneration;
  isAnimating = true;
  try {
    await processRemoteUpdateInner(snapshot, events, logEntries, generation);
  } finally {
    releaseDispatchMutex(generation);
  }
}

/**
 * Restore a previously captured GameState snapshot.
 * Returns null on success, or an error message string on failure.
 */
export async function restoreGameState(
  state: GameState,
  options: { preserveCheckpoints?: boolean } = {},
): Promise<string | null> {
  const { adapter, gameId } = useGameStore.getState();
  if (!adapter) return "No adapter available";

  abandonDispatchesForStateRestore();
  try {
    await adapter.restoreState(state);
  } catch (err) {
    return err instanceof Error ? err.message : "Failed to restore state";
  }

  // Post-restore fetch — newest-by-construction, so it always passes the gate.
  const snapshot = await adapter.getSnapshot();
  const preservedCheckpoints = options.preserveCheckpoints
    ? useGameStore.getState().turnCheckpoints
    : [];
  useGameStore.getState().commitEngineSnapshot(snapshot, {
    extraState: {
      events: [],
      eventHistory: [],
      logHistory: [],
      nextLogSeq: 0,
      stateHistory: [],
      turnCheckpoints: preservedCheckpoints,
    },
  });
  if (gameId) {
    await saveAuthoritativeGame(gameId, adapter, snapshot.state);
    await saveCheckpoints(gameId, preservedCheckpoints);
  }

  return null;
}

const BATCH_CHUNK_SIZE = 5;
// Under "Instant" stack pressure (a multi-hundred/thousand identical-trigger
// storm, e.g. Scute Swarm) the 5-at-a-time animated countdown is wasted. Keep
// large storms in engine-owned fast-forward batches so partial stacks collapse
// before the frontend pays the per-chunk `getSnapshot` cost.
// The value is intentionally large: the worker boundary already keeps the main
// thread responsive, while this still lets the overlay update during truly
// pathological stacks.
const BATCH_CHUNK_INSTANT = 5_000;
const BATCH_CHUNK_BASE_DELAY_MS = 150;
let batchResolveInProgress = false;

export async function dispatchResolveAll(
  requester: number,
  aiSeats: { playerId: number; difficulty: string }[],
): Promise<void> {
  if (batchResolveInProgress) return;
  const { adapter: batchAdapter } = useGameStore.getState();
  if (!batchAdapter) {
    debugLog("dispatchResolveAll: no adapter");
    return;
  }
  if (!batchAdapter.resolveAll || aiSeats.length === 0) {
    // No batch drain (multiplayer transports), or no AI deciders for the other
    // seats (local hotseat — every seat is a human, #4978): those seats are
    // humans, and CR 117.4 entitles each of them to their own priority window
    // before anything resolves — the engine must not pass on their behalf.
    // Arena-style "Resolve All" instead: an engine-side auto-yield for THIS
    // seat only (AutoPassMode::UntilStackEmpty), which auto-passes whenever
    // this player receives priority and clears itself when the stack empties
    // or grows (an opponent responded).
    await dispatchAction(
      { type: "SetAutoPass", data: { mode: { type: "UntilStackEmpty" } } },
      requester,
    );
    return;
  }

  batchResolveInProgress = true;
  const multiplier = usePreferencesStore.getState().animationSpeedMultiplier;
  const { setIsResolvingAll, setResolutionProgress } = useGameStore.getState();
  setIsResolvingAll(true);
  // Storm-origin denominator: latched from the FIRST chunk's `total` because
  // the engine reports the *remaining* stack per chunk (shrinks as it drains),
  // so only the first chunk carries the true origin count.
  let latchedTotal = 0;
  // Engine-authoritative gross resolved count, accumulated across chunks.
  let resolvedSoFar = 0;

  try {
    for (;;) {
      // Re-evaluate pressure each iteration: a storm shrinks as it drains, so
      // it eventually drops back to the animated 5-at-a-time path near the end.
      const stackLen = useGameStore.getState().gameState?.stack.length ?? 0;
      const instant = stackPressureFromLength(stackLen) === "Instant";
      const chunkSize = instant ? BATCH_CHUNK_INSTANT : BATCH_CHUNK_SIZE;

      const batchResult: BatchResolveResult = await batchAdapter.resolveAll(
        requester, aiSeats, chunkSize,
      );

      if (latchedTotal === 0) latchedTotal = batchResult.total;
      resolvedSoFar += batchResult.itemsResolved;
      // Keep the throughput tracker warm so a storm draining below Instant keeps
      // its animated tail fast instead of snapping back to full pacing.
      // `itemsResolved` is a net-shrink count (can lag the true gross when a
      // resolution spawns triggers) — an acceptable under-count here since the
      // batch path is already depth-gated, where the depth axis dominates pacing.
      if (batchResult.itemsResolved > 0) recordStackResolutions(batchResult.itemsResolved);
      // Surface progress only for a genuine storm (trivial multi-item resolves
      // drain too fast to render). Clamp to the latched total: `itemsResolved`
      // is a net-shrink count that can lag the true gross when a resolution
      // spawns triggers, so clamping keeps the bar monotonic and lets it
      // complete. `resolved`/`total` are engine-provided — no frontend derivation.
      if (latchedTotal >= STACK_PRESSURE_ELEVATED) {
        setResolutionProgress({
          resolved: Math.min(resolvedSoFar, latchedTotal),
          total: latchedTotal,
        });
      }

      // One atomic pair per chunk, committed through the single authority. The
      // store's `waitingFor` therefore comes from the snapshot's own state, not
      // from `batchResult.waitingFor` — the pair must stay self-consistent.
      // Equivalent or fresher: only `WasmAdapter` implements `resolveAll`, and
      // worker FIFO guarantees this snapshot reflects at least the chunk's end
      // state.
      const snapshot = await batchAdapter.getSnapshot();
      useGameStore.getState().commitEngineSnapshot(snapshot);

      // Anything other than Priority ends the drain — GameOver included, since
      // the drain only continues while this seat keeps receiving priority.
      const done =
        batchResult.itemsResolved === 0 ||
        snapshot.state.stack.length === 0 ||
        snapshot.state.waiting_for.type !== "Priority";
      if (done) break;

      if (instant) {
        // Yield one frame so the resolution-progress overlay repaints between
        // chunks. This rAF is the load-bearing progress fix — without it,
        // back-to-back Instant chunks never let the browser paint, producing
        // the "wait, then N vanish at once" symptom.
        await new Promise<void>((r) => requestAnimationFrame(() => r()));
        continue;
      }

      const chunkDelay = Math.round(BATCH_CHUNK_BASE_DELAY_MS * multiplier);
      if (chunkDelay > 0) {
        await new Promise<void>((r) => setTimeout(r, chunkDelay));
      } else {
        await new Promise<void>((r) => requestAnimationFrame(() => r()));
      }
    }

    const { gameId, adapter } = useGameStore.getState();
    const newState = useGameStore.getState().gameState;
    if (gameId && adapter && newState) {
      await saveAuthoritativeGame(gameId, adapter, newState);
    }
  } finally {
    batchResolveInProgress = false;
    setIsResolvingAll(false);
    setResolutionProgress(null);
  }
}
