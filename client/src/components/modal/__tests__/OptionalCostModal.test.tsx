import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import type { GameAction, WaitingFor } from "../../../adapter/types.ts";
import { OptionalCostModalContent } from "../OptionalCostModal.tsx";

type OptionalCostWaitingFor = Extract<WaitingFor, { type: "OptionalCostChoice" }>;

/** Build an `OptionalCostChoice` waiting state for a repeatable {2} multikicker. */
function kickerWaitingFor(timesKicked: number): OptionalCostWaitingFor {
  return {
    type: "OptionalCostChoice",
    data: {
      player: 0,
      cost: {
        type: "Kicker",
        data: {
          costs: [{ type: "Mana", cost: { type: "Cost", shards: [], generic: 2 } }],
          repeatable: true,
        },
      },
      times_kicked: timesKicked,
      // `pending_cast` is opaque to the modal; cast through unknown for the test.
      pending_cast: {} as OptionalCostWaitingFor["data"]["pending_cast"],
    },
  };
}

function renderModal(waitingFor: OptionalCostWaitingFor) {
  const dispatch = vi.fn<(action: GameAction) => void>();
  render(<OptionalCostModalContent waitingFor={waitingFor} dispatch={dispatch} />);
  return dispatch;
}

afterEach(() => {
  cleanup();
});

describe("OptionalCostModalContent (issue #454)", () => {
  it("first prompt exposes three distinct affordances with the right actions", () => {
    const dispatch = renderModal(kickerWaitingFor(0));

    // Pay → DecideOptionalCost { pay: true }
    const payButton = screen.getByRole("button", { name: /kick it/i });
    // Decline → DecideOptionalCost { pay: false } — an explicit primary button.
    const declineButton = screen.getByRole("button", { name: /cast without kicking/i });
    // Abort → CancelCast — the separate close affordance.
    const closeButton = screen.getByRole("button", { name: "Close" });
    expect(payButton).not.toBe(declineButton);
    expect(closeButton).not.toBe(declineButton);

    fireEvent.click(declineButton);
    expect(dispatch).toHaveBeenCalledWith({
      type: "DecideOptionalCost",
      data: { pay: false },
    });

    dispatch.mockClear();
    fireEvent.click(payButton);
    expect(dispatch).toHaveBeenCalledWith({
      type: "DecideOptionalCost",
      data: { pay: true },
    });

    dispatch.mockClear();
    fireEvent.click(closeButton);
    expect(dispatch).toHaveBeenCalledWith({ type: "CancelCast" });
  });

  it("re-prompt shows the kick count and a 'finish casting' decline button", () => {
    const dispatch = renderModal(kickerWaitingFor(1));

    // The kick count appears in both the title and the decline button.
    expect(screen.getAllByText(/kicked 1×/i).length).toBeGreaterThan(0);
    const declineButton = screen.getByRole("button", {
      name: /done — finish casting \(kicked 1×\)/i,
    });
    fireEvent.click(declineButton);
    expect(dispatch).toHaveBeenCalledWith({
      type: "DecideOptionalCost",
      data: { pay: false },
    });
  });
});
