import { AI_BASE_DELAY_MS, AI_DELAY_VARIANCE_MS } from "../../constants/game";
import { useGameStore } from "../../stores/gameStore";
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

  const aiPlayerIds = new Set(config.playerIds);

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

    scheduleAction(waitingFor.data.player);
  }

  function scheduleAction(playerId: number) {
    if (pending) return;
    pending = true;

    // Start computing immediately — in parallel with the artificial delay.
    // This turns additive latency (delay + compute) into max(delay, compute),
    // which matters most for VeryHard where the pool search takes 1-2 seconds.
    const { adapter } = useGameStore.getState();
    const actionPromise = adapter
      ? adapter.getAiAction(config.difficulty, playerId)
      : Promise.resolve(null);
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
      try {
        const { gameState } = useGameStore.getState();
        const action = await actionPromise;
        if (action == null) {
          debugLog(
            `AI getAiAction returned null for player ${playerId} (waitingFor: ${gameState?.waiting_for?.type ?? "none"})`,
            "warn",
          );
          pending = false;
          return;
        }
        await dispatchAction(action);
      } catch (e) {
        debugLog(`AI error choosing action: ${e instanceof Error ? e.message : String(e)}`);
      } finally {
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
