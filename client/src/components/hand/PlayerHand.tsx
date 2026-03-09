import { useState, useCallback, useMemo } from "react";
import { AnimatePresence, motion } from "framer-motion";
import type { PanInfo } from "framer-motion";

import { CardImage } from "../card/CardImage.tsx";
import { useGameStore } from "../../stores/gameStore.ts";
import { useUiStore } from "../../stores/uiStore.ts";
import { useLongPress } from "../../hooks/useLongPress.ts";
import { dispatchAction } from "../../game/dispatch.ts";
import type { GameAction } from "../../adapter/types.ts";

const DRAG_PLAY_THRESHOLD = -50;

export function PlayerHand() {
  const player = useGameStore((s) => s.gameState?.players[0]);
  const objects = useGameStore((s) => s.gameState?.objects);
  const waitingFor = useGameStore((s) => s.waitingFor);
  // Use dispatchAction (animation pipeline) instead of store dispatch
  const inspectObject = useUiStore((s) => s.inspectObject);

  const [expanded, setExpanded] = useState(false);
  const [selectedCardId, setSelectedCardId] = useState<number | null>(null);

  const legalActions = useGameStore((s) => s.legalActions);

  const hasPriority =
    waitingFor?.type === "Priority" && waitingFor.data.player === 0;

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

      if (obj.card_types.core_types.includes("Land")) {
        dispatchAction({ type: "PlayLand", data: { card_id: obj.card_id } });
      } else {
        dispatchAction({ type: "CastSpell", data: { card_id: obj.card_id, targets: [] } });
      }
    },
    [hasPriority, objects],
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
          const isPlayable = hasPriority && playableCardIds.has(Number(obj.card_id));

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
  const setDragging = useUiStore((s) => s.setDragging);

  const longPressHandlers = useLongPress(() => {
    inspectObject(objectId);
  });

  const glowClass = hasPriority
    ? isPlayable
      ? "shadow-[0_0_12px_3px_rgba(34,211,238,0.6)] ring-2 ring-cyan-400"
      : "opacity-60"
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
      drag
      dragConstraints={{ top: -300, bottom: 0, left: -200, right: 200 }}
      dragElastic={0.3}
      dragSnapToOrigin
      onDragStart={() => {
        setDragging(true);
        inspectObject(null);
      }}
      onDragEnd={(event, info) => {
        setDragging(false);
        onDragEnd(objectId, event, info);
      }}
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
