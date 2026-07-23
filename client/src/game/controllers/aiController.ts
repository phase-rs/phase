import { AI_BASE_DELAY_MS, AI_DELAY_VARIANCE_MS, PLAYER_ID } from "../../constants/game";
import { useGameStore } from "../../stores/gameStore";
import type { GameAction, GameState, WaitingFor } from "../../adapter/types";
import { AdapterError, AdapterErrorCode } from "../../adapter/types";
import { pressureMultiplier } from "../../utils/stackPressure";
import { effectiveStackPressure } from "../../utils/stackThroughput";
import { debugLog } from "../debugLog";
import { dispatchAction } from "../dispatch";
import { attemptStateRehydrate, isEnginePanic, notifyEngineLost, routePanic } from "../engineRecovery";
import type { OpponentController } from "./types";

/**
 * Hard stop on AI controller after this many total consecutive failures on
 * the same WaitingFor key — pre-fallback *and* post-fallback failures both
 * count. Previously the controller would spin indefinitely once post-fallback
 * failures started accumulating, generating 300k+ log lines per minute.
 */
const MAX_TOTAL_FAILURES = 6;

/** Per-seat config: each AI player has its own difficulty. Multiple seats
 *  can share a difficulty; the map is keyed by `playerId` so lookups match
 *  the `waiting_for.data.player` value that drives scheduling. */
export interface AISeatBinding {
  playerId: number;
  difficulty: string;
}

export interface AIControllerConfig {
  seats: AISeatBinding[];
}

export interface AIController extends OpponentController {
  start(): void;
  stop(): void;
  dispose(): void;
}

function isStateLost(err: unknown): boolean {
  return err instanceof AdapterError && err.code === AdapterErrorCode.STATE_LOST;
}

function choiceTypeKey(choiceType: string | Record<string, unknown>): string {
  if (typeof choiceType === "string") return choiceType;
  return Object.keys(choiceType)[0] ?? "Unknown";
}

function describeAiCardPredicateGuess(
  action: GameAction,
  waitingFor: WaitingFor | null | undefined,
  _gameState: GameState | null | undefined,
): string | null {
  if (action.type !== "ChooseOption" || waitingFor?.type !== "NamedChoice") return null;
  if (choiceTypeKey(waitingFor.data.choice_type) !== "CardPredicateGuess") return null;

  const sourceName = waitingFor.data.source?.prompt.display_name ?? null;
  return sourceName == null
    ? `guesses ${action.data.choice}`
    : `guesses ${action.data.choice} for ${sourceName}`;
}

function waitingForFingerprint(waitingFor: WaitingFor | null | undefined): string {
  return JSON.stringify(waitingFor ?? null);
}

function waitingForDebugLabel(waitingFor: WaitingFor | null | undefined): string {
  if (waitingFor == null) return "none";
  const data = (waitingFor as { data?: { player?: number } }).data;
  const player = data?.player == null ? "unknown" : String(data.player);
  if (waitingFor.type !== "NamedChoice") return `${waitingFor.type} for player ${player}`;
  return `${waitingFor.type}/${choiceTypeKey(waitingFor.data.choice_type)} for player ${player}`;
}

