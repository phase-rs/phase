import { type CSSProperties, type ReactNode, useCallback, useEffect, useLayoutEffect, useRef, useState } from "react";
import { AnimatePresence, motion } from "framer-motion";
import { useTranslation } from "react-i18next";

import { StackEntry } from "./StackEntry.tsx";
import { pressureMultiplier } from "../../utils/stackPressure.ts";
import { effectiveStackPressure } from "../../utils/stackThroughput.ts";
import { StackTargetArcs } from "./StackTargetArcs.tsx";
import { useGameStore } from "../../stores/gameStore.ts";
import { usePreferencesStore } from "../../stores/preferencesStore.ts";
import type { MultiplayerBoardLayout } from "../../stores/preferencesStore.ts";
import { useTargetingPromptStore } from "../../stores/targetingPromptStore.ts";
import {
  getSeatCount,
  getWaitingForObjectChoiceIds,
  isSplitBoardActive,
} from "../../viewmodel/gameStateView.ts";
import type { ObjectId, StackDisplayGroup, StackEntry as StackEntryType, StackEntryDisplay } from "../../adapter/types.ts";
import { getStackCardSize } from "../board/boardSizing.ts";
import { printingComparison } from "../../services/cardImageLookup.ts";
import { DraggableWidget } from "../flexlayout/DraggableWidget.tsx";

const EMPTY_STACK: StackEntryType[] = [];
const EMPTY_GROUPS: StackDisplayGroup[] = [];
const EMPTY_DETAILS: Record<string, StackEntryDisplay> = {};

interface DisplayStackEntry {
  entry: StackEntryType;
  choiceObjectId?: ObjectId;
  groupedObjectIds?: ObjectId[];
  groupCount: number;
}

function ownerArtKey(details: StackEntryDisplay | undefined): string {
  return JSON.stringify(printingComparison(details?.printing, details?.printed_ref));
}

