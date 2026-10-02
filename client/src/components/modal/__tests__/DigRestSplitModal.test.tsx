import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { GameObject, WaitingFor } from "../../../adapter/types.ts";
import { useGameStore } from "../../../stores/gameStore.ts";
import { buildGameObject } from "../../../test/factories/gameObjectFactory.ts";
import { buildGameState, buildPlayer } from "../../../test/factories/gameStateFactory.ts";
import { DigRestSplitModal } from "../cardChoice/libraryModals.tsx";

const dispatchMock = vi.fn();

vi.mock("../../../hooks/useGameDispatch.ts", () => ({
  useGameDispatch: () => dispatchMock,
}));

function makeObject(id: number, name: string): GameObject {
  return buildGameObject({
    id,
    card_id: id,
    zone: "Library",
    name,
    card_types: { supertypes: [], core_types: ["Instant"], subtypes: [] },
    mana_cost: { type: "Cost", shards: [], generic: 1 },
    timestamp: id,
  });
}

const OBJECTS: Record<string, GameObject> = {
  41: makeObject(41, "Alpha"),
  42: makeObject(42, "Bravo"),
  43: makeObject(43, "Cosmo"),
};

// Renders the production component with the exact payload shape the engine
// parks, so the controls under test are the real ones and Confirm goes through
// the real `useGameDispatch` seam.
function mount(data: Extract<WaitingFor, { type: "DigRestSplitChoice" }>["data"]) {
  const waitingFor: WaitingFor = { type: "DigRestSplitChoice", data };
  useGameStore.setState({
    gameMode: "online",
    gameState: buildGameState({
      players: [buildPlayer({ id: 0, library: data.cards }), buildPlayer({ id: 1 })],
      objects: OBJECTS,
      waiting_for: waitingFor,
      next_object_id: 100,
    }),
    waitingFor,
  });
  render(<DigRestSplitModal data={data} />);
}

/** The full permutation the modal dispatched on Confirm. */
function submittedArrangement(): number[] {
  fireEvent.click(screen.getByRole("button", { name: /confirm/i }));
  const calls = dispatchMock.mock.calls;
  const call = calls[calls.length - 1]?.[0];
  expect(call?.type).toBe("SelectCards");
  return call.data.cards as number[];
}

describe("DigRestSplitModal keyboard/button reordering", () => {
  beforeEach(() => {
    dispatchMock.mockClear();
  });

  afterEach(() => cleanup());

  // BLOCKER 2 (a): a keyboard-only player can change WHICH cards are in the
  // top pile. Telling Time's two legal partitions for a two-card remainder are
  // [A,B] and [B,A]; reaching the second one required a drag before this fix.
  it("moving a card across the top/bottom boundary changes the submitted partition", () => {
    mount({
      player: 0,
      library_owner: 0,
      cards: [41, 42],
      top_count: 1,
      bottom_count: 1,
      scope: "partition_and_order",
      source_id: 99,
    });

    // Reach guard: with no interaction the default arrangement is submitted.
    expect(submittedArrangement()).toEqual([41, 42]);
    dispatchMock.mockClear();

    // Move "Bravo" (index 1, bottom pile) one slot earlier — across the
    // boundary and onto the top.
    fireEvent.click(screen.getByRole("button", { name: "Move Bravo earlier" }));
    expect(submittedArrangement()).toEqual([42, 41]);
  });

  // BLOCKER 2 (b): a keyboard-only player can reorder WITHIN a pile of 2+
  // cards — the CR 401.4 arrangement choice.
  it("arrow keys reorder within a pile", () => {
    mount({
      player: 0,
      library_owner: 0,
      cards: [41, 42, 43],
      top_count: 1,
      bottom_count: 2,
      scope: "partition_and_order",
      source_id: 99,
    });

    // Focus lives on a per-card control; ArrowRight moves that card later.
    const cosmoEarlier = screen.getByRole("button", { name: "Move Cosmo earlier" });
    fireEvent.keyDown(cosmoEarlier, { key: "ArrowLeft" });
    expect(submittedArrangement()).toEqual([41, 43, 42]);
  });

  // CR 401.4 + the `order_only` scope: the partition is another player's spent
  // decision, so boundary-crossing controls are disabled while within-pile
  // reordering stays available.
  it("an order-only prompt disables boundary crossing but still reorders within a pile", () => {
    mount({
      player: 1,
      library_owner: 1,
      cards: [41, 42, 43],
      top_count: 1,
      bottom_count: 2,
      scope: "order_only",
      source_id: 99,
    });

    // Bravo is the first BOTTOM card; moving it earlier would put it on top.
    expect(screen.getByRole("button", { name: "Move Bravo earlier" })).toBeDisabled();
    // Alpha is the only TOP card; moving it later would push it to the bottom.
    expect(screen.getByRole("button", { name: "Move Alpha later" })).toBeDisabled();

    // Within the bottom pile the owner's CR 401.4 choice is still live.
    fireEvent.click(screen.getByRole("button", { name: "Move Cosmo earlier" }));
    expect(submittedArrangement()).toEqual([41, 43, 42]);
  });

  // NON-BLOCKING 2: the bottom-pile count in the subtitle is engine-supplied,
  // not derived from `cards.length - top_count` in the component.
  it("renders the engine-supplied bottom count rather than deriving one", () => {
    mount({
      player: 0,
      library_owner: 0,
      cards: [41, 42, 43],
      top_count: 1,
      // Deliberately NOT `cards.length - top_count`: if the component still
      // derived the value it would render 2 and this assertion would fail.
      bottom_count: 7,
      scope: "partition_and_order",
      source_id: 99,
    });
    expect(screen.getByText(/the other 7 go on the bottom/i)).toBeInTheDocument();
  });
});