export function createAIController(config: AIControllerConfig): AIController {
  let active = false;
  let pending = false;
  let timeoutId: ReturnType<typeof setTimeout> | null = null;
  let unsubscribe: (() => void) | null = null;
  let attemptGeneration = 0;

  interface AIAttempt {
    generation: number;
    gameSessionGeneration: number;
    waitingForFingerprint: string;
    playerId: number;
    isPriority: boolean;
  }

  let currentAttempt: AIAttempt | null = null;

  // Failure tracking on the same WaitingFor state to break infinite loops.
  // `MAX_CONSECUTIVE_FAILURES` gates the normal→fallback transition; the
  // separate `MAX_TOTAL_FAILURES` hard-stops the controller so post-fallback
  // failures (e.g., engine rejecting even the safe fallback) cannot spin.
  let lastWaitingForKey: string | null = null;
  let consecutiveFailures = 0;
  let totalFailures = 0;
  let lastDispatchError: string | null = null;
  const MAX_CONSECUTIVE_FAILURES = 3;

  const difficultyByPlayerId = new Map(config.seats.map((s) => [s.playerId, s.difficulty]));
  const aiPlayerIds = new Set(difficultyByPlayerId.keys());

  /**
   * Stable identity key for a WaitingFor — type + player so Priority{0} ≠ Priority{1}.
   *
   * For simultaneous-mulligan states (`MulliganDecision`,
   * `OpeningHandBottomCards`)
   * `data.player` is undefined, so falling back to -1 would collapse every
   * pending seat to the same key. We instead key by the AI seat that the
   * controller is currently driving, so failure counters reset between seats
   * and a failing P0 submission does not consume P1's budget.
   */
  function waitingForKey(wf: WaitingFor, drivingPlayerId: number | null): string {
    const data = (wf as { data?: { player?: number } }).data;
    const player = drivingPlayerId ?? data?.player ?? -1;
    return `${wf.type}:${player}`;
  }

  /**
   * CR 103.5: For simultaneous mulligan states, return the first AI-controlled
   * player in `pending` so the AI controller can act for them. Returns null
   * if no AI player is pending (the local human still owes a decision).
   */
  function aiPendingForMulligan(wf: {
    type: string;
    data?: { pending?: { player: number }[] };
  }): number | null {
    if (
      wf.type !== "MulliganDecision" &&
      wf.type !== "OpeningHandBottomCards"
    ) {
      return null;
    }
    const pending = wf.data?.pending ?? [];
    for (const entry of pending) {
      if (entry.player !== PLAYER_ID && aiPlayerIds.has(entry.player)) {
        return entry.player;
      }
    }
    return null;
  }

  function authorizedAiPlayer(
    waitingFor: WaitingFor,
    state: GameState,
  ): number | null {
    const mulliganPid = aiPendingForMulligan(
      waitingFor as { type: string; data?: { pending?: { player: number }[] } },
    );
    if (mulliganPid !== null) return mulliganPid;
    if (
      waitingFor.type === "MulliganDecision" ||
      waitingFor.type === "OpeningHandBottomCards"
    ) {
      return null;
    }
    if (
      !("data" in waitingFor) ||
      !waitingFor.data ||
      (!("player" in waitingFor.data) &&
        waitingFor.type !== "LoopShortcut" &&
        waitingFor.type !== "PrecastCopyShortcutOffer")
    ) {
      return null;
    }
    return state.priority_player === PLAYER_ID ? null : state.priority_player;
  }

  function beginAttempt(waitingFor: WaitingFor, playerId: number): AIAttempt {
    const store = useGameStore.getState();
    const attempt: AIAttempt = {
      generation: ++attemptGeneration,
      gameSessionGeneration: store.gameSessionGeneration,
      waitingForFingerprint: waitingForFingerprint(waitingFor),
      playerId,
      isPriority: waitingFor.type === "Priority",
    };
    currentAttempt = attempt;
    pending = true;
    return attempt;
  }

  function isAttemptCurrent(attempt: AIAttempt): boolean {
    if (!active || attempt.generation !== attemptGeneration) return false;
    const store = useGameStore.getState();
    if (store.gameSessionGeneration !== attempt.gameSessionGeneration) return false;
    const state = store.gameState;
    const waitingFor = state?.waiting_for ?? null;
    if (!state || !waitingFor) return false;
    if (waitingForFingerprint(waitingFor) !== attempt.waitingForFingerprint) return false;
    if (authorizedAiPlayer(waitingFor, state) !== attempt.playerId) return false;
    return !attempt.isPriority || !store.isResolvingAll;
  }

  function finishAttempt(attempt: AIAttempt): boolean {
    if (attempt.generation !== attemptGeneration) return false;
    currentAttempt = null;
    pending = false;
    return true;
  }

  function invalidateAttempt(): void {
    attemptGeneration++;
    currentAttempt = null;
    pending = false;
    if (timeoutId != null) {
      clearTimeout(timeoutId);
      timeoutId = null;
    }
  }

  function checkAndSchedule() {
    if (!active || pending) return;

    const state = useGameStore.getState().gameState;
    if (!state?.waiting_for) return;

    const waitingFor = state.waiting_for;

    // Game over -- stop scheduling
    if (waitingFor.type === "GameOver") return;

    // CR 103.5: Simultaneous mulligan — pending may contain multiple players;
    // route to the first AI seat that still owes a decision/bottom selection.
    // For all other states, the engine-authored `priority_player` is the
    // authorized submitter, including controlled turns (CR 723.5).
    const mulliganPid = aiPendingForMulligan(
      waitingFor as { type: string; data?: { pending?: { player: number }[] } },
    );
    const waitingPlayerId = authorizedAiPlayer(waitingFor, state);
    if (waitingPlayerId === null) return;

    // Resolve All is an explicit, user-started owner of Priority passing. The
    // AI only steps aside while that session is actually active; stack depth is
    // never consent. Mandatory non-Priority decisions continue normally.
    if (waitingFor.type === "Priority" && useGameStore.getState().isResolvingAll) return;

    // Reset failure counters when the WaitingFor state changes (type or player).
    // `consecutiveFailures` gates normal→fallback escalation; `totalFailures`
    // is the absolute hard stop that kills the controller.
    const key = waitingForKey(waitingFor, mulliganPid);
    if (key !== lastWaitingForKey) {
      lastWaitingForKey = key;
      consecutiveFailures = 0;
      totalFailures = 0;
      lastDispatchError = null;
    }

    // Hard stop: if we've burned through both the normal and fallback paths
    // on the same key without progress, the engine is unrecoverably stuck
    // for this seat. Surface to the user instead of spinning. Previously
    // there was no absolute cap — fallback failures could loop indefinitely,
    // generating log storms.
    if (totalFailures >= MAX_TOTAL_FAILURES) {
      debugLog(
        `AI controller halting: ${totalFailures} failures on ${waitingFor.type}`,
        "error",
      );
      notifyEngineLost(`ai-controller-stuck:${waitingFor.type}`);
      stop();
      return;
    }

    if (consecutiveFailures >= MAX_CONSECUTIVE_FAILURES) {
      debugLog(
        `AI stuck: ${MAX_CONSECUTIVE_FAILURES} consecutive failures on ${waitingFor.type}, dispatching fallback`,
        "warn",
      );
      const attempt = beginAttempt(waitingFor, waitingPlayerId);
      runEscapeFallback(waitingFor, waitingPlayerId, attempt).finally(() => {
        if (finishAttempt(attempt) && active) checkAndSchedule();
      });
      return;
    }

    scheduleAction(waitingPlayerId);
  }

  function pickEscapeAction(
    waitingFor: WaitingFor,
    state: GameState,
  ): Promise<GameAction> {
    if (state.has_pending_cast) {
      return Promise.resolve({ type: "CancelCast" });
    }
    const { adapter } = useGameStore.getState();
    if (!adapter) return Promise.resolve({ type: "PassPriority" });
    return adapter.getLegalActions().then((result) => {
      if (waitingFor.type === "Priority") {
        return (
          result.actions.find((a) => a.type === "PassPriority") ??
          { type: "PassPriority" }
        );
      }
      return result.actions[0] ?? { type: "PassPriority" };
    });
  }

  async function runEscapeFallback(
    waitingFor: WaitingFor,
    waitingPlayerId: number,
    attempt: AIAttempt,
  ): Promise<void> {
    const state = useGameStore.getState().gameState;
    if (!state || !isAttemptCurrent(attempt)) return;
    try {
      const fallback = await pickEscapeAction(waitingFor, state);
      if (!isAttemptCurrent(attempt)) return;
      await dispatchAction(fallback, waitingPlayerId);
      if (!isAttemptCurrent(attempt)) return;
      consecutiveFailures = 0;
      totalFailures = 0;
      lastDispatchError = null;
    } catch (e) {
      if (!isAttemptCurrent(attempt)) return;
      consecutiveFailures++;
      totalFailures++;
      lastDispatchError = e instanceof Error ? e.message : String(e);
      debugLog(
        `AI fallback also failed (${consecutiveFailures}/${totalFailures}): ${lastDispatchError}`,
        "warn",
      );
    }
  }

  function scheduleAction(playerId: number) {
    if (pending) return;

    // Start computing immediately — in parallel with the artificial delay.
    // This turns additive latency (delay + compute) into max(delay, compute),
    // which matters most for VeryHard where the pool search takes 1-2 seconds.
    const { adapter, gameState } = useGameStore.getState();
    // Each seat has its own difficulty — a controller driving three AI players
    // can simultaneously run Easy, Medium, and VeryHard policies.
    const difficulty = difficultyByPlayerId.get(playerId) ?? "Medium";
    const waitingForType = gameState?.waiting_for?.type;
    const scheduledWaitingFor = gameState?.waiting_for ?? null;
    if (!scheduledWaitingFor) return;
    const attempt = beginAttempt(scheduledWaitingFor, playerId);
    const actionPromise: Promise<GameAction | null> = Promise.resolve(
      adapter?.getAiAction(difficulty, playerId, waitingForType) ?? null,
    );
    // Suppress unhandled-rejection warnings if stop() cancels the timeout
    // before it fires and nothing else awaits this promise.
    actionPromise.catch(() => {});

    // Mulligan is a binary keep/mulligan decision with no strategic complexity to
    // humanize — skip the artificial delay so the decision resolves as soon as the
    // engine returns (computation is near-instant after our optimizations).
    const isMulligan =
      waitingForType === "MulliganDecision" ||
      waitingForType === "OpeningHandBottomCards";
    // Stack pressure scales only the artificial humanization delay; it never
    // owns or skips the AI decision. Rate-driven pressure keeps low-depth,
    // high-churn loops from paying a full 500–900ms beat on every cycle
    // (Rapid → ~75ms).
    const stackLen = gameState?.stack?.length ?? 0;
    const baseDelay = isMulligan ? 0 : AI_BASE_DELAY_MS + Math.random() * AI_DELAY_VARIANCE_MS;
    const delay = Math.round(baseDelay * pressureMultiplier(effectiveStackPressure(stackLen)));
    timeoutId = setTimeout(async () => {
      timeoutId = null;
      let failed = false;
      try {
        let action: GameAction | null;
        try {
          action = await actionPromise;
        } catch (err) {
          if (!isAttemptCurrent(attempt)) return;
          // Engine panic: re-running the same AI search against the same
          // (deterministic) state will re-panic. This is the path the
          // user-reported "ai-getAction-retry" came from — short-circuit
          // with the captured panic so the modal can show the real cause.
          if (isEnginePanic(err)) {
            await routePanic("ai-getAction-panic", err.panic);
            if (!isAttemptCurrent(attempt)) return;
            throw err;
          }
          if (!isStateLost(err)) throw err;
          // Engine lost state between scheduleAction and the timeout firing.
          // Try to rehydrate from the store snapshot and recompute the AI
          // action once. If recovery fails (or the retry still throws because
          // restoreState silently failed in the worker), escalate to the
          // user-prompt path.
          debugLog("AI getAiAction hit STATE_LOST; attempting rehydrate", "warn");
          if (!isAttemptCurrent(attempt)) return;
          const recovered = await attemptStateRehydrate();
          if (!isAttemptCurrent(attempt)) return;
          if (!recovered) {
            notifyEngineLost("ai-getAction");
            throw err;
          }
          try {
            if (!isAttemptCurrent(attempt)) return;
            action = await adapter!.getAiAction(difficulty, playerId, waitingForType);
          } catch (retryErr) {
            if (!isAttemptCurrent(attempt)) return;
            if (isEnginePanic(retryErr)) {
              await routePanic("ai-getAction-retry-panic", retryErr.panic);
              if (!isAttemptCurrent(attempt)) return;
            } else {
              notifyEngineLost("ai-getAction-retry");
            }
            throw retryErr;
          }
        }
        // Re-check the complete attempt identity after every await. A matching
        // WaitingFor payload in a new game/session is still stale, as is a
        // Priority action computed before Resolve All took ownership.
        if (!isAttemptCurrent(attempt)) {
          const currentWaitingFor = useGameStore.getState().gameState?.waiting_for ?? null;
          debugLog(
            `AI ignored stale ${action?.type ?? "action"} for player ${playerId + 1}: waitingFor changed from ${waitingForDebugLabel(scheduledWaitingFor)} to ${waitingForDebugLabel(currentWaitingFor)}`,
            "info",
          );
          return;
        }
        const currentGameState = useGameStore.getState().gameState;
        const currentWaitingFor = currentGameState?.waiting_for ?? null;
        if (action == null) {
          debugLog(
            `AI getAiAction returned null for player ${playerId} (waitingFor: ${currentWaitingFor?.type ?? "none"}), dispatching legal-action fallback`,
            "warn",
          );
          const waitingFor = currentWaitingFor;
          if (waitingFor != null) {
            await runEscapeFallback(waitingFor, playerId, attempt);
          } else {
            failed = true;
          }
          return;
        }
        const guess = describeAiCardPredicateGuess(action, currentWaitingFor, currentGameState);
        if (guess != null) {
          debugLog(`AI player ${playerId + 1} randomly ${guess}`, "info");
        }
        // Pass `playerId` (the AI seat we're driving) as actor. The engine
        // guard in `apply` verifies actor matches the authorized submitter;
        // dispatching as the human here would be rejected.
        // dispatch.ts has its own STATE_LOST recovery; any error that reaches
        // here after that retry is genuinely unrecoverable for this attempt.
        if (!isAttemptCurrent(attempt)) return;
        await dispatchAction(action, playerId);
        if (!isAttemptCurrent(attempt)) return;
        // Successful dispatch — reset both failure counters
        consecutiveFailures = 0;
        totalFailures = 0;
        lastDispatchError = null;
      } catch (e) {
        if (!isAttemptCurrent(attempt)) return;
        lastDispatchError = e instanceof Error ? e.message : String(e);
        debugLog(`AI error choosing action: ${lastDispatchError}`);
        failed = true;
      } finally {
        if (finishAttempt(attempt)) {
          if (failed) {
            consecutiveFailures++;
            totalFailures++;
          }
          if (active) checkAndSchedule();
        }
      }
    }, delay);
  }

  function start() {
    active = true;
    if (unsubscribe) {
      unsubscribe();
      unsubscribe = null;
    }
    debugLog(`AI controller started (configured seats: [${[...aiPlayerIds].join(",")}], dynamic for all non-human)`, "warn");
    // Event-driven design: subscribe to WaitingFor changes and let each
    // seat's turn naturally surface via the store. This means reconnect
    // is implicit — whichever seat holds priority after a reconnect
    // triggers `checkAndSchedule`, regardless of how many AI seats the
    // controller supervises. No per-seat iteration needed; the bug that
    // previously stalled P3/P4 was caused by `getAiAction` accepting a
    // default `playerId` elsewhere, not by this loop.
    let observedWaitingFor = useGameStore.getState().waitingFor;
    let observedSessionGeneration = useGameStore.getState().gameSessionGeneration;
    let observedResolveAll = useGameStore.getState().isResolvingAll;
    unsubscribe = useGameStore.subscribe(
      (s) => s,
      () => {
        if (!active) return;
        const store = useGameStore.getState();
        const waitingForChanged = store.waitingFor !== observedWaitingFor;
        const sessionChanged = store.gameSessionGeneration !== observedSessionGeneration;
        const resolveAllStarted = store.isResolvingAll && !observedResolveAll;
        const resolveAllEnded = !store.isResolvingAll && observedResolveAll;

        observedWaitingFor = store.waitingFor;
        observedSessionGeneration = store.gameSessionGeneration;
        observedResolveAll = store.isResolvingAll;

        if (waitingForChanged || sessionChanged) {
          invalidateAttempt();
          // A new snapshot gets a fresh failure budget even for an A→A
          // transition whose serialized WaitingFor payload is identical.
          lastWaitingForKey = null;
        } else if (resolveAllStarted && currentAttempt?.isPriority) {
          invalidateAttempt();
        }

        // Resolve All owns Priority while active. Mandatory non-Priority
        // decisions continue, and ending Resolve All schedules exactly once.
        if (
          resolveAllEnded ||
          waitingForChanged ||
          sessionChanged ||
          !store.isResolvingAll ||
          store.gameState?.waiting_for?.type !== "Priority"
        ) {
          checkAndSchedule();
        }
      },
    );
    checkAndSchedule();
  }

  function stop() {
    active = false;
    invalidateAttempt();
  }

  function dispose() {
    stop();
    if (unsubscribe) {
      unsubscribe();
      unsubscribe = null;
    }
  }

  return { start, stop, dispose };
}
