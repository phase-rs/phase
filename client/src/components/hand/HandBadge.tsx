import { useTranslation } from "react-i18next";

import { useGameStore } from "../../stores/gameStore.ts";
import { useUiStore } from "../../stores/uiStore.ts";
import { usePerspectivePlayerId } from "../../hooks/usePlayerId.ts";

export function HandBadge({ className }: { className?: string } = {}) {
  const { t } = useTranslation("game");
  const playerId = usePerspectivePlayerId();
  const handSize = useGameStore((s) => s.gameState?.players[playerId]?.hand.length ?? 0);
  const setMobileHandOpen = useUiStore((s) => s.setMobileHandOpen);

  if (handSize === 0) return null;

  return (
    <button
      aria-label={t("hand.viewFullHand", { count: handSize })}
      className={`tabletop-control-chip flex min-h-11 items-center justify-center gap-1.5 border border-cyan-400/20 px-3 py-1 text-[10px] font-semibold uppercase tracking-[0.14em] text-stone-300 transition-all duration-200 hover:border-cyan-300/40 hover:text-white lg:min-h-0 lg:px-3.5 lg:py-1.5 lg:text-[11px] ${className ?? ""}`}
      onClick={() => setMobileHandOpen(true)}
    >
      <svg
        aria-hidden="true"
        viewBox="0 0 24 24"
        className="h-3 w-3 text-cyan-300"
        fill="none"
        stroke="currentColor"
        strokeWidth="2"
        strokeLinecap="round"
        strokeLinejoin="round"
      >
        <rect x="4" y="3" width="11" height="16" rx="2" />
        <path d="M9 7l11 3-3 11-7-2" />
      </svg>
      {t("hand.handLabel", { count: handSize })}
    </button>
  );
}
