import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { GameState } from "../../../adapter/types.ts";
import { useGameStore } from "../../../stores/gameStore.ts";
import { useMultiplayerStore } from "../../../stores/multiplayerStore.ts";
import { buildGameObject, buildObjectMap } from "../../../test/factories/gameObjectFactory.ts";
import { buildGameState } from "../../../test/factories/gameStateFactory.ts";
import { CardChoiceModal } from "../CardChoiceModal.tsx";

const dispatchMock = vi.fn();

vi.mock("../../../hooks/useGameDispatch.ts", () => ({
  useGameDispatch: () => dispatchMock,
}));

// CR 709.5f-g: build a ChooseRoomDoor prompt over a single Room permanent.
function makeState(
  options: [
    { type: "Unlock" | "Lock" | "LockOrUnlock" },
    "Left" | "Right",
  ][],
): GameState {
  const room = buildGameObject({
    id: 20,
    card_id: 20,
    name: "Bottomless Pool // Locker Room",
    entered_battlefield_turn: 1,
    card_types: {
      supertypes: [],
      core_types: ["Enchantment"],
      subtypes: ["Room"],
    },
  });

  return buildGameState({
    turn_number: 2,
    phase: "PreCombatMain",
    objects: buildObjectMap(room),
    next_object_id: 21,
    battlefield: [20],
    waiting_for: {
      type: "ChooseRoomDoor",
      data: { player: 0, object_id: 20, options },
    },
    next_timestamp: 3,
  });
}

function mount(state: GameState) {
  useMultiplayerStore.setState({ activePlayerId: 0 });
  useGameStore.setState({
    gameMode: "online",
    gameState: state,
    waitingFor: state.waiting_for,
  });
}

describe("RoomDoorChoiceModal", () => {
  beforeEach(() => {
    dispatchMock.mockClear();
  });

  afterEach(() => {
    cleanup();
  });

  it("dispatches the chosen (op, door) for a fixed-op Unlock prompt", () => {
    mount(
      makeState([
        [{ type: "Unlock" }, "Left"],
        [{ type: "Unlock" }, "Right"],
      ]),
    );
    render(<CardChoiceModal />);

    fireEvent.click(screen.getByRole("button", { name: "Unlock right door" }));
    fireEvent.click(screen.getByRole("button", { name: "Confirm" }));

    // Reverting the dispatch wiring (or sending the door alone, or the wrong
    // option index) flips this exact-payload assertion.
    expect(dispatchMock).toHaveBeenCalledWith({
      type: "ChooseRoomDoor",
      data: { object_id: 20, op: { type: "Unlock" }, door: "Right" },
    });
  });

  it("dispatches the chosen op for a lock-or-unlock prompt", () => {
    mount(
      makeState([
        [{ type: "Unlock" }, "Left"],
        [{ type: "Lock" }, "Right"],
      ]),
    );
    render(<CardChoiceModal />);

    // The same door can appear under different ops, so the unit of choice is the
    // (op, door) pair, not the door — pick the Lock-Right option.
    fireEvent.click(screen.getByRole("button", { name: "Lock right door" }));
    fireEvent.click(screen.getByRole("button", { name: "Confirm" }));

    expect(dispatchMock).toHaveBeenCalledWith({
      type: "ChooseRoomDoor",
      data: { object_id: 20, op: { type: "Lock" }, door: "Right" },
    });
  });

  it("disables confirm until a door is selected", () => {
    mount(makeState([[{ type: "Unlock" }, "Left"]]));
    render(<CardChoiceModal />);

    const confirm = screen.getByRole("button", { name: "Confirm" });
    expect(confirm).toBeDisabled();

    fireEvent.click(screen.getByRole("button", { name: "Unlock left door" }));
    expect(confirm).not.toBeDisabled();
  });
});
