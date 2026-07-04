import { useTranslation } from "react-i18next";

import { isMultiplayerMode, useGameStore } from "../../stores/gameStore.ts";

/**
 * Undo button for the local player, rendered attached to the player HUD beside
 * the Manual mana toggle. Undo is a single-player affordance only — multiplayer
 * games have authoritative shared state and can't safely rewind one client.
 */
export function UndoButton() {
  const { t } = useTranslation("game");
  const canUndo = useGameStore(
    (s) => s.stateHistory.length > 0 && !isMultiplayerMode(s.gameMode),
  );
  const undo = useGameStore((s) => s.undo);

  if (!canUndo) return null;
  return (
    <button
      onClick={undo}
      className="flex items-center gap-1 rounded-md bg-gray-800/80 px-2.5 py-1 text-[11px] font-medium text-gray-400 transition-colors hover:bg-gray-700/80 hover:text-gray-200"
    >
      <svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 16 16" fill="currentColor" className="h-3 w-3">
        <path fillRule="evenodd" d="M14 8a6 6 0 1 1-12 0 6 6 0 0 1 12 0ZM7.72 4.22a.75.75 0 0 0-1.06 0L4.97 5.91a.75.75 0 0 0 0 1.06l1.69 1.69a.75.75 0 1 0 1.06-1.06l-.47-.47h1.63a1.25 1.25 0 0 1 0 2.5H7.5a.75.75 0 0 0 0 1.5h1.38a2.75 2.75 0 0 0 0-5.5H7.25l.47-.47a.75.75 0 0 0 0-1.06Z" clipRule="evenodd" />
      </svg>
      {t("board.undo")}
    </button>
  );
}
