import { act, type CSSProperties, type ReactNode } from "react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen } from "@testing-library/react";

import type { StackEntryDisplay } from "../../../adapter/types.ts";
import { StackDisplay } from "../StackDisplay.tsx";
import { getArcPath } from "../../targeting/arcPath.ts";
import { useGameStore } from "../../../stores/gameStore.ts";
import { useMultiplayerStore } from "../../../stores/multiplayerStore.ts";
import { usePreferencesStore } from "../../../stores/preferencesStore.ts";
import { useTargetingPromptStore } from "../../../stores/targetingPromptStore.ts";
import { buildStackEntry, gameStateFactory } from "../../../test/factories/gameStateFactory.ts";
import { setGameStoreForTest } from "../../../test/helpers/gameStoreHelpers.ts";

vi.mock("../../../hooks/useCardImage.ts", () => ({
  useCardImage: () => ({ src: "/test-card.png", isLoading: false }),
}));

// The widget owns the panel's fixed anchor. It forwards the anchor style here
// so the panel's clamped `top` is what these tests read.
vi.mock("../../flexlayout/DraggableWidget.tsx", () => ({
  DraggableWidget: ({ children, style }: { children: ReactNode; style?: CSSProperties }) => (
    <div data-testid="stack-panel-anchor" style={style}>{children}</div>
  ),
}));

// A landscape phone, the device class of the report (#11119).
const VIEWPORT_WIDTH = 844;
const VIEWPORT_HEIGHT = 390;
// At 844x390 the cards scale by 0.648 (0.72 for the width times 0.9 for the
// height): two entries draw 132x185 cards, and five or more draw the 112x156
// size floor. Entries stagger 24px down, the pile has 14px of padding above
// and below, the header is 36px tall, and the border takes 2px.
const PADDING_Y = 14;
const HEADER_HEIGHT = 36;
const BORDER_HEIGHT = 2;
const VIEWPORT_INSET = 12;
const RAIL_GAP = 8;
// Eleven entries, as in the report: a 396px pile in a 460px panel, taller
// than the viewport. The entry that resolves next is drawn last.
const DEEP_STACK = 11;
const DEEP_CARD_HEIGHT = 156;
const NEXT_TO_RESOLVE_ID = 100 + DEEP_STACK - 1;
// Two entries make a 273px panel. Centred on 43% of the height, its top lands
// at 31.2px and its bottom at 304.2px.
const SHORT_STACK = 2;
const SHORT_CARD_HEIGHT = 185;
const SHORT_PANEL_HEIGHT = 273;
const SHORT_PANEL_TOP = VIEWPORT_HEIGHT * 0.43 - SHORT_PANEL_HEIGHT / 2;
// The rail's top as a landscape phone docks it, with the status row above
// Resolve and Resolve All.
const RAIL_TOP = 300;
// The target of the arc test: the opponent's HUD.
const OPPONENT_HUD_RECT = new DOMRect(380, 8, 120, 48);
// Long enough for the arcs to measure, then stop after ten still frames.
const ARC_SETTLE_MS = 16 * 20;

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

function setViewport(width: number, height: number) {
  Object.defineProperty(window, "innerWidth", { configurable: true, writable: true, value: width });
  Object.defineProperty(window, "innerHeight", { configurable: true, writable: true, value: height });
}

function panelTop() {
  return Number.parseFloat(screen.getByTestId("stack-panel-anchor").style.top);
}

function panelHeight() {
  return Number.parseFloat(screen.getByTestId("stack-panel").style.height);
}

function pileScroller() {
  return screen.getByTestId("stack-pile-scroller");
}

function stackEntry(id: number) {
  const entry = document.querySelector<HTMLElement>(`[data-stack-entry="${id}"]`);
  if (!entry) throw new Error(`stack entry ${id} is not rendered`);
  return entry;
}

/** Where an entry is drawn on screen: under the header and the pile's top
 *  padding at its stagger, moved up by however far the pile is scrolled. */
