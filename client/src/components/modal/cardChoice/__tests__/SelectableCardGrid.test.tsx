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

function setup(value: Set<ObjectId>, cap: number, onChange = vi.fn()) {
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
    />,
  );
  return onChange;
}

afterEach(cleanup);

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
