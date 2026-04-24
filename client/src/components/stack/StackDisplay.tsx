import { useCallback, useEffect, useMemo, useState } from "react";
import { AnimatePresence, motion } from "framer-motion";

import { StackEntry } from "./StackEntry.tsx";
import {
  pressureMultiplier,
  stackPressureFromLength,
} from "../../utils/stackPressure.ts";
import { StackTargetArcs } from "./StackTargetArcs.tsx";
import { useGameStore } from "../../stores/gameStore.ts";
import type { ObjectId, StackDisplayGroup, StackEntry as StackEntryType, WaitingFor } from "../../adapter/types.ts";
import { getStackCardSize } from "../board/boardSizing.ts";

const EMPTY_STACK: StackEntryType[] = [];
const EMPTY_GROUPS: StackDisplayGroup[] = [];

// CR 601.2a + CR 601.2b-f: Post-announcement, the spell sits on the engine's
// stack while modes/targets/costs are chosen. This helper identifies the
// ObjectId of the cast currently in that pre-finalization window so the UI can
// render the "Casting…" badge on it.
//
// Most mid-cast WaitingFor variants carry the PendingCast inline (including
// ChooseXValue) — read the object_id directly. `ManaPayment` is the one
// variant where the engine keeps the PendingCast on outer GameState; in that
// case the topmost stack entry is always the current cast by engine invariant
// (no other stack push/pop can interleave within a single cast).
function getPendingCastObjectId(
  waitingFor: WaitingFor | null | undefined,
  topOfStackId: ObjectId | null,
): ObjectId | null {
  if (!waitingFor) return null;
  switch (waitingFor.type) {
    case "TargetSelection":
    case "ModeChoice":
    case "OptionalCostChoice":
    case "DefilerPayment":
    case "DiscardForCost":
    case "SacrificeForCost":
    case "ReturnToHandForCost":
    case "BlightChoice":
    case "TapCreaturesForSpellCost":
    case "ExileFromGraveyardForCost":
    case "HarmonizeTapChoice":
    case "ChooseXValue":
      return waitingFor.data.pending_cast.object_id;
    case "ManaPayment":
      return topOfStackId;
    default:
      return null;
  }
}

const STAGGER_Y = 24;
const STAGGER_X = 10;
const PANEL_PADDING_X = 16;
const PANEL_PADDING_Y = 14;
const PANEL_HEADER_HEIGHT = 36;
const COLLAPSED_PEEK_PX = 28;
const STACK_RIGHT_OFFSET_PX = 112;

function getViewportSize() {
  if (typeof window === "undefined") {
    return { width: 1440, height: 900 };
  }
  return { width: window.innerWidth, height: window.innerHeight };
}

