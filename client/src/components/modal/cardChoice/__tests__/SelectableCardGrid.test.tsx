import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import type { GameObject, ObjectId } from "../../../../adapter/types.ts";
import SelectableCardGrid from "../SelectableCardGrid.tsx";

function obj(id: number, name: string): GameObject {
  return {
    id,
    name,
    zone: "Hand",
    card_types: { supertypes: [], core_types: ["Creature"], subtypes: [] },
    mana_cost: { type: "Cost", shards: [], generic: id },
    color: [],
  } as unknown as GameObject;
}

const objects: Record<ObjectId, GameObject> = { 1: obj(1, "Alpha"), 2: obj(2, "Bravo"), 3: obj(3, "Cosmo") };
const cards: ObjectId[] = [1, 2, 3];
const tone = { ring: "ring-red-400/80", overlay: "bg-red-500/20", badge: "bg-red-500/90" };

function setup(
  value: Set<ObjectId>,
  cap: number,
  onChange = vi.fn(),
  opts: { onConfirm?: () => void; canConfirm?: boolean } = {},
) {
  render(
    <SelectableCardGrid
      cards={cards}
      objects={objects}
      value={value}
      onChange={onChange}
      cap={cap}
      tone={tone}
      badgeLabel="Discard"
      counterText={`Discard ${value.size} of ${cap}`}
      hoverProps={() => ({})}
      onConfirm={opts.onConfirm}
      canConfirm={opts.canConfirm}
    />,
  );
  return onChange;
}

afterEach(cleanup);

describe("SelectableCardGrid toolbar + keyboard", () => {
  it("select-all fills to cap in display order", () => {
    const onChange = setup(new Set(), 2);
    fireEvent.click(screen.getByRole("button", { name: "Select all" }));
    expect(onChange).toHaveBeenCalledWith(new Set([1, 2]));
  });

  it("invert takes the capped complement", () => {
    const onChange = setup(new Set([1]), 2);
    fireEvent.click(screen.getByRole("button", { name: "Invert" }));
    expect(onChange).toHaveBeenCalledWith(new Set([2, 3]));
  });

  it("clear empties the selection", () => {
    const onChange = setup(new Set([1, 2]), 2);
    fireEvent.click(screen.getByRole("button", { name: "Clear" }));
    expect(onChange).toHaveBeenCalledWith(new Set());
  });

  it("shift-click selects an inclusive range up to cap", () => {
    const onChange = setup(new Set(), 3);
    fireEvent.click(screen.getByRole("button", { name: /Alpha/i }));          // anchor idx 0
    fireEvent.click(screen.getByRole("button", { name: /Cosmo/i }), { shiftKey: true }); // idx 2
    expect(onChange).toHaveBeenLastCalledWith(new Set([1, 2, 3]));
  });

  it("'a' key selects all, 'c' clears", () => {
    const onChange = setup(new Set(), 2);
    const grid = screen.getByRole("status").parentElement as HTMLElement;
    fireEvent.keyDown(grid, { key: "a" });
    expect(onChange).toHaveBeenCalledWith(new Set([1, 2]));
    fireEvent.keyDown(grid, { key: "c" });
    expect(onChange).toHaveBeenCalledWith(new Set());
  });
});

describe("SelectableCardGrid core", () => {
  it("renders one tile per card and a live counter", () => {
    setup(new Set(), 2);
    expect(screen.getByRole("button", { name: /Alpha/i })).toBeInTheDocument();
    expect(screen.getByRole("status")).toHaveTextContent("Discard 0 of 2");
  });

  it("toggles a card on click", () => {
    const onChange = setup(new Set(), 2);
    fireEvent.click(screen.getByRole("button", { name: /Bravo/i }));
    expect(onChange).toHaveBeenCalledWith(new Set([2]));
  });

  it("blocks adding beyond the cap", () => {
    const onChange = setup(new Set([1, 2]), 2);
    fireEvent.click(screen.getByRole("button", { name: /Cosmo/i }));
    expect(onChange).not.toHaveBeenCalled();
  });

  it("always allows deselecting an already-selected card at cap", () => {
    const onChange = setup(new Set([1, 2]), 2);
    fireEvent.click(screen.getByRole("button", { name: /Alpha/i }));
    expect(onChange).toHaveBeenCalledWith(new Set([2]));
  });
});

describe("SelectableCardGrid Enter-to-confirm", () => {
  it("confirms on Enter only when the grid container itself is focused", () => {
    const onConfirm = vi.fn();
    setup(new Set([1, 2]), 2, vi.fn(), { onConfirm, canConfirm: true });
    const grid = screen.getByRole("status").parentElement as HTMLElement;
    fireEvent.keyDown(grid, { key: "Enter" });
    expect(onConfirm).toHaveBeenCalledTimes(1);
  });

  it("does not confirm on Enter while a child button is focused", () => {
    // Native button activation (e.target) must win; the container's confirm
    // must not shadow Enter when a toolbar/card button holds focus.
    const onConfirm = vi.fn();
    setup(new Set([1, 2]), 2, vi.fn(), { onConfirm, canConfirm: true });
    fireEvent.keyDown(screen.getByRole("button", { name: "Select all" }), { key: "Enter" });
    expect(onConfirm).not.toHaveBeenCalled();
  });
});

describe("SelectableCardGrid shift-anchor reset", () => {
  it("clears the shift anchor when the card list changes, so no range spans a reordered list", () => {
    const onChange = vi.fn();
    const { rerender } = render(
      <SelectableCardGrid
        cards={[1, 2, 3]}
        objects={objects}
        value={new Set()}
        onChange={onChange}
        cap={3}
        tone={tone}
        badgeLabel="Discard"
        counterText="Discard 0 of 3"
        hoverProps={() => ({})}
      />,
    );
    fireEvent.click(screen.getByRole("button", { name: /Alpha/i })); // anchor at idx 0 (id 1)
    // The hand mutates (Alpha leaves); the effect must drop the stale anchor.
    rerender(
      <SelectableCardGrid
        cards={[2, 3]}
        objects={objects}
        value={new Set()}
        onChange={onChange}
        cap={3}
        tone={tone}
        badgeLabel="Discard"
        counterText="Discard 0 of 2"
        hoverProps={() => ({})}
      />,
    );
    onChange.mockClear();
    // With the anchor reset, this shift-click is a plain toggle ({3}); without
    // the reset it would anchor on the stale idx 0 and add the range {2,3}.
    fireEvent.click(screen.getByRole("button", { name: /Cosmo/i }), { shiftKey: true });
    expect(onChange).toHaveBeenLastCalledWith(new Set([3]));
  });
});
