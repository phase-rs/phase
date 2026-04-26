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
import type { GameAction, ManaCost, ObjectId } from "../../adapter/types.ts";
import { collectObjectActions } from "../../viewmodel/cardActionChoice.ts";
import { DRAG_PLAY_THRESHOLD } from "../../hooks/useDragToCast.ts";

function getHandOverlap(handSize: number): string {
  if (handSize <= 5) return "calc(var(--card-w) * -0.25)";
  if (handSize <= 7) return "calc(var(--card-w) * -0.45)";
  return "calc(var(--card-w) * -0.6)";
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

  const legalActions = useGameStore((s) => s.legalActions);
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

  // Build a set of object_ids that have PlayLand, CastSpell, or
  // CastSpellAsSneak legal actions. Sneak casts (CR 702.190a) originate from
  // the hand via `hand_object` rather than `object_id`, so they need an
  // explicit branch to light up the card's castable indicator.
  // Coerce to Number since serde_wasm_bindgen may serialize u64 as BigInt.
  const playableObjectIds = useMemo(() => {
    const ids = new Set<number>();
    for (const action of legalActions) {
      if (action.type === "PlayLand" || action.type === "CastSpell") {
        ids.add(Number((action as Extract<GameAction, { type: "PlayLand" | "CastSpell" }>).data.object_id));
      } else if (action.type === "CastSpellAsSneak") {
        ids.add(Number(action.data.hand_object));
      }
    }
    return ids;
  }, [legalActions]);

  // Build a set of object_ids that have ActivateAbility legal actions (Channel, etc.)
  const activatableObjectIds = useMemo(() => {
    const ids = new Set<number>();
    for (const action of legalActions) {
      if (action.type === "ActivateAbility") {
        ids.add(Number(action.data.source_id));
      }
    }
    return ids;
  }, [legalActions]);

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

  // Drag-to-play applies the same gesture rule as `useDragToCast` (the
  // Commander-zone single-cast path): release above DRAG_PLAY_THRESHOLD
  // while holding priority and outside the source zone. A React hook cannot
  // be called once per hand card, so we inline the rule here but share the
  // threshold constant with `useDragToCast` — there is exactly one
  // definition of "how far up counts as a play."
  const handleDragEnd = useCallback(
    (objectId: number, _event: MouseEvent | TouchEvent | PointerEvent, info: PanInfo) => {
      if (!hasPriority) return false;
      const bounds = handContainerRef.current?.getBoundingClientRect();
      const releasedInsideHand =
        bounds != null
        && info.point.x >= bounds.left
        && info.point.x <= bounds.right
        && info.point.y >= bounds.top
        && info.point.y <= bounds.bottom;
      if (releasedInsideHand) return false;
      if (info.offset.y >= DRAG_PLAY_THRESHOLD) return false;
      playCard(objectId);
      return true;
    },
    [hasPriority, playCard],
  );

  const handleCardClick = useCallback(
    (objectId: number) => {
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
      className={`relative flex shrink-0 items-end justify-center overflow-visible px-4 py-1 ${
        isCompactHeight ? "min-h-[40px]" : "min-h-[calc(var(--card-h)*1.4)]"
      }`}
      style={{ perspective: "800px", zIndex: draggingCardId != null ? 30 : undefined }}
      onClick={handleContainerClick}
      onMouseLeave={() => {
        setExpanded(false);
        setSelectedCardId(null);
      }}
    >
      {handObjects.length > 0 && (
        <button
          aria-label={`View full hand (${handObjects.length} cards)`}
          className="absolute right-2 top-1 z-20 flex items-center gap-1.5 rounded-full border border-white/20 bg-slate-900/70 px-3 py-1 text-xs font-semibold text-white shadow-lg shadow-black/40 ring-1 ring-cyan-400/40 backdrop-blur-md transition hover:border-cyan-300/60 hover:bg-slate-800/80 hover:ring-cyan-300/70 active:scale-95"
          onClick={(e) => { e.stopPropagation(); setMobileHandOpen(true); }}
        >
          <svg
            aria-hidden="true"
            viewBox="0 0 24 24"
            className="h-3.5 w-3.5 text-cyan-300"
            fill="none"
            stroke="currentColor"
            strokeWidth="2"
            strokeLinecap="round"
            strokeLinejoin="round"
          >
            <rect x="4" y="3" width="11" height="16" rx="2" />
            <path d="M9 7l11 3-3 11-7-2" />
          </svg>
          <span>Hand</span>
          <span className="tabular-nums text-cyan-300">{handObjects.length}</span>
        </button>
      )}
      <AnimatePresence>
        {handObjects.map((obj, i) => {
          const rotation = (i - center) * 6;
          const isPlayable = hasPriority && (playableObjectIds.has(Number(obj.id)) || activatableObjectIds.has(Number(obj.id)));

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
  onClick: (objectId: number) => void;
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
      initial={{ opacity: 0, y: 40 }}
      animate={{
        opacity: 1,
        y: (expanded ? -20 : 30) + arcOffset,
        rotate: rotation,
      }}
      exit={{ opacity: 0, scale: 0.8 }}
      whileHover={{ y: -30 + arcOffset, scale: 1.08, zIndex: 30 }}
      whileDrag={{ scale: 1.05, zIndex: 9999 }}
      transition={{ delay: index * 0.03, duration: 0.25 }}
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
        onClick(objectId);
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
