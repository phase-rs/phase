import { useMemo, useCallback, useState, useEffect, useRef, type CSSProperties } from "react";
import { motion, AnimatePresence } from "framer-motion";

import type { GameAction, ObjectId } from "../../adapter/types.ts";
import { useCardImage } from "../../hooks/useCardImage.ts";
import { useInspectHoverProps } from "../../hooks/useInspectHoverProps.ts";
import { usePlayerId } from "../../hooks/usePlayerId.ts";
import { useGameStore } from "../../stores/gameStore.ts";
import { useUiStore } from "../../stores/uiStore.ts";
import { dispatchAction } from "../../game/dispatch.ts";
import { collectObjectActions } from "../../viewmodel/cardActionChoice.ts";

interface ZoneHandProps {
  zone: "exile" | "graveyard";
}

const ZONE_LABELS: Record<string, string> = {
  exile: "Exile",
  graveyard: "Graveyard",
};

/** Self-contained card sizing — same values as playerZoneRailStyle but owned by the component */
const ZONE_CARD_STYLE: CSSProperties = {
  "--card-w": "clamp(60px, 7vw, 95px)",
  "--card-h": "clamp(84px, 9.8vw, 133px)",
} as CSSProperties;

/** Fixed placeholder width so the flex row stays symmetric when no castable cards exist */
const PLACEHOLDER_WIDTH = "calc(clamp(60px, 7vw, 95px) * 1.15)";

/** Max visible stagger layers in collapsed stack */
const MAX_STACK_DEPTH = 5;

export function ZoneHand({ zone }: ZoneHandProps) {
  const playerId = usePlayerId();
  const objects = useGameStore((s) => s.gameState?.objects);
  const legalActions = useGameStore((s) => s.legalActions);
  const legalActionsByObject = useGameStore((s) => s.legalActionsByObject);
  const waitingFor = useGameStore((s) => s.waitingFor);
  const setPendingAbilityChoice = useUiStore((s) => s.setPendingAbilityChoice);
  const inspectObject = useUiStore((s) => s.inspectObject);

  const graveyard = useGameStore((s) => s.gameState?.players[playerId]?.graveyard);
  const exile = useGameStore((s) => s.gameState?.exile);

  const [expanded, setExpanded] = useState(false);
  const expandedRef = useRef<HTMLDivElement>(null);

  const zoneObjectIds = useMemo(() => {
    if (zone === "graveyard") {
      return graveyard ?? [];
    }
    if (!exile || !objects) return [];
    return exile.filter((id) => {
      const obj = objects[id];
      return obj?.owner === playerId;
    });
  }, [zone, graveyard, exile, objects, playerId]);

  // Build sets of castable object_ids and activatable object_ids from legal actions
  const { castableObjectIds, activatableObjectIds } = useMemo(() => {
    const castable = new Set<number>();
    const activatable = new Set<number>();
    for (const action of legalActions) {
      if (action.type === "PlayLand" || action.type === "CastSpell") {
        castable.add(Number((action as Extract<GameAction, { type: "PlayLand" | "CastSpell" }>).data.object_id));
      }
      if (action.type === "ActivateAbility") {
        activatable.add(Number(action.data.source_id));
      }
    }
    return { castableObjectIds: castable, activatableObjectIds: activatable };
  }, [legalActions]);

  // Filter zone objects to only castable/activatable ones, excluding face-down cards
  const castableObjects = useMemo(() => {
    if (!objects) return [];
    return zoneObjectIds
      .map((id) => objects[id])
      .filter(
        (obj) =>
          obj &&
          !obj.face_down &&
          (castableObjectIds.has(Number(obj.id)) ||
            activatableObjectIds.has(Number(obj.id))),
      );
  }, [zoneObjectIds, objects, castableObjectIds, activatableObjectIds]);

  const playCard = useCallback(
    (objectId: number) => {
      if (!objects) return;
      const obj = objects[objectId];
      if (!obj) return;

      const allActions = collectObjectActions(legalActionsByObject, objectId as ObjectId);

      if (allActions.length === 0) return;
      inspectObject(null);
      if (allActions.length === 1) {
        dispatchAction(allActions[0]);
      } else {
        setPendingAbilityChoice({ objectId: objectId as ObjectId, actions: allActions });
      }
    },
    [objects, legalActionsByObject, inspectObject, setPendingAbilityChoice],
  );

  // Auto-collapse when targeting mode activates
  useEffect(() => {
    if (waitingFor?.type === "TargetSelection" || waitingFor?.type === "TriggerTargetSelection") {
      setExpanded(false);
    }
  }, [waitingFor?.type]);

  // Click-outside to collapse expanded view
  useEffect(() => {
    if (!expanded) return;
    const handler = (e: MouseEvent) => {
      if (expandedRef.current && !expandedRef.current.contains(e.target as Node)) {
        setExpanded(false);
      }
    };
    document.addEventListener("mousedown", handler);
    return () => document.removeEventListener("mousedown", handler);
  }, [expanded]);

  // Empty state: invisible placeholder to keep hand centered
  if (castableObjects.length === 0) {
    return <div style={{ width: PLACEHOLDER_WIDTH }} />;
  }

  const stackDepth = Math.min(castableObjects.length - 1, MAX_STACK_DEPTH);

  return (
    <div className="relative shrink-0" style={ZONE_CARD_STYLE}>
      <AnimatePresence mode="wait">
        {!expanded ? (
          <motion.button
            key="stack"
            className="group relative cursor-pointer"
            style={{
              width: "calc(var(--card-w) * 1.15)",
              height: "var(--card-h)",
            }}
            onClick={() => setExpanded(true)}
            whileHover={{ scale: 1.05 }}
            initial={{ opacity: 0, scale: 0.9 }}
            animate={{ opacity: 1, scale: 1 }}
            exit={{ opacity: 0, scale: 0.9 }}
            transition={{ duration: 0.2 }}
            title={`${ZONE_LABELS[zone]} — ${castableObjects.length} castable`}
          >
            {/* Shadow stack layers (behind top card) */}
            {Array.from({ length: stackDepth }).map((_, i) => (
              <div
                key={i}
                className="absolute rounded-lg border border-purple-500/30 bg-purple-950/40"
                style={{
                  width: "var(--card-w)",
                  height: "var(--card-h)",
                  bottom: (i + 1) * 3,
                  left: (i + 1) * -1,
                }}
              />
            ))}

            {/* Top card */}
            <div className="relative h-full w-full" style={{ width: "var(--card-w)", height: "var(--card-h)" }}>
              <TopCardImage cardName={castableObjects[castableObjects.length - 1].name} />

              {/* Purple translucent overlay */}
              <div className="absolute inset-0 rounded-lg bg-purple-600/30 transition-colors group-hover:bg-purple-600/15" />

              {/* Castable glow ring */}
              <div className="absolute inset-0 rounded-lg ring-2 ring-purple-400/70 shadow-[0_0_12px_3px_rgba(147,51,234,0.5)]" />
            </div>

            {/* Count badge */}
            <div className="absolute -bottom-1 -right-1 z-10 flex h-5 w-5 items-center justify-center rounded-full bg-purple-900 text-[9px] font-bold text-purple-200 ring-1 ring-purple-500/60">
              {castableObjects.length}
            </div>

            {/* Hover expand indicator */}
            <div className="absolute inset-0 flex items-center justify-center rounded-lg bg-black/0 opacity-0 transition-all group-hover:bg-black/30 group-hover:opacity-100" style={{ width: "var(--card-w)", height: "var(--card-h)" }}>
              <span className="rounded-md bg-purple-800/80 px-2 py-0.5 text-[10px] font-semibold text-purple-100 shadow">
                View {castableObjects.length}
              </span>
            </div>
          </motion.button>
        ) : (
          <motion.div
            key="expanded"
            ref={expandedRef}
            className="z-20 flex items-end gap-1 rounded-xl border border-purple-500/40 bg-black/60 p-2 backdrop-blur-sm"
            initial={{ opacity: 0, scale: 0.9 }}
            animate={{ opacity: 1, scale: 1 }}
            exit={{ opacity: 0, scale: 0.9 }}
            transition={{ duration: 0.2 }}
          >
            {/* Zone label */}
            <div className="absolute -top-2.5 left-1/2 -translate-x-1/2 z-10 rounded-sm bg-purple-700 px-2 py-px text-[9px] font-bold text-purple-100 shadow">
              {ZONE_LABELS[zone]}
            </div>

            {castableObjects.map((obj, i) => (
              <ZoneHandCard
                key={obj.id}
                objectId={obj.id}
                cardName={obj.name}
                zone={zone}
                index={i}
                onClick={playCard}
              />
            ))}
          </motion.div>
        )}
      </AnimatePresence>
    </div>
  );
}

