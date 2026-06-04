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
import {
  collectObjectActions,
  resolveSingleActionDispatch,
} from "../../viewmodel/cardActionChoice.ts";
import { DRAG_PLAY_THRESHOLD } from "../../hooks/useDragToCast.ts";
import { computeHandInsertionSlot } from "./handInsertionSlot.ts";

// Horizontal overlap between adjacent hand cards. Negative margin pulls each
// card leftward over the previous one. Tightens continuously as the hand grows
// so a Commander-sized hand (up to ~20 cards) still fits on screen.
function getHandOverlap(handSize: number): string {
  if (handSize <= 5) return "calc(var(--card-w) * -0.25)";
  if (handSize <= 7) return "calc(var(--card-w) * -0.45)";
  // For 8+ cards: target total width ≈ 4× card width.
  // First card occupies 1w; remaining (n-1) each contribute (1 + overlap)w.
  // (n-1)(1 + overlap) = 3  =>  overlap = 3/(n-1) - 1, clamped to [-0.85, -0.6].
  const overlap = Math.max(-0.85, Math.min(-0.6, 3 / (handSize - 1) - 1));
  return `calc(var(--card-w) * ${overlap})`;
}

// Per-card rotation in degrees. Total fan span is clamped to ±18° regardless of
// hand size, so the bigger the hand the more upright each card sits.
function getCardRotation(index: number, handSize: number): number {
  if (handSize <= 1) return 0;
  const center = (handSize - 1) / 2;
  // Cap per-card delta at 6° (preserves look for small hands), otherwise
  // distribute a 36° total fan across the hand.
  const delta = Math.min(6, 36 / (handSize - 1));
  return (index - center) * delta;
}

// Quadratic arc lift coefficient. Scales down as the hand grows so the parabola
// stays inside the hand band instead of pushing edge cards off-screen.
function getArcCoefficient(handSize: number): number {
  if (handSize <= 7) return 6;
  // Keep max arc lift (at the edges) roughly constant at ~54px.
  const maxDist = (handSize - 1) / 2;
  return 54 / (maxDist * maxDist);
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
      // #506: a lone card-consuming action (cycling / Channel — its cost
      // discards the card, CR 702.29a) must surface the choice modal so the
      // player explicitly opts in. resolveSingleActionDispatch is the single
      // decision authority.
      const auto = resolveSingleActionDispatch(allActions, obj);
      if (auto) {
        dispatchAction(auto);
      } else {
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
      // On mobile the fanned cards are `pointer-events-none` (the drawer is the
      // interaction surface), so every tap in the hand area falls through to this
      // container — or to the inner lift wrapper, which bubbles here. Any such tap
      // opens the full-hand drawer. This MUST run before the target===currentTarget
      // guard below: the lift wrapper makes `e.target` the wrapper rather than the
      // container, so the guard alone would swallow taps that land over a card.
      if (isMobile) {
        setMobileHandOpen(true);
        return;
      }
      // Desktop: only a click on the empty container area (card clicks stop
      // propagation) toggles the hand lift.
      if (e.target === e.currentTarget) {
        setSelectedCardId(null);
        setExpanded((prev) => !prev);
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
      {/* The whole hand lifts as one unit on hover. Keeping this uniform -50px
          lift on a container — rather than baking `expanded` into each card's
          animate target — lets the memoized HandCards skip re-rendering when the
          hand expands/collapses. The lift lives on an inner wrapper so the outer
          container (which owns onMouseLeave) stays put and its collapse hit-area
          doesn't move under the cursor. */}
      <motion.div
        className="flex items-end justify-center"
        animate={{ y: expanded ? -50 : 0 }}
        transition={{ duration: 0.25 }}
      >
        <AnimatePresence>
          {handObjects.map((obj, i) => {
          const rotation = getCardRotation(i, handObjects.length);
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
      </motion.div>
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

  // Quadratic arc: cards further from center drop more, forming a natural parabola.
  // Coefficient scales down with hand size so edge cards don't fly off-screen.
  const distFromCenter = Math.abs(index - (handSize - 1) / 2);
  const arcOffset = distFromCenter * distFromCenter * getArcCoefficient(handSize);

  return (
    <motion.div
      data-card-hover
      data-object-id={objectId}
      layout
      initial={{ opacity: 0, y: 40 }}
      animate={{
        opacity: 1,
        y: 30 + arcOffset,
        rotate: rotation,
      }}
      exit={{ opacity: 0, scale: 0.8 }}
      whileHover={{ y: 20 + arcOffset, scale: 1.08, zIndex: 30 }}
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
