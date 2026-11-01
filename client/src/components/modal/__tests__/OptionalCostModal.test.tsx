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

  it("Gift origin uses localized promise copy from optionalCost.gift keys", () => {
    const waitingFor: OptionalCostWaitingFor = {
      type: "OptionalCostChoice",
      data: {
        player: 0,
        cost: {
          type: "Optional",
          data: {
            cost: { type: "Mana", cost: { type: "Cost", shards: [], generic: 0 } },
            repeatable: false,
          },
        },
        times_kicked: 0,
        origin: "Gift",
        gift_kind: { type: "Treasure" },
        pending_cast: {} as OptionalCostWaitingFor["data"]["pending_cast"],
      },
    };
    const dispatch = renderModal(waitingFor);

    expect(screen.getByText("Promise a gift?")).toBeTruthy();
    const payButton = screen.getByRole("button", { name: /promise a treasure/i });
    const declineButton = screen.getByRole("button", {
      name: /cast without promising/i,
    });
    fireEvent.click(payButton);
    expect(dispatch).toHaveBeenCalledWith({
      type: "DecideOptionalCost",
      data: { pay: true },
    });
    dispatch.mockClear();
    fireEvent.click(declineButton);
    expect(dispatch).toHaveBeenCalledWith({
      type: "DecideOptionalCost",
      data: { pay: false },
    });
  });

  // CR 118.8 + CR 601.2b (issue #10442): a three-way choice of additional
  // costs dispatches the INDEX of the announced cost. `DecideOptionalCost
  // { pay }` reaches only the first and the last, so the middle option is
  // unreachable without this action.
  it("announces the middle option of a three-way choice by index", () => {
    const waitingFor: OptionalCostWaitingFor = {
      type: "OptionalCostChoice",
      data: {
        player: 0,
        cost: {
          type: "Choice",
          data: [
            { type: "Sacrifice", count: 1 },
            { type: "Discard", count: { type: "Fixed", value: 1 } },
            { type: "PayLife", amount: { type: "Fixed", value: 4 } },
          ],
        },
        times_kicked: 0,
        pending_cast: {} as OptionalCostWaitingFor["data"]["pending_cast"],
      },
    };
    const dispatch = renderModal(waitingFor);

    // CR 601.2f: a mandatory choice has no abort affordance.
    expect(screen.queryByRole("button", { name: "Close" })).toBeNull();

    const discardButton = screen.getByRole("button", { name: /discard/i });
    fireEvent.click(discardButton);
    expect(dispatch).toHaveBeenCalledWith({
      type: "ChooseAdditionalCostBranch",
      data: { index: 1 },
    });

    dispatch.mockClear();
    fireEvent.click(screen.getByRole("button", { name: /life/i }));
    expect(dispatch).toHaveBeenCalledWith({
      type: "ChooseAdditionalCostBranch",
      data: { index: 2 },
    });
  });

  // CR 702.174g: the one promised gift that is not an object. The label switch
  // falls back to "a card" for anything it does not name, so an unlabelled kind
  // does not look unlabelled — it looks like a DIFFERENT promise (#7286).
  it("Gift an extra turn is named, not folded into the card fallback", () => {
    const waitingFor: OptionalCostWaitingFor = {
      type: "OptionalCostChoice",
      data: {
        player: 0,
        cost: {
          type: "Optional",
          data: {
            cost: { type: "Mana", cost: { type: "Cost", shards: [], generic: 0 } },
            repeatable: false,
          },
        },
        times_kicked: 0,
        origin: "Gift",
        gift_kind: { type: "ExtraTurn" },
        pending_cast: {} as OptionalCostWaitingFor["data"]["pending_cast"],
      },
    };
    renderModal(waitingFor);

    expect(screen.getByRole("button", { name: /promise an extra turn/i })).toBeTruthy();
    expect(screen.queryByRole("button", { name: /promise a card/i })).toBeNull();
  });
});
