import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { GameObject, WaitingFor } from "../../../adapter/types.ts";
import { useGameStore } from "../../../stores/gameStore.ts";
import { buildGameObjectWithCoreTypes, buildObjectMap } from "../../../test/factories/gameObjectFactory.ts";
import { buildGameState, buildPendingCast } from "../../../test/factories/gameStateFactory.ts";
import { SpliceOfferModal } from "../SpliceOfferModal.tsx";

const dispatchMock = vi.fn();

vi.mock("../../../hooks/useGameDispatch.ts", () => ({
  useGameDispatch: () => dispatchMock,
}));

function makeObject(id: number, name: string): GameObject {
  return buildGameObjectWithCoreTypes(["Instant"], {
    id,
    card_id: id,
    zone: "Hand",
    name,
    card_types: { supertypes: [], core_types: ["Instant"], subtypes: ["Arcane"] },
    mana_cost: { type: "Cost", shards: ["Blue"], generic: 1 },
    color: ["Blue"],
    base_color: ["Blue"],
    timestamp: 1,
    entered_battlefield_turn: null,
  });
}

function setWaitingFor(waitingFor: WaitingFor) {
  const gameState = buildGameState({
    objects: buildObjectMap(makeObject(42, "Peer Through Depths")),
    priority_player: 0,
    waiting_for: waitingFor,
  });

  useGameStore.setState({
    gameState,
    waitingFor,
  });
}

describe("SpliceOfferModal", () => {
  beforeEach(() => {
    dispatchMock.mockReset();
    dispatchMock.mockResolvedValue(undefined);
    setWaitingFor({
      type: "SpliceOffer",
      data: {
        player: 0,
        pending_cast: buildPendingCast(),
        eligible: [42],
      },
    });
  });

  afterEach(() => {
    cleanup();
  });

  it("dispatches the selected splice card or decline response", () => {
    render(<SpliceOfferModal />);

    fireEvent.click(screen.getByRole("button", { name: /Splice Peer Through Depths/ }));
    expect(dispatchMock).toHaveBeenCalledWith({
      type: "RespondToSpliceOffer",
      data: { card: 42 },
    });

    fireEvent.click(screen.getByRole("button", { name: /Don.t Splice/ }));
    expect(dispatchMock).toHaveBeenCalledWith({
      type: "RespondToSpliceOffer",
      data: { card: null },
    });
  });
});
