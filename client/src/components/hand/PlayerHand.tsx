import { memo, useState, useCallback, useMemo, useRef } from "react";
import { AnimatePresence, motion } from "framer-motion";
import type { PanInfo } from "framer-motion";

import { CardImage } from "../card/CardImage.tsx";
import { ManaCostPips } from "../mana/ManaCostPips.tsx";
import { useGameStore } from "../../stores/gameStore.ts";
import { useUiStore } from "../../stores/uiStore.ts";
import { useLongPress } from "../../hooks/useLongPress.ts";
import { useIsMobile } from "../../hooks/useIsMobile.ts";
import { useIsCompactHeight } from "../../hooks/useIsCompactHeight.ts";
import { useCanActForWaitingState, usePerspectivePlayerId } from "../../hooks/usePlayerId.ts";
import { dispatchAction } from "../../game/dispatch.ts";
import type { ManaCost, ObjectId } from "../../adapter/types.ts";
import { collectObjectActions } from "../../viewmodel/cardActionChoice.ts";
import { DRAG_PLAY_THRESHOLD } from "../../hooks/useDragToCast.ts";

function getHandOverlap(handSize: number): string {
  if (handSize <= 5) return "calc(var(--card-w) * -0.25)";
  if (handSize <= 7) return "calc(var(--card-w) * -0.45)";
  return "calc(var(--card-w) * -0.6)";
}

interface HandSlotRect {
  objectId: number;
  left: number;
  width: number;
}

export function computeHandInsertionSlot(
  cards: HandSlotRect[],
  clientX: number,
  draggingId: number,
): number | null {
  if (cards.length === 0) return null;

  const remaining = cards.filter((card) => card.objectId !== draggingId);
  for (let slot = 0; slot < remaining.length; slot++) {
    const card = remaining[slot];
    const center = card.left + card.width / 2;
    if (clientX < center) return slot;
  }

  return remaining.length;
}