function stackEntryRect(entry: HTMLElement) {
  const anchor = screen.queryByTestId("stack-panel-anchor");
  const scroller = screen.queryByTestId("stack-pile-scroller");
  const panelTopPx = anchor ? Number.parseFloat(anchor.style.top) : 0;
  const top = panelTopPx + BORDER_HEIGHT / 2 + HEADER_HEIGHT + PADDING_Y
    + Number.parseFloat(entry.style.top) - (scroller?.scrollTop ?? 0);
  return new DOMRect(600, top, 112, DEEP_CARD_HEIGHT);
}

/**
 * The layout happy-dom cannot compute, read from the live DOM: the rail's
 * edge, where the entries and the opponent's HUD are, and the pile scroller's
 * room (the panel's height inside its border, under the header) and scroll
 * height (the pile's box with its margin above).
 */
function mockLayout() {
  const originalRect = HTMLElement.prototype.getBoundingClientRect;
  vi.spyOn(HTMLElement.prototype, "getBoundingClientRect").mockImplementation(function (this: HTMLElement) {
    if (this.dataset.flexZone === "actionRail") {
      return new DOMRect(520, railTopPx, 312, VIEWPORT_HEIGHT - VIEWPORT_INSET - railTopPx);
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
    const isScroller = this instanceof HTMLElement && this.dataset.testid === "stack-pile-scroller";
    const pile = this.firstElementChild;
    if (!isScroller || !(pile instanceof HTMLElement)) return originalScrollHeight?.call(this) ?? 0;
    const pileBottom = Number.parseFloat(pile.style.marginTop) + Number.parseFloat(pile.style.height);
    return Math.max(pileBottom, this.clientHeight);
  });
}

function seed(entryCount: number, details?: Record<string, StackEntryDisplay>) {
  // Abilities triggered by a permanent that has since left: the panel's
  // geometry depends only on how many entries it shows.
  const triggers = Array.from({ length: entryCount }, (_, index) =>
    buildStackEntry({
      id: 100 + index,
      source_id: 0,
      controller: 0,
      kind: {
        type: "TriggeredAbility",
        data: { source_id: 0, ability: { targets: [] }, source_name: "Test Trigger" },
      },
    }),
  );
  const state = gameStateFactory.withPlayers(0, 1).withStack(...triggers);
  const gameState = details
    ? state.params({ derived: { stack_entry_details: details } }).build()
    : state.build();
  setGameStoreForTest({ gameState });
}

function board({ rail = true }: { rail?: boolean } = {}) {
  // GamePage renders the stack panel first and the action rail, which docks
  // in the bottom-right corner, after it.
  return (
    <>
      <StackDisplay effectiveMultiplayerBoardLayout="focused" />
      {rail && <div data-flex-zone="actionRail" />}
      <div data-player-hud="1" />
    </>
  );
}

function railElement() {
  const rail = document.querySelector<HTMLElement>('[data-flex-zone="actionRail"]');
  if (!rail) throw new Error("the action rail is not rendered");
  return rail;
}

/** The entry's whole card lies inside what the pile scroller shows. */
function expectInView(entryId: number, cardHeight: number) {
  const scroller = pileScroller();
  const cardTop = PADDING_Y + Number.parseFloat(stackEntry(entryId).style.top);
  expect(cardTop).toBeGreaterThanOrEqual(scroller.scrollTop);
  expect(cardTop + cardHeight).toBeLessThanOrEqual(scroller.scrollTop + scroller.clientHeight);
}

function expectScrolledToEnd() {
  const scroller = pileScroller();
  expect(scroller.scrollTop).toBe(scroller.scrollHeight - scroller.clientHeight);
}

describe("StackDisplay with a stack taller than its room", () => {
  beforeEach(() => {
    setViewport(VIEWPORT_WIDTH, VIEWPORT_HEIGHT);
    railTopPx = RAIL_TOP;
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

  it.each(["right", "left"] as const)(
    "ends a deep %s-docked panel above the action rail and scrolls its pile to the entry that resolves next",
    (side) => {
      usePreferencesStore.setState({ stackDockSide: side });
      seed(DEEP_STACK);
      render(board());

      // Only a left dock anchors the panel by its left edge.
      expect(screen.getByTestId("stack-panel-anchor").style.left === "").toBe(side === "right");
      // Too tall to centre, the panel pins at the top inset and ends the gap
      // above the rail: 292px - 12px = 280px of its 460px.
      expect(panelTop()).toBe(VIEWPORT_INSET);
      expect(panelTop() + panelHeight()).toBe(RAIL_TOP - RAIL_GAP);
      expect(panelHeight()).toBe(280);

      const scroller = pileScroller();
      expect(scroller.scrollHeight).toBeGreaterThan(scroller.clientHeight);
      expectScrolledToEnd();
      expectInView(NEXT_TO_RESOLVE_ID, DEEP_CARD_HEIGHT);
    },
  );

  it.each([
    ["no action rail", false],
    ["the action rail well below it", true],
  ] as const)("leaves a short stack that fits where it was, with %s", (_case, rail) => {
    // The rail starts 320px down, so the panel's bottom at 304.2px is more
    // than the gap above it.
    railTopPx = 320;
    seed(SHORT_STACK);
    render(board({ rail }));

    expect(panelTop()).toBeCloseTo(SHORT_PANEL_TOP);
    expect(panelHeight()).toBe(SHORT_PANEL_HEIGHT);
    // The pile fills its room exactly, so there is nothing to scroll.
    const scroller = pileScroller();
    expect(scroller.scrollHeight).toBe(scroller.clientHeight);
    expect(scroller.scrollTop).toBe(0);
    expectInView(100 + SHORT_STACK - 1, SHORT_CARD_HEIGHT);
  });

  it("holds a short stack up so it ends above the action rail", () => {
    // Centred, the 273px panel would end at 304.2px, inside the gap above the
    // rail at 300px. It moves up to end at 292px instead, keeping its height.
    seed(SHORT_STACK);
    render(board());

    expect(panelHeight()).toBe(SHORT_PANEL_HEIGHT);
    expect(panelTop()).toBe(RAIL_TOP - RAIL_GAP - SHORT_PANEL_HEIGHT);
  });

  // The panel's header and one card need 220px (36px, 14px of padding twice,
  // and a 156px card). A rail that leaves at least that much between the top
  // inset and the gap above it bounds the panel; a higher one, dragged up
  // beside the panel in Flex Layout, leaves the panel to the bottom inset.
  it.each([
    ["exactly room for the header and one card above it", 240, 240 - RAIL_GAP - VIEWPORT_INSET],
    ["less room than that above it", 239, VIEWPORT_HEIGHT - VIEWPORT_INSET - VIEWPORT_INSET],
  ] as const)("with a rail that leaves %s, bounds a deep panel accordingly", (_case, railTop, height) => {
    railTopPx = railTop;
    seed(DEEP_STACK);
    render(board());

    expect(panelTop()).toBe(VIEWPORT_INSET);
    expect(panelHeight()).toBe(height);
  });

  it("ends a deep panel pushed down by a targeting prompt above the action rail", () => {
    // The panel starts 8px under the prompt's bottom edge (#11024), which
    // leaves it 292px - 72px = 220px.
    useTargetingPromptStore.setState({ topPx: 4, bottomPx: 64 });
    seed(DEEP_STACK);
    render(board());

    expect(panelTop()).toBe(72);
    expect(panelTop() + panelHeight()).toBe(RAIL_TOP - RAIL_GAP);
    expectScrolledToEnd();
    expectInView(NEXT_TO_RESOLVE_ID, DEEP_CARD_HEIGHT);
  });

  it("keeps the header when a targeting prompt leaves less room than it", () => {
    // Under a prompt ending at 280px the panel starts at 288px, 4px above
    // the bottom limit. It keeps its 36px header, the controls that collapse
    // it or move it to the other side.
    useTargetingPromptStore.setState({ topPx: 4, bottomPx: 280 });
    seed(DEEP_STACK);
    render(board());

    expect(panelTop()).toBe(288);
    expect(panelHeight()).toBe(HEADER_HEIGHT);
  });

  it("follows the rail as it grows without a render", () => {
    railTopPx = 330;
    seed(DEEP_STACK);
    render(board());
    expect(panelHeight()).toBe(330 - RAIL_GAP - VIEWPORT_INSET);
    // The player scrolls up to the oldest entries.
    pileScroller().scrollTop = 0;

    // The rail grows upward as Resolve and Resolve All appear. StackDisplay
    // does not render for it, so only the rail's own observer sees it.
    const railObservers = resizeObservers.filter((observer) =>
      observer.targets.some((target) => target.getAttribute("data-flex-zone") === "actionRail"),
    );
    expect(railObservers).toHaveLength(1);
    railTopPx = RAIL_TOP;
    act(() => railObservers[0].callback([], {} as ResizeObserver));

    expect(panelTop() + panelHeight()).toBe(RAIL_TOP - RAIL_GAP);
    // The pile has less room, so it shows the entry that resolves next again.
    expectScrolledToEnd();
    expectInView(NEXT_TO_RESOLVE_ID, DEEP_CARD_HEIGHT);
  });

  it("follows the rail when Flex Layout moves it", async () => {
    railTopPx = 330;
    seed(DEEP_STACK);
    render(board());
    expect(panelHeight()).toBe(330 - RAIL_GAP - VIEWPORT_INSET);

    // DraggableWidget moves the rail with an inline transform: no resize, no
    // render of StackDisplay.
    railTopPx = RAIL_TOP;
    await act(async () => {
      railElement().setAttribute("style", "transform: translateX(0px) translateY(-30px)");
    });

    expect(panelTop() + panelHeight()).toBe(RAIL_TOP - RAIL_GAP);
  });

  it("follows the rail when the window's height changes", () => {
    seed(DEEP_STACK);
    render(board());
    expect(panelTop() + panelHeight()).toBe(RAIL_TOP - RAIL_GAP);

    // A window 30px shorter carries the bottom-docked rail up 30px without
    // resizing it or changing its style.
    setViewport(VIEWPORT_WIDTH, VIEWPORT_HEIGHT - 30);
    railTopPx = RAIL_TOP - 30;
    act(() => {
      window.dispatchEvent(new Event("resize"));
    });

    expect(panelTop() + panelHeight()).toBe(RAIL_TOP - 30 - RAIL_GAP);
  });

  it("keeps the player's scroll until the pile changes, then shows the entry that resolves next", () => {
    seed(DEEP_STACK);
    render(board());
    expectScrolledToEnd();

    // The player scrolls up to the oldest entries.
    pileScroller().scrollTop = 0;
    // The game state updates without changing the pile.
    seed(DEEP_STACK);
    expect(pileScroller().scrollTop).toBe(0);

    // A new entry goes on top of the stack.
    seed(DEEP_STACK + 1);
    expectScrolledToEnd();
    expectInView(NEXT_TO_RESOLVE_ID + 1, DEEP_CARD_HEIGHT);
  });

  it("lets a swipe on the pile scroll it", () => {
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

  it("redraws a target arc from its entry when the pile scrolls", () => {
    vi.useFakeTimers();
    seed(DEEP_STACK, {
      [String(NEXT_TO_RESOLVE_ID)]: {
        source_name: "Test Trigger",
        kind_label: "Triggered Ability",
        targets: [{ target: { Player: 1 }, label: "Opponent" }],
      },
    });
    render(board());

    const centre = (rect: DOMRect) => ({ x: rect.left + rect.width / 2, y: rect.top + rect.height / 2 });
    const expectedArc = () => getArcPath(
      centre(stackEntryRect(stackEntry(NEXT_TO_RESOLVE_ID))),
      centre(OPPONENT_HUD_RECT),
    );
    const drawnArc = () => document.querySelector("path[marker-end]")?.getAttribute("d");
    act(() => {
      vi.advanceTimersByTime(ARC_SETTLE_MS);
    });
    const arcAtEnd = drawnArc();
    expect(arcAtEnd).toBe(expectedArc());

    // The arcs have stopped measuring. The player scrolls the pile up to its
    // oldest entries, which carries the entry down the screen.
    const scroller = pileScroller();
    scroller.scrollTop = 0;
    fireEvent.scroll(scroller);
    act(() => {
      vi.advanceTimersByTime(ARC_SETTLE_MS);
    });

    expect(drawnArc()).toBe(expectedArc());
    expect(drawnArc()).not.toBe(arcAtEnd);
  });
});
