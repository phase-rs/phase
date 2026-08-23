import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { GameState, WaitingFor } from "../../../adapter/types.ts";
import { CardChoiceModal } from "../CardChoiceModal.tsx";
import { isWaitingForHandled } from "../../../game/waitingForRegistry.ts";
import { useGameStore } from "../../../stores/gameStore.ts";
import { useMultiplayerStore } from "../../../stores/multiplayerStore.ts";
import { buildObjectMap } from "../../../test/factories/gameObjectFactory.ts";
import { buildGameState, buildPlayers } from "../../../test/factories/gameStateFactory.ts";

const dispatchMock = vi.fn();

vi.mock("../../../hooks/useGameDispatch.ts", () => ({
  useGameDispatch: () => dispatchMock,
}));

// CR 309.4a: each dungeon option carries the topmost room it enters.
const chooseDungeon: WaitingFor = {
  type: "ChooseDungeon",
  data: {
    player: 0,
    options: [
      {
        dungeon: "LostMineOfPhandelver",
        name: "Lost Mine of Phandelver",
        entry_room: { index: 0, name: "Cave Entrance", text: "Scry 1." },
      },
      {
        dungeon: "TombOfAnnihilation",
        name: "Tomb of Annihilation",
        entry_room: { index: 0, name: "Trapped Entry", text: "Each player loses 1 life." },
      },
    ],
  },
};

// CR 309.5a: a branch point offers each reachable room with its printed effect.
const chooseRoom: WaitingFor = {
  type: "ChooseDungeonRoom",
  data: {
    player: 0,
    dungeon: "LostMineOfPhandelver",
    dungeon_name: "Lost Mine of Phandelver",
    options: [
      { index: 1, name: "Goblin Lair", text: "Create a 1/1 red Goblin creature token." },
      { index: 2, name: "Mine Tunnels", text: "Create a Treasure token." },
    ],
  },
};

function makeState(waitingFor: WaitingFor): GameState {
  return buildGameState({
    players: buildPlayers([0, 1]),
    objects: buildObjectMap(),
    next_object_id: 100,
    waiting_for: waitingFor,
    next_timestamp: 2,
  });
}

function mount(waitingFor: WaitingFor) {
  useMultiplayerStore.setState({ activePlayerId: 0 });
  useGameStore.setState({
    gameMode: "online",
    gameState: makeState(waitingFor),
    waitingFor,
  });
  render(<CardChoiceModal />);
}

describe("DungeonChoiceModal", () => {
  beforeEach(() => {
    dispatchMock.mockClear();
  });

  afterEach(() => {
    cleanup();
  });

  it("shows each dungeon's entry room and what it does", () => {
    mount(chooseDungeon);

    expect(screen.getByText("Cave Entrance")).toBeInTheDocument();
    expect(screen.getByText("Scry 1.")).toBeInTheDocument();
    expect(screen.getByText("Trapped Entry")).toBeInTheDocument();
    expect(screen.getByText("Each player loses 1 life.")).toBeInTheDocument();
  });

  it("dispatches the chosen dungeon", () => {
    mount(chooseDungeon);

    fireEvent.click(screen.getByText("Scry 1."));
    fireEvent.click(screen.getByRole("button", { name: "Confirm" }));

    expect(dispatchMock).toHaveBeenCalledWith({
      type: "ChooseDungeon",
      data: { dungeon: "LostMineOfPhandelver" },
    });
  });

  it("is registered as a handled waiting-for state", () => {
    expect(isWaitingForHandled(chooseDungeon)).toBe(true);
  });
});

describe("RoomChoiceModal", () => {
  beforeEach(() => {
    dispatchMock.mockClear();
  });

  afterEach(() => {
    cleanup();
  });

  it("shows each reachable room's name and printed effect", () => {
    mount(chooseRoom);

    expect(screen.getByText("Goblin Lair")).toBeInTheDocument();
    expect(screen.getByText("Create a 1/1 red Goblin creature token.")).toBeInTheDocument();
    expect(screen.getByText("Mine Tunnels")).toBeInTheDocument();
    expect(screen.getByText("Create a Treasure token.")).toBeInTheDocument();
  });

  it("dispatches the engine's room index, not the button position", () => {
    mount(chooseRoom);

    fireEvent.click(screen.getByText("Create a Treasure token."));
    fireEvent.click(screen.getByRole("button", { name: "Confirm" }));

    expect(dispatchMock).toHaveBeenCalledWith({
      type: "ChooseDungeonRoom",
      data: { room_index: 2 },
    });
  });

  it("is registered as a handled waiting-for state", () => {
    expect(isWaitingForHandled(chooseRoom)).toBe(true);
  });
});
