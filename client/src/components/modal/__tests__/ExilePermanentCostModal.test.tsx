import { cleanup, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { GameObject, WaitingFor } from "../../../adapter/types.ts";
import { useGameStore } from "../../../stores/gameStore.ts";
import { useMultiplayerStore } from "../../../stores/multiplayerStore.ts";
import { buildGameObject } from "../../../test/factories/gameObjectFactory.ts";
import { buildGameState, buildPendingCast } from "../../../test/factories/gameStateFactory.ts";
import { CardChoiceModal } from "../CardChoiceModal.tsx";

const dispatchMock = vi.fn();

vi.mock("../../../hooks/useGameDispatch.ts", () => ({
  useGameDispatch: () => dispatchMock,
}));

function makeObject(id: number, name: string): GameObject {
  return buildGameObject({
    id,
    card_id: id,
    zone: "Battlefield",
    name,
    card_types: { supertypes: [], core_types: ["Land"], subtypes: [] },
    timestamp: id,
  });
}

function setWaitingFor(waitingFor: WaitingFor, objects?: Record<string, GameObject>) {
  const state = buildGameState({
    objects: objects ?? {},
    waiting_for: waitingFor,
    has_pending_cast: true,
    next_object_id: 100,
  });
  useGameStore.setState({
    gameMode: "online",
    gameState: state,
    waitingFor,
  });
}

describe("Exile-permanent cost board choice", () => {
  beforeEach(() => {
    dispatchMock.mockClear();
    useMultiplayerStore.setState({ activePlayerId: 0 });
  });

  afterEach(() => {
    cleanup();
  });

  it("suppresses the modal for battlefield ExilePermanent costs", () => {
    setWaitingFor(
      {
        type: "PayCost",
        data: {
          player: 0,
          kind: { type: "ExilePermanent", filter: null },
          choices: [10],
          count: 1,
          min_count: 1,
          resume: { type: "Spell", Spell: buildPendingCast() },
        },
      },
      { 10: makeObject(10, "Forest") },
    );

    render(<CardChoiceModal />);

    expect(screen.queryByRole("button")).toBeNull();
    expect(dispatchMock).not.toHaveBeenCalled();
  });
});
