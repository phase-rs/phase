import { AI_BASE_DELAY_MS, AI_DELAY_VARIANCE_MS } from "../../constants/game";
import { useGameStore } from "../../stores/gameStore";
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
    if (!state) return;

    const waitingFor = state.waiting_for;
    if (!waitingFor) return;

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

    const delay = AI_BASE_DELAY_MS + Math.random() * AI_DELAY_VARIANCE_MS;
    timeoutId = setTimeout(async () => {
      try {
        const { adapter } = useGameStore.getState();
        if (!adapter) {
          pending = false;
          return;
        }
        const action = await adapter.getAiAction(config.difficulty, playerId);
        if (action == null) {
          pending = false;
          return;
        }
        try {
          await dispatchAction(action);
        } catch (dispatchErr) {
          console.warn("[AI] Action rejected, falling back to PassPriority:", dispatchErr);
          await dispatchAction({ type: "PassPriority" });
        }
      } catch (e) {
        console.error("[AI] Error choosing action:", e);
      } finally {
        pending = false;
        if (active) checkAndSchedule();
      }
    }, delay);
  }

  function start() {
    active = true;
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
