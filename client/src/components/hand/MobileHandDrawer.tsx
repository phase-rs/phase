import { memo, useCallback, useEffect, useMemo } from "react";
import { AnimatePresence, motion } from "framer-motion";

import { ManaCostPips } from "../mana/ManaCostPips.tsx";
import { useGameStore } from "../../stores/gameStore.ts";
import { useUiStore } from "../../stores/uiStore.ts";
import { useCardImage } from "../../hooks/useCardImage.ts";
import { useLongPress } from "../../hooks/useLongPress.ts";
import { useCanActForWaitingState, usePerspectivePlayerId } from "../../hooks/usePlayerId.ts";
import { dispatchAction } from "../../game/dispatch.ts";
import type { GameAction, ManaCost, ObjectId } from "../../adapter/types.ts";
import { collectObjectActions } from "../../viewmodel/cardActionChoice.ts";

export function MobileHandDrawer() {
  const isOpen = useUiStore((s) => s.mobileHandOpen);
  const setOpen = useUiStore((s) => s.setMobileHandOpen);
  const playerId = usePerspectivePlayerId();
  const player = useGameStore((s) => s.gameState?.players[playerId]);
  const objects = useGameStore((s) => s.gameState?.objects);
  const legalActions = useGameStore((s) => s.legalActions);
  const legalActionsByObject = useGameStore((s) => s.legalActionsByObject);
  const inspectObject = useUiStore((s) => s.inspectObject);
  const setPendingAbilityChoice = useUiStore((s) => s.setPendingAbilityChoice);

  const canActForWaitingState = useCanActForWaitingState();
  const hasPriority = useGameStore((s) =>
    canActForWaitingState && s.waitingFor?.type === "Priority",
  );

  const waitingForType = useGameStore((s) => s.waitingFor?.type);

  const pendingObjectId = useGameStore((s) => {
    const wf = s.waitingFor;
    if (wf?.type === "TargetSelection") return wf.data.pending_cast.object_id;
    return null;
  });

  useEffect(() => {
    if (waitingForType === "TargetSelection" || waitingForType === "TriggerTargetSelection") {
      setOpen(false);
    }
  }, [waitingForType, setOpen]);

  useEffect(() => {
    if (!isOpen) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") setOpen(false);
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [isOpen, setOpen]);

  const playableObjectIds = useMemo(() => {
    const ids = new Set<number>();
    for (const action of legalActions) {
      if (action.type === "PlayLand" || action.type === "CastSpell") {
        ids.add(Number((action as Extract<GameAction, { type: "PlayLand" | "CastSpell" }>).data.object_id));
      } else if (action.type === "CastSpellAsSneak") {
        // CR 702.190a: Sneak casts originate from `hand_object` (not `object_id`).
        ids.add(Number(action.data.hand_object));
      }
    }
    return ids;
  }, [legalActions]);

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
      setOpen(false);

      if (allActions.length === 1) {
        dispatchAction(allActions[0]);
      } else {
        setPendingAbilityChoice({ objectId: objectId as ObjectId, actions: allActions });
      }
    },
    [hasPriority, objects, legalActionsByObject, inspectObject, setPendingAbilityChoice, setOpen],
  );

  if (!player || !objects) return null;

  const handObjects = player.hand
    .map((id) => objects[id])
    .filter((obj) => obj && obj.id !== pendingObjectId);

  return (
    <AnimatePresence>
      {isOpen && (
        <>
          <motion.div
            className="fixed inset-0 z-[90] bg-black/60"
            initial={{ opacity: 0 }}
            animate={{ opacity: 1 }}
            exit={{ opacity: 0 }}
            transition={{ duration: 0.2 }}
            onPointerDown={() => setOpen(false)}
          />

          <motion.div
            className="fixed inset-x-0 top-0 bottom-0 z-[91] flex flex-col border-t border-white/10 bg-[#0b1020]/96 backdrop-blur-md"
            style={{
              paddingTop: "env(safe-area-inset-top)",
              paddingBottom: "env(safe-area-inset-bottom)",
            }}
            initial={{ y: "100%" }}
            animate={{ y: 0 }}
            exit={{ y: "100%" }}
            transition={{ type: "spring", damping: 28, stiffness: 300 }}
            drag="y"
            dragConstraints={{ top: 0, bottom: 0 }}
            dragElastic={{ top: 0, bottom: 0.4 }}
            onDragEnd={(_, info) => {
              if (info.offset.y > 120 || info.velocity.y > 600) {
                setOpen(false);
              }
            }}
          >
            <div className="flex shrink-0 items-center justify-between px-4 pt-3 pb-2">
              <span className="text-sm font-semibold text-white/80">
                Hand ({handObjects.length})
              </span>
              <button
                onClick={() => setOpen(false)}
                className="rounded-lg px-3 py-1 text-xs font-medium text-white/70 hover:bg-white/10 active:bg-white/20"
              >
                Close
              </button>
            </div>

            <div
              className="grid gap-3 overflow-y-auto overscroll-contain px-3 pb-4"
              style={{ gridTemplateColumns: "repeat(auto-fill, minmax(170px, 1fr))" }}
            >
              {handObjects.map((obj) => {
                const isPlayable = hasPriority && (
                  playableObjectIds.has(Number(obj.id)) ||
                  activatableObjectIds.has(Number(obj.id))
                );
                return (
                  <DrawerCard
                    key={obj.id}
                    objectId={obj.id}
                    cardName={obj.name}
                    manaCost={obj.mana_cost}
                    isPlayable={isPlayable}
                    hasPriority={hasPriority}
                    onPlay={playCard}
                  />
                );
              })}
            </div>
          </motion.div>
        </>
      )}
    </AnimatePresence>
  );
}

