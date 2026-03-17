import { useState, useCallback, useMemo, useRef } from "react";
import { AnimatePresence, motion } from "framer-motion";
import type { PanInfo } from "framer-motion";

import { CardImage } from "../card/CardImage.tsx";
import { useGameStore } from "../../stores/gameStore.ts";
import { useUiStore } from "../../stores/uiStore.ts";
import { useLongPress } from "../../hooks/useLongPress.ts";
import { usePlayerId } from "../../hooks/usePlayerId.ts";
import { dispatchAction } from "../../game/dispatch.ts";
import type { GameAction } from "../../adapter/types.ts";

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
  const waitingFor = useGameStore((s) => s.waitingFor);
  // Use dispatchAction (animation pipeline) instead of store dispatch
  const inspectObject = useUiStore((s) => s.inspectObject);

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

  const hasPriority =
    waitingFor?.type === "Priority" && waitingFor.data.player === playerId;

  // Build a set of card_ids that have PlayLand or CastSpell legal actions.
  // Coerce to Number since serde_wasm_bindgen may serialize u64 as BigInt.
  const playableCardIds = useMemo(() => {
    const ids = new Set<number>();
    for (const action of legalActions) {
      if (action.type === "PlayLand" || action.type === "CastSpell") {
        const cardId = (action as Extract<GameAction, { type: "PlayLand" | "CastSpell" }>).data.card_id;
        ids.add(Number(cardId));
      }
    }
    return ids;
  }, [legalActions]);

  const playCard = useCallback(
    (objectId: number) => {
      if (!hasPriority || !objects) return;
      const obj = objects[objectId];
      if (!obj) return;

      // Find the matching legal action from the engine — no frontend rule logic needed
      const action = legalActions.find(
        (a) =>
          (a.type === "PlayLand" || a.type === "CastSpell") &&
          Number((a as Extract<GameAction, { type: "PlayLand" | "CastSpell" }>).data.card_id) === Number(obj.card_id),
      );
      if (action) {
        inspectObject(null);
        dispatchAction(action);
      }
    },
    [hasPriority, objects, legalActions, inspectObject],
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

      // Touch flow: tap to select, tap again to play
      if (selectedCardId === objectId) {
        playCard(objectId);
        setSelectedCardId(null);
      } else {
        setSelectedCardId(objectId);
        inspectObject(objectId);
      }
    },
    [hasPriority, selectedCardId, playCard, inspectObject],
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

  if (!player || !objects) return null;

  const handObjects = player.hand
    .map((id) => objects[id])
    .filter((obj) => obj && obj.id !== pendingObjectId);

  const center = (handObjects.length - 1) / 2;

  return (
    <div
      className="relative flex min-h-[calc(var(--card-h)*0.85)] shrink-0 items-end justify-center px-4 py-1"
      style={{ perspective: "800px" }}
      onClick={handleContainerClick}
      onMouseLeave={() => {
        setExpanded(false);
        setSelectedCardId(null);
      }}
    >
      <AnimatePresence>
        {handObjects.map((obj, i) => {
          const rotation = (i - center) * 6;
          const isPlayable = hasPriority && playableCardIds.has(Number(obj.card_id));

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
              isDragging={draggingCardId === obj.id}
              onDragStart={() => setDraggingCardId(obj.id)}
              onDragStop={() => setDraggingCardId(null)}
              onMouseEnter={() => { setExpanded(true); inspectObject(obj.id); }}
              onMouseLeave={() => inspectObject(null)}
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
  onDragStart: () => void;
  onDragStop: () => void;
  onDragEnd: (objectId: number, event: MouseEvent | TouchEvent | PointerEvent, info: PanInfo) => void;
  onClick: (objectId: number) => void;
  onMouseEnter: () => void;
  onMouseLeave: () => void;
}

function HandCard({
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
  onMouseEnter,
  onMouseLeave,
}: HandCardProps) {
  const inspectObject = useUiStore((s) => s.inspectObject);
  const setDragging = useUiStore((s) => s.setDragging);
  const playedRef = useRef(false);

  const longPressHandlers = useLongPress(() => {
    inspectObject(objectId);
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
      initial={{ opacity: 0, y: 40 }}
      animate={{
        opacity: 1,
        y: (expanded ? -20 : 30) + arcOffset,
        rotate: rotation,
      }}
      exit={{ opacity: 0, scale: 0.8 }}
      whileHover={{ y: -30 + arcOffset, scale: 1.08, zIndex: 30 }}
      whileDrag={{ scale: 1.05, zIndex: 50 }}
      transition={{ delay: index * 0.03, duration: 0.25 }}
      drag
      dragConstraints={false}
      dragElastic={0}
      dragSnapToOrigin={!playedRef.current}
      onDragStart={() => {
        playedRef.current = false;
        setDragging(true);
        inspectObject(null);
        onDragStartProp();
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
        onClick(objectId);
      }}
      onMouseEnter={onMouseEnter}
      onMouseLeave={onMouseLeave}
      className={`relative cursor-pointer rounded-lg leading-[0] ${glowClass} ${
        isSelected ? "ring-2 ring-cyan-400" : ""
      }`}
      style={{
        marginLeft: index === 0 ? 0 : getHandOverlap(handSize),
        zIndex: isDragging ? 50 : isSelected ? 20 : index,
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
}
