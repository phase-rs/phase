import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import type { GameAction, WaitingFor } from "../../../adapter/types.ts";
import { isWaitingForHandled } from "../../../game/waitingForRegistry.ts";
import { useMultiplayerStore } from "../../../stores/multiplayerStore.ts";
import { AssistChoosePlayerModalContent } from "../AssistChoosePlayerModal.tsx";

type AssistChoosePlayerWaitingFor = Extract<WaitingFor, { type: "AssistChoosePlayer" }>;

function assistChooseWaitingFor(): AssistChoosePlayerWaitingFor {
  return {
    type: "AssistChoosePlayer",
    data: {
      player: 0,
      candidates: [1, 2],
      max_generic: 3,
    },
  };
}

function renderModal(waitingFor: AssistChoosePlayerWaitingFor) {
  const dispatch = vi.fn<(action: GameAction) => void>();
  render(
    <AssistChoosePlayerModalContent waitingFor={waitingFor} dispatch={dispatch} />,
  );
  return dispatch;
}

afterEach(() => {
  cleanup();
  useMultiplayerStore.setState({ playerNames: new Map() });
});

describe("AssistChoosePlayerModalContent", () => {
  it("registers the waiting state as handled", () => {
    expect(isWaitingForHandled(assistChooseWaitingFor())).toBe(true);
  });

  it("renders a button per candidate plus a decline button", () => {
    useMultiplayerStore.setState({
      playerNames: new Map([
        [1, "Alice"],
        [2, "Bob"],
      ]),
    });
    renderModal(assistChooseWaitingFor());

    expect(screen.getByRole("button", { name: "Alice" })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Bob" })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Decline" })).toBeInTheDocument();
  });

  it("dispatches the chosen helper id", () => {
    useMultiplayerStore.setState({
      playerNames: new Map([
        [1, "Alice"],
        [2, "Bob"],
      ]),
    });
    const dispatch = renderModal(assistChooseWaitingFor());

    fireEvent.click(screen.getByRole("button", { name: "Bob" }));

    expect(dispatch).toHaveBeenCalledWith({
      type: "ChooseAssistPlayer",
      data: { player: 2 },
    });
  });

  it("dispatches a null player when declining", () => {
    const dispatch = renderModal(assistChooseWaitingFor());

    fireEvent.click(screen.getByRole("button", { name: "Decline" }));

    expect(dispatch).toHaveBeenCalledWith({
      type: "ChooseAssistPlayer",
      data: { player: null },
    });
  });
});
