import { cleanup, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { GameObject, WaitingFor } from "../../../adapter/types.ts";
import { useGameStore } from "../../../stores/gameStore.ts";
import { useMultiplayerStore } from "../../../stores/multiplayerStore.ts";
import { buildGameObject } from "../../../test/factories/gameObjectFactory.ts";
import { buildGameState, buildPlayer } from "../../../test/factories/gameStateFactory.ts";
import { CardChoiceModal } from "../CardChoiceModal.tsx";

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

describe("SearchChoice modal", () => {
  beforeEach(() => {
    dispatchMock.mockClear();
    useMultiplayerStore.setState({ activePlayerId: 0 });
  });

  afterEach(() => {
    cleanup();
  });

  it("shows every card the engine exposed during a library search", () => {
    const waitingFor: WaitingFor = {
      type: "SearchChoice",
      data: { player: 0, cards: [42], count: 1 },
    };
    const state = buildGameState({
      players: [buildPlayer({ id: 0, library: [42, 43] }), buildPlayer({ id: 1 })],
      objects: {
        42: makeObject(42, "Eligible Card"),
        43: makeObject(43, "Ineligible Card"),
      },
      waiting_for: waitingFor,
      active_library_searches: {
        0: {
          searcher: 0,
          searched_zone_owner: 0,
          effective_library_owner: 0,
          learned_audience: [0],
          looked_at: [
            [0, "Library", { object_id: 42, incarnation: 0 }],
            [0, "Library", { object_id: 43, incarnation: 0 }],
          ],
        },
      },
    });
    useGameStore.setState({ gameMode: "online", gameState: state, waitingFor });

    render(<CardChoiceModal />);

    expect(screen.getByLabelText(/Eligible Card/)).toBeInTheDocument();
    const ineligibleCard = screen.getByLabelText(/Ineligible Card/);
    expect(ineligibleCard.closest("button")).toBeDisabled();
  });
});