/** Top card image for the collapsed stack */
function TopCardImage({ cardName }: { cardName: string }) {
  const { src } = useCardImage(cardName, { size: "normal" });
  return (
    <div className="h-full w-full overflow-hidden rounded-lg border border-purple-400/60 shadow-md">
      {src ? (
        <img src={src} alt={cardName} className="h-full w-full object-cover" draggable={false} />
      ) : (
        <div className="h-full w-full bg-gray-700" />
      )}
    </div>
  );
}

/** Horizontal overlap for expanded card row */
const EXPANDED_OVERLAP = "calc(var(--card-w) * -0.3)";

interface ZoneHandCardProps {
  objectId: number;
  cardName: string;
  zone: "exile" | "graveyard";
  index: number;
  onClick: (objectId: number) => void;
}

function ZoneHandCard({ objectId, cardName, zone, index, onClick }: ZoneHandCardProps) {
  const { src } = useCardImage(cardName, { size: "normal" });
  const hoverProps = useInspectHoverProps();

  return (
    <button
      data-card-hover
      onClick={() => onClick(objectId)}
      {...hoverProps(objectId)}
      className="group relative cursor-pointer transition-transform hover:z-10 hover:scale-105"
      title={`Cast from ${ZONE_LABELS[zone]}: ${cardName}`}
      style={{
        width: "var(--card-w)",
        height: "var(--card-h)",
        marginLeft: index === 0 ? 0 : EXPANDED_OVERLAP,
        zIndex: index,
      }}
    >
      {/* Card image with purple border */}
      <div className="relative h-full w-full overflow-hidden rounded-lg border border-purple-400/60 shadow-md">
        {src ? (
          <img
            src={src}
            alt={cardName}
            className="h-full w-full object-cover"
            draggable={false}
          />
        ) : (
          <div className="h-full w-full bg-gray-700" />
        )}

        {/* Purple translucent overlay (Arena-style) */}
        <div className="absolute inset-0 bg-purple-600/30 transition-colors group-hover:bg-purple-600/10" />
      </div>

      {/* Castable glow ring */}
      <div className="absolute inset-0 rounded-lg ring-2 ring-purple-400/70 shadow-[0_0_12px_3px_rgba(147,51,234,0.5)]" />
    </button>
  );
}
