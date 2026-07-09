import { useMemo, useRef } from "react";
import { motion } from "framer-motion";
import type { PanInfo } from "framer-motion";
import { useTranslation } from "react-i18next";

import type { GameAction, GameObject, PlayerId } from "../../adapter/types.ts";
import { dispatchAction } from "../../game/dispatch.ts";
import { useCardHover } from "../../hooks/useCardHover.ts";
import { useCardImage } from "../../hooks/useCardImage.ts";
import { useIsCompactHeight } from "../../hooks/useIsCompactHeight.ts";
import { useDragToCast } from "../../hooks/useDragToCast.ts";
import { useGameStore } from "../../stores/gameStore.ts";
import { useUiStore } from "../../stores/uiStore.ts";
import { CASTABLE_AFFORDANCE_ACTIVE } from "../../viewmodel/castableAffordance.ts";
import { commandersInZone } from "../../viewmodel/commanderColumn.ts";
import { ManaCostPips } from "../mana/ManaCostPips.tsx";

interface CommanderCardZoneProps {
  playerId: PlayerId;
  /** Split multiplayer overview pane: the card is ~40px wide, so the centered
   *  "Commander" wordmark spans the whole card and hides the cost pips. Drop
   *  the wordmark (the amber frame + dock position + tooltip still mark the
   *  commander) and shrink the pips so the cost reads instead. */
  splitOverview?: boolean;
}

/**
 * Renders commander cards in the command zone as full card images in the
 * right-side zone rail. Shows castability glow when legal to cast and
 * displays effective cost (including commander tax).
 */
export function CommanderCardZone({ playerId, splitOverview = false }: CommanderCardZoneProps) {
  const gameState = useGameStore((s) => s.gameState);

  const commanders = useMemo(
    () => (gameState ? commandersInZone(gameState, playerId) : []),
    [gameState, playerId],
  );

  if (commanders.length === 0) return null;

  return (
    <div className="flex flex-col gap-1">
      {commanders.map((cmd) => (
        <CommanderCard key={cmd.id} commander={cmd} splitOverview={splitOverview} />
      ))}
    </div>
  );
}

function CommanderCard({
  commander,
  splitOverview,
}: {
  commander: GameObject;
  splitOverview: boolean;
}) {
  const { t } = useTranslation("game");
  const isCompactHeight = useIsCompactHeight();
  const legalActions = useGameStore((s) => s.legalActions);
  const effectiveCost = useGameStore(
    (s) => s.spellCosts[String(commander.id)],
  );
  const inspectObject = useUiStore((s) => s.inspectObject);
  const { src } = useCardImage(commander.name, { size: "normal" });
  const { handlers: hoverHandlers, firedRef } = useCardHover(commander.id);
  const tax = commander.commander_tax ?? 0;

  const castAction = useMemo(() => {
    for (const action of legalActions) {
      if (action.type === "CastSpell") {
        const data = (
          action as Extract<GameAction, { type: "CastSpell" }>
        ).data;
        if (Number(data.object_id) === commander.id) return action;
      }
    }
    return null;
  }, [legalActions, commander.id]);

  const canCast = castAction !== null;
  const displayCost = effectiveCost ?? commander.mana_cost;
  // canCast is engine-authoritative: the action is in legalActions only when
  // priority + mana + timing all permit the cast. Reuse it as the drag gate
  // rather than threading a separate hasPriority check through.
  const dragCast = useDragToCast({ castAction, hasPriority: canCast, useDistanceThreshold: true });
  // Framer Motion does not suppress the synthetic click that follows a
  // drag gesture on a <motion.button>. Without this guard, a successful
  // drag-cast would immediately trigger the click handler and open the
  // inspector on top of the newly-cast spell. Set the flag when drag-cast
  // fires and read-reset it on the next click.
  const dragCastedRef = useRef(false);
  const onDragEnd = (event: MouseEvent | TouchEvent | PointerEvent, info: PanInfo) => {
    const fired = dragCast(event, info);
    if (fired) dragCastedRef.current = true;
  };

  return (
    <motion.button
      {...hoverHandlers}
      onClick={(e: React.MouseEvent) => {
        if (dragCastedRef.current) {
          dragCastedRef.current = false;
          return;
        }
        if (firedRef.current) return;
        if (useUiStore.getState().debugInteractionMode) {
          e.stopPropagation();
          useUiStore.getState().openDebugContextMenu({ objectId: commander.id, x: e.clientX, y: e.clientY });
          return;
        }
        inspectObject(commander.id);
      }}
      onDoubleClick={canCast ? () => dispatchAction(castAction) : undefined}
      drag={canCast || false}
      dragSnapToOrigin
      onDragEnd={onDragEnd}
      whileDrag={{ cursor: "grabbing", scale: 1.04 }}
      className={`group relative ${canCast ? "cursor-grab" : "cursor-default"}`}
      title={
        canCast
          ? tax > 0
            ? t("zone.castCommanderTax", { name: commander.name, tax })
            : t("zone.castCommander", { name: commander.name })
          : tax > 0
            ? t("zone.commanderTitleTax", { name: commander.name, tax })
            : t("zone.commanderTitle", { name: commander.name })
      }
      style={{ width: "var(--card-w)", height: "var(--card-h)" }}
    >
      {/* Card image */}
      <div className="relative h-full w-full overflow-hidden rounded-lg border border-amber-400/60 shadow-md">
        {src ? (
          <img
            src={src}
            alt={commander.name}
            className="h-full w-full object-cover"
            draggable={false}
          />
        ) : (
          <div className="flex h-full w-full items-center justify-center bg-gray-700 text-[10px] text-gray-400">
            {commander.name}
          </div>
        )}

        {/* Translucent overlay — amber tint, lighter when castable */}
        <div
          className={`absolute inset-0 transition-colors ${
            canCast
              ? "bg-amber-600/20 group-hover:bg-amber-600/5"
              : "bg-gray-900/50"
          }`}
        />
      </div>

      {/* Commander badge — omitted in split panes where it would blanket the
          card and hide the cost pips. */}
      {!splitOverview && (
        <div className="absolute -top-1 left-1/2 z-10 -translate-x-1/2 whitespace-nowrap rounded-sm bg-amber-700 px-1.5 py-px text-[8px] font-bold text-amber-100 shadow">
          {t("zone.commander")}
        </div>
      )}

      {/* Castable glow ring */}
      {canCast && (
        <div className={`absolute inset-0 rounded-lg ${CASTABLE_AFFORDANCE_ACTIVE}`} />
      )}

      {/* Commander tax badge — nowrap: the absolute box is clamped to the
          card's width, so on narrow cards "Tax: +N" would otherwise break
          into two lines; centered overhang beats a wrapped pill. */}
      {tax > 0 && (
        <div
          className={`absolute -bottom-1 left-1/2 z-10 -translate-x-1/2 whitespace-nowrap rounded-sm bg-amber-900 py-px font-bold text-amber-200 shadow ${
            splitOverview ? "px-1 text-[7px]" : "px-1.5 text-[8px]"
          }`}
        >
          {t("zone.tax", { tax })}
        </div>
      )}

      {/* Effective mana cost (includes tax) */}
      {displayCost && (
        <ManaCostPips
          cost={displayCost}
          isReduced={false}
          size={splitOverview || isCompactHeight ? "2xs" : "xs"}
          className="absolute right-[4%] top-[2%]"
        />
      )}
    </motion.button>
  );
}
