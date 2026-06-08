import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { GameObject, GameState, WaitingFor } from "../../../adapter/types.ts";
import { useGameStore } from "../../../stores/gameStore.ts";
import { SpliceOfferModal } from "../SpliceOfferModal.tsx";

const dispatchMock = vi.fn();

vi.mock("../../../hooks/useGameDispatch.ts", () => ({
  useGameDispatch: () => dispatchMock,
}));

function makeObject(id: number, name: string): GameObject {
  return {
    id,
    card_id: id,
    owner: 0,
    controller: 0,
    zone: "Hand",
    tapped: false,
    face_down: false,
    flipped: false,
    transformed: false,
    damage_marked: 0,
    dealt_deathtouch_damage: false,
    attached_to: null,
    attachments: [],
    counters: {},
    name,
    power: null,
    toughness: null,
    loyalty: null,
    card_types: { supertypes: [], core_types: ["Instant"], subtypes: ["Arcane"] },
    mana_cost: { type: "Cost", shards: ["Blue"], generic: 1 },
    keywords: [],
    abilities: [],
    trigger_definitions: [],
    replacement_definitions: [],
    static_definitions: [],
    color: ["Blue"],
    base_power: null,
    base_toughness: null,
    base_keywords: [],
    base_color: ["Blue"],
    timestamp: 1,
    entered_battlefield_turn: null,
  };
}

function setWaitingFor(waitingFor: WaitingFor) {
  const gameState = {
    active_player: 0,
    objects: {
      42: makeObject(42, "Peer Through Depths"),
    },
    priority_player: 0,
    waiting_for: waitingFor,
  } as unknown as GameState;

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
        pending_cast: {} as Extract<
          WaitingFor,
          { type: "SpliceOffer" }
        >["data"]["pending_cast"],
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
