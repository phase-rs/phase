import type { CSSProperties } from "react";

import { motion } from "framer-motion";

import { useCardImage } from "../../hooks/useCardImage.ts";
import { useLongPress } from "../../hooks/useLongPress.ts";
import { usePlayerId } from "../../hooks/usePlayerId.ts";
import { useSeatColor } from "../../hooks/useSeatColor.ts";
import { dispatchAction } from "../../game/dispatch.ts";
import { useGameStore } from "../../stores/gameStore.ts";
import { useUiStore } from "../../stores/uiStore.ts";
import { renderDescription } from "../../utils/description.ts";
import { ManaCostPips } from "../mana/ManaCostPips.tsx";
import type { StackEntry as StackEntryType } from "../../adapter/types.ts";

interface StackEntryProps {
  entry: StackEntryType;
  index: number;
  isTop: boolean;
  isPending?: boolean;
  cardSize: { width: number; height: number };
  style?: CSSProperties;
  onHoverChange?: (hovered: boolean) => void;
  /**
   * Pacing multiplier for the stagger delay, sourced from the engine's
   * StackPressure (see utils/stackPressure.ts). 1.0 = Normal, 0 = Instant
   * (mount animation skipped). Defaults to 1.0 so callers that haven't
   * plumbed pressure keep the prior behavior.
   */
  pacingMultiplier?: number;
  /**
   * Engine-authored coalesce count (from `stack_display_groups`). When > 1,
   * renders a ×N badge on the representative card. Defaults to 1 so callers
   * that don't proxy group data keep the prior per-entry rendering.
   */
  groupCount?: number;
}

