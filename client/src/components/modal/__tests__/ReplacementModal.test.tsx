import { act } from "react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";

import type { ReplacementChoiceKind, WaitingFor } from "../../../adapter/types.ts";
import { useGameStore, type GameStore } from "../../../stores/gameStore.ts";
import { ReplacementModal } from "../ReplacementModal.tsx";

const dispatchMock = vi.fn<GameStore["dispatch"]>().mockResolvedValue([]);

function prompt(kind: ReplacementChoiceKind = { type: "Order" }, available = true): WaitingFor {
  return {
    type: "ReplacementChoice",
    data: {
      player: 0,
      candidate_count: 3,
      candidates: [
        { source_id: 10, source_name: "First source", description: "Double" },
        { source_id: 11, source_name: "Second source", description: "Add one" },
        { source_id: 12, source_name: "Third source", description: "Add three" },
      ],
      kind,
      ...(available ? {
        remember_identity: { player: 0, event: "GainLife", kind, candidates: [{ incarnation: 1 }] },
      } : {}),
    },
  };
}

function seed(waitingFor: WaitingFor) {
  act(() => useGameStore.setState({ waitingFor, dispatch: dispatchMock }));
}

beforeEach(() => {
  useGameStore.getState().reset();
  dispatchMock.mockClear();
});
afterEach(cleanup);

describe("ReplacementModal", () => {
  it("submits a complete remembered permutation after keyboard reordering", async () => {
    const user = userEvent.setup();
    seed(prompt());
    render(<ReplacementModal />);
    fireEvent.click(screen.getByRole("checkbox", { name: "Remember this choice this game" }));
    screen.getByRole("button", { name: "Move Add one earlier" }).focus();
    await user.keyboard("{Enter}");
    fireEvent.click(screen.getByRole("button", { name: "Confirm Order" }));
    expect(dispatchMock).toHaveBeenCalledWith({ type: "ChooseReplacementAndRemember", data: { choice: { type: "Order", data: { order: [1, 0, 2] } } } });
  });

  it("remembers a singleton ordering with the engine's Order kind", () => {
    const singleton = prompt();
    if (singleton.type !== "ReplacementChoice") throw new Error("fixture");
    singleton.data.candidate_count = 1;
    singleton.data.candidates = singleton.data.candidates!.slice(0, 1);
    seed(singleton);
    render(<ReplacementModal />);
    fireEvent.click(screen.getByRole("checkbox"));
    fireEvent.click(screen.getByRole("button", { name: /Double/ }));
    expect(dispatchMock).toHaveBeenCalledWith({ type: "ChooseReplacementAndRemember", data: { choice: { type: "Order", data: { order: [0] } } } });
  });

  it("keeps the ordinary choose-first action when unchecked", () => {
    seed(prompt());
    render(<ReplacementModal />);
    fireEvent.click(screen.getByRole("button", { name: "Move Add one earlier" }));
    fireEvent.click(screen.getByRole("button", { name: "Confirm Order" }));
    expect(dispatchMock).toHaveBeenCalledWith({ type: "ChooseReplacement", data: { index: 1 } });
  });

  it.each([0, 1])("remembers optional branch %i as a branch, not an ordering", (index) => {
    const optional = prompt({ type: "OptionalBranch" });
    if (optional.type !== "ReplacementChoice") throw new Error("fixture");
    optional.data.candidate_count = 2;
    optional.data.candidates = [
      { source_id: 10, source_name: "Source", description: "Accept" },
      { source_id: 10, source_name: "Source", description: "Decline" },
    ];
    seed(optional);
    render(<ReplacementModal />);
    fireEvent.click(screen.getByRole("checkbox"));
    fireEvent.click(screen.getByRole("button", { name: index === 0 ? /Accept/ : /Decline/ }));
    expect(dispatchMock).toHaveBeenCalledWith({ type: "ChooseReplacementAndRemember", data: { choice: { type: "Optional", data: { index } } } });
  });

  it("resets the checkbox for a new source incarnation with unchanged labels", () => {
    seed(prompt());
    render(<ReplacementModal />);
    fireEvent.click(screen.getByRole("checkbox"));
    expect(screen.getByRole("checkbox")).toBeChecked();
    const next = prompt();
    if (next.type !== "ReplacementChoice") throw new Error("fixture");
    next.data.remember_identity = { player: 0, event: "GainLife", kind: { type: "Order" }, candidates: [{ incarnation: 2 }] };
    seed(next);
    expect(screen.getByRole("checkbox")).not.toBeChecked();
    fireEvent.click(screen.getByRole("button", { name: "Confirm Order" }));
    expect(dispatchMock).toHaveBeenCalledWith({ type: "ChooseReplacement", data: { index: 0 } });
  });

  it.each(["OptionalBranch", "SearchFoundDestination"] as const)("hides remember for ineligible %s", (type) => {
    seed(prompt({ type }, false));
    render(<ReplacementModal />);
    expect(screen.queryByRole("checkbox")).not.toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: /Double/ }));
    expect(dispatchMock).toHaveBeenCalledWith({ type: "ChooseReplacement", data: { index: 0 } });
  });
});
