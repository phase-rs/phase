import { cleanup, fireEvent, render, screen } from "@testing-library/react";
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

function setWaitingFor(waitingFor: WaitingFor, objects: Record<string, GameObject>) {
  const state = buildGameState({
    players: [buildPlayer({ id: 0, library: [42, 43] }), buildPlayer({ id: 1 })],
    objects,
    waiting_for: waitingFor,
    next_object_id: 100,
  });
  useGameStore.setState({
    gameMode: "online",
    gameState: state,
    waitingFor,
  });
}

const DIG_BOTTOM: WaitingFor = {
  type: "DigBottomOrder",
  data: {
    player: 0,
    library_owner: 0,
    cards: [42, 43],
    source_id: 7,
  },
};

describe("DigBottomOrder modal", () => {
  beforeEach(() => {
    dispatchMock.mockClear();
    useMultiplayerStore.setState({ activePlayerId: 0 });
  });

  afterEach(() => {
    cleanup();
  });

  it("dispatches SelectCards with the offered order on confirm", () => {
    setWaitingFor(DIG_BOTTOM, {
      42: makeObject(42, "Spell B"),
      43: makeObject(43, "Spell C"),
    });

    render(<CardChoiceModal />);

    expect(screen.getByText(/looked-at cards on the bottom/i)).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "Confirm" }));
    expect(dispatchMock).toHaveBeenCalledWith({
      type: "SelectCards",
      data: { cards: [42, 43] },
    });
  });

  it("does not render when the local player is not the acting player", () => {
    useMultiplayerStore.setState({ activePlayerId: 1 });
    setWaitingFor(DIG_BOTTOM, {
      42: makeObject(42, "Spell B"),
      43: makeObject(43, "Spell C"),
    });

    render(<CardChoiceModal />);

    expect(screen.queryByText(/looked-at cards on the bottom/i)).not.toBeInTheDocument();
    expect(dispatchMock).not.toHaveBeenCalled();
  });
});
