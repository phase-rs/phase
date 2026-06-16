/**
 * Turn-by-turn history timeline shown in the sidebar. Displays a compact list
 * of per-turn snapshots so the player can review how draws and land drops
 * developed over the game.
 */
import { useTranslation } from "react-i18next";
import type { TurnSnapshot } from "../../services/playtestSession";

interface Props {
  history: TurnSnapshot[];
  currentTurn: number;
}

export function TurnHistoryNav({ history, currentTurn }: Props) {
  const { t } = useTranslation("playtest");

  if (history.length === 0) {
    return (
      <div className="py-3 text-center text-xs text-slate-500">
        {t("history.empty")}
      </div>
    );
  }

  return (
    <div className="space-y-1">
      <div className="mb-2 text-[0.65rem] font-semibold uppercase tracking-wider text-slate-500">
        {t("history.title")}
      </div>
      <div className="max-h-60 overflow-y-auto">
        {[...history].reverse().map((snap) => {
          const isCurrent = snap.turnNumber === currentTurn;
          return (
            <div
              key={snap.turnNumber}
              className={`flex items-center gap-2 rounded-lg px-2 py-1.5 text-xs ${
                isCurrent
                  ? "bg-emerald-500/15 text-emerald-200"
                  : "text-slate-400 hover:bg-white/4"
              }`}
            >
              {/* Turn badge */}
              <span
                className={`shrink-0 rounded-md px-1.5 py-0.5 text-[0.6rem] font-bold ${
                  isCurrent ? "bg-emerald-500/30 text-emerald-300" : "bg-white/8 text-slate-500"
                }`}
              >
                {t("history.turn", { turn: snap.turnNumber })}
              </span>

              {/* Metrics row */}
              <div className="flex min-w-0 flex-1 gap-2 overflow-hidden">
                {/* Hand size */}
                <span title={t("history.handSizeTitle")} className="flex items-center gap-0.5">
                  <span className="text-slate-500">✋</span>
                  {snap.handSize}
                </span>
                {/* Lands in play */}
                <span title={t("history.landsTitle")} className="flex items-center gap-0.5">
                  <span className="text-amber-600">🏔</span>
                  {snap.landsInPlay}
                </span>
                {/* Available mana */}
                <span title={t("history.manaTitle")} className="flex items-center gap-0.5 text-blue-400">
                  <span>◆</span>
                  {snap.availableMana}
                </span>
                {/* Playable */}
                <span title={t("history.playableTitle")} className="flex items-center gap-0.5 text-emerald-400">
                  <span>★</span>
                  {snap.playableCount}
                </span>
              </div>

              {/* Drew this turn marker */}
              {snap.cardsDrawn > 0 && (
                <span className="shrink-0 text-[0.55rem] text-blue-400" title={t("history.drewTitle")}>
                  +{snap.cardsDrawn}
                </span>
              )}
            </div>
          );
        })}
      </div>
    </div>
  );
}
