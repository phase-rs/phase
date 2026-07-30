import type { GameLogEntry } from "../adapter/types";
import { useGameStore } from "../stores/gameStore";

/**
 * Inject a debug message into the game log panel.
 * Shows up inline with game events, styled distinctly as "Debug" category.
 * Also logs to console for environments where dev tools are available.
 */
type DebugLogLevel = "info" | "warn" | "error";

export function debugLog(message: string, level: DebugLogLevel = "error"): void {
  if (level === "error") {
    console.error(`[Debug] ${message}`);
  } else if (level === "warn") {
    console.warn(`[Debug] ${message}`);
  } else {
    console.info(`[Debug] ${message}`);
  }

  const store = useGameStore.getState();
  const gameState = store.gameState;

  const entry: GameLogEntry = {
    seq: store.nextLogSeq,
    turn: gameState?.turn_number ?? 0,
    phase: gameState?.phase ?? "PreCombatMain",
    category: "Debug",
    segments: [{ type: "Text", value: `[${level.toUpperCase()}] ${message}` }],
  };

  useGameStore.setState((prev) => ({
    logHistory: [...prev.logHistory, entry].slice(-2000),
    nextLogSeq: prev.nextLogSeq + 1,
  }));
}
