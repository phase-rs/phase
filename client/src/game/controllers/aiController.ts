import { AI_BASE_DELAY_MS, AI_DELAY_VARIANCE_MS, PLAYER_ID } from "../../constants/game";
import { useGameStore } from "../../stores/gameStore";
import type { GameAction } from "../../adapter/types";
import { AdapterError, AdapterErrorCode } from "../../adapter/types";
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

export function createAIController(config: AIControllerConfig): AIController {
  let active = false;
  let pending = false;
  let timeoutId: ReturnType<typeof setTimeout> | null = null;
  let unsubscribe: (() => void) | null = null;

  // Failure tracking on the same WaitingFor state to break infinite loops.
  // `MAX_CONSECUTIVE_FAILURES` gates the normal→fallback transition; the
  // separate `MAX_TOTAL_FAILURES` hard-stops the controller so post-fallback
  // failures (e.g., engine rejecting even the safe fallback) cannot spin.
  let lastWaitingForKey: string | null = null;
  let consecutiveFailures = 0;
  let totalFailures = 0;
  const MAX_CONSECUTIVE_FAILURES = 3;

  const difficultyByPlayerId = new Map(config.seats.map((s) => [s.playerId, s.difficulty]));
  const aiPlayerIds = new Set(difficultyByPlayerId.keys());

  /** Stable identity key for a WaitingFor — type + player so Priority{0} ≠ Priority{1}. */
  function waitingForKey(wf: { type: string; data?: { player?: number } }): string {
    const player = wf.data?.player ?? -1;
    return `${wf.type}:${player}`;
  }

  function checkAndSchedule() {
    if (!active || pending) return;

    const state = useGameStore.getState().gameState;
    if (!state?.waiting_for) return;

    const waitingFor = state.waiting_for;

    // Game over -- stop scheduling
    if (waitingFor.type === "GameOver") return;

    // Check if it's an AI player's turn — any non-human player is AI.
    // This is dynamic rather than gating on a static set so that
    // restoreGameState (debug panel import) with a different player count
    // works without rebuilding the controller.
    if (!("data" in waitingFor) || !waitingFor.data || !("player" in waitingFor.data)) return;
    const waitingPlayerId = waitingFor.data.player;
    if (waitingPlayerId === PLAYER_ID) return;

    // Reset failure counters when the WaitingFor state changes (type or player).
    // `consecutiveFailures` gates normal→fallback escalation; `totalFailures`
    // is the absolute hard stop that kills the controller.
    const key = waitingForKey(waitingFor);
    if (key !== lastWaitingForKey) {
      lastWaitingForKey = key;
      consecutiveFailures = 0;
      totalFailures = 0;
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
      notifyEngineLost("ai-controller-stuck");
      stop();
      return;
    }

    if (consecutiveFailures >= MAX_CONSECUTIVE_FAILURES) {
      debugLog(
        `AI stuck: ${MAX_CONSECUTIVE_FAILURES} consecutive failures on ${waitingFor.type}, dispatching fallback`,
        "warn",
      );
      // Instead of freezing the game, dispatch a safe escape action.
      // CancelCast during casting flow, empty combat submissions during combat,
      // PassPriority otherwise.
      // has_pending_cast is computed by the engine — no parallel list needed.
      let fallback: GameAction;
      if (state.has_pending_cast) {
        fallback = { type: "CancelCast" };
      } else if (waitingFor.type === "DeclareAttackers") {
        fallback = { type: "DeclareAttackers", data: { attacks: [] } };
      } else if (waitingFor.type === "DeclareBlockers") {
        fallback = { type: "DeclareBlockers", data: { assignments: [] } };
      } else {
        fallback = { type: "PassPriority" };
      }
      // Guard against re-entry: set pending so subscription callbacks during
      // the fallback dispatch don't trigger another fallback cascade.
      pending = true;
      // Dispatch the fallback as the AI seat that's being unstuck — NEVER
      // as the local human. The engine guard would reject a human-seat actor
      // while the WaitingFor belongs to the AI.
      dispatchAction(fallback, waitingFor.data.player)
        .then(() => {
          consecutiveFailures = 0;
          totalFailures = 0;
        })
        .catch((e) => {
          // Increment both counters to prevent infinite fallback retry.
          consecutiveFailures++;
          totalFailures++;
          debugLog(
            `AI fallback also failed (${consecutiveFailures}/${totalFailures}): ${e instanceof Error ? e.message : String(e)}`,
            "warn",
          );
        })
        .finally(() => {
          pending = false;
          if (active) checkAndSchedule();
        });
      return;
    }

    scheduleAction(waitingFor.data.player);
  }

  function scheduleAction(playerId: number) {
    if (pending) return;
    pending = true;

    // Start computing immediately — in parallel with the artificial delay.
    // This turns additive latency (delay + compute) into max(delay, compute),
    // which matters most for VeryHard where the pool search takes 1-2 seconds.
    const { adapter } = useGameStore.getState();
    // Each seat has its own difficulty — a controller driving three AI players
    // can simultaneously run Easy, Medium, and VeryHard policies.
    const difficulty = difficultyByPlayerId.get(playerId) ?? "Medium";
    const actionPromise: Promise<GameAction | null> = Promise.resolve(
      adapter?.getAiAction(difficulty, playerId) ?? null,
    );
    // Suppress unhandled-rejection warnings if stop() cancels the timeout
    // before it fires and nothing else awaits this promise.
    actionPromise.catch(() => {});

    const delay = AI_BASE_DELAY_MS + Math.random() * AI_DELAY_VARIANCE_MS;
    timeoutId = setTimeout(async () => {
      timeoutId = null;
      if (!active) {
        pending = false;
        return;
      }
      let failed = false;
      try {
        const { gameState } = useGameStore.getState();
        let action: GameAction | null;
        try {
          action = await actionPromise;
        } catch (err) {
          // Engine panic: re-running the same AI search against the same
          // (deterministic) state will re-panic. This is the path the
          // user-reported "ai-getAction-retry" came from — short-circuit
          // with the captured panic so the modal can show the real cause.
          if (isEnginePanic(err)) {
            await routePanic("ai-getAction-panic", err.panic);
            throw err;
          }
          if (!isStateLost(err)) throw err;
          // Engine lost state between scheduleAction and the timeout firing.
          // Try to rehydrate from the store snapshot and recompute the AI
          // action once. If recovery fails (or the retry still throws because
          // restoreState silently failed in the worker), escalate to the
          // user-prompt path.
          debugLog("AI getAiAction hit STATE_LOST; attempting rehydrate", "warn");
          const recovered = await attemptStateRehydrate();
          if (!recovered) {
            notifyEngineLost("ai-getAction");
            throw err;
          }
          try {
            action = await adapter!.getAiAction(difficulty, playerId);
          } catch (retryErr) {
            if (isEnginePanic(retryErr)) {
              await routePanic("ai-getAction-retry-panic", retryErr.panic);
            } else {
              notifyEngineLost("ai-getAction-retry");
            }
            throw retryErr;
          }
        }
        // Re-check active after await — the AI computation may have completed
        // after stop() was called, and dispatching a stale action from the old
        // game into a new game session would corrupt state.
        if (!active) return;
        if (action == null) {
          debugLog(
            `AI getAiAction returned null for player ${playerId} (waitingFor: ${gameState?.waiting_for?.type ?? "none"})`,
            "warn",
          );
          failed = true;
          return;
        }
        // Pass `playerId` (the AI seat we're driving) as actor. The engine
        // guard in `apply` verifies actor matches the authorized submitter;
        // dispatching as the human here would be rejected.
        // dispatch.ts has its own STATE_LOST recovery; any error that reaches
        // here after that retry is genuinely unrecoverable for this attempt.
        await dispatchAction(action, playerId);
        // Successful dispatch — reset both failure counters
        consecutiveFailures = 0;
        totalFailures = 0;
      } catch (e) {
        debugLog(`AI error choosing action: ${e instanceof Error ? e.message : String(e)}`);
        failed = true;
      } finally {
        if (failed) {
          consecutiveFailures++;
          totalFailures++;
        }
        pending = false;
        if (active) checkAndSchedule();
      }
    }, delay);
  }

  function start() {
    active = true;
    debugLog(`AI controller started (configured seats: [${[...aiPlayerIds].join(",")}], dynamic for all non-human)`, "warn");
    // Event-driven design: subscribe to WaitingFor changes and let each
    // seat's turn naturally surface via the store. This means reconnect
    // is implicit — whichever seat holds priority after a reconnect
    // triggers `checkAndSchedule`, regardless of how many AI seats the
    // controller supervises. No per-seat iteration needed; the bug that
    // previously stalled P3/P4 was caused by `getAiAction` accepting a
    // default `playerId` elsewhere, not by this loop.
    unsubscribe = useGameStore.subscribe(
      (s) => s.waitingFor,
      () => {
        if (active) checkAndSchedule();
      },
    );
    checkAndSchedule();
  }

  function stop() {
    active = false;
    if (timeoutId != null) {
      clearTimeout(timeoutId);
      timeoutId = null;
    }
    pending = false;
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
