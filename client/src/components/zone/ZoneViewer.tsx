import { useMemo } from "react";

import { CardImage } from "../card/CardImage.tsx";
import { ModalPanelShell } from "../ui/ModalPanelShell.tsx";
import { useGameStore } from "../../stores/gameStore.ts";
import { useUiStore } from "../../stores/uiStore.ts";

interface ZoneViewerProps {
  zone: "graveyard" | "exile";
  playerId: number;
  onClose: () => void;
}

const ZONE_TITLES: Record<string, string> = {
  graveyard: "Graveyard",
  exile: "Exile",
};

export function ZoneViewer({ zone, playerId, onClose }: ZoneViewerProps) {
  const gameState = useGameStore((s) => s.gameState);
  const inspectObject = useUiStore((s) => s.inspectObject);

  const cards = useMemo(() => {
    if (!gameState) return [];

    const ids =
      zone === "graveyard"
        ? (gameState.players[playerId]?.graveyard ?? [])
        : gameState.exile.filter((id) => gameState.objects[id]?.owner === playerId);

    return ids.map((id) => gameState.objects[id]).filter(Boolean);
  }, [gameState, zone, playerId]);

  return (
    <ModalPanelShell
      title={`${ZONE_TITLES[zone]} (${cards.length})`}
      onClose={onClose}
      maxWidthClassName="max-w-5xl"
      bodyClassName="flex min-h-0 flex-col"
    >
      <div className="min-h-0 flex-1 overflow-y-auto px-3 pb-3 sm:px-6 sm:pb-6">
        {cards.length === 0 ? (
          <p className="py-8 text-center text-sm italic text-gray-600">
            No cards in {ZONE_TITLES[zone].toLowerCase()}
          </p>
        ) : (
          <div className="grid grid-cols-[repeat(auto-fill,minmax(96px,1fr))] gap-3 sm:grid-cols-[repeat(auto-fill,minmax(120px,1fr))]">
            {cards.map((obj) => (
              <div
                key={obj.id}
                className="cursor-pointer rounded-lg border border-gray-700 bg-gray-800/60 p-1 transition-colors hover:border-gray-500"
                onMouseEnter={() => inspectObject(obj.id)}
                onMouseLeave={() => inspectObject(null)}
              >
                <CardImage cardName={obj.name} size="normal" className="aspect-[5/7] !h-auto !w-full" />
              </div>
            ))}
          </div>
        )}
      </div>
    </ModalPanelShell>
  );
}
