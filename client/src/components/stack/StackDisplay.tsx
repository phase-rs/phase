import { type CSSProperties, useCallback, useEffect, useMemo, useState } from "react";
import { AnimatePresence, motion } from "framer-motion";
import { useTranslation } from "react-i18next";

import { StackEntry } from "./StackEntry.tsx";
import {
  pressureMultiplier,
  stackPressureFromLength,
} from "../../utils/stackPressure.ts";
import { StackTargetArcs } from "./StackTargetArcs.tsx";
import { useGameStore } from "../../stores/gameStore.ts";
import { usePreferencesStore } from "../../stores/preferencesStore.ts";
import type { ObjectId, StackDisplayGroup, StackEntry as StackEntryType, StackEntryDisplay, WaitingFor } from "../../adapter/types.ts";
import { getStackCardSize } from "../board/boardSizing.ts";

const EMPTY_STACK: StackEntryType[] = [];
const EMPTY_GROUPS: StackDisplayGroup[] = [];
const EMPTY_DETAILS: Record<string, StackEntryDisplay> = {};

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
    case "BlightChoice":
    case "HarmonizeTapChoice":
    case "ChooseXValue":
      return waitingFor.data.pending_cast.object_id;
    // CR 601.2b: PayCost carries its pending cast inside `resume` (only the
    // spell-cast resume; mana-ability cost payment has no pending cast).
    case "PayCost":
      return waitingFor.data.resume.type === "Spell"
        ? waitingFor.data.resume.Spell.object_id
        : null;
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
// Minimum gap kept between the panel and the top/bottom viewport edges so the
// header controls never clip off-screen on short windows or deep stacks.
const VERTICAL_VIEWPORT_INSET = 12;

function getViewportSize() {
  if (typeof window === "undefined") {
    return { width: 1440, height: 900 };
  }
  return { width: window.innerWidth, height: window.innerHeight };
}

