import { act, type ComponentProps, type ReactNode } from "react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen, within } from "@testing-library/react";
import type { DragControls } from "framer-motion";

import type { ReplacementCandidateSummary } from "../../../adapter/types.ts";
import { useGameStore, type GameStore } from "../../../stores/gameStore.ts";
import { useUiStore } from "../../../stores/uiStore.ts";
import { gameStateFactory } from "../../../test/factories/gameStateFactory.ts";
import { setGameStoreForTest } from "../../../test/helpers/gameStoreHelpers.ts";
import { ReplacementModal } from "../ReplacementModal.tsx";

const dispatchMock = vi.fn<GameStore["dispatch"]>().mockResolvedValue([]);

interface ItemDragProps {
  dragListener?: boolean;
  dragControls?: DragControls;
}

/** The drag props each row last handed `Reorder.Item`, keyed by its value. */
const itemDragProps = new Map<number, ItemDragProps>();

/** The last `onReorder` handler `Reorder.Group` was rendered with. */
let latestOnReorder: ((next: number[]) => void) | null = null;

/**
 * A real framer-motion drag has no happy-dom equivalent (it needs layout
 * geometry), so the drag is exercised at its contracts, as in
 * `DigRestSplitModalDrag.test.tsx`: `Reorder.Item` records the drag props each
 * row gives it, and `Reorder.Group` keeps the `onReorder` it calls with each new
 * order as a drag moves a row past another. The stand-in item renders every DOM
 * prop the row passes, so a pointer handler on the row element still fires.
 */
vi.mock("framer-motion", async (importOriginal) => {
  const actual = await importOriginal<typeof import("framer-motion")>();
  const Group = ({
    onReorder,
    children,
    className,
  }: {
    onReorder: (next: number[]) => void;
    children: ReactNode;
    className?: string;
  }) => {
    latestOnReorder = onReorder;
    return <ol className={className}>{children}</ol>;
  };
  const Item = ({
    value,
    dragListener,
    dragControls,
    as: _as,
    whileDrag: _whileDrag,
    children,
    ...domProps
  }: { value: number; as?: string; whileDrag?: unknown } & ItemDragProps & ComponentProps<"li">) => {
    itemDragProps.set(value, { dragListener, dragControls });
    return <li {...domProps}>{children}</li>;
  };
  return { ...actual, Reorder: { Group, Item } };
});

const THREE_EFFECTS: ReplacementCandidateSummary[] = [
  { source_id: 10, source_name: "First source", description: "Double" },
  { source_id: 11, source_name: "Second source", description: "Add one" },
  { source_id: 12, source_name: "Third source", description: "Add three" },
];

/** An ordering prompt over `candidates`; rows start in candidate order. */
function renderOrderingPrompt(candidates = THREE_EFFECTS) {
  setGameStoreForTest({
    gameState: gameStateFactory.replacementChoice({ candidates }).build(),
    dispatch: dispatchMock,
  });
  render(<ReplacementModal />);
}

/** Hands the list a new order, as `Reorder.Group` does during a drag. */
function reorderTo(next: number[]) {
  act(() => {
    if (!latestOnReorder) throw new Error("Reorder.Group has no onReorder");
    latestOnReorder(next);
  });
}

/** The rows' grip names, top to bottom: the order the list shows. */
function gripNamesInOrder() {
  return screen
    .getAllByRole("img", { name: /^Drag .+ to reorder$/ })
    .map((grip) => grip.getAttribute("aria-label"));
}

beforeEach(() => {
  useGameStore.getState().reset();
  useUiStore.setState({ inspectedObjectId: null, hoveredObjectId: null, previewSticky: false });
  dispatchMock.mockClear();
  itemDragProps.clear();
  latestOnReorder = null;
});
afterEach(() => {
  vi.useRealTimers();
  cleanup();
});