export function StackEntry({ entry, index, isTop, isPending, cardSize, style, onHoverChange, pacingMultiplier = 1, groupCount = 1 }: StackEntryProps) {
  const playerId = usePlayerId();
  const objects = useGameStore((s) => s.gameState?.objects);
  const waitingFor = useGameStore((s) => s.gameState?.waiting_for);
  const inspectObject = useUiStore((s) => s.inspectObject);

  const setPreviewSticky = useUiStore((s) => s.setPreviewSticky);
  const { handlers: longPressHandlers, firedRef: longPressFired } = useLongPress(() => {
    inspectObject(entry.source_id);
    setPreviewSticky(true);
  });

  const sourceObj = objects?.[entry.source_id];
  const sourceName = sourceObj?.name ?? "Unknown";

  const { src, isLoading } = useCardImage(sourceName, { size: "normal" });

  const isSpell = entry.kind.type === "Spell";
  const abilityLabel =
    entry.kind.type === "ActivatedAbility" ? "Activated" : "Triggered";
  const triggerDescription =
    entry.kind.type === "TriggeredAbility"
      ? entry.kind.data.description && renderDescription(entry.kind.data.description, sourceName)
      : undefined;
  const controllerLabel = entry.controller === playerId ? "You" : "Opp";
  const seatColor = useSeatColor(entry.controller);
  const controllerInitial =
    entry.controller === playerId ? "Y" : `P${entry.controller}`;

  // Targeting: check if this stack entry is a valid target for the current selection
  const isHumanTargetSelection =
    (waitingFor?.type === "TargetSelection" || waitingFor?.type === "TriggerTargetSelection")
    && waitingFor.data.player === playerId;
  const currentTargetRefs = isHumanTargetSelection
    ? waitingFor.data.selection.current_legal_targets
    : [];
  const isValidTarget = isHumanTargetSelection && currentTargetRefs.some(
    (target) => "Object" in target && target.Object === entry.id,
  );

  // Ring style: targeting glow overrides default ring
  const ringClass = isValidTarget
    ? "ring-2 ring-amber-400/60 shadow-[0_0_12px_3px_rgba(201,176,55,0.8)]"
    : "ring-1 ring-white/10";

  const handleClick = () => {
    if (longPressFired.current) { longPressFired.current = false; return; }
    if (isValidTarget) {
      dispatchAction({ type: "ChooseTarget", data: { target: { Object: entry.id } } });
    } else {
      inspectObject(entry.source_id);
    }
  };

  return (
    <motion.div
      layout
      initial={{ opacity: 0, x: 30, scale: 0.9 }}
      animate={{ opacity: 1, x: 0, scale: 1 }}
      exit={{ opacity: 0, x: 30, scale: 0.9 }}
      transition={{
        delay: index * 0.03 * pacingMultiplier,
        duration: pacingMultiplier === 0 ? 0 : undefined,
      }}
      style={style}
      data-stack-entry={entry.id}
      data-card-hover
      className="relative cursor-pointer"
      onClick={handleClick}
      onMouseEnter={() => {
        inspectObject(entry.source_id);
        onHoverChange?.(true);
      }}
      onMouseLeave={() => {
        inspectObject(null);
        onHoverChange?.(false);
      }}
      {...longPressHandlers}
    >
      {/* Seat-color left-edge bar — identifies controller at a glance in multiplayer. */}
      <div
        className="pointer-events-none absolute inset-y-0 left-0 z-[1] w-[3px] rounded-l-lg"
        style={{ backgroundColor: seatColor }}
      />
      {/* Card image with explicit inline dimensions (Tailwind can't handle dynamic values) */}
      <div
        style={{ width: cardSize.width, height: cardSize.height }}
        className={`overflow-hidden rounded-lg shadow-lg ${ringClass}`}
      >
        {isLoading || !src ? (
          <div
            className="animate-pulse rounded-lg bg-gray-700 border border-gray-600"
            style={{ width: cardSize.width, height: cardSize.height }}
          />
        ) : (
          <img
            src={src}
            alt={sourceName}
            className="h-full w-full object-cover"
            draggable={false}
          />
        )}
        {isSpell && sourceObj?.mana_cost && (
          <ManaCostPips cost={sourceObj.mana_cost} size="sm" className="absolute right-[5%] top-[2.5%]" />
        )}
      </div>

      {/* Badge: ×N coalesce count for engine-grouped mass triggers. */}
      {groupCount > 1 && (
        <span className="absolute -left-2 -top-2 rounded-full bg-purple-600 px-2 py-0.5 text-[11px] font-bold text-white shadow-md">
          ×{groupCount}
        </span>
      )}

      {/* Badge: "Casting..." for pending spells, "Next" for top of stack */}
      {isPending ? (
        <span className="absolute -right-1 -top-2 animate-pulse rounded-full bg-cyan-500 px-2 py-0.5 text-[10px] font-bold text-black shadow-md">
          Casting…
        </span>
      ) : isTop && (
        <span className="absolute -right-1 -top-2 rounded-full bg-amber-500 px-2 py-0.5 text-[10px] font-bold text-black shadow-md">
          Next
        </span>
      )}

      {/* Ability badge overlay (non-spell entries: triggered/activated) */}
      {!isSpell && (
        <div className="absolute inset-x-0 bottom-0 rounded-b-lg border-t border-white/10 bg-gray-900/95 px-1.5 py-1 backdrop-blur-sm">
          <div className="pr-8 text-[9px] font-semibold text-purple-300">{abilityLabel}</div>
          {triggerDescription && (
            <div className="mt-0.5 line-clamp-3 pr-6 text-[8px] leading-tight text-gray-300">
              {triggerDescription}
            </div>
          )}
        </div>
      )}

      {/* Controller seat avatar — colored initial anchors identity to every surface
          where this player appears (stack, HUD, log). */}
      <span
        title={controllerLabel}
        className={`absolute flex h-4 min-w-4 items-center justify-center rounded-full border border-black/30 px-[3px] text-[9px] font-bold text-black shadow ${
          isSpell ? "bottom-1 left-1" : "bottom-1 right-1"
        }`}
        style={{ backgroundColor: seatColor }}
      >
        {controllerInitial}
      </span>
    </motion.div>
  );
}
