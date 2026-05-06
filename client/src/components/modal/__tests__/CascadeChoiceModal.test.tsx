import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { GameObject, GameState, WaitingFor } from "../../../adapter/types.ts";
import { useGameStore } from "../../../stores/gameStore.ts";
import { CascadeChoiceModal } from "../CascadeChoiceModal.tsx";

const dispatchMock = vi.fn();

function makeObject(id: number, name: string): GameObject {
  return {
    id,
    card_id: id,
    owner: 0,
    controller: 0,
    zone: "Exile",
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
    card_types: { supertypes: [], core_types: ["Instant"], subtypes: [] },
    mana_cost: { type: "Cost", shards: ["Red"], generic: 0 },
    keywords: [],
    abilities: [],
    trigger_definitions: [],
    replacement_definitions: [],
    static_definitions: [],
    color: ["Red"],
    base_power: null,
    base_toughness: null,
    base_keywords: [],
    base_color: ["Red"],
    timestamp: 1,
    entered_battlefield_turn: null,
  };
}

function setWaitingFor(waitingFor: WaitingFor) {
  const gameState = {
    active_player: 0,
    objects: {
      52: makeObject(52, "Lightning Bolt"),
    },
    priority_player: 0,
    waiting_for: waitingFor,
  } as unknown as GameState;

  useGameStore.setState({
    gameState,
    waitingFor,
    dispatch: dispatchMock,
  });
}

describe("CascadeChoiceModal", () => {
  beforeEach(() => {
    dispatchMock.mockReset();
    dispatchMock.mockResolvedValue(undefined);
  });

  afterEach(() => {
    cleanup();
  });

  it("renders DiscoverChoice and dispatches DiscoverChoice actions", () => {
    setWaitingFor({
      type: "DiscoverChoice",
      data: { player: 0, hit_card: 52, exiled_misses: [1, 2, 3] },
    });

    render(<CascadeChoiceModal />);

    expect(screen.getByText("Discover")).toBeInTheDocument();
    expect(screen.getByText("Cast Lightning Bolt?")).toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: /Cast Lightning Bolt/ }));
    expect(dispatchMock).toHaveBeenCalledWith({
      type: "DiscoverChoice",
      data: { choice: { type: "Cast" } },
    });

    fireEvent.click(screen.getByRole("button", { name: /Put into hand/ }));
    expect(dispatchMock).toHaveBeenCalledWith({
      type: "DiscoverChoice",
      data: { choice: { type: "Decline" } },
    });
  });

  it("keeps CascadeChoice routing intact", () => {
    setWaitingFor({
      type: "CascadeChoice",
      data: { player: 0, hit_card: 52, exiled_misses: [1], source_mv: 3 },
    });

    render(<CascadeChoiceModal />);

    expect(screen.getByText("Cascade")).toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: /Decline/ }));
    expect(dispatchMock).toHaveBeenCalledWith({
      type: "CascadeChoice",
      data: { choice: { type: "Decline" } },
    });
  });
});
