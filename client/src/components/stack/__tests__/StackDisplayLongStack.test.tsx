import { act, type CSSProperties, type ReactNode } from "react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen } from "@testing-library/react";

import type { StackEntryDisplay } from "../../../adapter/types.ts";
import { StackDisplay } from "../StackDisplay.tsx";
import { useGameStore } from "../../../stores/gameStore.ts";
import { useMultiplayerStore } from "../../../stores/multiplayerStore.ts";
import { usePreferencesStore } from "../../../stores/preferencesStore.ts";
import { useTargetingPromptStore } from "../../../stores/targetingPromptStore.ts";
import { buildStackEntry, gameStateFactory } from "../../../test/factories/gameStateFactory.ts";
import { setGameStoreForTest } from "../../../test/helpers/gameStoreHelpers.ts";

vi.mock("../../../hooks/useCardImage.ts", () => ({
  useCardImage: () => ({ src: "/test-card.png", isLoading: false }),
}));

// The widget owns the panel's fixed anchor and marks it with its Flex Layout
// zone, as the real one does. It forwards the anchor style here so the
// panel's clamped `top` is what these tests read.
vi.mock("../../flexlayout/DraggableWidget.tsx", () => ({
  DraggableWidget: (
    { children, flexZone, style }: { children: ReactNode; flexZone: string; style?: CSSProperties },
  ) => (
    <div data-testid="stack-panel-anchor" data-flex-zone={flexZone} style={style}>{children}</div>
  ),
}));

// A landscape phone, the device class of the report (#11119).
const VIEWPORT_WIDTH = 844;
const VIEWPORT_HEIGHT = 390;
// At 844x390 the cards scale by 0.648 (0.72 for the width times 0.9 for the
// height): two entries draw 132x185 cards, and three or more draw the 112x156
// size floor. Entries stagger 24px down and 10px across, the pile has 14px of
// padding above and below, the header is 36px tall, the border takes 2px, and
// a right-docked panel ends 28px from the right edge.
const PADDING_Y = 14;
const HEADER_HEIGHT = 36;
const BORDER_HEIGHT = 2;
const DOCK_OFFSET = 28;
const VIEWPORT_INSET = 12;
const RAIL_GAP = 8;
// Eleven entries, as in the report: a 396px pile in a 460px panel, taller
// than the viewport. The entry that resolves next is drawn last.
const DEEP_STACK = 11;
const FLOOR_CARD_HEIGHT = 156;
const NEXT_TO_RESOLVE_ID = 100 + DEEP_STACK - 1;
// Under the rail at 300px, the deep panel pins at the 12px top inset and ends
// at 292px: 280px tall, with 242px of room under the header for a 422px pile
// box (with its margin), so it scrolls 180px to its end.
const DEEP_CAPPED_HEIGHT = 280;
const DEEP_SCROLL_END = 180;
// With nothing under it, the deep panel runs to the bottom inset: 366px.
const DEEP_INSET_HEIGHT = VIEWPORT_HEIGHT - VIEWPORT_INSET - VIEWPORT_INSET;
// Two entries make a 273px panel. Centred on 43% of the height, its top lands
// at 31.2px and its bottom at 304.2px.
const SHORT_STACK = 2;
const SHORT_CARD_HEIGHT = 185;
const SHORT_PANEL_HEIGHT = 273;
const SHORT_PANEL_TOP = VIEWPORT_HEIGHT * 0.43 - SHORT_PANEL_HEIGHT / 2;
// The rail's top as a landscape phone docks it, with the status row above
// Resolve and Resolve All, and its span: the bottom-right corner in
// landscape, the whole width in portrait.
const RAIL_TOP = 300;
const RAIL_HEIGHT = 78;
const CORNER_RAIL = { left: 520, right: 832 };
const FULL_WIDTH_RAIL = { left: 12, right: 832 };
// The target of the arc test: the opponent's HUD.
const OPPONENT_HUD_RECT = new DOMRect(380, 8, 120, 48);
// Long enough for the arcs to measure, then stop after ten still frames.
const ARC_SETTLE_MS = 16 * 20;
// Entries are drawn 600px from the left, 112px wide.
const ENTRY_LEFT = 600;
const ENTRY_CENTRE_X = ENTRY_LEFT + 112 / 2;