export function StackDisplay() {
  const stack = useGameStore((s) => s.gameState?.stack ?? EMPTY_STACK);
  const waitingFor = useGameStore((s) => s.waitingFor);
  // Engine-authored stack grouping rides on the same state snapshot that
  // carries `state.stack` (see `engine::game::derived_views`). Reading
  // directly from the selector makes the grouped view atomically
  // consistent with the stack it describes — no async RPC, no race guard,
  // no generation counter. Absent `derived` (legacy cached state) falls
  // through to one-per-entry rendering below.
  const groups = useGameStore(
    (s) => s.gameState?.derived?.stack_display_groups ?? EMPTY_GROUPS,
  );
  const [isCollapsed, setIsCollapsed] = useState(false);
  const [viewport, setViewport] = useState(getViewportSize);
  const [hoveredStackEntryId, setHoveredStackEntryId] = useState<ObjectId | null>(null);

  useEffect(() => {
    function handleResize() {
      setViewport(getViewportSize());
    }

    window.addEventListener("resize", handleResize);
    return () => window.removeEventListener("resize", handleResize);
  }, []);

  // CR 601.2a: The engine places the spell on the stack at announcement, so
  // no ghost synthesis is needed here. Identify the in-progress cast so the
  // "Casting…" badge can be applied to that entry.
  const topOfStackId = stack.length > 0 ? stack[stack.length - 1].id : null;
  const pendingCastId = useMemo(
    () => getPendingCastObjectId(waitingFor, topOfStackId),
    [waitingFor, topOfStackId],
  );

  const activeStackEntryId = hoveredStackEntryId ?? stack[stack.length - 1]?.id ?? null;

  const handleStackEntryHover = useCallback((entryId: ObjectId, hovered: boolean) => {
    setHoveredStackEntryId(hovered ? entryId : null);
  }, []);

  if (stack.length === 0) return null;

  // When engine-authored groups are available and actually coalesce anything,
  // render one entry per group (with ×N badge) instead of per raw entry.
  // Falling back to the raw stack when groups are unavailable preserves the
  // prior behavior for adapters that don't proxy the call yet.
  const entryById = new Map(stack.map((e) => [e.id, e] as const));
  const groupedStack: { entry: StackEntryType; count: number }[] =
    groups.length > 0 && groups.some((g) => g.count > 1)
      ? groups
          .map((g) => {
            const entry = entryById.get(g.representative);
            return entry ? { entry, count: g.count } : null;
          })
          .filter((x): x is { entry: StackEntryType; count: number } => x !== null)
      : stack.map((entry) => ({ entry, count: 1 }));
  const displayStack = groupedStack.map((g) => g.entry);
  const rawCardSize = getStackCardSize(displayStack.length);
  const widthScale =
    viewport.width < 640 ? 0.58 :
      viewport.width < 1024 ? 0.72 :
        viewport.width < 1440 ? 0.86 : 1;
  const heightScale = viewport.height < 820 ? 0.9 : 1;
  const responsiveScale = widthScale * heightScale;
  const cardSize = {
    width: Math.max(112, Math.round(rawCardSize.width * responsiveScale)),
    height: Math.max(156, Math.round(rawCardSize.height * responsiveScale)),
  };
  const staggerX = viewport.width < 768 ? 5 : STAGGER_X;
  const staggerY = viewport.width < 768 ? 20 : viewport.width < 1024 ? 24 : STAGGER_Y;
  const panelPaddingX = viewport.width < 768 ? 12 : PANEL_PADDING_X;
  const panelPaddingY = viewport.width < 768 ? 10 : PANEL_PADDING_Y;
  const rightOffsetPx =
    viewport.width < 640 ? 12 :
      viewport.width < 1024 ? 28 :
        viewport.width < 1440 ? 56 : STACK_RIGHT_OFFSET_PX;
  const topPosition =
    viewport.width < 640 ? "38%" :
      viewport.width < 1024 ? "43%" : "50%";
  const collapsedPeekPx = viewport.width < 768 ? 24 : COLLAPSED_PEEK_PX;

  const pileWidth = cardSize.width + staggerX * (displayStack.length - 1);
  const pileHeight = cardSize.height + staggerY * (displayStack.length - 1);
  const panelWidth = pileWidth + panelPaddingX * 2;
  const panelHeight = pileHeight + panelPaddingY * 2 + PANEL_HEADER_HEIGHT;
  const collapsedOffset = Math.max(0, panelWidth - collapsedPeekPx);

  const entryStyles = displayStack.map((_, index) => ({
    position: "absolute" as const,
    top: index * staggerY,
    left: index * staggerX,
    zIndex: index + 1,
  }));

  return (
    <AnimatePresence>
      <motion.div
        key="stack-container"
        initial={{ opacity: 0, x: 60 }}
        animate={{ opacity: 1, x: 0 }}
        exit={{ opacity: 0, x: 60 }}
        transition={{ type: "spring", stiffness: 300, damping: 30 }}
        className="fixed top-1/2 z-30 -translate-y-1/2"
        style={{
          top: topPosition,
          right: `calc(env(safe-area-inset-right) + ${rightOffsetPx}px + var(--game-right-rail-offset, 0px))`,
        }}
      >
        <motion.div
          animate={{ x: isCollapsed ? collapsedOffset : 0 }}
          transition={{ type: "spring", stiffness: 340, damping: 34 }}
          className="relative"
          style={{ width: panelWidth, height: panelHeight }}
        >
          {isCollapsed && (
            <button
              type="button"
              onClick={() => setIsCollapsed(false)}
              className="absolute left-0 top-1/2 z-20 flex h-20 w-7 -translate-x-1/2 -translate-y-1/2 items-center justify-center rounded-l-xl rounded-r-md border border-white/10 bg-gray-950/95 text-gray-300 shadow-[0_18px_36px_rgba(0,0,0,0.45)] transition-colors hover:bg-gray-900 hover:text-white"
              aria-label="Expand stack panel"
            >
              <svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 20 20" fill="currentColor" className="h-5 w-5">
                <path
                  fillRule="evenodd"
                  d="M12.78 4.22a.75.75 0 0 1 0 1.06L8.06 10l4.72 4.72a.75.75 0 1 1-1.06 1.06l-5.25-5.25a.75.75 0 0 1 0-1.06l5.25-5.25a.75.75 0 0 1 1.06 0Z"
                  clipRule="evenodd"
                />
              </svg>
            </button>
          )}

          <div className="relative h-full overflow-hidden rounded-2xl border border-white/10 bg-gray-950/88 shadow-[0_24px_60px_rgba(0,0,0,0.55)] backdrop-blur-md">
            <div className="flex h-9 items-center justify-between border-b border-white/10 px-3">
              <div className="flex items-center gap-2">
                <span className="text-[11px] font-semibold uppercase tracking-[0.24em] text-gray-400">
                  Stack
                </span>
                <span className="rounded-full bg-cyan-500/15 px-2 py-0.5 text-[10px] font-semibold text-cyan-200">
                  {stack.length}
                </span>
              </div>
              <button
                type="button"
                onClick={() => setIsCollapsed(true)}
                className="rounded-md p-1 text-gray-400 transition-colors hover:bg-white/8 hover:text-white"
                aria-label="Collapse stack panel"
              >
                <svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 20 20" fill="currentColor" className="h-4 w-4">
                  <path
                    fillRule="evenodd"
                    d="M7.22 4.22a.75.75 0 0 1 1.06 0l5.25 5.25a.75.75 0 0 1 0 1.06l-5.25 5.25a.75.75 0 1 1-1.06-1.06L11.94 10 7.22 5.28a.75.75 0 0 1 0-1.06Z"
                    clipRule="evenodd"
                  />
                </svg>
              </button>
            </div>

            <div
              className="relative"
              style={{
                width: pileWidth,
                height: pileHeight,
                marginLeft: panelPaddingX,
                marginTop: panelPaddingY,
              }}
            >
              <AnimatePresence mode="popLayout">
                {(() => {
                  // Mass-trigger pacing: engine-authored StackPressure thresholds
                  // (10/30/100) collapse per-entry animation under stack pressure.
                  // See crates/engine/src/game/stack.rs + utils/stackPressure.ts.
                  const pacing = pressureMultiplier(
                    stackPressureFromLength(displayStack.length),
                  );
                  return groupedStack.map(({ entry, count }, index) => (
                    <StackEntry
                      key={entry.id}
                      entry={entry}
                      index={index}
                      isTop={index === displayStack.length - 1}
                      isPending={pendingCastId != null && entry.id === pendingCastId}
                      cardSize={cardSize}
                      onHoverChange={(hovered) => handleStackEntryHover(entry.id, hovered)}
                      style={entryStyles[index]}
                      pacingMultiplier={pacing}
                      groupCount={count}
                    />
                  ));
                })()}
              </AnimatePresence>
            </div>
          </div>
        </motion.div>
        <StackTargetArcs
          stack={displayStack}
          activeEntryId={activeStackEntryId}
          isCollapsed={isCollapsed}
        />
      </motion.div>
    </AnimatePresence>
  );
}