export function StackDisplay() {
  const { t } = useTranslation("game");
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
  const stackEntryDetails = useGameStore(
    (s) => s.gameState?.derived?.stack_entry_details ?? EMPTY_DETAILS,
  );
  const [isCollapsed, setIsCollapsed] = useState(false);
  const [viewport, setViewport] = useState(getViewportSize);
  const [hoveredStackEntryId, setHoveredStackEntryId] = useState<ObjectId | null>(null);
  // User-chosen dock edge. Lives in preferences (not local state) because this
  // component unmounts whenever the stack empties — local state would reset the
  // choice on every resolution.
  const stackDockSide = usePreferencesStore((s) => s.stackDockSide);
  const setStackDockSide = usePreferencesStore((s) => s.setStackDockSide);
  const dockedLeft = stackDockSide === "left";

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
  const stackEntryRepresentatives = new Map<ObjectId, ObjectId>();
  for (const group of groups) {
    for (const memberId of group.member_ids) {
      stackEntryRepresentatives.set(memberId, group.representative);
    }
  }
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
  // Vertical anchor as a fraction of viewport height (narrower viewports nudge
  // the panel above dead-center to clear the hand/board). The fraction is
  // clamped to a pixel top below so the panel header — the only controls (swap,
  // collapse, count) — can never be pushed off the top edge when the pile is
  // taller than the viewport.
  const topFraction =
    viewport.width < 640 ? 0.38 :
      viewport.width < 1024 ? 0.43 : 0.5;
  const collapsedPeekPx = viewport.width < 768 ? 24 : COLLAPSED_PEEK_PX;

  const pileWidth = cardSize.width + staggerX * (displayStack.length - 1);
  const pileHeight = cardSize.height + staggerY * (displayStack.length - 1);
  const panelWidth = pileWidth + panelPaddingX * 2;
  const panelHeight = pileHeight + panelPaddingY * 2 + PANEL_HEADER_HEIGHT;
  const collapsedOffset = Math.max(0, panelWidth - collapsedPeekPx);
  // Collapse slides the panel out toward its docked edge: right dock → positive
  // x (off the right), left dock → negative x (off the left).
  const collapsedX = dockedLeft ? -collapsedOffset : collapsedOffset;
  // Resolve the fractional anchor to an explicit, viewport-clamped pixel top.
  // Centering on `topFraction` is the default, but when the pile is taller than
  // the viewport (deep stacks / short windows) a centered top edge slides above
  // y=0 and clips the header controls. Clamp the top edge into
  // [inset, viewport - panelHeight - inset]; when the panel can't fit, the lower
  // bound collapses to `inset` so it pins to the top (header stays visible) and
  // overflow spills off the bottom (oldest entries) instead.
  const panelTopPx = Math.min(
    Math.max(viewport.height * topFraction - panelHeight / 2, VERTICAL_VIEWPORT_INSET),
    Math.max(VERTICAL_VIEWPORT_INSET, viewport.height - panelHeight - VERTICAL_VIEWPORT_INSET),
  );
  // Anchor to the docked edge. Only the right side reserves room for the right
  // action rail (`--game-right-rail-offset`); the left edge has no such rail.
  const panelAnchorStyle: CSSProperties = dockedLeft
    ? { top: panelTopPx, left: `calc(env(safe-area-inset-left) + ${rightOffsetPx}px)` }
    : {
        top: panelTopPx,
        right: `calc(env(safe-area-inset-right) + ${rightOffsetPx}px + var(--game-right-rail-offset, 0px))`,
      };

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
        initial={{ opacity: 0, x: dockedLeft ? -60 : 60 }}
        animate={{ opacity: 1, x: 0 }}
        exit={{ opacity: 0, x: dockedLeft ? -60 : 60 }}
        transition={{ type: "spring", stiffness: 300, damping: 30 }}
        // `pointer-events-none`: the outer box keeps its full panel width even
        // when the inner panel is transform-collapsed offscreen, so a
        // transparent region would otherwise hover over (and swallow clicks
        // meant for) battlefield objects. Click-through here; the real
        // interactive surfaces below opt back in with `pointer-events-auto`.
        // Vertical position comes entirely from the clamped pixel `top` in
        // `panelAnchorStyle` — no `top-1/2`/`-translate-y-1/2` centering, which
        // would let a tall panel's header slide above the viewport top edge.
        className="pointer-events-none fixed z-[35]"
        style={panelAnchorStyle}
      >
        <motion.div
          animate={{ x: isCollapsed ? collapsedX : 0 }}
          transition={{ type: "spring", stiffness: 340, damping: 34 }}
          className="relative"
          style={{ width: panelWidth, height: panelHeight }}
        >
          {isCollapsed && (
            <button
              type="button"
              onClick={() => setIsCollapsed(false)}
              className={`pointer-events-auto absolute top-1/2 z-20 flex h-20 w-7 -translate-y-1/2 items-center justify-center border border-white/10 bg-gray-950/95 text-gray-300 shadow-[0_18px_36px_rgba(0,0,0,0.45)] transition-colors hover:bg-gray-900 hover:text-white ${dockedLeft ? "right-0 translate-x-1/2 rounded-l-md rounded-r-xl" : "left-0 -translate-x-1/2 rounded-l-xl rounded-r-md"}`}
              aria-label={t("stack.expandPanel")}
            >
              {/* Chevron points back toward the board (the direction the panel
                  expands): ◀ for a right dock, ▶ (rotated) for a left dock. */}
              <svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 20 20" fill="currentColor" className={`h-5 w-5 ${dockedLeft ? "rotate-180" : ""}`}>
                <path
                  fillRule="evenodd"
                  d="M12.78 4.22a.75.75 0 0 1 0 1.06L8.06 10l4.72 4.72a.75.75 0 1 1-1.06 1.06l-5.25-5.25a.75.75 0 0 1 0-1.06l5.25-5.25a.75.75 0 0 1 1.06 0Z"
                  clipRule="evenodd"
                />
              </svg>
            </button>
          )}

          <div className="pointer-events-auto relative h-full overflow-hidden rounded-2xl border border-white/10 bg-gray-950/88 shadow-[0_24px_60px_rgba(0,0,0,0.55)] backdrop-blur-md">
            <div className="flex h-9 items-center justify-between border-b border-white/10 px-3">
              <div className="flex items-center gap-2">
                <span className="text-[11px] font-semibold uppercase tracking-[0.24em] text-gray-400">
                  {t("stack.title")}
                </span>
                <span className="rounded-full bg-cyan-500/15 px-2 py-0.5 text-[10px] font-semibold text-cyan-200">
                  {stack.length}
                </span>
              </div>
              <div className="flex items-center gap-0.5">
                <button
                  type="button"
                  onClick={() => setStackDockSide(dockedLeft ? "right" : "left")}
                  className="rounded-md p-1 text-gray-400 transition-colors hover:bg-white/8 hover:text-white"
                  aria-label={t(dockedLeft ? "stack.dockRight" : "stack.dockLeft")}
                  title={t(dockedLeft ? "stack.dockRight" : "stack.dockLeft")}
                >
                  <svg xmlns="http://www.w3.org/2000/svg" fill="none" viewBox="0 0 24 24" strokeWidth={2} stroke="currentColor" className="h-4 w-4">
                    <path strokeLinecap="round" strokeLinejoin="round" d="M7.5 21 3 16.5m0 0L7.5 12M3 16.5h13.5m0-9L21 3m0 0-4.5 4.5M21 3H7.5" />
                  </svg>
                </button>
                <button
                  type="button"
                  onClick={() => setIsCollapsed(true)}
                  className="rounded-md p-1 text-gray-400 transition-colors hover:bg-white/8 hover:text-white"
                  aria-label={t("stack.collapsePanel")}
                >
                  {/* Points toward the docked edge — the direction it collapses. */}
                  <svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 20 20" fill="currentColor" className={`h-4 w-4 ${dockedLeft ? "rotate-180" : ""}`}>
                    <path
                      fillRule="evenodd"
                      d="M7.22 4.22a.75.75 0 0 1 1.06 0l5.25 5.25a.75.75 0 0 1 0 1.06l-5.25 5.25a.75.75 0 1 1-1.06-1.06L11.94 10 7.22 5.28a.75.75 0 0 1 0-1.06Z"
                      clipRule="evenodd"
                    />
                  </svg>
                </button>
              </div>
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
                      details={stackEntryDetails[String(entry.id)]}
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
          detailsByEntry={stackEntryDetails}
          stackEntryRepresentatives={stackEntryRepresentatives}
        />
      </motion.div>
    </AnimatePresence>
  );
}
