import { useTranslation } from "react-i18next";

import type { PlayerId } from "../../adapter/types.ts";
import { useCardImage } from "../../hooks/useCardImage.ts";
import { useGameStore } from "../../stores/gameStore.ts";
import { dispatchAction } from "../../game/dispatch.ts";

interface CompanionZoneProps {
  playerId: PlayerId;
}

export function CompanionZone({ playerId }: CompanionZoneProps) {
  const { t } = useTranslation("game");
  const companion = useGameStore(
    (s) => s.gameState?.players[playerId]?.companion,
  );
  const canActivate = useGameStore((s) =>
    s.legalActions.some((a) => a.type === "CompanionToHand"),
  );

  const cardName = companion?.card.card.name ?? "";
  const { src } = useCardImage(cardName, { size: "normal" });

  if (!companion) return null;

  return (
    <button
      onClick={canActivate ? () => dispatchAction({ type: "CompanionToHand" }) : undefined}
      className={`group relative ${canActivate ? "cursor-pointer" : "cursor-default"}`}
      title={canActivate
        ? t("zone.companionActivate", { name: cardName })
        : companion.used
          ? t("zone.companionTitleUsed", { name: cardName })
          : t("zone.companionTitle", { name: cardName })}
      style={{ width: "var(--card-w)", height: "var(--card-h)" }}
    >
      {/* Card image */}
      <div className="relative h-full w-full overflow-hidden rounded-lg border border-purple-400/60 shadow-md">
        {src ? (
          <img
            src={src}
            alt={cardName}
            className="h-full w-full object-cover"
            draggable={false}
          />
        ) : (
          <div className="h-full w-full bg-gray-700" />
        )}

        {/* Purple translucent overlay (Arena-style) */}
        <div className={`absolute inset-0 transition-colors ${
          companion.used
            ? "bg-gray-900/60"
            : canActivate
              ? "bg-purple-600/30 group-hover:bg-purple-600/10"
              : "bg-purple-600/40"
        }`} />
      </div>

      {/* Companion badge */}
      <div className="absolute -top-1 left-1/2 -translate-x-1/2 z-10 rounded-sm bg-purple-700 px-1.5 py-px text-[8px] font-bold text-purple-100 shadow">
        {t("zone.companion")}
      </div>

      {/* Activatable glow ring */}
      {canActivate && (
        <div className="absolute inset-0 rounded-lg ring-2 ring-purple-400/70 shadow-[0_0_12px_3px_rgba(147,51,234,0.5)]" />
      )}

      {companion.used && (
        <div className="absolute bottom-1 left-1/2 -translate-x-1/2 z-10 rounded-sm bg-gray-900/80 px-1.5 py-px text-[8px] text-gray-400">
          {t("zone.used")}
        </div>
      )}
    </button>
  );
}