describe("ReplacementModal ordering rows (#11007)", () => {
  it("leaves every row free to scroll on touch, with only its grip claiming the gesture", () => {
    renderOrderingPrompt();
    const rows = screen.getAllByRole("listitem");
    expect(rows).toHaveLength(3);

    for (const [candidateIndex, row] of rows.entries()) {
      // A row framer listens to itself gets an inline `touch-action`, and a
      // `touch-none` class does the same: either one stops a swipe that
      // starts there from scrolling the dialog.
      expect(itemDragProps.get(candidateIndex)?.dragListener).toBe(false);
      expect(row).not.toHaveClass("touch-none");
      expect(row).not.toHaveClass("!touch-none");

      const grip = within(row).getByRole("img", { name: /^Drag .+ to reorder$/ });
      expect(grip).toHaveClass("touch-none", "min-h-11", "min-w-11");
      expect([...row.querySelectorAll('[class*="touch-none"]')]).toEqual([grip]);
    }
  });

  it("starts only the pressed grip's own row drag, and nothing from the rest of a row", () => {
    renderOrderingPrompt();
    const starts = [0, 1, 2].map((candidateIndex) => {
      const controls = itemDragProps.get(candidateIndex)?.dragControls;
      if (!controls) throw new Error(`row ${candidateIndex} has no drag controls`);
      return vi.spyOn(controls, "start");
    });
    const startCounts = () => starts.map((start) => start.mock.calls.length);
    const row = screen.getAllByRole("listitem")[1];

    fireEvent.pointerDown(within(row).getByRole("img", { name: "Drag Add one to reorder" }));
    expect(startCounts()).toEqual([0, 1, 0]);

    fireEvent.pointerDown(within(row).getByText("Add one"));
    fireEvent.pointerDown(within(row).getByText("Second source"));
    fireEvent.pointerDown(row);
    expect(startCounts()).toEqual([0, 1, 0]);
  });

  it("opens no card preview from a touch held on a grip", () => {
    vi.useFakeTimers({ toFake: ["setTimeout", "clearTimeout"] });
    renderOrderingPrompt();
    const row = screen.getAllByRole("listitem")[1];
    const touch = { button: 0, clientX: 10, clientY: 10, isPrimary: true, pointerType: "touch" };

    fireEvent.pointerDown(
      within(row).getByRole("img", { name: "Drag Add one to reorder" }),
      { ...touch, pointerId: 1 },
    );
    act(() => vi.advanceTimersByTime(500));
    expect(useUiStore.getState().previewSticky).toBe(false);

    // Reach guard: the same touch held on the row's text opens its card.
    fireEvent.pointerDown(within(row).getByText("Add one"), { ...touch, pointerId: 2 });
    act(() => vi.advanceTimersByTime(500));
    expect(useUiStore.getState().inspectedObjectId).toBe(11);
    expect(useUiStore.getState().previewSticky).toBe(true);
  });

  it("submits the order Reorder.Group reports", () => {
    renderOrderingPrompt();
    reorderTo([2, 0, 1]);

    expect(gripNamesInOrder()).toEqual([
      "Drag Add three to reorder",
      "Drag Double to reorder",
      "Drag Add one to reorder",
    ]);
    fireEvent.click(screen.getByRole("button", { name: "Confirm Order" }));
    expect(dispatchMock).toHaveBeenCalledWith({ type: "ChooseReplacement", data: { index: 2 } });
  });

  it("moves rows with the arrow buttons", () => {
    renderOrderingPrompt();
    fireEvent.click(screen.getByRole("button", { name: "Move Double later" }));
    fireEvent.click(screen.getByRole("button", { name: "Move Add three earlier" }));

    expect(gripNamesInOrder()).toEqual([
      "Drag Add one to reorder",
      "Drag Add three to reorder",
      "Drag Double to reorder",
    ]);
    fireEvent.click(screen.getByRole("button", { name: "Confirm Order" }));
    expect(dispatchMock).toHaveBeenCalledWith({ type: "ChooseReplacement", data: { index: 1 } });
  });

  it("keeps each row's arrows and position on its place in the list, not its effect's index", () => {
    renderOrderingPrompt();
    reorderTo([2, 0, 1]);

    expect(screen.getByRole("button", { name: "Move Add three earlier" })).toBeDisabled();
    expect(screen.getByRole("button", { name: "Move Add three later" })).toBeEnabled();
    expect(screen.getByRole("button", { name: "Move Double earlier" })).toBeEnabled();
    expect(screen.getByRole("button", { name: "Move Add one later" })).toBeDisabled();

    fireEvent.click(screen.getByRole("button", { name: "Move Double later" }));
    expect(gripNamesInOrder()).toEqual([
      "Drag Add three to reorder",
      "Drag Add one to reorder",
      "Drag Double to reorder",
    ]);
    const [firstRow, , lastRow] = screen.getAllByRole("listitem");
    expect(within(firstRow).getByText("Position 1 of 3")).toBeInTheDocument();
    expect(within(firstRow).getByText("Add three")).toBeInTheDocument();
    expect(within(lastRow).getByText("Position 3 of 3")).toBeInTheDocument();
    expect(within(lastRow).getByText("Double")).toBeInTheDocument();
  });

  it("points the drag hint at the grip, with the glyph hidden from screen readers", () => {
    renderOrderingPrompt();
    const glyph = screen.getByText("⠿", { selector: "p > span" });
    expect(glyph).toHaveAttribute("aria-hidden", "true");
    expect(glyph.parentElement).toHaveTextContent("Drag the ⠿ handle to reorder, or use the arrows.");
  });

  it("names a row's controls in the text its mana symbols read as", () => {
    renderOrderingPrompt([
      { source_id: 10, source_name: "First source", description: "Add {G}" },
      { source_id: 11, source_name: "Second source", description: "Double" },
    ]);

    expect(screen.getByRole("img", { name: "Drag Add G to reorder" })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Move Add G earlier" })).toBeDisabled();
    expect(screen.getByRole("button", { name: "Move Add G later" })).toBeEnabled();
  });
});
