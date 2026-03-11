import { getPlayerId } from "../../hooks/usePlayerId";
import { useGameStore } from "../../stores/gameStore";
import { usePreferencesStore } from "../../stores/preferencesStore";
import { useUiStore } from "../../stores/uiStore";
import { shouldAutoPass } from "../autoPass";
import { dispatchAction } from "../dispatch";
import { createAIController } from "./aiController";
import type { OpponentController } from "./types";

const AUTO_PASS_BEAT_MS = 200;

export interface GameLoopConfig {
  mode: "ai" | "online" | "local";
  difficulty?: string;
  playerCount?: number;
}

export interface GameLoopController {
  start(): void;
  stop(): void;
  dispose(): void;
}

export function createGameLoopController(config: GameLoopConfig): GameLoopController {
  let active = false;
  let opponentController: OpponentController | null = null;
  let unsubscribe: (() => void) | null = null;
  let autoPassTimeout: ReturnType<typeof setTimeout> | null = null;

  function onWaitingForChanged(): void {
    if (!active) return;

    const { waitingFor, gameState } = useGameStore.getState();
    if (!waitingFor || waitingFor.type === "GameOver") return;

    // Only auto-pass Priority prompts for the human player
    if (waitingFor.type !== "Priority") return;
    if (!("data" in waitingFor) || waitingFor.data.player !== getPlayerId()) return;

    const { fullControl } = useUiStore.getState();
    const { phaseStops } = usePreferencesStore.getState();

    if (!gameState) return;

    const { legalActions } = useGameStore.getState();
    if (shouldAutoPass(gameState, waitingFor, phaseStops, fullControl, legalActions)) {
      scheduleAutoPass();
    }
  }

  function scheduleAutoPass(): void {
    // Clear any existing pending auto-pass
    if (autoPassTimeout != null) {
      clearTimeout(autoPassTimeout);
    }

    autoPassTimeout = setTimeout(() => {
      autoPassTimeout = null;
      if (!active) return;
      dispatchAction({ type: "PassPriority" });
    }, AUTO_PASS_BEAT_MS);
  }

  function start(): void {
    active = true;

    if (config.mode === "ai") {
      const count = config.playerCount ?? 2;
      const playerIds = Array.from({ length: count - 1 }, (_, i) => i + 1);
      opponentController = createAIController({
        difficulty: config.difficulty ?? "Medium",
        playerIds,
      });
      opponentController.start();
    }

    unsubscribe = useGameStore.subscribe(
      (s) => s.waitingFor,
      () => onWaitingForChanged(),
    );

    // Process current state immediately
    onWaitingForChanged();
  }

  function stop(): void {
    active = false;

    if (autoPassTimeout != null) {
      clearTimeout(autoPassTimeout);
      autoPassTimeout = null;
    }

    if (opponentController) {
      opponentController.stop();
    }
  }

  function dispose(): void {
    stop();

    if (unsubscribe) {
      unsubscribe();
      unsubscribe = null;
    }

    if (opponentController) {
      opponentController.dispose();
      opponentController = null;
    }
  }

  return { start, stop, dispose };
}
