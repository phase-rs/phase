import { useState, useCallback } from "react";
import { AnimatePresence, motion } from "framer-motion";
import type { PanInfo } from "framer-motion";

import { CardImage } from "../card/CardImage.tsx";
import { useGameStore } from "../../stores/gameStore.ts";
import { useUiStore } from "../../stores/uiStore.ts";
import { useLongPress } from "../../hooks/useLongPress.ts";

const DRAG_PLAY_THRESHOLD = -50;

export function PlayerHand() {
  const player = useGameStore((s) => s.gameState?.players[0]);
  const objects = useGameStore((s) => s.gameState?.objects);
  const waitingFor = useGameStore((s) => s.waitingFor);
  const dispatch = useGameStore((s) => s.dispatch);
  const inspectObject = useUiStore((s) => s.inspectObject);

  const [expanded, setExpanded] = useState(false);
  const [selectedCardId, setSelectedCardId] = useState<number | null>(null);

  const hasPriority =
    waitingFor?.type === "Priority" && waitingFor.data.player === 0;

  const playCard = useCallback(
    (objectId: number) => {
      if (!hasPriority || !objects) return;
      const obj = objects[objectId];
      if (!obj) return;

      if (obj.card_types.core_types.includes("Land")) {
        dispatch({ type: "PlayLand", data: { card_id: obj.card_id } });
      } else {
        dispatch({ type: "CastSpell", data: { card_id: obj.card_id, targets: [] } });
      }
    },
    [hasPriority, objects, dispatch],
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
    .filter(Boolean);

  const center = (handObjects.length - 1) / 2;

  return (
    <div
      className="relative flex items-end justify-center px-4 py-1"
      onClick={handleContainerClick}
      onMouseEnter={() => setExpanded(true)}
      onMouseLeave={() => {
        setExpanded(false);
        setSelectedCardId(null);
      }}
    >
      <AnimatePresence>
        {handObjects.map((obj, i) => {
          const rotation = (i - center) * 3;
          const isPlayable = hasPriority;

          return (
            <HandCard
              key={obj.id}
              objectId={obj.id}
              cardName={obj.name}
              hasUnimplementedMechanics={obj.has_unimplemented_mechanics}
              index={i}
              rotation={rotation}
              expanded={expanded}
              isPlayable={isPlayable}
              isSelected={selectedCardId === obj.id}
              hasPriority={hasPriority}
              onDragEnd={handleDragEnd}
              onClick={handleCardClick}
              onMouseEnter={() => inspectObject(obj.id)}
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
  hasUnimplementedMechanics?: boolean;
  index: number;
  rotation: number;
  expanded: boolean;
  isPlayable: boolean;
  isSelected: boolean;
  hasPriority: boolean;
  onDragEnd: (objectId: number, event: MouseEvent | TouchEvent | PointerEvent, info: PanInfo) => void;
  onClick: (objectId: number) => void;
  onMouseEnter: () => void;
  onMouseLeave: () => void;
}

function HandCard({
  objectId,
  cardName,
  hasUnimplementedMechanics,
  index,
  rotation,
  expanded,
  isPlayable,
  isSelected,
  hasPriority,
  onDragEnd,
  onClick,
  onMouseEnter,
  onMouseLeave,
}: HandCardProps) {
  const inspectObject = useUiStore((s) => s.inspectObject);

  const longPressHandlers = useLongPress(() => {
    inspectObject(objectId);
  });

  const glowClass = hasPriority
    ? isPlayable
      ? "shadow-[0_0_12px_3px_rgba(34,197,94,0.7)]"
      : "opacity-70"
    : "";

  return (
    <motion.div
      layout
      initial={{ opacity: 0, y: 40 }}
      animate={{
        opacity: 1,
        y: expanded ? -20 : 30,
        rotate: rotation,
      }}
      exit={{ opacity: 0, y: 40 }}
      whileHover={{ y: -40, scale: 1.08, zIndex: 30 }}
      whileDrag={{ scale: 1.05, zIndex: 50 }}
      transition={{ duration: 0.2 }}
      drag="y"
      dragConstraints={{ top: -300, bottom: 0 }}
      dragElastic={0.3}
      dragSnapToOrigin
      onDragEnd={(event, info) => onDragEnd(objectId, event, info)}
      onClick={(e) => {
        e.stopPropagation();
        onClick(objectId);
      }}
      onMouseEnter={onMouseEnter}
      onMouseLeave={onMouseLeave}
      className={`relative cursor-pointer rounded-lg ${glowClass} ${
        isSelected ? "ring-2 ring-cyan-400" : ""
      }`}
      style={{
        marginLeft: index === 0 ? 0 : "-12px",
        zIndex: isSelected ? 20 : index,
      }}
      {...longPressHandlers}
    >
      <CardImage cardName={cardName} size="small" hasUnimplementedMechanics={hasUnimplementedMechanics} />
    </motion.div>
  );
}
