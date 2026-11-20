import { act, type CSSProperties, type ReactNode } from "react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen } from "@testing-library/react";

import { StackDisplay } from "../StackDisplay.tsx";
import { TargetingOverlay } from "../../targeting/TargetingOverlay.tsx";
import { useGameStore } from "../../../stores/gameStore.ts";
import { useMultiplayerStore } from "../../../stores/multiplayerStore.ts";
import { usePreferencesStore } from "../../../stores/preferencesStore.ts";
import type { MultiplayerBoardLayout } from "../../../stores/preferencesStore.ts";
import { useTargetingPromptStore } from "../../../stores/targetingPromptStore.ts";
import {
  buildStackEntry,
  buildTargetSelectionProgress,
  buildTargetSelectionSlot,
  gameStateFactory,
} from "../../../test/factories/gameStateFactory.ts";
import { setGameStoreForTest } from "../../../test/helpers/gameStoreHelpers.ts";

vi.mock("../../../hooks/useCardImage.ts", () => ({
  useCardImage: () => ({ src: "/test-card.png", isLoading: false }),
}));

vi.mock("../StackTargetArcs.tsx", () => ({
  StackTargetArcs: () => null,
}));

// The widget owns the panel's fixed anchor. It forwards the anchor style here
// so the panel's clamped `top` is what these tests read.
vi.mock("../../flexlayout/DraggableWidget.tsx", () => ({
  DraggableWidget: ({ children, style }: { children: ReactNode; style?: CSSProperties }) => (
    <div data-testid="stack-panel-anchor" style={style}>{children}</div>
  ),
}));

// A landscape phone, the device class of the report (#11004).
const VIEWPORT_WIDTH = 844;
const VIEWPORT_HEIGHT = 390;
// Two entries at 844x390 make a 273px panel: 204x285 cards scaled by 0.648
// (0.72 for the viewport width times 0.9 for its height) to 132x185, a 24px
// stagger, 14px of padding above and below the pile, and the 36px header.
// Centred on 43% of the height, its top lands at 31.2px, which puts the header
// at 31.2px to 67.2px, under the focused-layout prompt fixture below. This is
// the panel's position before the fix, and it must stay the position whenever
// no prompt is shown.
const UNCOVERED_TOP = VIEWPORT_HEIGHT * 0.43 - 273 / 2;
// The focused layout starts the prompt 0.25rem down; the split layout starts
// it 4.25rem down on a phone, below the seat panes. These fixtures give the
// block a round 60px in both; a one-line block measures about 55px.
const FOCUSED_PROMPT = { top: 4, bottom: 64 };
const SPLIT_PROMPT = { top: 68, bottom: 128 };
const PROMPT_GAP = 8;
const DESCRIPTION = "Put a +1/+1 counter on up to one other target creature.";

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

function setViewport(width: number, height: number) {
  Object.defineProperty(window, "innerWidth", { configurable: true, writable: true, value: width });
  Object.defineProperty(window, "innerHeight", { configurable: true, writable: true, value: height });
}

type PromptEdges = { top: number; bottom: number };

/** The prompt block's layout, which happy-dom cannot compute, keyed on its live DOM. */
function mockPromptEdges(edgesOf: (block: HTMLElement) => PromptEdges) {
  const originalRect = HTMLElement.prototype.getBoundingClientRect;
  vi.spyOn(HTMLElement.prototype, "getBoundingClientRect").mockImplementation(function (this: HTMLElement) {
    if (!this.hasAttribute("data-targeting-prompt")) return originalRect.call(this);
    const { top, bottom } = edgesOf(this);
    return new DOMRect(0, top, VIEWPORT_WIDTH, bottom - top);
  });
}

type SeedOptions = {
  /** Waiting on a trigger's target prompt, or on plain priority without one. */
  prompt?: boolean;
  entryCount?: number;
  seatCount?: number;
};

function seed({ prompt = false, entryCount = 2, seatCount = 2 }: SeedOptions = {}) {
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
  const state = gameStateFactory
    .withPlayers(...Array.from({ length: seatCount }, (_, seat) => seat))
    .withStack(...triggers);
  // The engine sends `current_target_kind` only while a target prompt is live.
  const gameState = prompt
    ? state
      .params({ derived: { current_target_kind: { type: "Players" } } })
      .triggerTargetSelection({
        player: 0,
        target_slots: [buildTargetSelectionSlot({ legal_targets: [{ Player: 1 }] })],
        selection: buildTargetSelectionProgress({ current_legal_targets: [{ Player: 1 }] }),
        description: DESCRIPTION,
      })
      .build()
    : state.build();
  setGameStoreForTest({ gameState });
}

