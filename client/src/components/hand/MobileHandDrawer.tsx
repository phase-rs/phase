import { memo, useCallback, useEffect, useMemo } from "react";
import { AnimatePresence, motion } from "framer-motion";
import { useTranslation } from "react-i18next";

import { ManaCostPips } from "../mana/ManaCostPips.tsx";
import { useGameStore } from "../../stores/gameStore.ts";
import { useUiStore } from "../../stores/uiStore.ts";
import { useCardImage } from "../../hooks/useCardImage.ts";
import { useCardHover } from "../../hooks/useCardHover.ts";
import { useCanActForWaitingState, usePerspectivePlayerId } from "../../hooks/usePlayerId.ts";
import { dispatchAction } from "../../game/dispatch.ts";
import type { ManaCost, ObjectId } from "../../adapter/types.ts";
import {
  collectObjectActions,
  resolveSingleActionDispatch,
} from "../../viewmodel/cardActionChoice.ts";

export function MobileHandDrawer() {
  const { t } = useTranslation("game");
  const isOpen = useUiStore((s) => s.mobileHandOpen);
  const setOpen = useUiStore((s) => s.setMobileHandOpen);
  const playerId = usePerspectivePlayerId();
  const player = useGameStore((s) => s.gameState?.players[playerId]);
  const objects = useGameStore((s) => s.gameState?.objects);
  const legalActionsByObject = useGameStore((s) => s.legalActionsByObject);
  const inspectObject = useUiStore((s) => s.inspectObject);
  const setPendingAbilityChoice = useUiStore((s) => s.setPendingAbilityChoice);
  const openDebugContextMenu = useUiStore((s) => s.openDebugContextMenu);

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
    if (
      waitingForType === "TargetSelection"
      || waitingForType === "TriggerTargetSelection"
      || waitingForType === "ChooseXValue"
      || waitingForType === "PayAmountChoice"
    ) {
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
    return new Set(Object.keys(legalActionsByObject ?? {}).map(Number));
  }, [legalActionsByObject]);

  // Close the drawer first so the context menu isn't rendered beneath the
  // drawer's full-screen panel; coordinates flow through from the tap.
  const handleDebugOpen = useCallback(
    (objectId: number, x: number, y: number) => {
      setOpen(false);
      openDebugContextMenu({ objectId, x, y });
    },
    [setOpen, openDebugContextMenu],
  );

  const playCard = useCallback(
    (objectId: number) => {
      if (!hasPriority || !objects) return;
      const obj = objects[objectId];
      if (!obj) return;

      const allActions = collectObjectActions(legalActionsByObject, objectId as ObjectId);
      if (allActions.length === 0) return;

      inspectObject(null);
      setOpen(false);

      // #506: a lone card-consuming action (cycling / Channel — its cost
      // discards the card, CR 702.29a) must surface the choice modal so the
      // player explicitly opts in. resolveSingleActionDispatch is the single
      // decision authority.
      const auto = resolveSingleActionDispatch(allActions, obj);
      if (auto) {
        dispatchAction(auto);
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
                {t("hand.handTitle", { count: handObjects.length })}
              </span>
              <button
                onClick={() => setOpen(false)}
                className="rounded-lg px-3 py-1 text-xs font-medium text-white/70 hover:bg-white/10 active:bg-white/20"
              >
                {t("common:actions.close")}
              </button>
            </div>

            <div
              className="grid gap-3 overflow-y-auto overscroll-contain px-3 pb-4"
              style={{ gridTemplateColumns: "repeat(auto-fill, minmax(170px, 1fr))" }}
            >
              {handObjects.map((obj) => {
                const isPlayable = hasPriority && playableObjectIds.has(Number(obj.id));
                return (
                  <DrawerCard
                    key={obj.id}
                    objectId={obj.id}
                    cardName={obj.name}
                    manaCost={obj.mana_cost}
                    isPlayable={isPlayable}
                    hasPriority={hasPriority}
                    onPlay={playCard}
                    onDebugOpen={handleDebugOpen}
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
  onDebugOpen: (objectId: number, x: number, y: number) => void;
}

const DrawerCard = memo(function DrawerCard({
  objectId,
  cardName,
  manaCost,
  isPlayable,
  hasPriority,
  onPlay,
  onDebugOpen,
}: DrawerCardProps) {
  const inspectObject = useUiStore((s) => s.inspectObject);
  const setPreviewSticky = useUiStore((s) => s.setPreviewSticky);
  const effectiveCost = useGameStore((s) => s.spellCosts[String(objectId)]);
  const { src } = useCardImage(cardName, { size: "normal" });
  const displayCost = effectiveCost ?? manaCost;
  const isReduced = effectiveCost?.type === "Cost" && manaCost.type === "Cost"
    && (effectiveCost.generic < manaCost.generic || effectiveCost.shards.length < manaCost.shards.length);

  // Mouse hover (desktop) + long-press (touch) both open the card preview, and
  // the hook tags the element with `data-card-hover` so usePreviewDismiss's
  // pointer poll keeps the preview alive while the cursor is over the card.
  // This is what lets a player read any card in the full-hand modal: the fanned
  // hand overlaps cards, so the modal is the only place to inspect the ones
  // hidden behind others — and that inspection must work for mouse and touch.
  const { handlers, firedRef } = useCardHover(objectId);

  const handleClick = useCallback(
    (e: React.MouseEvent) => {
      if (firedRef.current) {
        firedRef.current = false;
        return;
      }
      // Click-mode (sandbox debug interaction) routes the tap to the debug
      // context menu instead of the play/inspect path. The desktop fanned
      // hand handles this in `PlayerHand.handleCardClick`; mobile users only
      // see this drawer, so the same branch must live here too.
      if (useUiStore.getState().debugInteractionMode) {
        e.stopPropagation();
        onDebugOpen(objectId, e.clientX, e.clientY);
        return;
      }
      if (isPlayable) {
        onPlay(objectId);
      } else {
        inspectObject(objectId);
        setPreviewSticky(true);
      }
    },
    [objectId, isPlayable, onPlay, onDebugOpen, inspectObject, setPreviewSticky, firedRef],
  );

  const glowClass = hasPriority && isPlayable
    ? "ring-2 ring-cyan-400 shadow-[0_0_12px_3px_rgba(34,211,238,0.5)]"
    : "ring-1 ring-white/10";

  return (
    <button
      className={`relative aspect-[5/7] w-full overflow-hidden rounded-lg bg-gray-800 ${glowClass}`}
      onClick={handleClick}
      {...handlers}
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