const originalViewport = { width: window.innerWidth, height: window.innerHeight };
const resizeObservers: ControlledResizeObserver[] = [];
class ControlledResizeObserver {
  readonly callback: ResizeObserverCallback;
  readonly targets: Element[] = [];
  constructor(callback: ResizeObserverCallback) {
    this.callback = callback;
    resizeObservers.push(this);
  }
  observe = vi.fn((target: Element) => {
    this.targets.push(target);
  });
  unobserve = vi.fn();
  disconnect = vi.fn();
}

let railTopPx = RAIL_TOP;
let railSpan = CORNER_RAIL;
// A Flex Layout drag of the panel sideways.
let panelDragX = 0;

function setViewport(width: number, height: number) {
  Object.defineProperty(window, "innerWidth", { configurable: true, writable: true, value: width });
  Object.defineProperty(window, "innerHeight", { configurable: true, writable: true, value: height });
}

function panelAnchor() {
  return screen.getByTestId("stack-panel-anchor");
}

function panelTop() {
  return Number.parseFloat(panelAnchor().style.top);
}

function panelHeight() {
  return Number.parseFloat(screen.getByTestId("stack-panel").style.height);
}

function pileScroller() {
  return screen.getByTestId("stack-pile-scroller");
}

function railElement() {
  const rail = document.querySelector<HTMLElement>('[data-flex-zone="actionRail"]');
  if (!rail) throw new Error("the action rail is not rendered");
  return rail;
}

function stackEntry(id: number) {
  const entry = document.querySelector<HTMLElement>(`[data-stack-entry="${id}"]`);
  if (!entry) throw new Error(`stack entry ${id} is not rendered`);
  return entry;
}

/** The pile's box with its margin above, the pile's own extent. */
function pileBottom(scroller: HTMLElement) {
  const pile = scroller.firstElementChild;
  if (!(pile instanceof HTMLElement)) throw new Error("the pile is not rendered");
  return Number.parseFloat(pile.style.marginTop) + Number.parseFloat(pile.style.height);
}

/** The scroll that shows the end of the pile's own box. */
function pileScrollEnd() {
  const scroller = pileScroller();
  return Math.max(0, pileBottom(scroller) - scroller.clientHeight);
}

/** Where the pile scroller's band starts on screen: under the panel's top
 *  border and its header. */
function pileBandTop() {
  const anchor = screen.queryByTestId("stack-panel-anchor");
  const panelTopPx = anchor ? Number.parseFloat(anchor.style.top) : 0;
  return panelTopPx + BORDER_HEIGHT / 2 + HEADER_HEIGHT;
}

/** Where an entry is drawn on screen: under the pile's top padding at its
 *  stagger, moved up by however far the pile is scrolled. */
function stackEntryRect(entry: HTMLElement) {
  const scroller = screen.queryByTestId("stack-pile-scroller");
  const top = pileBandTop() + PADDING_Y + Number.parseFloat(entry.style.top) - (scroller?.scrollTop ?? 0);
  return new DOMRect(ENTRY_LEFT, top, 112, FLOOR_CARD_HEIGHT);
}

/** Where the panel's anchor is across the screen: against its dock edge,
 *  moved by any drag. */
function panelAnchorRect(anchor: HTMLElement) {
  const panel = screen.queryByTestId("stack-panel");
  const width = panel ? Number.parseFloat(panel.style.width) : 0;
  const height = panel ? Number.parseFloat(panel.style.height) : 0;
  const left = usePreferencesStore.getState().stackDockSide === "left"
    ? DOCK_OFFSET
    : VIEWPORT_WIDTH - DOCK_OFFSET - width;
  return new DOMRect(left + panelDragX, Number.parseFloat(anchor.style.top), width, height);
}

