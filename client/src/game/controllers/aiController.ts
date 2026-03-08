import { get_ai_action } from "../../wasm/engine_wasm";
import type { GameAction, GameState } from "../../adapter/types";
import { useGameStore } from "../../stores/gameStore";

const AI_BASE_DELAY = 800;
const AI_DELAY_VARIANCE = 400;

export interface AIControllerConfig {
  difficulty: string;
}

export interface AIController {
  start(): void;
  stop(): void;
  dispose(): void;
}

export function createAIController(
  getState: () => GameState | null,
  submitAction: (action: GameAction) => Promise<void>,
  config: AIControllerConfig,
): AIController {
  let active = false;
  let pending = false;
  let timeoutId: ReturnType<typeof setTimeout> | null = null;
  let unsubscribe: (() => void) | null = null;

  function checkAndSchedule() {
    if (!active || pending) return;

    const state = getState();
    if (!state) return;

    const waitingFor = state.waiting_for;
    if (!waitingFor) return;

    // Game over -- stop scheduling
    if (waitingFor.type === "GameOver") return;

    // Check if it's the AI's turn (player 1)
    const aiPlayer = 1;
    if (!("data" in waitingFor) || !waitingFor.data || !("player" in waitingFor.data)) return;
    if (waitingFor.data.player !== aiPlayer) return;

    scheduleAction();
  }

  function scheduleAction() {
    if (pending) return;
    pending = true;

    const delay = AI_BASE_DELAY + Math.random() * AI_DELAY_VARIANCE;
    timeoutId = setTimeout(async () => {
      try {
        const actionValue = get_ai_action(config.difficulty);
        if (actionValue == null) {
          pending = false;
          return;
        }
        const action = actionValue as GameAction;
        await submitAction(action);
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
