import { useEffect } from "react";

import { isMultiplayerMode, useGameStore } from "../stores/gameStore";
import { useUiStore } from "../stores/uiStore";
import { dispatchAction } from "../game/dispatch";
import { useAltToggle } from "./useAltToggle";
import {
  copyGameStateDebugSnapshot,
  exportGameStateDebugZip,
} from "../services/gameStateExport";

/**
 * Registers global keyboard shortcuts for the game.
 * - Alt: Toggle parsed-abilities preview (shared via useAltToggle)
 * - Space: Pass priority / advance phase
 * - Enter: Toggle end-turn mode
 * - F: Toggle full control
 * - Z: Undo last unrevealed-info action
 * - T: Tap all untapped lands (when in ManaPayment)
 * - Escape: Cancel current action / cancel end-turn mode
 * - D: Copy game state JSON to clipboard (debug)
 * - Ctrl+D: Export game state JSON as a compressed ZIP (debug)
 * - `: Toggle debug panel
 * - Triple-tap (touch): Toggle debug panel (iPad/mobile)
 */
export function useKeyboardShortcuts(): void {
  useAltToggle();

  // Triple-tap gesture for debug panel on touch devices (no keyboard)
  useEffect(() => {
    let tapCount = 0;
    let lastTap = 0;
    const TAP_WINDOW = 500; // ms between taps
    const REQUIRED_FINGERS = 3;

    const handler = (e: TouchEvent) => {
      if (e.touches.length !== REQUIRED_FINGERS) {
        tapCount = 0;
        return;
      }
      const now = Date.now();
      if (now - lastTap > TAP_WINDOW) tapCount = 0;
      tapCount++;
      lastTap = now;
      if (tapCount >= 2) {
        tapCount = 0;
        useUiStore.getState().toggleDebugPanel();
      }
    };

    window.addEventListener("touchstart", handler);
    return () => window.removeEventListener("touchstart", handler);
  }, []);

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

      const { gameState, waitingFor, dispatch, undo, stateHistory, gameMode } =
        useGameStore.getState();
      const uiState = useUiStore.getState();

      switch (e.key) {
        case " ":
          if (waitingFor?.type === "Priority") {
            e.preventDefault();
            dispatch({ type: "PassPriority" });
          }
          break;

        case "Enter": {
          e.preventDefault();
          // Toggle auto-pass: if any auto-pass is active, cancel it; otherwise set UntilEndOfTurn
          const playerId = gameState?.active_player ?? 0;
          const currentAutoPass = gameState?.auto_pass?.[playerId];
          if (currentAutoPass) {
            dispatchAction({ type: "CancelAutoPass" });
          } else {
            dispatchAction({ type: "SetAutoPass", data: { mode: { type: "UntilEndOfTurn" } } });
          }
          break;
        }

        case "f":
        case "F":
          e.preventDefault();
          uiState.toggleFullControl();
          break;

        case "z":
        case "Z":
          // Only plain Z (no Ctrl/Cmd modifier to avoid conflict with browser undo).
          // Suppressed in multiplayer — the store's undo() already returns
          // early in that mode, but gating the shortcut here also avoids
          // swallowing the keystroke.
          if (!e.ctrlKey && !e.metaKey && !isMultiplayerMode(gameMode)) {
            e.preventDefault();
            if (stateHistory.length > 0) {
              undo();
            }
          }
          break;

        case "t":
        case "T":
          if (waitingFor?.type === "ManaPayment") {
            e.preventDefault();
            // Tap all untapped lands controlled by the player
            const gs = useGameStore.getState().gameState;
            const mp = waitingFor.data.player;
            if (gs) {
              for (const id of gs.battlefield) {
                const o = gs.objects[id];
                if (o && !o.tapped && o.controller === mp
                    && o.card_types.core_types.includes("Land")) {
                  dispatch({ type: "TapLandForMana", data: { object_id: id } });
                }
              }
            }
          }
          break;

        case "Escape": {
          e.preventDefault();
          const escPlayerId = gameState?.active_player ?? 0;
          if (gameState?.auto_pass?.[escPlayerId]) {
            dispatchAction({ type: "CancelAutoPass" });
          } else if (waitingFor?.type === "ManaPayment") {
            dispatch({ type: "CancelCast" });
          } else if (waitingFor?.type === "TargetSelection") {
            dispatch({ type: "CancelCast" });
          } else if (waitingFor?.type === "TriggerTargetSelection") {
            const activeSlot =
              waitingFor.data.target_slots[waitingFor.data.selection.current_slot];
            if (activeSlot?.optional) {
              dispatch({ type: "ChooseTarget", data: { target: null } });
            }
          } else {
            uiState.clearSelectedCards();
          }
          break;
        }

        case "d":
        case "D":
          if (e.ctrlKey && !e.metaKey && !e.altKey && !e.shiftKey) {
            e.preventDefault();
            if (gameState) {
              exportGameStateDebugZip(gameState)
                .then((filename) => console.log(`[Debug] Game state exported to ${filename}`))
                .catch((err) => console.error("[Debug] Failed to export:", err));
            }
          } else if (!e.ctrlKey && !e.metaKey) {
            e.preventDefault();
            if (gameState) {
              copyGameStateDebugSnapshot(gameState)
                .then(() => console.log("[Debug] Game state copied to clipboard"))
                .catch((err) => console.error("[Debug] Failed to copy:", err));
            }
          }
          break;

        case "`":
          e.preventDefault();
          uiState.toggleDebugPanel();
          break;
      }
    };

    window.addEventListener("keydown", handler);
    return () => {
      window.removeEventListener("keydown", handler);
    };
  }, []);
}
