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

function setWaitingFor(waitingFor: WaitingFor, objects: Record<string, GameObject>) {
  const state = buildGameState({
    players: [buildPlayer({ id: 0, library: [42] }), buildPlayer({ id: 1 })],
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

describe("DigChoice modal", () => {
  beforeEach(() => {
    dispatchMock.mockClear();
    useMultiplayerStore.setState({ activePlayerId: 0 });
  });

  afterEach(() => {
    cleanup();
  });

  it("labels kept library cards as going on top of the library", () => {
    setWaitingFor(
      {
        type: "DigChoice",
        data: {
          player: 0,
          cards: [42],
          keep_count: 1,
          up_to: true,
          selectable_cards: [42],
          kept_destination: "Library",
          rest_destination: "Graveyard",
        },
      },
      { 42: makeObject(42, "Lightning Bolt") },
    );

    render(<CardChoiceModal />);

    expect(screen.getByText(/on top of your library/i)).toBeInTheDocument();
    expect(screen.queryByText(/into your hand/i)).not.toBeInTheDocument();
  });
});
