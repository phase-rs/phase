import { getPlayerId } from "../../hooks/usePlayerId";
import { useGameStore } from "../../stores/gameStore";
import { usePreferencesStore } from "../../stores/preferencesStore";
import { useUiStore } from "../../stores/uiStore";
import { pressureMultiplier } from "../../utils/stackPressure";
import { effectiveStackPressure } from "../../utils/stackThroughput";
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
  // Publish the AI seat bindings to the store so telemetry `game_end` can
  // classify `winner_kind`. Only local "ai" mode starts client-side AI
  // controllers (`start()` gates on `mode === "ai"`) with bindings this client
  // owns. "local" is human hotseat (GameProvider passes fabricated `aiSeats`
  // for BOTH "ai" and "local", so we gate on the mode, not on the presence of
  // `config.aiSeats`, or hotseat would be mislabeled vs-AI). Online games CAN
  // have AI seats, but those are server-hosted (`CreateGameWithSettings::ai_seats`)
  // and not identifiable from client-owned config — a guest cannot know them —
  // so we publish none and telemetry treats an online winner as unknown.
  // Cleared with the rest of game state on `reset`.
  const aiSeatIds =
    config.mode === "ai" ? (config.aiSeats?.map((seat) => seat.playerId) ?? []) : [];
  useGameStore.setState({ aiSeatIds });

  let active = false;
  let opponentController: OpponentController | null = null;
  let unsubscribe: (() => void) | null = null;
  let autoPassTimeout: ReturnType<typeof setTimeout> | null = null;

  function clearAutoPassTimeout(): void {
    if (autoPassTimeout != null) {
      clearTimeout(autoPassTimeout);
      autoPassTimeout = null;
    }
  }

  function onWaitingForChanged(): void {
    if (!active) return;
    clearAutoPassTimeout();

    const { waitingFor, gameState } = useGameStore.getState();
    if (!waitingFor || waitingFor.type === "GameOver") return;
    if (waitingFor.type !== "Priority") return;
    if (!("data" in waitingFor)) return;
    if (!gameState) return;

    if (gameState.priority_player !== getPlayerId()) return;

    const { fullControl } = useUiStore.getState();
    const { autoPassRecommended } = useGameStore.getState();
    if (shouldAutoPass(gameState, waitingFor, fullControl, autoPassRecommended)) {
      scheduleAutoPass();
    }
  }

  function scheduleAutoPass(): void {
    clearAutoPassTimeout();
    // Scale the auto-pass beat by stack pressure. A low-depth-high-churn loop
    // (Exquisite Blood + Sanguine Bond) keeps depth < Elevated forever, so the
    // batch path never engages and the human seat pays a full 200ms beat per
    // cycle — the dominant artificial wait in that case. Rate-driven pressure
    // collapses the beat (Rapid → ~30ms) once the loop is churning.
    // The user's animation-speed preference composes multiplicatively on top of
    // the pressure scaling (re-read here, like scheduleDiceAdvance, so a live
    // slider change applies to the next beat); 0 = immediate pass (0ms beat) but
    // the pass still dispatches — we never skip it, only its wait.
    const stackLen = useGameStore.getState().gameState?.stack?.length ?? 0;
    const speed = usePreferencesStore.getState().animationSpeedMultiplier;
    const beat = Math.round(
      AUTO_PASS_BEAT_MS * pressureMultiplier(effectiveStackPressure(stackLen)) * speed,
    );
    autoPassTimeout = setTimeout(() => {
      autoPassTimeout = null;
      if (!active) return;
      const { waitingFor, gameState, autoPassRecommended } = useGameStore.getState();
      const { fullControl } = useUiStore.getState();
      if (
        !waitingFor ||
        !gameState ||
        !shouldAutoPass(gameState, waitingFor, fullControl, autoPassRecommended)
      ) {
        return;
      }
      dispatchAction({ type: "PassPriority" });
    }, beat);
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

    clearAutoPassTimeout();

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
