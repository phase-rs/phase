import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { GameObject, ViewerInteraction, WaitingFor } from "../../../adapter/types.ts";
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

function setWaitingFor(
  waitingFor: WaitingFor,
  objects: Record<string, GameObject>,
  viewerInteraction?: ViewerInteraction,
) {
  const state = buildGameState({
    players: [buildPlayer({ id: 0, library: [10, 11] }), buildPlayer({ id: 1 })],
    objects,
    waiting_for: waitingFor,
    next_object_id: 100,
  });
  useGameStore.setState({
    gameMode: "online",
    gameState: state,
    waitingFor,
    viewerInteraction,
  });
}

function selectInteraction(id: string): ViewerInteraction {
  return {
    opportunities: [
      {
        interactionId: id,
        response: { type: "schema", data: { spec: { type: "select" } } },
      },
    ],
  } as unknown as ViewerInteraction;
}

describe("RevealUntilBottomOrderModal", () => {
  beforeEach(() => {
    dispatchMock.mockClear();
    useMultiplayerStore.setState({ activePlayerId: 0 });
  });

  afterEach(() => {
    cleanup();
  });

  it("renders cards and dispatches SelectCards with ordered cards on confirm", () => {
    setWaitingFor(
      {
        type: "RevealUntilBottomOrder",
        data: {
          player: 0,
          source_id: 1,
          cards: [10, 11],
        },
      },
      {
        10: makeObject(10, "Lightning Bolt"),
        11: makeObject(11, "Counterspell"),
      },
    );

    render(<CardChoiceModal />);

    expect(screen.getByText(/Order the rest on the bottom/i)).toBeInTheDocument();
    expect(screen.getByText(/Put 2 revealed cards on the bottom of your library/i)).toBeInTheDocument();
    expect(screen.getByLabelText(/Lightning Bolt/i)).toBeInTheDocument();
    expect(screen.getByLabelText(/Counterspell/i)).toBeInTheDocument();

    const confirmButton = screen.getByRole("button", { name: /Done|Confirm/i });
    fireEvent.click(confirmButton);

    expect(dispatchMock).toHaveBeenCalledWith({
      type: "SelectCards",
      data: { cards: [10, 11] },
    });
  });

  it("allows reordering cards with move buttons and dispatches changed permutation", () => {
    setWaitingFor(
      {
        type: "RevealUntilBottomOrder",
        data: {
          player: 0,
          source_id: 1,
          cards: [10, 11],
        },
      },
      {
        10: makeObject(10, "Lightning Bolt"),
        11: makeObject(11, "Counterspell"),
      },
    );

    render(<CardChoiceModal />);

    const moveRightButtons = screen.getAllByRole("button", { name: /Move right/i });
    expect(moveRightButtons[0]).not.toBeDisabled();
    fireEvent.click(moveRightButtons[0]);

    const confirmButton = screen.getByRole("button", { name: /Done|Confirm/i });
    fireEvent.click(confirmButton);

    expect(dispatchMock).toHaveBeenCalledWith({
      type: "SelectCards",
      data: { cards: [11, 10] },
    });
  });

  it("resets the order for a new interaction with the same cards", () => {
    const waitingFor: WaitingFor = {
      type: "RevealUntilBottomOrder",
      data: { player: 0, source_id: 1, cards: [10, 11] },
    };
    const objects = {
      10: makeObject(10, "Lightning Bolt"),
      11: makeObject(11, "Counterspell"),
    };
    setWaitingFor(waitingFor, objects, selectInteraction("session.1.1"));
    render(<CardChoiceModal />);

    fireEvent.click(screen.getAllByRole("button", { name: /Move right/i })[0]);
    act(() => {
      setWaitingFor(waitingFor, objects, selectInteraction("session.1.2"));
    });

    fireEvent.click(screen.getByRole("button", { name: /Done|Confirm/i }));
    expect(dispatchMock).toHaveBeenLastCalledWith({
      type: "SelectCards",
      data: { cards: [10, 11] },
    });
  });
});
