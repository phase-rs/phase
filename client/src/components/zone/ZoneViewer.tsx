import { useCallback, useMemo } from "react";

import type { GameAction, GameObject } from "../../adapter/types.ts";
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
import { playOrCastActionsForObject } from "../../viewmodel/cardActionChoice.ts";
import { abilityChoiceLabel } from "../../viewmodel/costLabel.ts";

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
  const objects = useGameStore((s) => s.gameState?.objects);
  const gameState = useGameStore((s) => s.gameState);
  const waitingFor = useGameStore((s) => s.waitingFor);
  const dispatch = useGameStore((s) => s.dispatch);
  const legalActionsByObject = useGameStore((s) => s.legalActionsByObject);
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
              // CR 702.143a + CR 715.3a + CR 702.62a + CR 702.170d + CR 702.185a:
              // Engine surfaces a CastSpell-family action for every legally
              // castable exile-zone card (Adventure, Foretell, Suspend, Plot,
              // Warp, etc.). The zone viewer surfaces whatever the engine
              // reports — no per-mechanic permission inspection.
              const castActions = zone === "exile" && isMyZone && hasPriority
                ? playOrCastActionsForObject(legalActionsByObject, obj.id)
                : [];
              const isValidTarget = currentLegalTargets.has(obj.id);
              return (
                <ZoneCard
                  key={obj.id}
                  obj={obj}
                  isValidTarget={isValidTarget}
                  castActions={castActions}
                  onTarget={() => dispatchAction({ type: "ChooseTarget", data: { target: { Object: obj.id } } })}
                  onCast={(action) => dispatch(action)}
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
  castActions,
  onTarget,
  onCast,
}: {
  obj: GameObject;
  isValidTarget: boolean;
  castActions: GameAction[];
  onTarget: () => void;
  onCast: (action: GameAction) => void;
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

  const handleClick = useCallback((e: React.MouseEvent) => {
    if (longPressFired.current) { longPressFired.current = false; return; }
    if (useUiStore.getState().debugInteractionMode) {
      e.stopPropagation();
      useUiStore.getState().openDebugContextMenu({ objectId: obj.id, x: e.clientX, y: e.clientY });
      return;
    }
    if (isValidTarget) onTarget();
  }, [obj.id, isValidTarget, onTarget, longPressFired]);

  const canCast = castActions.length > 0;
  return (
    <div
      className={`shrink-0 cursor-pointer rounded transition-colors ${
        isValidTarget
          ? "ring-2 ring-amber-400/60 shadow-[0_0_12px_3px_rgba(201,176,55,0.8)]"
          : canCast
            ? "ring-1 ring-amber-500/60 hover:ring-amber-400"
            : "hover:ring-1 hover:ring-white/20"
      }`}
      data-card-hover
      {...hoverProps(obj.id)}
      onClick={handleClick}
      {...longPressHandlers}
    >
      <CardImage cardName={obj.name} size="normal" />
      {canCast && !isValidTarget && (
        <div className="mt-1 flex flex-col gap-1">
          {castActions.map((action, i) => {
            const { label } = abilityChoiceLabel(action, obj);
            return (
              <button
                key={i}
                onClick={() => onCast(action)}
                className="w-full rounded-md bg-amber-600/80 px-2 py-1 text-xs font-semibold text-white transition hover:bg-amber-500"
              >
                {label}
              </button>
            );
          })}
        </div>
      )}
    </div>
  );
}
