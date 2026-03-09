import { motion } from "framer-motion";
import { useCallback } from "react";

import { CardImage } from "../card/CardImage.tsx";
import { PTBox } from "./PTBox.tsx";
import { COMBAT_TILT_DEGREES } from "../../constants/ui.ts";
import { useLongPress } from "../../hooks/useLongPress.ts";
import { useGameStore } from "../../stores/gameStore.ts";
import { useUiStore } from "../../stores/uiStore.ts";
import { computePTDisplay } from "../../viewmodel/cardProps.ts";

interface PermanentCardProps {
  objectId: number;
}

const COUNTER_COLORS: Record<string, string> = {
  Plus1Plus1: "bg-green-600",
  Minus1Minus1: "bg-red-600",
  Loyalty: "bg-amber-600",
};

const ATTACHMENT_OFFSET_PX = 15;

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
  const combatMode = useUiStore((s) => s.combatMode);
  const selectedAttackers = useUiStore((s) => s.selectedAttackers);
  const toggleAttacker = useUiStore((s) => s.toggleAttacker);
  const blockerAssignments = useUiStore((s) => s.blockerAssignments);
  const combatClickHandler = useUiStore((s) => s.combatClickHandler);
  const validTargetIds = useUiStore((s) => s.validTargetIds);

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

  const ptDisplay = computePTDisplay(obj);
  const isSelected = selectedObjectId === objectId;
  const isTarget = selectedTargets.includes(objectId);
  const isValidTarget = targetingMode && validTargetIds.includes(objectId);

  // Combat state
  const isAttacking =
    combatMode === "attackers" && selectedAttackers.includes(objectId);
  const isBlocking =
    combatMode === "blockers" && blockerAssignments.has(objectId);

  // Glow ring styles (combat takes priority)
  let glowClass = "";
  if (isAttacking) {
    glowClass =
      "ring-2 ring-orange-500 shadow-[0_0_12px_3px_rgba(249,115,22,0.7)]";
  } else if (isBlocking) {
    glowClass =
      "ring-2 ring-blue-400 shadow-[0_0_12px_3px_rgba(96,165,250,0.7)]";
  } else if (isTarget) {
    glowClass =
      "ring-2 ring-cyan-400 shadow-[0_0_10px_2px_rgba(34,211,238,0.5)]";
  } else if (isValidTarget) {
    glowClass =
      "ring-2 ring-cyan-400/60 shadow-[0_0_12px_3px_rgba(0,229,255,0.8)]";
  } else if (isSelected) {
    glowClass =
      "ring-2 ring-white shadow-[0_0_8px_2px_rgba(255,255,255,0.6)]";
  }

  const sicknessFilter = hasSummoningSickness ? "saturate(50%)" : undefined;
  const sicknessGlow = hasSummoningSickness
    ? "0 0 6px 1px rgba(255,255,255,0.3)"
    : undefined;

  const counters = Object.entries(obj.counters);

  const handleClick = () => {
    if (combatMode === "attackers") {
      toggleAttacker(objectId);
    } else if (combatMode === "blockers" && combatClickHandler) {
      combatClickHandler(objectId);
    } else if (targetingMode) {
      addTarget(objectId);
    } else {
      selectObject(isSelected ? null : objectId);
    }
  };

  return (
    <motion.div
      data-object-id={objectId}
      layoutId={`permanent-${objectId}`}
      className={`relative cursor-pointer rounded-lg ${glowClass}`}
      style={{
        filter: sicknessFilter,
        boxShadow: sicknessGlow,
        // Reserve space above for tucked attachments
        marginTop:
          obj.attachments.length > 0
            ? `${obj.attachments.length * ATTACHMENT_OFFSET_PX}px`
            : undefined,
      }}
      animate={{ rotate: isAttacking ? COMBAT_TILT_DEGREES : 0 }}
      transition={{ type: "spring", stiffness: 300, damping: 20 }}
      onClick={handleClick}
      onMouseEnter={() => hoverObject(objectId)}
      onMouseLeave={() => hoverObject(null)}
      {...longPressHandlers}
    >
      {/* Attachments rendered behind, tucked with top edge visible */}
      {obj.attachments.map((attachId, i) => (
        <div
          key={attachId}
          className="absolute left-0 z-0"
          style={{
            top: `${-(i + 1) * ATTACHMENT_OFFSET_PX}px`,
          }}
        >
          <PermanentCard objectId={attachId} />
        </div>
      ))}

      {/* Main card */}
      <div className="relative z-10">
        <CardImage cardName={obj.name} tapped={obj.tapped} size="small" />
      </div>

      {/* P/T box for creatures */}
      {ptDisplay && <PTBox ptDisplay={ptDisplay} />}

      {/* Damage overlay for non-creatures only (creatures use P/T box) */}
      {!ptDisplay && obj.damage_marked > 0 && (
        <div className="absolute inset-x-0 bottom-0 z-20 flex h-6 items-center justify-center rounded-b-lg bg-red-600/60 text-xs font-bold text-white">
          -{obj.damage_marked}
        </div>
      )}

      {/* Loyalty shield for planeswalkers */}
      {obj.loyalty != null && (
        <div className="absolute bottom-0 left-1/2 z-20 -translate-x-1/2 rounded-t bg-gray-900/90 px-1.5 py-0.5 text-xs font-bold text-amber-300">
          {obj.loyalty}
        </div>
      )}

      {/* Counter badges (top-right to avoid overlap with P/T box) */}
      {counters.length > 0 && (
        <div className="absolute right-1 top-1 z-20 flex flex-col gap-0.5">
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
