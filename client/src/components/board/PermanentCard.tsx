import { motion } from "framer-motion";
import { useCallback } from "react";

import { CardImage } from "../card/CardImage.tsx";
import { useLongPress } from "../../hooks/useLongPress.ts";
import { useGameStore } from "../../stores/gameStore.ts";
import { useUiStore } from "../../stores/uiStore.ts";

interface PermanentCardProps {
  objectId: number;
}

const COUNTER_COLORS: Record<string, string> = {
  Plus1Plus1: "bg-green-600",
  Minus1Minus1: "bg-red-600",
  Loyalty: "bg-amber-600",
};

export function PermanentCard({ objectId }: PermanentCardProps) {
  const obj = useGameStore((s) => s.gameState?.objects[objectId]);
  const turnNumber = useGameStore((s) => s.gameState?.turn_number ?? 0);
  const selectedObjectId = useUiStore((s) => s.selectedObjectId);
  const targetingMode = useUiStore((s) => s.targetingMode);
  const selectedTargets = useUiStore((s) => s.selectedTargets);
  const selectObject = useUiStore((s) => s.selectObject);
  const addTarget = useUiStore((s) => s.addTarget);
  const hoverObject = useUiStore((s) => s.hoverObject);
  const inspectObject = useUiStore((s) => s.inspectObject);

  const longPressHandlers = useLongPress(
    useCallback(() => {
      inspectObject(objectId);
    }, [inspectObject, objectId]),
  );

  if (!obj) return null;

  const isCreature = obj.card_types.core_types.includes("Creature");
  const hasSummoningSickness =
    isCreature &&
    obj.entered_battlefield_turn === turnNumber &&
    !obj.keywords.some((k) => k.toLowerCase() === "haste");

  const isSelected = selectedObjectId === objectId;
  const isTarget = selectedTargets.includes(objectId);

  // Glow ring styles
  let glowClass = "";
  if (isTarget) {
    glowClass = "ring-2 ring-cyan-400 shadow-[0_0_10px_2px_rgba(34,211,238,0.5)]";
  } else if (isSelected) {
    glowClass = "ring-2 ring-white shadow-[0_0_8px_2px_rgba(255,255,255,0.6)]";
  }

  const sicknessFilter = hasSummoningSickness ? "saturate(50%)" : undefined;
  const sicknessGlow = hasSummoningSickness
    ? "0 0 6px 1px rgba(255,255,255,0.3)"
    : undefined;

  const counters = Object.entries(obj.counters);

  const handleClick = () => {
    if (targetingMode) {
      addTarget(objectId);
    } else {
      selectObject(isSelected ? null : objectId);
    }
  };

  return (
    <motion.div
      layoutId={`permanent-${objectId}`}
      className={`relative cursor-pointer rounded-lg ${glowClass}`}
      style={{
        filter: sicknessFilter,
        boxShadow: sicknessGlow,
      }}
      onClick={handleClick}
      onMouseEnter={() => hoverObject(objectId)}
      onMouseLeave={() => hoverObject(null)}
      {...longPressHandlers}
    >
      {/* Attachments rendered behind */}
      {obj.attachments.map((attachId, i) => (
        <div
          key={attachId}
          className="absolute left-0 top-0 z-0"
          style={{ transform: `translateY(${-(i + 1) * 10}px)` }}
        >
          <PermanentCard objectId={attachId} />
        </div>
      ))}

      {/* Main card */}
      <div className="relative z-10">
        <CardImage cardName={obj.name} tapped={obj.tapped} size="small" />
      </div>

      {/* Damage overlay */}
      {obj.damage_marked > 0 && (
        <div className="absolute inset-x-0 bottom-0 z-20 flex h-6 items-center justify-center rounded-b-lg bg-red-600/60 text-xs font-bold text-white">
          -{obj.damage_marked}
        </div>
      )}

      {/* Counter badges */}
      {counters.length > 0 && (
        <div className="absolute bottom-1 right-1 z-20 flex flex-col gap-0.5">
          {counters.map(([type, count]) => (
            <span
              key={type}
              className={`rounded px-1 text-[10px] font-bold text-white ${COUNTER_COLORS[type] ?? "bg-purple-600"}`}
            >
              {formatCounterType(type)} x{count}
            </span>
          ))}
        </div>
      )}
    </motion.div>
  );
}

function formatCounterType(type: string): string {
  if (type === "Plus1Plus1") return "+1/+1";
  if (type === "Minus1Minus1") return "-1/-1";
  return type;
}
