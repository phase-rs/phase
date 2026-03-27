import { memo, useState, useCallback, useMemo, useRef } from "react";
import { AnimatePresence, motion } from "framer-motion";
import type { PanInfo } from "framer-motion";

import { CardImage } from "../card/CardImage.tsx";
import { useGameStore } from "../../stores/gameStore.ts";
import { useUiStore } from "../../stores/uiStore.ts";
import { useLongPress } from "../../hooks/useLongPress.ts";
import { usePlayerId } from "../../hooks/usePlayerId.ts";
import { dispatchAction } from "../../game/dispatch.ts";
import type { GameAction, ObjectId } from "../../adapter/types.ts";

/** Cards are played when dragged above their starting position (any upward drag counts). */
const DRAG_PLAY_THRESHOLD = -20;

function getHandOverlap(handSize: number): string {
  if (handSize <= 5) return "calc(var(--card-w) * -0.25)";
  if (handSize <= 7) return "calc(var(--card-w) * -0.45)";
  return "calc(var(--card-w) * -0.6)";
}

export function PlayerHand() {
  const playerId = usePlayerId();
  const player = useGameStore((s) => s.gameState?.players[playerId]);
  const objects = useGameStore((s) => s.gameState?.objects);
  // Use dispatchAction (animation pipeline) instead of store dispatch
  const inspectObject = useUiStore((s) => s.inspectObject);
  const setPendingAbilityChoice = useUiStore((s) => s.setPendingAbilityChoice);

  const [expanded, setExpanded] = useState(false);
  const [selectedCardId, setSelectedCardId] = useState<number | null>(null);
  const [draggingCardId, setDraggingCardId] = useState<number | null>(null);

  const legalActions = useGameStore((s) => s.legalActions);

  // Hide the card being cast (shown on stack as preview during TargetSelection)
  const pendingObjectId = useGameStore((s) => {
    const wf = s.waitingFor;
    if (wf?.type === "TargetSelection") return wf.data.pending_cast.object_id;
    return null;
  });

  const hasPriority = useGameStore((s) =>
    s.waitingFor?.type === "Priority" && s.waitingFor.data.player === playerId,
  );

  // Build a set of object_ids that have PlayLand or CastSpell legal actions.
  // Coerce to Number since serde_wasm_bindgen may serialize u64 as BigInt.
  const playableObjectIds = useMemo(() => {
    const ids = new Set<number>();
    for (const action of legalActions) {
      if (action.type === "PlayLand" || action.type === "CastSpell") {
        ids.add(Number((action as Extract<GameAction, { type: "PlayLand" | "CastSpell" }>).data.object_id));
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

      // Find cast/play action by object_id
      const castAction = legalActions.find(
        (a) =>
          (a.type === "PlayLand" || a.type === "CastSpell") &&
          Number((a as Extract<GameAction, { type: "PlayLand" | "CastSpell" }>).data.object_id) === objectId,
      );
      // Find hand-activated ability actions by object_id (Channel, etc.)
      const abilityActions = legalActions.filter(
        (a) => a.type === "ActivateAbility" && Number(a.data.source_id) === objectId,
      );

      const allActions: GameAction[] = [];
      if (castAction) allActions.push(castAction);
      allActions.push(...abilityActions);

      if (allActions.length === 0) return;
      inspectObject(null);
      if (allActions.length === 1) {
        dispatchAction(allActions[0]);
      } else {
        // Multiple options (e.g., cast + Channel) — show choice modal
        setPendingAbilityChoice({ objectId: objectId as ObjectId, actions: allActions });
      }
    },
    [hasPriority, objects, legalActions, inspectObject, setPendingAbilityChoice],
  );

  const handleDragEnd = useCallback(
    (objectId: number, _event: MouseEvent | TouchEvent | PointerEvent, info: PanInfo) => {
      if (info.offset.y < DRAG_PLAY_THRESHOLD && hasPriority) {
        playCard(objectId);
      }
    },
    [hasPriority, playCard],
  );

  const handleCardClick = useCallback(
    (objectId: number) => {
      if (!hasPriority) return;

      // Single click: select and inspect only
      setSelectedCardId(objectId);
      inspectObject(objectId);
    },
    [hasPriority, inspectObject],
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
        setSelectedCardId(null);
        setExpanded((prev) => !prev);
      }
    },
    [],
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
      className="relative flex min-h-[calc(var(--card-h)*1.4)] shrink-0 items-end justify-center overflow-visible px-4 py-1"
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
          const isPlayable = hasPriority && (playableObjectIds.has(Number(obj.id)) || activatableObjectIds.has(Number(obj.id)));

          return (
            <HandCard
              key={obj.id}
              objectId={obj.id}
              cardName={obj.name}
              unimplementedMechanics={obj.unimplemented_mechanics}
              index={i}
              handSize={handObjects.length}
              rotation={rotation}
              expanded={expanded}
              isPlayable={isPlayable}
              isSelected={selectedCardId === obj.id}
              hasPriority={hasPriority}
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
  unimplementedMechanics?: string[];
  index: number;
  handSize: number;
  rotation: number;
  expanded: boolean;
  isPlayable: boolean;
  isSelected: boolean;
  isDragging: boolean;
  hasPriority: boolean;
  onDragStart: (id: number) => void;
  onDragStop: () => void;
  onDragEnd: (objectId: number, event: MouseEvent | TouchEvent | PointerEvent, info: PanInfo) => void;
  onClick: (objectId: number) => void;
  onDoubleClick: (objectId: number) => void;
  onMouseEnter: (id: number) => void;
  onMouseLeave: () => void;
}

const HandCard = memo(function HandCard({
  objectId,
  cardName,
  unimplementedMechanics,
  index,
  handSize,
  rotation,
  expanded,
  isPlayable,
  isSelected,
  isDragging,
  hasPriority,
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
      layout
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
        const willPlay = info.offset.y < DRAG_PLAY_THRESHOLD && hasPriority && isPlayable;
        if (willPlay) {
          playedRef.current = true;
        }
        onDragEnd(objectId, event, info);
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
      }`}
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
    </motion.div>
  );
});
