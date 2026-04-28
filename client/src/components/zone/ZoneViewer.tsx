import { useCallback, useMemo } from "react";

import type { GameObject } from "../../adapter/types.ts";
import { CardImage } from "../card/CardImage.tsx";
import { ModalPanelShell } from "../ui/ModalPanelShell.tsx";
import { ScrollableCardStrip } from "../modal/ChoiceOverlay.tsx";
import { useLongPress } from "../../hooks/useLongPress.ts";
import { useInspectHoverProps } from "../../hooks/useInspectHoverProps.ts";
import { useGameStore } from "../../stores/gameStore.ts";
import { useUiStore } from "../../stores/uiStore.ts";
import { useCanActForWaitingState, usePerspectivePlayerId } from "../../hooks/usePlayerId.ts";
import { useGameDispatch } from "../../hooks/useGameDispatch.ts";
import { getPlayerZoneIds, getWaitingForObjectChoiceIds } from "../../viewmodel/gameStateView.ts";

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

export function ZoneViewer({ zone, playerId, onClose }: ZoneViewerProps) {
  const objects = useGameStore((s) => s.gameState?.objects);
  const gameState = useGameStore((s) => s.gameState);
  const waitingFor = useGameStore((s) => s.waitingFor);
  const dispatch = useGameStore((s) => s.dispatch);
  const dispatchAction = useGameDispatch();
  const currentPlayerId = usePerspectivePlayerId();
  const canActForWaitingState = useCanActForWaitingState();
  const zoneIds = useMemo(
    () => getPlayerZoneIds(gameState, zone, playerId),
    [gameState, playerId, zone],
  );

  const cards = useMemo(() => {
    if (!objects) return [];
    return zoneIds.map((id) => objects[id]).filter(Boolean);
  }, [objects, zoneIds]);

  const isMyZone = playerId === currentPlayerId;
  const hasPriority = waitingFor?.type === "Priority" && canActForWaitingState;

  const currentLegalTargets = useMemo(() => {
    const targets = new Set<number>();
    if (!canActForWaitingState) return targets;
    for (const objectId of getWaitingForObjectChoiceIds(waitingFor)) {
      targets.add(objectId);
    }
    return targets;
  }, [canActForWaitingState, waitingFor]);

  return (
    <ModalPanelShell
      title={`${ZONE_TITLES[zone]} (${cards.length})`}
      onClose={onClose}
      maxWidthClassName="max-w-5xl"
      bodyClassName="flex min-h-0 flex-col"
    >
      <div className="min-h-0 flex-1 px-2 pb-2 lg:px-6 lg:pb-6">
        {cards.length === 0 ? (
          <p className="py-8 text-center text-sm italic text-gray-600">
            No cards in {ZONE_TITLES[zone].toLowerCase()}
          </p>
        ) : (
          <ScrollableCardStrip
            stripClassName="zone-viewer-strip"
            innerClassName="flex items-center gap-2 lg:gap-3"
          >
            {cards.map((obj) => {
              const canCastAdventure = zone === "exile" && isMyZone && hasPriority
                && hasAdventureCreaturePermission(obj);
              const isValidTarget = currentLegalTargets.has(obj.id);
              return (
                <ZoneCard
                  key={obj.id}
                  obj={obj}
                  isValidTarget={isValidTarget}
                  canCastAdventure={canCastAdventure}
                  onTarget={() => dispatchAction({ type: "ChooseTarget", data: { target: { Object: obj.id } } })}
                  onCastAdventure={() => dispatch({ type: "CastSpell", data: { object_id: obj.id, card_id: obj.card_id, targets: [] } })}
                />
              );
            })}
          </ScrollableCardStrip>
        )}
      </div>
    </ModalPanelShell>
  );
}

function ZoneCard({
  obj,
  isValidTarget,
  canCastAdventure,
  onTarget,
  onCastAdventure,
}: {
  obj: GameObject;
  isValidTarget: boolean;
  canCastAdventure: boolean;
  onTarget: () => void;
  onCastAdventure: () => void;
}) {
  const inspectObject = useUiStore((s) => s.inspectObject);
  const setPreviewSticky = useUiStore((s) => s.setPreviewSticky);
  const hoverProps = useInspectHoverProps();
  const { handlers: longPressHandlers, firedRef: longPressFired } = useLongPress(
    useCallback(() => {
      inspectObject(obj.id);
      setPreviewSticky(true);
    }, [inspectObject, setPreviewSticky, obj.id]),
  );

  return (
    <div
      className={`shrink-0 cursor-pointer rounded transition-colors ${
        isValidTarget
          ? "ring-2 ring-amber-400/60 shadow-[0_0_12px_3px_rgba(201,176,55,0.8)]"
          : canCastAdventure
            ? "ring-1 ring-amber-500/60 hover:ring-amber-400"
            : "hover:ring-1 hover:ring-white/20"
      }`}
      data-card-hover
      {...hoverProps(obj.id)}
      onClick={isValidTarget
        ? () => { if (!longPressFired.current) onTarget(); else longPressFired.current = false; }
        : undefined}
      {...longPressHandlers}
    >
      <CardImage cardName={obj.name} size="normal" />
      {canCastAdventure && !isValidTarget && (
        <button
          onClick={onCastAdventure}
          className="mt-1 w-full rounded-md bg-amber-600/80 px-2 py-1 text-xs font-semibold text-white transition hover:bg-amber-500"
        >
          Cast Creature
        </button>
      )}
    </div>
  );
}
