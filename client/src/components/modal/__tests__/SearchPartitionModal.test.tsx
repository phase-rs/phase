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

describe("SearchPartition modal", () => {
  beforeEach(() => {
    dispatchMock.mockClear();
    useMultiplayerStore.setState({ activePlayerId: 0 });
  });

  afterEach(() => {
    cleanup();
  });

  // Final Parting shape: primary goes to hand, the rest to the graveyard. The
  // subtitle must name the REAL destinations, not the hard-coded battlefield.
  it("names the real primary and rest destination zones", async () => {
    setWaitingFor(
      {
        type: "SearchPartitionChoice",
        data: {
          player: 0,
          cards: [42, 43],
          primary_destination: "Hand",
          primary_count: 1,
          primary_enter_tapped: false,
          rest_destination: "Graveyard",
          source_id: 99,
        },
      },
      { 42: makeObject(42, "Lightning Bolt"), 43: makeObject(43, "Dark Ritual") },
    );

    render(<CardChoiceModal />);

    expect(await screen.findByText(/your hand/i)).toBeInTheDocument();
    expect(await screen.findByText(/your graveyard/i)).toBeInTheDocument();
    // Revert-failing: the pre-fix modal hard-coded "the battlefield" and never
    // mentioned the graveyard.
    expect(screen.queryByText(/battlefield/i)).not.toBeInTheDocument();
  });
});
