import { useTranslation } from "react-i18next";

import { isAuthorityRemote, useGameStore } from "../../stores/gameStore.ts";

/**
 * Undo button for the local player, rendered within the player HUD treatment.
 * Undo is a single-player affordance only — multiplayer games have authoritative
 * shared state and can't safely rewind one client.
 */
export function UndoButton({ iconOnly = false }: { iconOnly?: boolean }) {
  const { t } = useTranslation("game");
  const canUndo = useGameStore(
    (s) => s.stateHistory.length > 0 && !isAuthorityRemote(s.gameMode),
  );
  const undo = useGameStore((s) => s.undo);

  if (!canUndo) return null;
  return (
    <button
      type="button"
      onClick={undo}
      aria-label={t("board.undo")}
      data-icon-only={iconOnly ? "true" : undefined}
      data-undo-button=""
      className={`flex items-center justify-center gap-1 text-[11px] font-medium transition-colors ${iconOnly ? "tabletop-liquid-glass-control h-11 w-11 p-0 text-slate-200 hover:text-white focus-visible:outline focus-visible:outline-2 focus-visible:outline-cyan-200/70" : "rounded-md bg-gray-800/80 px-2.5 py-1 text-gray-400 hover:bg-gray-700/80 hover:text-gray-200"}`}
    >
      <svg
        xmlns="http://www.w3.org/2000/svg"
        viewBox="0 0 24 24"
        fill="none"
        stroke="currentColor"
        strokeWidth="2.4"
        strokeLinecap="round"
        strokeLinejoin="round"
        className={iconOnly ? "h-5 w-5" : "h-3 w-3"}
      >
        <path d="m9 15-6-6m0 0 6-6M3 9h12a6 6 0 0 1 0 12h-3" />
      </svg>
      {!iconOnly ? <span data-control-label="">{t("board.undo")}</span> : null}
    </button>
  );
}
