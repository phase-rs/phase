import { useEffect } from "react";

import { useGameStore } from "../stores/gameStore";
import { useUiStore } from "../stores/uiStore";

/**
 * Unrevealed-information action types that are safe to undo.
 * These don't reveal hidden information (unlike CardDrawn).
 */
const UNDOABLE_ACTIONS = new Set([
  "PassPriority",
  "DeclareAttackers",
  "DeclareBlockers",
  "ActivateAbility",
]);

/** Maximum number of undo history entries to keep. */
const MAX_UNDO_HISTORY = 5;

/**
 * Registers global keyboard shortcuts for the game.
 * - Space/Enter: Pass priority (when waiting for Priority)
 * - F: Toggle full control
 * - Z: Undo last unrevealed-info action
 * - T: Tap all untapped lands (when in ManaPayment)
 * - Escape: Cancel current action
 */
export function useKeyboardShortcuts(): void {
  useEffect(() => {
    const handler = (e: KeyboardEvent) => {
      // Don't fire shortcuts when typing in input fields
      const target = e.target as HTMLElement;
      if (
        target.tagName === "INPUT" ||
        target.tagName === "TEXTAREA" ||
        target.tagName === "SELECT" ||
        target.isContentEditable
      ) {
        return;
      }

      const { gameState, waitingFor, dispatch, undo, stateHistory } =
        useGameStore.getState();
      const uiState = useUiStore.getState();

      switch (e.key) {
        case " ":
        case "Enter":
          if (waitingFor?.type === "Priority") {
            e.preventDefault();
            dispatch({ type: "PassPriority" });
          }
          break;

        case "f":
        case "F":
          e.preventDefault();
          uiState.toggleFullControl();
          break;

        case "z":
        case "Z":
          // Only plain Z (no Ctrl/Cmd modifier to avoid conflict with browser undo)
          if (!e.ctrlKey && !e.metaKey) {
            e.preventDefault();
            if (stateHistory.length > 0) {
              undo();
            }
          }
          break;

        case "t":
        case "T":
          if (waitingFor?.type === "ManaPayment" && gameState) {
            e.preventDefault();
            // Tap all untapped lands controlled by the active player
            const player = gameState.players[gameState.priority_player];
            if (player) {
              for (const objId of gameState.battlefield) {
                const obj = gameState.objects[objId];
                if (
                  obj &&
                  obj.controller === player.id &&
                  !obj.tapped &&
                  obj.card_types.core_types.includes("Land")
                ) {
                  dispatch({
                    type: "TapLandForMana",
                    data: { object_id: obj.id },
                  });
                }
              }
            }
          }
          break;

        case "Escape":
          e.preventDefault();
          uiState.clearTargets();
          break;
      }
    };

    window.addEventListener("keydown", handler);
    return () => window.removeEventListener("keydown", handler);
  }, []);
}

export { UNDOABLE_ACTIONS, MAX_UNDO_HISTORY };
