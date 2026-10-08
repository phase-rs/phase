import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import type { GameAction, WaitingFor } from "../../../adapter/types.ts";
import { ResolutionOptionalPaymentModalContent } from "../ResolutionOptionalPaymentModal.tsx";

type PaymentWaitingFor = Extract<
  WaitingFor,
  { type: "ResolutionOptionalPaymentChoice" }
>;

const waitingFor: PaymentWaitingFor = {
  type: "ResolutionOptionalPaymentChoice",
  data: {
    player: 0,
    source_id: 7,
    costs: [
      { index: 0, cost: { type: "Mana", cost: { type: "Cost", shards: [], generic: 1 } } },
      {
        index: 2,
        cost: {
          type: "Discard",
          count: { type: "Fixed", value: 1 },
          filter: {
            type: "Typed",
            type_filters: [{ Non: "Land" }],
            controller: null,
            properties: [],
          },
        },
      },
    ],
  },
};

afterEach(cleanup);

describe("ResolutionOptionalPaymentModalContent", () => {
  it("dispatches exact original-index pay and decline payloads", () => {
    const dispatch = vi.fn<(action: GameAction) => void>();
    const { rerender } = render(
      <ResolutionOptionalPaymentModalContent
        waitingFor={waitingFor}
        canActForWaitingState
        dispatch={dispatch}
      />,
    );

    fireEvent.click(screen.getByRole("button", { name: "Pay Discard 1 nonLand card" }));
    expect(dispatch).toHaveBeenCalledWith({
      type: "ChooseResolutionOptionalPaymentBranch",
      data: { choice: { type: "Pay", data: { index: 2 } } },
    });

    rerender(
      <ResolutionOptionalPaymentModalContent
        waitingFor={waitingFor}
        canActForWaitingState
        dispatch={dispatch}
      />,
    );
    fireEvent.click(screen.getByRole("button", { name: "Decline" }));
    expect(dispatch).toHaveBeenLastCalledWith({
      type: "ChooseResolutionOptionalPaymentBranch",
      data: { choice: { type: "Decline" } },
    });
  });

  it("renders nothing for the wrong local player", () => {
    const { container } = render(
      <ResolutionOptionalPaymentModalContent
        waitingFor={waitingFor}
        canActForWaitingState={false}
        dispatch={vi.fn()}
      />,
    );
    expect(container).toBeEmptyDOMElement();
  });
});