interface DrawerCardProps {
  objectId: number;
  cardName: string;
  manaCost: ManaCost;
  isPlayable: boolean;
  hasPriority: boolean;
  onPlay: (objectId: number) => void;
}

const DrawerCard = memo(function DrawerCard({
  objectId,
  cardName,
  manaCost,
  isPlayable,
  hasPriority,
  onPlay,
}: DrawerCardProps) {
  const inspectObject = useUiStore((s) => s.inspectObject);
  const setPreviewSticky = useUiStore((s) => s.setPreviewSticky);
  const effectiveCost = useGameStore((s) => s.spellCosts[String(objectId)]);
  const { src } = useCardImage(cardName, { size: "normal" });
  const displayCost = effectiveCost ?? manaCost;
  const isReduced = effectiveCost?.type === "Cost" && manaCost.type === "Cost"
    && (effectiveCost.generic < manaCost.generic || effectiveCost.shards.length < manaCost.shards.length);

  const { handlers: longPressHandlers, firedRef: longPressFired } = useLongPress(() => {
    inspectObject(objectId);
    setPreviewSticky(true);
  });

  const handleClick = useCallback(() => {
    if (longPressFired.current) {
      longPressFired.current = false;
      return;
    }
    if (isPlayable) {
      onPlay(objectId);
    } else {
      inspectObject(objectId);
      setPreviewSticky(true);
    }
  }, [objectId, isPlayable, onPlay, inspectObject, setPreviewSticky, longPressFired]);

  const glowClass = hasPriority && isPlayable
    ? "ring-2 ring-cyan-400 shadow-[0_0_12px_3px_rgba(34,211,238,0.5)]"
    : "ring-1 ring-white/10";

  return (
    <button
      className={`relative aspect-[5/7] w-full overflow-hidden rounded-lg bg-gray-800 ${glowClass}`}
      onClick={handleClick}
      {...longPressHandlers}
    >
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
      <ManaCostPips cost={displayCost} isReduced={isReduced} className="absolute right-[4%] top-[2%]" />
    </button>
  );
});