/**
 * The layout happy-dom cannot compute, read from the live DOM: where the rail,
 * the panel's anchor, the pile scroller's band, the entries and the
 * opponent's HUD are; the scroller's room (the panel's height inside its
 * border, under the header); and its scroll height, which takes in the
 * pile's box and every entry's card, an entry still animating out included.
 */
function mockLayout() {
  const originalRect = HTMLElement.prototype.getBoundingClientRect;
  vi.spyOn(HTMLElement.prototype, "getBoundingClientRect").mockImplementation(function (this: HTMLElement) {
    if (this.dataset.flexZone === "actionRail") {
      return new DOMRect(railSpan.left, railTopPx, railSpan.right - railSpan.left, RAIL_HEIGHT);
    }
    if (this.dataset.flexZone === "stackPanel") return panelAnchorRect(this);
    if (this.hasAttribute("data-stack-pile-scroller")) {
      return new DOMRect(0, pileBandTop(), VIEWPORT_WIDTH, this.clientHeight);
    }
    if (this.hasAttribute("data-stack-entry")) return stackEntryRect(this);
    if (this.hasAttribute("data-player-hud")) return OPPONENT_HUD_RECT;
    return originalRect.call(this);
  });

  const originalClientHeight = Object.getOwnPropertyDescriptor(HTMLElement.prototype, "clientHeight")?.get;
  vi.spyOn(HTMLElement.prototype, "clientHeight", "get").mockImplementation(function (this: HTMLElement) {
    if (this.dataset.testid !== "stack-pile-scroller") return originalClientHeight?.call(this) ?? 0;
    return panelHeight() - BORDER_HEIGHT - HEADER_HEIGHT;
  });

  const originalScrollHeight = Object.getOwnPropertyDescriptor(Element.prototype, "scrollHeight")?.get;
  vi.spyOn(Element.prototype, "scrollHeight", "get").mockImplementation(function (this: Element) {
    if (!(this instanceof HTMLElement) || this.dataset.testid !== "stack-pile-scroller") {
      return originalScrollHeight?.call(this) ?? 0;
    }
    let contentBottom = pileBottom(this);
    for (const entry of this.querySelectorAll<HTMLElement>("[data-stack-entry]")) {
      const cardBottom = PADDING_Y + Number.parseFloat(entry.style.top) + FLOOR_CARD_HEIGHT;
      contentBottom = Math.max(contentBottom, cardBottom);
    }
    return Math.max(contentBottom, this.clientHeight);
  });
}

function stackEntries(ids: number[]) {
  // Abilities triggered by a permanent that has since left: the panel's
  // geometry depends only on how many entries it shows.
  return ids.map((id) =>
    buildStackEntry({
      id,
      source_id: 0,
      controller: 0,
      kind: {
        type: "TriggeredAbility",
        data: { source_id: 0, ability: { targets: [] }, source_name: "Test Trigger" },
      },
    }),
  );
}

/** Entries 100 up, the last of them on top of the stack. */
function seed(entryCount: number, details?: Record<string, StackEntryDisplay>) {
  seedIds(Array.from({ length: entryCount }, (_, index) => 100 + index), details);
}

function seedIds(ids: number[], details?: Record<string, StackEntryDisplay>) {
  const state = gameStateFactory.withPlayers(0, 1).withStack(...stackEntries(ids));
  const gameState = details
    ? state.params({ derived: { stack_entry_details: details } }).build()
    : state.build();
  setGameStoreForTest({ gameState });
}

function board({ rail = true }: { rail?: boolean } = {}) {
  // GamePage renders the stack panel first and the action rail after it, both
  // children of the board column the rail is fixed to the bottom of.
  return (
    <div data-testid="board-column">
      <StackDisplay effectiveMultiplayerBoardLayout="focused" />
      {rail && <div data-flex-zone="actionRail" />}
      <div data-player-hud="1" />
    </div>
  );
}

