import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import type { GameAction, WaitingFor } from "../../../adapter/types.ts";
import { isWaitingForHandled } from "../../../game/waitingForRegistry.ts";
import { useMultiplayerStore } from "../../../stores/multiplayerStore.ts";
import { EntryControllerModalContent } from "../EntryControllerModal.tsx";

type EntryControllerWaitingFor = Extract<WaitingFor, { type: "EntryControllerChoice" }>;

function entryControllerWaitingFor(): EntryControllerWaitingFor {
  return {
    type: "EntryControllerChoice",
    data: { player: 0, candidates: [2, 1] },
  };
}

afterEach(() => {
  cleanup();
  useMultiplayerStore.setState({ playerNames: new Map() });
});

describe("EntryControllerModalContent", () => {
  it("registers the waiting state as handled", () => {
    expect(isWaitingForHandled(entryControllerWaitingFor())).toBe(true);
  });

  it("dispatches the selected entry controller", () => {
    useMultiplayerStore.setState({
      playerNames: new Map([
        [1, "Alice"],
        [2, "Bob"],
      ]),
    });
    const dispatch = vi.fn<(action: GameAction) => void>();
    render(
      <EntryControllerModalContent
        waitingFor={entryControllerWaitingFor()}
        dispatch={dispatch}
      />,
    );

    expect(screen.getByRole("heading", { name: "Choose Entry Controller" })).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "Bob" }));

    expect(dispatch).toHaveBeenCalledWith({
      type: "ChooseEntryController",
      data: { opponent: 2 },
    });
  });
});
