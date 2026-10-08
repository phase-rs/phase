import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import type { ComponentProps, ReactNode } from "react";
import { act } from "react";
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

/**
 * The last `onReorder` callback `Reorder.Group` was rendered with.
 *
 * A real framer-motion drag has no jsdom equivalent (it needs pointer capture
 * and layout geometry), so the drag seam is exercised at its CONTRACT instead:
 * `Reorder.Group` hands the handler a whole candidate permutation. Calling it
 * with a boundary-crossing permutation is exactly what a cross-pile drag
 * produces, and it is the only input the handler under test ever sees.
 */
let latestOnReorder: ((next: number[]) => void) | null = null;

vi.mock("framer-motion", async (importOriginal) => {
  const actual = await importOriginal<typeof import("framer-motion")>();
  const Group = ({
    onReorder,
    children,
    className,
  }: {
    values: number[];
    onReorder: (next: number[]) => void;
    children: ReactNode;
  } & ComponentProps<"div">) => {
    latestOnReorder = onReorder;
    return <div className={className}>{children}</div>;
  };
  const Item = ({ children, className, onKeyDown }: ComponentProps<"div">) => (
    <div className={className} onKeyDown={onKeyDown}>
      {children}
    </div>
  );
  return { ...actual, Reorder: { Group, Item } };
});

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
  44: makeObject(44, "Delta"),
};

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

function drag(next: number[]) {
  act(() => {
    latestOnReorder?.(next);
  });
}

describe("DigRestSplitModal drag constraint", () => {
  beforeEach(() => {
    dispatchMock.mockClear();
    latestOnReorder = null;
  });

  afterEach(() => cleanup());

  // NON-BLOCKING 2: the move buttons already refuse to cross the CR 401.4
  // boundary during `order_only`; drag did not, so a player could assemble an
  // arrangement the engine can only reject.
  it("drops a drag that would move a card across the boundary during order_only", () => {
    mount({
      player: 1,
      library_owner: 1,
      cards: [41, 42, 43, 44],
      top_count: 2,
      bottom_count: 2,
      scope: "order_only",
      source_id: 99,
    });

    // Reach guard: the handler IS wired and a legal within-pile drag lands, so
    // the negative below cannot pass merely because nothing is hooked up.
    drag([42, 41, 43, 44]);
    expect(submittedArrangement()).toEqual([42, 41, 43, 44]);
    dispatchMock.mockClear();

    // THE REGRESSION ASSERTION: dragging a bottom card (43) onto the top pile
    // changes the settled partition, so it is refused and the arrangement is
    // unchanged.
    drag([42, 43, 41, 44]);
    expect(submittedArrangement()).toEqual([42, 41, 43, 44]);
  });

  // PAIRED POSITIVE: with the partition still open the drag must be free —
  // this is the only prompt where crossing the boundary IS the decision
  // (CR 608.2d), so the clamp must not fire there.
  it("accepts a boundary-crossing drag when the partition is still open", () => {
    mount({
      player: 0,
      library_owner: 0,
      cards: [41, 42, 43, 44],
      top_count: 2,
      bottom_count: 2,
      scope: "partition_and_order",
      source_id: 99,
    });

    drag([43, 44, 41, 42]);
    expect(submittedArrangement()).toEqual([43, 44, 41, 42]);
  });

  // PAIRED POSITIVE: `partition_only` is also an open partition (it is the
  // chooser's own prompt), so it must behave like `partition_and_order` and
  // not like `order_only`.
  it("accepts a boundary-crossing drag during partition_only", () => {
    mount({
      player: 0,
      library_owner: 1,
      cards: [41, 42, 43, 44],
      top_count: 2,
      bottom_count: 2,
      scope: "partition_only",
      source_id: 99,
    });

    drag([43, 41, 42, 44]);
    expect(submittedArrangement()).toEqual([43, 41, 42, 44]);
  });
});
