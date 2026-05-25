import { useTranslation } from "react-i18next";

import { useGameStore } from "../../stores/gameStore.ts";
import { useCanActForWaitingState, usePlayerId } from "../../hooks/usePlayerId.ts";
import { getPlayerZoneIds, getWaitingForObjectChoiceIds } from "../../viewmodel/gameStateView.ts";

interface ZoneIndicatorProps {
  zone: "graveyard" | "exile";
  playerId: number;
  onClick: () => void;
}

const ZONE_LABEL_KEYS: Record<string, string> = {
  graveyard: "zone.graveyardShort",
  exile: "zone.exile",
};

export function ZoneIndicator({ zone, playerId, onClick }: ZoneIndicatorProps) {
  const { t } = useTranslation("game");
  const myId = usePlayerId();
  const canActForWaitingState = useCanActForWaitingState();
  const count = useGameStore((s) => {
    return getPlayerZoneIds(s.gameState, zone, playerId).length;
  });

  const hasSelectableCards = useGameStore((s) => {
    if (!canActForWaitingState) return false;
    const objectChoiceIds = new Set(getWaitingForObjectChoiceIds(s.waitingFor));
    return getPlayerZoneIds(s.gameState, zone, playerId).some((id) => objectChoiceIds.has(id));
  });

  return (
    <button
      onClick={onClick}
      className={`cursor-pointer rounded bg-gray-800 px-2 py-0.5 text-xs text-gray-400 transition-colors hover:bg-gray-700 hover:text-gray-200 ${
        hasSelectableCards ? "ring-2 ring-amber-400/60 shadow-[0_0_12px_3px_rgba(201,176,55,0.8)]" : ""
      }`}
    >
      {playerId !== myId ? t("zone.indicatorOpponentPrefix") : ""}{t(ZONE_LABEL_KEYS[zone])} ({count})
    </button>
  );
}
