import { useMemo } from "react";

import type { GameObject, TargetRef } from "../../adapter/types.ts";
import { CardImage } from "../card/CardImage.tsx";
import { ModalPanelShell } from "../ui/ModalPanelShell.tsx";
import { useGameStore } from "../../stores/gameStore.ts";
import { useUiStore } from "../../stores/uiStore.ts";
import { usePlayerId } from "../../hooks/usePlayerId.ts";
import { useGameDispatch } from "../../hooks/useGameDispatch.ts";

interface ZoneViewerProps {
  zone: "graveyard" | "exile";
  playerId: number;
  onClose: () => void;
}

const ZONE_TITLES: Record<string, string> = {
  graveyard: "Graveyard",
  exile: "Exile",
};

function hasAdventureCreaturePermission(obj: GameObject): boolean {
  return obj.casting_permissions?.some((p) => p.type === "AdventureCreature") ?? false;
}

function isObjectATarget(targetRefs: TargetRef[], objectId: number): boolean {
  return targetRefs.some((t) => "Object" in t && t.Object === objectId);
}

export function ZoneViewer({ zone, playerId, onClose }: ZoneViewerProps) {
  const gameState = useGameStore((s) => s.gameState);
  const dispatch = useGameStore((s) => s.dispatch);
  const dispatchAction = useGameDispatch();
  const inspectObject = useUiStore((s) => s.inspectObject);
  const currentPlayerId = usePlayerId();

  const cards = useMemo(() => {
    if (!gameState) return [];

    const ids =
      zone === "graveyard"
        ? (gameState.players[playerId]?.graveyard ?? [])
        : gameState.exile.filter((id) => gameState.objects[id]?.owner === playerId);

    return ids.map((id) => gameState.objects[id]).filter(Boolean);
  }, [gameState, zone, playerId]);

  const isMyZone = playerId === currentPlayerId;
  const hasPriority = gameState?.waiting_for?.type === "Priority"
    && gameState.waiting_for.data.player === currentPlayerId;

  const waitingFor = gameState?.waiting_for;
  const isHumanTargetSelection =
    (waitingFor?.type === "TargetSelection" || waitingFor?.type === "TriggerTargetSelection")
    && waitingFor.data.player === currentPlayerId;
  const currentLegalTargets: TargetRef[] = isHumanTargetSelection
    ? waitingFor.data.selection.current_legal_targets
    : [];

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
            {cards.map((obj) => {
              const canCastAdventure = zone === "exile" && isMyZone && hasPriority
                && hasAdventureCreaturePermission(obj);
              const isValidTarget = isHumanTargetSelection
                && isObjectATarget(currentLegalTargets, obj.id);
              return (
                <div
                  key={obj.id}
                  className={`cursor-pointer rounded-lg border bg-gray-800/60 p-1 transition-colors ${
                    isValidTarget
                      ? "border-amber-400 ring-2 ring-amber-400/60 shadow-[0_0_12px_3px_rgba(201,176,55,0.8)]"
                      : canCastAdventure
                        ? "border-amber-500/60 hover:border-amber-400"
                        : "border-gray-700 hover:border-gray-500"
                  }`}
                  onMouseEnter={() => inspectObject(obj.id)}
                  onMouseLeave={() => inspectObject(null)}
                  onClick={isValidTarget
                    ? () => dispatchAction({ type: "ChooseTarget", data: { target: { Object: obj.id } } })
                    : undefined}
                >
                  <CardImage cardName={obj.name} size="normal" className="aspect-[5/7] !h-auto !w-full" />
                  {canCastAdventure && !isValidTarget && (
                    <button
                      onClick={() => dispatch({ type: "CastSpell", data: { card_id: obj.card_id, targets: [] } })}
                      className="mt-1 w-full rounded-md bg-amber-600/80 px-2 py-1 text-xs font-semibold text-white transition hover:bg-amber-500"
                    >
                      Cast Creature
                    </button>
                  )}
                </div>
              );
            })}
          </div>
        )}
      </div>
    </ModalPanelShell>
  );
}
