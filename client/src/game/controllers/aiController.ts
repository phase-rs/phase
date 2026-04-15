import { AI_BASE_DELAY_MS, AI_DELAY_VARIANCE_MS } from "../../constants/game";
import { useGameStore } from "../../stores/gameStore";
import type { GameAction } from "../../adapter/types";
import { debugLog } from "../debugLog";
import { dispatchAction } from "../dispatch";
import type { OpponentController } from "./types";

export interface AIControllerConfig {
  difficulty: string;
  playerIds: number[];
}

export interface AIController extends OpponentController {
  start(): void;
  stop(): void;
  dispose(): void;
}

export function createAIController(config: AIControllerConfig): AIController {
  let active = false;
  let pending = false;
  let timeoutId: ReturnType<typeof setTimeout> | null = null;
  let unsubscribe: (() => void) | null = null;

  // Track consecutive failures on the same WaitingFor state to break infinite loops.
  // When the AI returns null or the engine rejects the action, the WaitingFor doesn't
  // change — without a cap, checkAndSchedule would retry forever.
  let lastWaitingForKey: string | null = null;
  let consecutiveFailures = 0;
  const MAX_CONSECUTIVE_FAILURES = 3;

  const aiPlayerIds = new Set(config.playerIds);

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

    // Check if it's an AI player's turn
    if (!("data" in waitingFor) || !waitingFor.data || !("player" in waitingFor.data)) return;
    if (!aiPlayerIds.has(waitingFor.data.player)) return;

    // Reset failure counter when the WaitingFor state changes (type or player)
    const key = waitingForKey(waitingFor);
    if (key !== lastWaitingForKey) {
      lastWaitingForKey = key;
      consecutiveFailures = 0;
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
        })
        .catch((e) => {
          // Increment to prevent infinite fallback retry on persistent errors
          consecutiveFailures++;
          debugLog(
            `AI fallback also failed (${consecutiveFailures}): ${e instanceof Error ? e.message : String(e)}`,
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
    const actionPromise: Promise<GameAction | null> = Promise.resolve(
  adapter?.getAiAction(config.difficulty, playerId) ?? null,
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
        const action = await actionPromise;
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
        await dispatchAction(action, playerId);
        // Successful dispatch — reset failure counter
        consecutiveFailures = 0;
      } catch (e) {
        debugLog(`AI error choosing action: ${e instanceof Error ? e.message : String(e)}`);
        failed = true;
      } finally {
        if (failed) {
          consecutiveFailures++;
        }
        pending = false;
        if (active) checkAndSchedule();
      }
    }, delay);
  }

  function start() {
    active = true;
    debugLog(`AI controller started for players [${[...aiPlayerIds].join(",")}]`, "warn");
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