/** The resize observers watching an element, of which there should be one. */
function observersOf(element: Element) {
  return resizeObservers.filter((observer) => observer.targets.includes(element));
}

/** The entry's whole card lies inside what the pile scroller shows. */
function expectInView(entryId: number, cardHeight: number) {
  const scroller = pileScroller();
  const cardTop = PADDING_Y + Number.parseFloat(stackEntry(entryId).style.top);
  expect(cardTop).toBeGreaterThanOrEqual(scroller.scrollTop);
  expect(cardTop + cardHeight).toBeLessThanOrEqual(scroller.scrollTop + scroller.clientHeight);
}

function expectScrolledToEnd() {
  expect(pileScroller().scrollTop).toBe(pileScrollEnd());
}

/** Where the drawn target arc starts. */
function arcStart() {
  const [, x, y] = (document.querySelector("path[marker-end]")?.getAttribute("d") ?? "").split(" ");
  return { x: Number(x), y: Number(y) };
}

describe("StackDisplay with a stack taller than its room", () => {
  beforeEach(() => {
    setViewport(VIEWPORT_WIDTH, VIEWPORT_HEIGHT);
    railTopPx = RAIL_TOP;
    railSpan = CORNER_RAIL;
    panelDragX = 0;
    resizeObservers.length = 0;
    vi.stubGlobal("ResizeObserver", ControlledResizeObserver);
    mockLayout();
    useGameStore.getState().reset();
    usePreferencesStore.setState(usePreferencesStore.getInitialState());
    useTargetingPromptStore.setState(useTargetingPromptStore.getInitialState());
    act(() => {
      useMultiplayerStore.setState({ activePlayerId: 0, isSpectator: false, playerNames: new Map() });
    });
  });

  afterEach(() => {
    cleanup();
    vi.useRealTimers();
    vi.restoreAllMocks();
    vi.unstubAllGlobals();
    setViewport(originalViewport.width, originalViewport.height);
  });

  describe("bounds", () => {
    it("ends a deep panel above the rail under it and scrolls its pile to the entry that resolves next", () => {
      seed(DEEP_STACK);
      render(board());

      // Too tall to centre, the panel pins at the top inset and ends the gap
      // above the rail.
      expect(panelTop()).toBe(VIEWPORT_INSET);
      expect(panelTop() + panelHeight()).toBe(RAIL_TOP - RAIL_GAP);
      expect(panelHeight()).toBe(DEEP_CAPPED_HEIGHT);

      const scroller = pileScroller();
      expect(scroller).toHaveClass("overflow-y-auto");
      expect(pileScrollEnd()).toBe(DEEP_SCROLL_END);
      expectScrolledToEnd();
      expectInView(NEXT_TO_RESOLVE_ID, FLOOR_CARD_HEIGHT);
    });

    it.each(["right", "left"] as const)(
      "ends a deep %s-docked panel above a rail spanning the whole width",
      (side) => {
        railSpan = FULL_WIDTH_RAIL;
        usePreferencesStore.setState({ stackDockSide: side });
        seed(DEEP_STACK);
        render(board());

        // Only a left dock anchors the panel by its left edge.
        expect(panelAnchor().style.left === "").toBe(side === "right");
        expect(panelTop() + panelHeight()).toBe(RAIL_TOP - RAIL_GAP);
        expectScrolledToEnd();
        expectInView(NEXT_TO_RESOLVE_ID, FLOOR_CARD_HEIGHT);
      },
    );

    it("runs a left-docked panel beside the corner rail down to the bottom inset", () => {
      usePreferencesStore.setState({ stackDockSide: "left" });
      seed(DEEP_STACK);
      render(board());

      expect(panelTop()).toBe(VIEWPORT_INSET);
      expect(panelHeight()).toBe(DEEP_INSET_HEIGHT);
      expectScrolledToEnd();
      expectInView(NEXT_TO_RESOLVE_ID, FLOOR_CARD_HEIGHT);
    });

    it.each([
      ["no action rail", false],
      ["the action rail well below it", true],
    ] as const)("leaves a short stack that fits at its centred anchor, with %s", (_case, rail) => {
      // The rail starts 320px down, more than the gap below the panel's
      // bottom at 304.2px.
      railTopPx = 320;
      seed(SHORT_STACK);
      render(board({ rail }));

      expect(panelTop()).toBeCloseTo(SHORT_PANEL_TOP);
      expect(panelHeight()).toBe(SHORT_PANEL_HEIGHT);
      // The pile fills its room exactly, and the body clips it rather than
      // scrolling it, so there is nothing to scroll and no scrollbar.
      const scroller = pileScroller();
      expect(scroller).toHaveClass("overflow-hidden");
      expect(scroller).not.toHaveClass("overflow-y-auto");
      expect(pileBottom(scroller)).toBe(scroller.clientHeight);
      expect(scroller.scrollTop).toBe(0);
      expectInView(100 + SHORT_STACK - 1, SHORT_CARD_HEIGHT);
    });

    it("holds a short stack up so it ends above the action rail", () => {
      // Centred, the 273px panel would end at 304.2px, 4.2px past the top of
      // the rail at 300px. It moves up to end at 292px instead, at full height.
      seed(SHORT_STACK);
      render(board());

      expect(panelHeight()).toBe(SHORT_PANEL_HEIGHT);
      expect(panelTop()).toBe(RAIL_TOP - RAIL_GAP - SHORT_PANEL_HEIGHT);
    });

    // At 844x700 the two-entry panel centres at 164.5px. A rail starting at
    // or below that top is under the panel, which moves up to end above it; a
    // rail starting higher, which Flex Layout can move it to, leaves the panel
    // be.
    it.each([
      ["100px, above the panel", 100, 164.5, SHORT_PANEL_HEIGHT],
      ["164px, just above the panel's top", 164, 164.5, SHORT_PANEL_HEIGHT],
      ["165px, just below the panel's top", 165, VIEWPORT_INSET, 165 - RAIL_GAP - VIEWPORT_INSET],
    ] as const)("with a rail starting at %s, places a short panel accordingly", (_case, railTop, top, height) => {
      setViewport(VIEWPORT_WIDTH, 700);
      railTopPx = railTop;
      seed(SHORT_STACK);
      render(board());

      expect(panelTop()).toBeCloseTo(top);
      expect(panelHeight()).toBe(height);
    });

    // The deep panel's 38px header (inside its border) needs a rail starting
    // at least 12px + 38px + the 8px gap = 58px down. A rail at 57px cannot be
    // cleared, so the inset bounds the panel.
    it.each([
      ["58px, leaving room for the header", 58, HEADER_HEIGHT + BORDER_HEIGHT],
      ["57px, too near the top to clear", 57, DEEP_INSET_HEIGHT],
    ] as const)("with a rail starting at %s, sizes a deep panel accordingly", (_case, railTop, height) => {
      railTopPx = railTop;
      seed(DEEP_STACK);
      render(board());

      expect(panelTop()).toBe(VIEWPORT_INSET);
      expect(panelHeight()).toBe(height);
    });

    it("runs a right-docked panel down to the bottom inset beside a rail wholly to its left", () => {
      railSpan = { left: 12, right: 300 };
      seed(DEEP_STACK);
      render(board());

      expect(panelHeight()).toBe(DEEP_INSET_HEIGHT);
    });

    it("ends a deep panel at the bottom inset above a rail dragged lower than it", () => {
      // The rail starts at 388px, so the gap above it ends at 380px, below the
      // inset at 378px.
      railTopPx = 388;
      seed(DEEP_STACK);
      render(board());

      expect(panelTop() + panelHeight()).toBe(VIEWPORT_HEIGHT - VIEWPORT_INSET);
    });

    it("ends a deep panel pushed down by a targeting prompt above the action rail", () => {
      // The panel starts 8px under the prompt's bottom edge (#11004), which
      // leaves it 292px - 72px = 220px.
      useTargetingPromptStore.setState({ topPx: 4, bottomPx: 64 });
      seed(DEEP_STACK);
      render(board());

      expect(panelTop()).toBe(72);
      expect(panelTop() + panelHeight()).toBe(RAIL_TOP - RAIL_GAP);
      expectScrolledToEnd();
      expectInView(NEXT_TO_RESOLVE_ID, FLOOR_CARD_HEIGHT);
    });

    it("keeps the whole header when a targeting prompt leaves less room than it", () => {
      // Under a prompt ending at 280px the panel starts at 288px, 4px above
      // the bottom limit. It keeps its 36px header and the border around it,
      // the controls that collapse the panel or move it to the other side.
      useTargetingPromptStore.setState({ topPx: 4, bottomPx: 280 });
      seed(DEEP_STACK);
      render(board());

      expect(panelTop()).toBe(288);
      expect(panelHeight()).toBe(HEADER_HEIGHT + BORDER_HEIGHT);
    });
  });

  describe("following the rail and the panel", () => {
    it("follows the rail as it grows without a render, keeping a player at the end there", () => {
      railTopPx = 330;
      seed(DEEP_STACK);
      render(board());
      expect(panelTop() + panelHeight()).toBe(330 - RAIL_GAP);
      expectScrolledToEnd();

      // The rail grows upward as Resolve and Resolve All appear. StackDisplay
      // does not render for it, so only the rail's own observer sees it.
      const railObservers = observersOf(railElement());
      expect(railObservers).toHaveLength(1);
      // The end is 150px. A browser that rounds a fractional scroll position
      // can leave the player half a pixel short of it.
      pileScroller().scrollTop = 149.5;
      railTopPx = RAIL_TOP;
      act(() => railObservers[0].callback([], {} as ResizeObserver));

      expect(panelTop() + panelHeight()).toBe(RAIL_TOP - RAIL_GAP);
      expectScrolledToEnd();
      expectInView(NEXT_TO_RESOLVE_ID, FLOOR_CARD_HEIGHT);
    });

    it("follows the rail when the board column it is fixed to shrinks", () => {
      railTopPx = 330;
      seed(DEEP_STACK);
      render(board());
      expect(panelTop() + panelHeight()).toBe(330 - RAIL_GAP);

      // The game log opens below the board column, which shrinks and carries
      // the rail on its bottom edge up with it. Neither the rail's size nor
      // its style changes, and StackDisplay does not render.
      const columnObservers = observersOf(screen.getByTestId("board-column"));
      expect(columnObservers).toHaveLength(1);
      railTopPx = RAIL_TOP;
      act(() => columnObservers[0].callback([], {} as ResizeObserver));

      expect(panelTop() + panelHeight()).toBe(RAIL_TOP - RAIL_GAP);
    });

    it("follows the rail when Flex Layout moves it", async () => {
      railTopPx = 330;
      seed(DEEP_STACK);
      render(board());
      expect(panelTop() + panelHeight()).toBe(330 - RAIL_GAP);

      // DraggableWidget moves the rail with an inline transform: no resize, no
      // render of StackDisplay.
      railTopPx = RAIL_TOP;
      await act(async () => {
        railElement().setAttribute("style", "transform: translateX(0px) translateY(-30px)");
      });

      expect(panelTop() + panelHeight()).toBe(RAIL_TOP - RAIL_GAP);
    });

    it("lets a panel that Flex Layout drags off the rail run to the bottom inset", async () => {
      seed(DEEP_STACK);
      render(board());
      expect(panelHeight()).toBe(DEEP_CAPPED_HEIGHT);

      // DraggableWidget moves the panel 400px to the left with an inline
      // transform, clear of the corner rail.
      panelDragX = -400;
      await act(async () => {
        panelAnchor().setAttribute("style", `${panelAnchor().getAttribute("style")}; transform: translateX(-400px)`);
      });

      expect(panelHeight()).toBe(DEEP_INSET_HEIGHT);
    });

    it("measures again when the player moves the panel to the other dock side", () => {
      usePreferencesStore.setState({ stackDockSide: "left" });
      seed(DEEP_STACK);
      render(board());
      expect(panelHeight()).toBe(DEEP_INSET_HEIGHT);

      fireEvent.click(screen.getByRole("button", { name: "Dock stack on the right" }));

      expect(panelTop() + panelHeight()).toBe(RAIL_TOP - RAIL_GAP);
    });

    it("measures again when the window's height changes", () => {
      seed(DEEP_STACK);
      render(board());
      expect(panelTop() + panelHeight()).toBe(RAIL_TOP - RAIL_GAP);

      // A window 30px shorter carries the bottom-docked rail up 30px.
      setViewport(VIEWPORT_WIDTH, VIEWPORT_HEIGHT - 30);
      railTopPx = RAIL_TOP - 30;
      act(() => {
        window.dispatchEvent(new Event("resize"));
      });

      expect(panelTop() + panelHeight()).toBe(RAIL_TOP - 30 - RAIL_GAP);
    });
  });

  describe("scrolling", () => {
    it("keeps the player's scroll through updates that change neither the pile nor its room", () => {
      seed(DEEP_STACK);
      render(board());
      expectScrolledToEnd();

      // The player scrolls up to the oldest entries; the game state updates
      // without changing the pile.
      pileScroller().scrollTop = 0;
      seed(DEEP_STACK);
      expect(pileScroller().scrollTop).toBe(0);

      // A new entry goes on top of the stack.
      seed(DEEP_STACK + 1);
      expectScrolledToEnd();
      expectInView(NEXT_TO_RESOLVE_ID + 1, FLOOR_CARD_HEIGHT);
    });

    it("shows a new entry that resolves next in a pile of the same height", () => {
      seed(DEEP_STACK);
      render(board());
      pileScroller().scrollTop = 0;

      // In one update the top entry resolves and another goes on the stack.
      seedIds([...Array.from({ length: DEEP_STACK - 1 }, (_, index) => 100 + index), 200]);

      expect(panelHeight()).toBe(DEEP_CAPPED_HEIGHT);
      expectScrolledToEnd();
      expectInView(200, FLOOR_CARD_HEIGHT);
    });

    it("shows the end again when the pile's height changes under the same entry", () => {
      seed(DEEP_STACK);
      render(board());
      pileScroller().scrollTop = 0;

      // The player scales the stack up: 113x159 cards, a 399px pile.
      act(() => {
        usePreferencesStore.setState((state) => ({
          flexLayout: { ...state.flexLayout, scales: { ...state.flexLayout.scales, stack: 1.25 } },
        }));
      });

      expect(panelHeight()).toBe(DEEP_CAPPED_HEIGHT);
      expect(pileScrollEnd()).toBe(DEEP_SCROLL_END + 3);
      expectScrolledToEnd();
      expectInView(NEXT_TO_RESOLVE_ID, 159);
    });

    it("leaves a player who scrolled up where they are when only the room changes", () => {
      railTopPx = 330;
      seed(DEEP_STACK);
      render(board());
      pileScroller().scrollTop = 40;

      railTopPx = RAIL_TOP;
      act(() => observersOf(railElement())[0].callback([], {} as ResizeObserver));

      expect(panelTop() + panelHeight()).toBe(RAIL_TOP - RAIL_GAP);
      expect(pileScroller().scrollTop).toBe(40);
    });

    it("keeps a deep pile at its end while its resolved top entry animates out", () => {
      seed(DEEP_STACK);
      render(board());

      seed(DEEP_STACK - 1);

      // The resolved entry is still drawn where it was, below the shorter
      // pile's box, so the scroll range runs past the pile's end for now.
      const scroller = pileScroller();
      expect(stackEntry(NEXT_TO_RESOLVE_ID)).toBeInTheDocument();
      expect(scroller.scrollHeight - scroller.clientHeight).toBeGreaterThan(pileScrollEnd());
      expectScrolledToEnd();
      expectInView(NEXT_TO_RESOLVE_ID - 1, FLOOR_CARD_HEIGHT);
    });

    it("gives a pile that fits no scroll while its resolved top entry animates out", () => {
      // Five entries fit in a 316px panel, and four in a 292px one.
      seed(5);
      render(board({ rail: false }));
      expect(pileScrollEnd()).toBe(0);

      seed(4);

      const scroller = pileScroller();
      expect(stackEntry(104)).toBeInTheDocument();
      expect(scroller.scrollHeight).toBeGreaterThan(scroller.clientHeight);
      expect(scroller).toHaveClass("overflow-hidden");
      expect(scroller.scrollTop).toBe(0);
    });

    it("gives a capped pile touch scrolling, with nothing in it claiming the swipe", () => {
      seed(DEEP_STACK);
      render(board());

      const scroller = pileScroller();
      expect(scroller).toHaveClass("overflow-y-auto", "touch-pan-y");
      // Nothing from an entry up to the scroller claims a swipe for itself, as
      // a drag source's `touch-action: none` would.
      let element: HTMLElement | null = stackEntry(NEXT_TO_RESOLVE_ID);
      while (element && element !== scroller) {
        expect(element).not.toHaveClass("touch-none");
        expect(element.style.touchAction).not.toBe("none");
        element = element.parentElement;
      }
      expect(element).toBe(scroller);
    });

    it("draws a target arc from the part of its entry the pile shows, following the scroll", () => {
      vi.useFakeTimers();
      // The oldest entry targets the opponent. At the pile's end it is
      // scrolled out above the pile's band, which runs from 49px to 291px.
      seed(DEEP_STACK, {
        "100": {
          source_name: "Test Trigger",
          kind_label: "Triggered Ability",
          targets: [{ target: { Player: 1 }, label: "Opponent" }],
        },
      });
      render(board());
      const settle = () => act(() => {
        vi.advanceTimersByTime(ARC_SETTLE_MS);
      });
      settle();
      expect(arcStart()).toEqual({ x: ENTRY_CENTRE_X, y: 49 });

      // The arcs have stopped measuring. The player scrolls up, and the lower
      // part of the entry's card shows, from 49px to 179px.
      const scroller = pileScroller();
      scroller.scrollTop = 40;
      fireEvent.scroll(scroller);
      settle();
      expect(arcStart()).toEqual({ x: ENTRY_CENTRE_X, y: 114 });

      // Scrolled back down a little, less of it shows, from 49px to 139px: its
      // visible part keeps its top edge and only gets shorter.
      scroller.scrollTop = 80;
      fireEvent.scroll(scroller);
      settle();
      expect(arcStart()).toEqual({ x: ENTRY_CENTRE_X, y: 94 });
    });

    // Scrolled to the oldest entries, the pile's band (49px to 291px) shows
    // the ninth entry's card from 255px down, and none of the top entry's,
    // which starts at 303px.
    it.each([
      ["partly", 108, (255 + 291) / 2],
      ["wholly", NEXT_TO_RESOLVE_ID, 291],
    ] as const)("draws a target arc from an entry scrolled %s below the pile's band", (_case, entryId, y) => {
      vi.useFakeTimers();
      seed(DEEP_STACK, {
        [String(entryId)]: {
          source_name: "Test Trigger",
          kind_label: "Triggered Ability",
          targets: [{ target: { Player: 1 }, label: "Opponent" }],
        },
      });
      render(board());
      const settle = () => act(() => {
        vi.advanceTimersByTime(ARC_SETTLE_MS);
      });
      settle();

      const scroller = pileScroller();
      scroller.scrollTop = 0;
      fireEvent.scroll(scroller);
      settle();
      expect(arcStart()).toEqual({ x: ENTRY_CENTRE_X, y });
    });
  });
});