export function PlayerHand() {
  const playerId = usePerspectivePlayerId();
  const handContainerRef = useRef<HTMLDivElement | null>(null);
  const player = useGameStore((s) => s.gameState?.players[playerId]);
  const objects = useGameStore((s) => s.gameState?.objects);
  // Use dispatchAction (animation pipeline) instead of store dispatch
  const inspectObject = useUiStore((s) => s.inspectObject);
  const setPendingAbilityChoice = useUiStore((s) => s.setPendingAbilityChoice);
  const setMobileHandOpen = useUiStore((s) => s.setMobileHandOpen);
  const isMobile = useIsMobile();
  const isCompactHeight = useIsCompactHeight();

  const [expanded, setExpanded] = useState(false);
  const [selectedCardId, setSelectedCardId] = useState<number | null>(null);
  const [draggingCardId, setDraggingCardId] = useState<number | null>(null);

  const legalActionsByObject = useGameStore((s) => s.legalActionsByObject);

  // Hide the card being cast (shown on stack as preview during TargetSelection)
  const pendingObjectId = useGameStore((s) => {
    const wf = s.waitingFor;
    if (wf?.type === "TargetSelection") return wf.data.pending_cast.object_id;
    return null;
  });

  const canActForWaitingState = useCanActForWaitingState();
  const hasPriority = useGameStore((s) =>
    canActForWaitingState && s.waitingFor?.type === "Priority",
  );

  const playableObjectIds = useMemo(() => {
    return new Set(Object.keys(legalActionsByObject ?? {}).map(Number));
  }, [legalActionsByObject]);

  const playCard = useCallback(
    (objectId: number) => {
      if (!hasPriority || !objects) return;
      const obj = objects[objectId];
      if (!obj) return;

      const allActions = collectObjectActions(legalActionsByObject, objectId as ObjectId);

      if (allActions.length === 0) return;
      inspectObject(null);
      if (allActions.length === 1) {
        dispatchAction(allActions[0]);
      } else {
        // Multiple options (e.g., cast + Channel) — show choice modal
        setPendingAbilityChoice({ objectId: objectId as ObjectId, actions: allActions });
      }
    },
    [hasPriority, objects, legalActionsByObject, inspectObject, setPendingAbilityChoice],
  );

  const hoveredSlotRef = useRef<number | null>(null);

  const computeSlotFromX = useCallback(
    (clientX: number, draggingId: number): number | null => {
      const container = handContainerRef.current;
      if (!container) return null;
      const cards = Array.from(
        container.querySelectorAll<HTMLElement>("[data-card-hover]"),
      );
      return computeHandInsertionSlot(
        cards.map((el) => {
          const r = el.getBoundingClientRect();
          return {
            objectId: Number(el.dataset.objectId),
            left: r.left,
            width: r.width,
          };
        }),
        clientX,
        draggingId,
      );
    },
    [],
  );

  const handleDrag = useCallback(
    (objectId: number, info: PanInfo) => {
      const slot = computeSlotFromX(info.point.x, objectId);
      hoveredSlotRef.current = slot;
    },
    [computeSlotFromX],
  );

  // Drag-to-play applies the same gesture rule as `useDragToCast` (the
  // Commander-zone single-cast path): release above DRAG_PLAY_THRESHOLD
  // while holding priority and outside the source zone. A React hook cannot
  // be called once per hand card, so we inline the rule here but share the
  // threshold constant with `useDragToCast` — there is exactly one
  // definition of "how far up counts as a play."
  const handleDragEnd = useCallback(
    (objectId: number, _event: MouseEvent | TouchEvent | PointerEvent, info: PanInfo) => {
      const bounds = handContainerRef.current?.getBoundingClientRect();
      const releasedInsideHand =
        bounds != null
        && info.point.x >= bounds.left
        && info.point.x <= bounds.right
        && info.point.y >= bounds.top
        && info.point.y <= bounds.bottom;

      // Reorder branch: released inside the hand, a different slot is hovered.
      if (releasedInsideHand) {
        const targetSlot = hoveredSlotRef.current;
        hoveredSlotRef.current = null;
        // Reorder is disabled while a cast is in progress: handObjects filters
        // out `pendingObjectId`, so the DOM has N-1 slots but `player.hand`
        // has N entries. The slot index from `computeSlotFromX` would map to
        // the wrong position in the unfiltered hand.
        if (pendingObjectId != null) return false;
        if (targetSlot == null || !player) return false;
        const currentOrder = player.hand.slice();
        const fromIdx = currentOrder.indexOf(objectId as ObjectId);
        if (fromIdx === -1 || fromIdx === targetSlot) return false;
        const [moved] = currentOrder.splice(fromIdx, 1);
        currentOrder.splice(targetSlot, 0, moved);
        dispatchAction({ type: "ReorderHand", data: { order: currentOrder } });
        return false;
      }

      // Play branch (unchanged from the existing implementation).
      if (!hasPriority) return false;
      if (info.offset.y >= DRAG_PLAY_THRESHOLD) return false;
      playCard(objectId);
      return true;
    },
    [hasPriority, playCard, player, pendingObjectId],
  );

  const handleCardClick = useCallback(
    (objectId: number, e?: React.MouseEvent) => {
      if (useUiStore.getState().debugInteractionMode && e) {
        e.stopPropagation();
        useUiStore.getState().openDebugContextMenu({ objectId, x: e.clientX, y: e.clientY });
        return;
      }
      if (isMobile) {
        setMobileHandOpen(true);
        return;
      }
      if (!hasPriority) return;

      setSelectedCardId(objectId);
      inspectObject(objectId);
    },
    [isMobile, hasPriority, inspectObject, setMobileHandOpen],
  );

  const handleCardDoubleClick = useCallback(
    (objectId: number) => {
      if (useUiStore.getState().debugInteractionMode) return;
      if (!hasPriority) return;
      playCard(objectId);
      setSelectedCardId(null);
    },
    [hasPriority, playCard],
  );

  const handleContainerClick = useCallback(
    (e: React.MouseEvent) => {
      // Only handle clicks directly on the container (not bubbled from cards)
      if (e.target === e.currentTarget) {
        if (isMobile) {
          setMobileHandOpen(true);
        } else {
          setSelectedCardId(null);
          setExpanded((prev) => !prev);
        }
      }
    },
    [isMobile, setMobileHandOpen],
  );

  const handleDragStart = useCallback((id: number) => setDraggingCardId(id), []);
  const handleDragStop = useCallback(() => setDraggingCardId(null), []);
  const handleMouseEnter = useCallback((id: number) => { setExpanded(true); inspectObject(id); }, [inspectObject]);
  const handleMouseLeave = useCallback(() => inspectObject(null), [inspectObject]);

  if (!player || !objects) return null;

  const handObjects = player.hand
    .map((id) => objects[id])
    .filter((obj) => obj && obj.id !== pendingObjectId);

  const center = (handObjects.length - 1) / 2;

  return (
    <div
      ref={handContainerRef}
      className={`relative flex items-end justify-center overflow-visible px-4 py-1 ${
        isCompactHeight ? "min-h-[40px]" : "min-h-[calc(var(--card-h)*0.7)]"
      }`}
      style={{ perspective: "800px", zIndex: draggingCardId != null ? 30 : undefined }}
      onClick={handleContainerClick}
      onMouseLeave={() => {
        setExpanded(false);
        setSelectedCardId(null);
      }}
    >
      <AnimatePresence>
        {handObjects.map((obj, i) => {
          const rotation = (i - center) * 6;
          const isPlayable = hasPriority && playableObjectIds.has(Number(obj.id));

          return (
            <HandCard
              key={obj.id}
              objectId={obj.id}
              cardName={obj.name}
              manaCost={obj.mana_cost}
              unimplementedMechanics={obj.unimplemented_mechanics}
              index={i}
              handSize={handObjects.length}
              rotation={rotation}
              expanded={expanded}
              isPlayable={isPlayable}
              isSelected={selectedCardId === obj.id}
              hasPriority={hasPriority}
              isMobile={isMobile}
              onDragEnd={handleDragEnd}
              onDrag={handleDrag}
              onClick={handleCardClick}
              onDoubleClick={handleCardDoubleClick}
              isDragging={draggingCardId === obj.id}
              onDragStart={handleDragStart}
              onDragStop={handleDragStop}
              onMouseEnter={handleMouseEnter}
              onMouseLeave={handleMouseLeave}
            />
          );
        })}
      </AnimatePresence>
    </div>
  );
}