type BoardOptions = {
  layout?: MultiplayerBoardLayout;
  /** False once GamePage's gate unmounts the overlay. */
  overlayMounted?: boolean;
};

function board({ layout = "focused", overlayMounted = true }: BoardOptions = {}) {
  return (
    <>
      <StackDisplay effectiveMultiplayerBoardLayout={layout} />
      {overlayMounted && <TargetingOverlay />}
    </>
  );
}

function panelTop() {
  return Number.parseFloat(screen.getByTestId("stack-panel-anchor").style.top);
}

function publishedEdges() {
  const { topPx, bottomPx } = useTargetingPromptStore.getState();
  return { top: topPx, bottom: bottomPx };
}

describe("StackDisplay under a targeting prompt", () => {
  beforeEach(() => {
    setViewport(VIEWPORT_WIDTH, VIEWPORT_HEIGHT);
    resizeObservers.length = 0;
    vi.stubGlobal("ResizeObserver", ControlledResizeObserver);
    useGameStore.getState().reset();
    usePreferencesStore.setState(usePreferencesStore.getInitialState());
    useTargetingPromptStore.setState(useTargetingPromptStore.getInitialState());
    act(() => {
      useMultiplayerStore.setState({ activePlayerId: 0, isSpectator: false, playerNames: new Map() });
    });
  });

  afterEach(() => {
    cleanup();
    vi.restoreAllMocks();
    vi.unstubAllGlobals();
    setViewport(originalViewport.width, originalViewport.height);
  });

  it("starts the panel below a shown prompt and returns it when the prompt ends", () => {
    mockPromptEdges(() => FOCUSED_PROMPT);
    seed();
    const view = render(board());
    expect(screen.queryByText(DESCRIPTION)).not.toBeInTheDocument();
    expect(panelTop()).toBeCloseTo(UNCOVERED_TOP);

    seed({ prompt: true });
    expect(screen.getByText(DESCRIPTION)).toBeInTheDocument();
    expect(publishedEdges()).toEqual(FOCUSED_PROMPT);
    expect(panelTop()).toBe(FOCUSED_PROMPT.bottom + PROMPT_GAP);

    // The prompt ends inside the overlay, which then renders nothing.
    seed();
    expect(screen.queryByText(DESCRIPTION)).not.toBeInTheDocument();
    expect(publishedEdges()).toEqual({ top: 0, bottom: 0 });
    expect(panelTop()).toBeCloseTo(UNCOVERED_TOP);

    // The prompt ends with GamePage's gate unmounting the whole overlay.
    seed({ prompt: true });
    expect(panelTop()).toBe(FOCUSED_PROMPT.bottom + PROMPT_GAP);
    view.rerender(board({ overlayMounted: false }));
    expect(screen.queryByText(DESCRIPTION)).not.toBeInTheDocument();
    expect(publishedEdges()).toEqual({ top: 0, bottom: 0 });
    expect(panelTop()).toBeCloseTo(UNCOVERED_TOP);
  });

  it("starts a left-docked panel below the prompt too", () => {
    mockPromptEdges(() => FOCUSED_PROMPT);
    usePreferencesStore.setState({ stackDockSide: "left" });
    seed({ prompt: true });
    render(board());

    const anchor = screen.getByTestId("stack-panel-anchor");
    expect(anchor.style.left).not.toBe("");
    expect(anchor.style.right).toBe("");
    expect(panelTop()).toBe(FOCUSED_PROMPT.bottom + PROMPT_GAP);
  });

  it("leaves a panel that already sits below the prompt where it is", () => {
    // At 844x700 the same 273px panel centres on 43% of the height, at
    // 164.5px, well below the prompt fixture's 64px bottom edge.
    const lowerTop = 700 * 0.43 - 273 / 2;
    setViewport(VIEWPORT_WIDTH, 700);
    mockPromptEdges(() => FOCUSED_PROMPT);
    seed();
    render(board());
    expect(panelTop()).toBeCloseTo(lowerTop);

    seed({ prompt: true });
    expect(publishedEdges()).toEqual(FOCUSED_PROMPT);
    expect(panelTop()).toBeCloseTo(lowerTop);
  });

  it("moves a deep stack pinned at the top inset below the prompt", () => {
    // Ten entries make a 436px panel (112x156 cards, the size floor, and a
    // 24px stagger), taller than the viewport, so with no prompt it pins at
    // the 12px top inset and its pile scrolls.
    mockPromptEdges(() => FOCUSED_PROMPT);
    seed({ entryCount: 10 });
    render(board());
    expect(panelTop()).toBe(12);

    seed({ prompt: true, entryCount: 10 });
    expect(panelTop()).toBe(FOCUSED_PROMPT.bottom + PROMPT_GAP);
  });

  it("follows the prompt as its description expands and collapses", () => {
    // The caption clips, so the prompt offers to show the full description.
    vi.spyOn(HTMLDivElement.prototype, "clientWidth", "get").mockReturnValue(100);
    vi.spyOn(HTMLDivElement.prototype, "scrollWidth", "get").mockReturnValue(120);
    const expandedBottom = 160;
    mockPromptEdges((block) => ({
      top: FOCUSED_PROMPT.top,
      bottom: block.querySelector("#targeting-description-panel") ? expandedBottom : FOCUSED_PROMPT.bottom,
    }));
    seed({ prompt: true });
    render(board());
    expect(panelTop()).toBe(FOCUSED_PROMPT.bottom + PROMPT_GAP);

    // Below the expanded prompt the 273px panel no longer fits above the
    // bottom inset, so it pins under the prompt with its header in view and
    // its pile scrolls in the room left above the inset.
    fireEvent.click(screen.getByRole("button", { name: `Show the full description: ${DESCRIPTION}` }));
    expect(panelTop()).toBe(expandedBottom + PROMPT_GAP);

    fireEvent.click(screen.getByRole("button", { name: `Show less: ${DESCRIPTION}` }));
    expect(panelTop()).toBe(FOCUSED_PROMPT.bottom + PROMPT_GAP);
  });

  it("follows a resize of the prompt that happens without a render", () => {
    let promptBottom = FOCUSED_PROMPT.bottom;
    mockPromptEdges(() => ({ top: FOCUSED_PROMPT.top, bottom: promptBottom }));
    seed({ prompt: true });
    render(board());
    expect(panelTop()).toBe(FOCUSED_PROMPT.bottom + PROMPT_GAP);

    // A narrower viewport wraps the instruction onto a second line. The
    // overlay does not re-render, so only the prompt block's own observer can
    // see it grow; firing just that one keeps any other observer's commit out
    // of the test.
    const promptObservers = resizeObservers.filter((observer) =>
      observer.targets.some((target) => target.hasAttribute("data-targeting-prompt")),
    );
    expect(promptObservers).toHaveLength(1);
    promptBottom = 84;
    act(() => promptObservers[0].callback([], {} as ResizeObserver));
    expect(panelTop()).toBe(84 + PROMPT_GAP);
  });

  it("follows the prompt when the page moves it without changing its size", () => {
    // Switching the board to the split layout makes GamePage move the prompt
    // down through `--game-targeting-prompt-top`. The block keeps its size, so
    // no observer fires; only the overlay re-rendering with the page sees it.
    let promptEdges = FOCUSED_PROMPT;
    mockPromptEdges(() => promptEdges);
    seed({ prompt: true, seatCount: 3 });
    const view = render(board());
    expect(panelTop()).toBe(FOCUSED_PROMPT.bottom + PROMPT_GAP);

    promptEdges = SPLIT_PROMPT;
    view.rerender(board({ layout: "split" }));
    expect(publishedEdges()).toEqual(SPLIT_PROMPT);
    expect(panelTop()).toBe(SPLIT_PROMPT.bottom + PROMPT_GAP);
  });

  // In the split layout the prompt starts 68px down, below the seat panes. Six
  // or seven entries make a 340px or 364px panel, held up so its bottom keeps
  // the 12px inset; ten make a 436px panel that cannot fit, so it pins at the
  // 12px top inset; two entries centre on 58% of the height, at 89.7px. A
  // header (36px) that ends at least 8px above the prompt stays where it is;
  // any other starts 8px below the prompt.
  it.each([
    ["is pinned at the top inset, well clear of", 10, 390, 12, 12],
    ["ends with room to spare above", 7, 390, 14, 14],
    ["ends exactly the gap above", 7, 400, 24, 24],
    ["ends inside the gap above", 7, 404, 28, SPLIT_PROMPT.bottom + PROMPT_GAP],
    ["overlaps the top of", 6, 390, 38, SPLIT_PROMPT.bottom + PROMPT_GAP],
    ["sits under", 2, 390, 89.7, SPLIT_PROMPT.bottom + PROMPT_GAP],
  ] as const)(
    "split layout: a header that %s the prompt",
    (_case, entryCount, viewportHeight, uncoveredTop, promptedTop) => {
      setViewport(VIEWPORT_WIDTH, viewportHeight);
      mockPromptEdges(() => SPLIT_PROMPT);
      seed({ entryCount, seatCount: 3 });
      render(board({ layout: "split" }));
      expect(panelTop()).toBeCloseTo(uncoveredTop);

      seed({ prompt: true, entryCount, seatCount: 3 });
      expect(publishedEdges()).toEqual(SPLIT_PROMPT);
      expect(panelTop()).toBeCloseTo(promptedTop);
    },
  );
});
