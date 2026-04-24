import { getPlayerId } from "../../hooks/usePlayerId";
import { useGameStore } from "../../stores/gameStore";
import { useUiStore } from "../../stores/uiStore";
import { shouldAutoPass } from "../autoPass";
import { dispatchAction } from "../dispatch";
import { createAIController, type AISeatBinding } from "./aiController";
import type { OpponentController } from "./types";

const AUTO_PASS_BEAT_MS = 200;

export interface GameLoopConfig {
  mode: "ai" | "online" | "local";
  /** Default difficulty used as a fallback when no per-seat binding is
   *  supplied (e.g. legacy resume with only a flat `difficulty` in the
   *  `ActiveGameMeta`). Every AI seat gets this difficulty. */
  difficulty?: string;
  /** Explicit per-seat AI policy. When present this takes precedence over
   *  `difficulty` + `playerCount`; each entry drives exactly one AI player. */
  aiSeats?: AISeatBinding[];
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

    if (!gameState) return;

    const { autoPassRecommended } = useGameStore.getState();
    if (shouldAutoPass(gameState, waitingFor, fullControl, autoPassRecommended)) {
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
      const fallbackDifficulty = config.difficulty ?? "Medium";
      // PlayerIds 1..N-1 are the AI seats (0 is the local human). Map each
      // engine-side AI playerId to its configured difficulty, falling back
      // to the flat `difficulty` when no per-seat binding is supplied.
      const seats: AISeatBinding[] = Array.from({ length: count - 1 }, (_, i) => {
        const playerId = i + 1;
        return {
          playerId,
          difficulty: config.aiSeats?.[i]?.difficulty ?? fallbackDifficulty,
        };
      });
      opponentController = createAIController({ seats });
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