/** A missing details row is missing comparison authority, not an absent printing. */
function shouldSplitStackGroup(
  group: StackDisplayGroup,
  legalMemberIds: readonly ObjectId[],
  detailsByEntry: Readonly<Record<string, StackEntryDisplay>>,
  showOwnerPrintings: boolean,
): boolean {
  if (legalMemberIds.length >= 2) return true;
  if (!showOwnerPrintings) return false;
  const firstKey = ownerArtKey(detailsByEntry[String(group.member_ids[0])]);
  return group.member_ids.some((id) => {
    const details = detailsByEntry[String(id)];
    return !details || ownerArtKey(details) !== firstKey;
  });
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
// Gap kept between the panel's header and the targeting prompt, above or below it.
const TARGETING_PROMPT_GAP = 8;
// Gap kept between the panel's bottom and the top of the action rail below it.
const ACTION_RAIL_GAP = 8;
// The panel's border (`border`, 1px on each edge) takes this much of its height.
const PANEL_BORDER_HEIGHT = 2;
// The least the panel shrinks to: its header inside its border.
const MIN_PANEL_HEIGHT = PANEL_HEADER_HEIGHT + PANEL_BORDER_HEIGHT;
// The Flex Layout zones of this panel and of the action rail: the `flexZone`
// this file and GamePage give their DraggableWidgets.
const STACK_PANEL_ZONE = "stackPanel";
const ACTION_RAIL_SELECTOR = '[data-flex-zone="actionRail"]';

function getViewportSize() {
  if (typeof window === "undefined") {
    return { width: 1440, height: 900 };
  }
  return { width: window.innerWidth, height: window.innerHeight };
}

/**
 * The top edge, in viewport pixels, of the action rail where it overlaps the
 * stack panel sideways, or null where it does not: it sits to one side of the
 * panel, or no rail or no panel is on screen.
 *
 * The rail holds Resolve and Resolve All on a layer below the panel, so the
 * panel ends above the rail instead of covering it (#11119). Both are Flex
 * Layout widgets the player can drag, so both are measured where they are on
 * screen, the rail found by its zone as the Flex Layout overlay finds widgets.
 * Sideways, the rail can lie under the panel only where the two overlap: in
 * landscape the rail sits in the bottom-right corner, clear of a left-docked
 * panel, and on a narrow portrait screen it spans the whole width.
 *
 * It measures after every render of StackDisplay, which renders for whatever
 * moves the panel sideways in the layout: its dock side, its width and the
 * viewport. Between renders it follows the rail resizing (its buttons and
 * status chips change as the game moves on); the board column resizing (the
 * rail is fixed to the bottom of that `contain-paint` box, its parent, which
 * shrinks when the game log opens below it); and the rail's and the panel's
 * inline styles (DraggableWidget applies drag offsets and box scale as
 * transforms).
 */
function useActionRailTopUnderPanel(panel: HTMLElement | null): number | null {
  const [railTopPx, setRailTopPx] = useState<number | null>(null);

  const measure = useCallback(() => {
    const rail = document.querySelector<HTMLElement>(ACTION_RAIL_SELECTOR);
    if (!panel || !rail) {
      setRailTopPx(null);
      return;
    }
    const railRect = rail.getBoundingClientRect();
    const panelRect = panel.getBoundingClientRect();
    const railUnderPanel = railRect.left < panelRect.right && railRect.right > panelRect.left;
    setRailTopPx(railUnderPanel ? railRect.top : null);
  }, [panel]);

  useLayoutEffect(() => measure());

  useLayoutEffect(() => {
    const rail = document.querySelector<HTMLElement>(ACTION_RAIL_SELECTOR);
    if (!panel || !rail) return;

    const resizeObserver = typeof ResizeObserver === "undefined" ? null : new ResizeObserver(measure);
    resizeObserver?.observe(rail);
    if (rail.parentElement) {
      resizeObserver?.observe(rail.parentElement);
    }
    const styleObserver = new MutationObserver(measure);
    for (const widget of [rail, panel]) {
      styleObserver.observe(widget, { attributes: true, attributeFilter: ["style"] });
    }

    return () => {
      resizeObserver?.disconnect();
      styleObserver.disconnect();
    };
  }, [panel, measure]);

  return railTopPx;
}

interface StackPileScrollerProps {
  /** The entry that resolves next, drawn last, at the bottom of the pile. */
  nextToResolveId: ObjectId | undefined;
  /** The whole pile's height. */
  pileHeight: number;
  /** How much of the pile the panel's height leaves out, which is the scroll
   *  that shows the pile's end. 0 when the whole pile fits. */
  scrollEnd: number;
  children: ReactNode;
}

/**
 * The panel's body under its header, which scrolls the pile when the panel is
 * too short for all of it (#11119), and clips it when it fits.
 *
 * It shows the end of the pile, where the entry that resolves next is drawn,
 * whenever that entry or the pile's height changes. When only the room
 * changes (the action rail growing as its buttons appear, a targeting prompt
 * pushing the panel down), it keeps a player who was at the end there and
 * leaves anyone who scrolled up where they are; within a pixel of the end
 * counts as there, since a browser can round a fractional scroll position.
 * The end comes from the panel's geometry rather than `scrollHeight`, which
 * also counts a resolved entry still animating out below the pile.
 *
 * A swipe here scrolls on a touch screen: nothing in the pile is draggable (an
 * entry's long-press preview reads pointer events without cancelling them),
 * and `touch-pan-y` claims vertical pans, as the game menu's scrolling list
 * does. `layoutScroll` lets the entries' layout animations account for the
 * scroll offset.
 */
function StackPileScroller({ nextToResolveId, pileHeight, scrollEnd, children }: StackPileScrollerProps) {
  const scrollerRef = useRef<HTMLDivElement | null>(null);
  const shownRef = useRef<Pick<StackPileScrollerProps, "nextToResolveId" | "pileHeight" | "scrollEnd"> | null>(null);

  useLayoutEffect(() => {
    const scroller = scrollerRef.current;
    if (!scroller) return;
    const shown = shownRef.current;
    shownRef.current = { nextToResolveId, pileHeight, scrollEnd };
    const showEnd =
      shown === null ||
      shown.nextToResolveId !== nextToResolveId ||
      shown.pileHeight !== pileHeight ||
      scroller.scrollTop >= shown.scrollEnd - 1;
    if (showEnd) {
      scroller.scrollTop = scrollEnd;
    }
  }, [nextToResolveId, pileHeight, scrollEnd]);

  const overflowClassName = scrollEnd > 0
    ? "touch-pan-y overflow-y-auto overflow-x-hidden overscroll-contain thin-scrollbar"
    : "overflow-hidden";
  return (
    <motion.div
      ref={scrollerRef}
      layoutScroll
      data-testid="stack-pile-scroller"
      data-stack-pile-scroller
      className={`relative min-h-0 flex-1 ${overflowClassName}`}
    >
      {children}
    </motion.div>
  );
}

export function StackDisplay({
  effectiveMultiplayerBoardLayout,
}: {
  effectiveMultiplayerBoardLayout: MultiplayerBoardLayout;
}) {
  const { t } = useTranslation("game");
  const gameState = useGameStore((s) => s.gameState);
  const stack = gameState?.stack ?? EMPTY_STACK;
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
  const waitingFor = useGameStore((s) => s.waitingFor);
  const [isCollapsed, setIsCollapsed] = useState(false);
  const [viewport, setViewport] = useState(getViewportSize);
  const [hoveredStackEntryId, setHoveredStackEntryId] = useState<ObjectId | null>(null);
  // User-chosen dock edge. Lives in preferences (not local state) because this
  // component unmounts whenever the stack empties — local state would reset the
  // choice on every resolution.
  const stackDockSide = usePreferencesStore((s) => s.stackDockSide);
  const setStackDockSide = usePreferencesStore((s) => s.setStackDockSide);
  const dockedLeft = stackDockSide === "left";
  // User size multiplier over the viewport-derived auto-scale (absent ⇒ 1).
  // Cards derive width AND height from one scale, so this stays aspect-correct.
  const userStackScale = usePreferencesStore((s) => s.flexLayout.scales?.stack) ?? 1;
  const showOwnerPrintings = usePreferencesStore((s) => s.showOwnerPrintings);
  // Where the targeting prompt sits, both edges 0 while none is shown.
  // TargetingOverlay publishes them; see the top clamp below for their use.
  const targetingPromptTopPx = useTargetingPromptStore((s) => s.topPx);
  const targetingPromptBottomPx = useTargetingPromptStore((s) => s.bottomPx);
  // The panel's Flex Layout anchor, found from the first box inside it, and
  // where an action rail overlapping it sideways starts; see the bottom limit
  // below.
  const [panelAnchor, setPanelAnchor] = useState<HTMLElement | null>(null);
  const attachPanel = useCallback((container: HTMLDivElement | null) => {
    setPanelAnchor(container?.closest<HTMLElement>(`[data-flex-zone="${STACK_PANEL_ZONE}"]`) ?? null);
  }, []);
  const actionRailTopPx = useActionRailTopUnderPanel(panelAnchor);

  useEffect(() => {
    function handleResize() {
      setViewport(getViewportSize());
    }

    window.addEventListener("resize", handleResize);
    return () => window.removeEventListener("resize", handleResize);
  }, []);

  const activeStackEntryId = hoveredStackEntryId ?? stack[stack.length - 1]?.id ?? null;

  const handleStackEntryHover = useCallback((entryId: ObjectId, hovered: boolean) => {
    setHoveredStackEntryId(hovered ? entryId : null);
  }, []);

  if (stack.length === 0) return null;

  // Engine groups remain authoritative. One display-only split decision governs
  // both the drawn entries and the DOM representatives used by target arcs.
  const entryById = new Map(stack.map((e) => [e.id, e] as const));
  const choiceObjectIds = new Set(getWaitingForObjectChoiceIds(waitingFor));
  const stackEntryRepresentatives = new Map<ObjectId, ObjectId>();
  for (const entry of stack) {
    stackEntryRepresentatives.set(entry.id, entry.id);
  }
  const groupedStack: DisplayStackEntry[] = [];
  if (groups.length > 0 && groups.some((g) => g.count > 1)) {
    for (const group of groups) {
      const legalMemberIds = group.member_ids.filter((id) => choiceObjectIds.has(id));
      const split = shouldSplitStackGroup(group, legalMemberIds, stackEntryDetails, showOwnerPrintings);
      for (const memberId of group.member_ids) {
        stackEntryRepresentatives.set(memberId, split ? memberId : group.representative);
      }
      const representative = entryById.get(group.representative);
      if (!representative) continue;
      if (split) {
        for (const memberId of group.member_ids) {
          const entry = entryById.get(memberId);
          if (entry) groupedStack.push({ entry, groupCount: 1 });
        }
      } else {
        groupedStack.push({
          entry: representative,
          choiceObjectId: legalMemberIds[0],
          groupedObjectIds: group.member_ids,
          groupCount: group.count,
        });
      }
    }
  } else {
    for (const entry of stack) {
      groupedStack.push({ entry, groupCount: 1 });
    }
  }
  const displayStack = groupedStack.map((g) => g.entry);
  const rawCardSize = getStackCardSize(displayStack.length);
  const widthScale =
    viewport.width < 640 ? 0.58 :
      viewport.width < 1024 ? 0.72 :
        viewport.width < 1440 ? 0.86 : 1;
  const heightScale = viewport.height < 820 ? 0.9 : 1;
  const responsiveScale = widthScale * heightScale * userStackScale;
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
  const splitBoardActive = isSplitBoardActive(effectiveMultiplayerBoardLayout, getSeatCount(gameState));
  const topFraction = splitBoardActive
    ? viewport.width < 640 ? 0.52 : viewport.width < 1024 ? 0.58 : 0.66
    : viewport.width < 640 ? 0.38 :
      viewport.width < 1024 ? 0.43 : 0.5;
  const collapsedPeekPx = viewport.width < 768 ? 24 : COLLAPSED_PEEK_PX;

  const pileWidth = cardSize.width + staggerX * (displayStack.length - 1);
  const pileHeight = cardSize.height + staggerY * (displayStack.length - 1);
  const panelWidth = pileWidth + panelPaddingX * 2;
  // The height that shows the whole pile.
  const fullPanelHeight = pileHeight + panelPaddingY * 2 + PANEL_HEADER_HEIGHT;
  const collapsedOffset = Math.max(0, panelWidth - collapsedPeekPx);
  // Collapse slides the panel out toward its docked edge: right dock → positive
  // x (off the right), left dock → negative x (off the left).
  const collapsedX = dockedLeft ? -collapsedOffset : collapsedOffset;
  // Resolve the fractional anchor to an explicit pixel top above a bottom
  // limit. The panel centres on `topFraction`, held up so its bottom keeps to
  // the limit. The top inset is a floor that wins over both: when the pile is
  // taller than the room (deep stacks / short windows) the held-up top edge
  // would slide above y=0 and clip the header controls, so the panel pins at
  // the floor (header stays visible) and its height is capped below instead.
  const centredTopPx = viewport.height * topFraction - fullPanelHeight / 2;
  const topAbove = (bottomLimitPx: number) =>
    Math.max(VERTICAL_VIEWPORT_INSET, Math.min(centredTopPx, bottomLimitPx - fullPanelHeight));
  // The bottom limit is the window's bottom inset, or the gap above the top of
  // an action rail under the panel, which holds Resolve and Resolve All on a
  // layer below it (#11119). A rail overlapping the panel sideways is under it
  // when it starts at or below the top of the panel as the inset alone places
  // it, and low enough that the panel's header, inside its border, fits
  // between the top inset and the gap above the rail. A rail starting higher,
  // which Flex Layout can move it to, leaves the panel as it is. As for the
  // prompt check below, a Flex Layout drag of the panel itself still applies
  // on top of the result.
  const viewportBottomLimitPx = viewport.height - VERTICAL_VIEWPORT_INSET;
  const railBottomLimitPx =
    actionRailTopPx !== null &&
    actionRailTopPx >= topAbove(viewportBottomLimitPx) &&
    actionRailTopPx - ACTION_RAIL_GAP >= VERTICAL_VIEWPORT_INSET + MIN_PANEL_HEIGHT
      ? actionRailTopPx - ACTION_RAIL_GAP
      : null;
  const panelBottomLimitPx = railBottomLimitPx === null
    ? viewportBottomLimitPx
    : Math.min(viewportBottomLimitPx, railBottomLimitPx);
  const viewportTopPx = topAbove(panelBottomLimitPx);
  // A targeting prompt renders on a layer above this panel
  // (`GAME_Z_LAYER.dialogHost`) and stays there, so the panel keeps its header
  // clear of the prompt instead (#11004). Covered, the header's controls can be
  // neither seen nor reliably tapped, and its dock-side toggle is the only
  // control that moves the panel to the other side, off the targets the prompt
  // asks for. A header that already ends at least the gap above the prompt
  // stays put: the split layout starts the prompt below its seat panes, and
  // pushing a tall panel under it there would only leave its pile less room.
  // Any other panel starts no higher than the gap below the prompt. With no
  // prompt both edges are 0, so the panel keeps the top found above its bottom
  // limit (the 12px inset already exceeds the 8px gap).
  //
  // The check is vertical only, even where the centred prompt does not reach
  // the panel horizontally: the panel's horizontal extent depends on the rail
  // and Flex Layout drag offsets, which this clamp cannot see (a drag offset
  // also still applies on top of the result). The prompt's edges are in
  // viewport pixels and this `top` resolves against GamePage's `contain-paint`
  // board container, which starts at the viewport top, so the two agree.
  const headerClearsPrompt =
    viewportTopPx + PANEL_HEADER_HEIGHT + TARGETING_PROMPT_GAP <= targetingPromptTopPx;
  const panelTopPx = headerClearsPrompt
    ? viewportTopPx
    : Math.max(viewportTopPx, targetingPromptBottomPx + TARGETING_PROMPT_GAP);
  // The panel ends at the bottom limit at the latest, and its pile scrolls in
  // whatever room that leaves. Pushed down under a targeting prompt, it can
  // have less room than its header: it keeps the header then, whose controls
  // collapse the panel or move it to the other side.
  const panelHeight = Math.min(
    fullPanelHeight,
    Math.max(MIN_PANEL_HEIGHT, panelBottomLimitPx - panelTopPx),
  );
  // Anchor to the docked edge. Only the right side reserves room for the right
  // action rail (`--game-right-rail-offset`); the left edge has no such rail.
  const panelAnchorStyle: CSSProperties = dockedLeft
    ? { top: panelTopPx, left: `calc(env(safe-area-inset-left) + ${rightOffsetPx}px)` }
    : {
        top: panelTopPx,
        right: `calc(env(safe-area-inset-right) + ${rightOffsetPx}px + var(--game-right-rail-offset, 0px))`,
      };

  const entryStyles = displayStack.map((entry, index) => ({
    position: "absolute" as const,
    top: index * staggerY,
    left: index * staggerX,
    // Cards overlap to form the stack pile. Raise the inspected card above
    // every sibling so its engine-provided targets, modes, and paid-cost chips
    // remain readable even when it is below the top stack entry.
    zIndex: entry.id === hoveredStackEntryId ? displayStack.length + 1 : index + 1,
  }));

  return (
    // The DraggableWidget owns the fixed dock anchor so its Flex Layout offset
    // composes with the panel's own entry/collapse animations below. Its
    // `pointer-events-none`: the outer box keeps its full panel width even when
    // the inner panel is transform-collapsed offscreen, so a transparent region
    // would otherwise hover over (and swallow clicks meant for) battlefield
    // objects. Click-through here; the real interactive surfaces below opt back
    // in with `pointer-events-auto`. Vertical position comes entirely from the
    // clamped pixel `top` in `panelAnchorStyle` — no `top-1/2`/`-translate-y-1/2`
    // centering, which would let a tall panel's header slide above the viewport.
    <DraggableWidget
      target={{ kind: "widget", key: "stackPanel" }}
      flexZone={STACK_PANEL_ZONE}
      className="pointer-events-none fixed z-[35]"
      style={panelAnchorStyle}
      scaleKey="stack"
      resizeCorner={dockedLeft ? "br" : "bl"}
    >
      <AnimatePresence>
        <motion.div
          key="stack-container"
          ref={attachPanel}
          initial={{ opacity: 0, x: dockedLeft ? -60 : 60 }}
          animate={{ opacity: 1, x: 0 }}
          exit={{ opacity: 0, x: dockedLeft ? -60 : 60 }}
          transition={{ type: "spring", stiffness: 300, damping: 30 }}
          className="pointer-events-none"
        >
          <motion.div
          animate={{ x: isCollapsed ? collapsedX : 0 }}
          transition={{ type: "spring", stiffness: 340, damping: 34 }}
          data-testid="stack-panel"
          className="relative"
          style={{ width: panelWidth, height: panelHeight }}
        >
          {isCollapsed && (
            <button
              type="button"
              onClick={() => setIsCollapsed(false)}
              className={`pointer-events-auto absolute top-1/2 z-20 flex h-16 w-7 -translate-y-1/2 items-center justify-center rounded-[6px] border border-white/12 bg-gray-950 text-gray-300 shadow-[0_8px_18px_rgba(0,0,0,0.36)] transition-colors hover:bg-gray-900 hover:text-white ${dockedLeft ? "right-0 translate-x-1/2" : "left-0 -translate-x-1/2"}`}
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

          <div className="pointer-events-auto relative flex h-full flex-col overflow-hidden rounded-[10px] border border-white/10 bg-gray-950/40 shadow-[0_16px_36px_rgba(0,0,0,0.25)] backdrop-blur-sm">
            <div
              className="flex shrink-0 items-center justify-between border-b border-white/10 px-3"
              style={{ height: PANEL_HEADER_HEIGHT }}
            >
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

            <StackPileScroller
              nextToResolveId={displayStack[displayStack.length - 1]?.id}
              pileHeight={pileHeight}
              scrollEnd={fullPanelHeight - panelHeight}
            >
              {/* The pile's box runs on below its last card by the bottom
                  padding, less the 2px the panel's border takes from its
                  height, so its scroll range is exactly `scrollEnd`: none for
                  a pile that fits, and for a capped pile an end that shows the
                  same space under the entry that resolves next. The space
                  belongs to the box rather than to a bottom margin, which not
                  every browser counts in a scroll range. */}
              <div
                className="relative"
                style={{
                  width: pileWidth,
                  height: pileHeight + panelPaddingY - PANEL_BORDER_HEIGHT,
                  marginLeft: panelPaddingX,
                  marginTop: panelPaddingY,
                }}
              >
                <AnimatePresence mode="popLayout">
                  {(() => {
                    // Mass-trigger pacing: collapse per-entry animation under stack
                    // pressure — depth (engine thresholds 10/30/100) OR recent
                    // resolution churn (rate axis, for low-depth-high-throughput
                    // loops depth can't see). See utils/stackThroughput.ts.
                    const pacing = pressureMultiplier(
                      effectiveStackPressure(displayStack.length),
                    );
                    return groupedStack.map(({ entry, choiceObjectId, groupedObjectIds, groupCount }, index) => (
                      <StackEntry
                        key={entry.id}
                        entry={entry}
                        choiceObjectId={choiceObjectId}
                        groupedObjectIds={groupedObjectIds}
                        index={index}
                        isTop={index === displayStack.length - 1}
                        isPending={stackEntryDetails[String(entry.id)]?.is_pending}
                        cardSize={cardSize}
                        onHoverChange={(hovered) => handleStackEntryHover(entry.id, hovered)}
                        style={entryStyles[index]}
                        pacingMultiplier={pacing}
                        groupCount={groupCount}
                        details={stackEntryDetails[String(entry.id)]}
                      />
                    ));
                  })()}
                </AnimatePresence>
              </div>
            </StackPileScroller>
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
    </DraggableWidget>
  );
}