interface HandCardProps {
  objectId: number;
  cardName: string;
  manaCost: ManaCost;
  unimplementedMechanics?: string[];
  index: number;
  handSize: number;
  rotation: number;
  expanded: boolean;
  isPlayable: boolean;
  isSelected: boolean;
  isDragging: boolean;
  hasPriority: boolean;
  isMobile: boolean;
  onDragStart: (id: number) => void;
  onDragStop: () => void;
  onDragEnd: (objectId: number, event: MouseEvent | TouchEvent | PointerEvent, info: PanInfo) => boolean;
  onDrag: (objectId: number, info: PanInfo) => void;
  onClick: (objectId: number, e?: React.MouseEvent) => void;
  onDoubleClick: (objectId: number) => void;
  onMouseEnter: (id: number) => void;
  onMouseLeave: () => void;
}

const HandCard = memo(function HandCard({
  objectId,
  cardName,
  manaCost,
  unimplementedMechanics,
  index,
  handSize,
  rotation,
  expanded,
  isPlayable,
  isSelected,
  isDragging,
  hasPriority,
  isMobile,
  onDragStart: onDragStartProp,
  onDragStop,
  onDragEnd,
  onDrag,
  onClick,
  onDoubleClick,
  onMouseEnter,
  onMouseLeave,
}: HandCardProps) {
  const inspectObject = useUiStore((s) => s.inspectObject);
  const setDragging = useUiStore((s) => s.setDragging);

  // Use effective spell cost from engine if available (reflects reductions),
  // otherwise fall back to printed mana cost.
  const effectiveCost = useGameStore((s) => s.spellCosts[String(objectId)]);
  const displayCost = effectiveCost ?? manaCost;
  // Detect cost reduction by comparing effective vs printed generic mana
  const isReduced = effectiveCost?.type === "Cost" && manaCost.type === "Cost"
    && (effectiveCost.generic < manaCost.generic || effectiveCost.shards.length < manaCost.shards.length);
  const playedRef = useRef(false);

  const setPreviewSticky = useUiStore((s) => s.setPreviewSticky);
  const { handlers: longPressHandlers, firedRef: longPressFired } = useLongPress(() => {
    inspectObject(objectId);
    setPreviewSticky(true);
  });

  const glowClass = hasPriority
    ? isPlayable
      ? "shadow-[0_0_16px_4px_rgba(34,211,238,0.6)] ring-2 ring-cyan-400"
      : "opacity-60"
    : "";

  // Quadratic arc: cards further from center drop more, forming a natural parabola
  const distFromCenter = Math.abs(index - (handSize - 1) / 2);
  const arcOffset = distFromCenter * distFromCenter * 6;

  return (
    <motion.div
      data-card-hover
      data-object-id={objectId}
      layout
      initial={{ opacity: 0, y: 40 }}
      animate={{
        opacity: 1,
        y: (expanded ? -20 : 30) + arcOffset,
        rotate: rotation,
      }}
      exit={{ opacity: 0, scale: 0.8 }}
      whileHover={{ y: -30 + arcOffset, scale: 1.08, zIndex: 30 }}
      whileDrag={{ scale: 1.05, zIndex: 9999 }}
      transition={{
        delay: index * 0.03,
        duration: 0.25,
        layout: { duration: 0.15, delay: 0 },
      }}
      drag
      dragConstraints={false}
      dragElastic={0}
      dragSnapToOrigin={!playedRef.current}
      onDragStart={() => {
        playedRef.current = false;
        setDragging(true);
        inspectObject(null);
        onDragStartProp(objectId);
      }}
      onDrag={(_event, info) => onDrag(objectId, info)}
      onDragEnd={(event, info) => {
        setDragging(false);
        onDragStop();
        const didPlay = onDragEnd(objectId, event, info);
        if (didPlay) {
          playedRef.current = true;
        }
      }}
      onClick={(e) => {
        e.stopPropagation();
        if (longPressFired.current) { longPressFired.current = false; return; }
        onClick(objectId, e);
      }}
      onDoubleClick={(e) => {
        e.stopPropagation();
        onDoubleClick(objectId);
      }}
      onMouseEnter={() => onMouseEnter(objectId)}
      onMouseLeave={onMouseLeave}
      className={`relative cursor-pointer rounded-lg leading-[0] select-none ${glowClass} ${
        isSelected ? "ring-2 ring-cyan-400" : ""
      } ${isMobile ? "pointer-events-none" : ""}`}
      style={{
        marginLeft: index === 0 ? 0 : getHandOverlap(handSize),
        zIndex: isDragging ? 9999 : isSelected ? 20 : index,
      }}
      {...longPressHandlers}
    >
      <CardImage
        cardName={cardName}
        size="normal"
        unimplementedMechanics={unimplementedMechanics}
        className="!w-[calc(var(--card-w)*1.14)] !h-[calc(var(--card-h)*1.14)] sm:!w-[calc(var(--card-w)*1.34)] sm:!h-[calc(var(--card-h)*1.34)] md:!w-[calc(var(--card-w)*1.4)] md:!h-[calc(var(--card-h)*1.4)]"
      />
      <ManaCostPips cost={displayCost} isReduced={isReduced} className="absolute right-[4%] top-[2%]" />
    </motion.div>
  );
});
