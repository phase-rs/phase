import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import type {
  CastOpponentChoicePurpose,
  GameAction,
  WaitingFor,
} from "../../../adapter/types.ts";
import { isWaitingForHandled } from "../../../game/waitingForRegistry.ts";
import { useMultiplayerStore } from "../../../stores/multiplayerStore.ts";
import { GiftRecipientModalContent } from "../GiftRecipientModal.tsx";

type GiftRecipientWaitingFor = Extract<WaitingFor, { type: "ChooseGiftRecipient" }>;

/** Invigorate's alternative cost as the engine serializes it. */
const OPPONENT_GAINS_THREE: CastOpponentChoicePurpose = {
  type: "EffectCost",
  cost: {
    type: "EffectCost",
    effect: {
      type: "GainLife",
      amount: { type: "Fixed", value: 3 },
      player: { type: "Typed", type_filters: [], controller: "Opponent", properties: [] },
    },
  },
};

function recipientWaitingFor(purpose?: CastOpponentChoicePurpose): GiftRecipientWaitingFor {
  return {
    type: "ChooseGiftRecipient",
    data: {
      player: 0,
      candidates: [1, 2],
      ...(purpose ? { purpose } : {}),
      pending_cast: {},
    },
  };
}

function renderModal(purpose?: CastOpponentChoicePurpose) {
  useMultiplayerStore.setState({
    playerNames: new Map([
      [1, "Alice"],
      [2, "Bob"],
    ]),
  });
  const dispatch = vi.fn<(action: GameAction) => void>();
  render(
    <GiftRecipientModalContent waitingFor={recipientWaitingFor(purpose)} dispatch={dispatch} />,
  );
  return dispatch;
}

afterEach(() => {
  cleanup();
  useMultiplayerStore.setState({ playerNames: new Map() });
});

describe("GiftRecipientModalContent", () => {
  it("registers the waiting state as handled for both purposes", () => {
    expect(isWaitingForHandled(recipientWaitingFor())).toBe(true);
    expect(isWaitingForHandled(recipientWaitingFor(OPPONENT_GAINS_THREE))).toBe(true);
  });

  it("shows the gift copy when the purpose is absent (serde default)", () => {
    renderModal();
    expect(screen.getByRole("heading", { name: "Choose Gift Recipient" })).toBeInTheDocument();
    expect(screen.getByText("Choose which opponent receives the promised gift.")).toBeInTheDocument();
  });

  it("shows the gift copy for an explicit Gift purpose", () => {
    renderModal({ type: "Gift" });
    expect(screen.getByRole("heading", { name: "Choose Gift Recipient" })).toBeInTheDocument();
  });

  it("shows the cost being paid for an effect-cost purpose and dispatches the choice", () => {
    const dispatch = renderModal(OPPONENT_GAINS_THREE);

    expect(screen.getByRole("heading", { name: "Choose an Opponent" })).toBeInTheDocument();
    expect(screen.queryByRole("heading", { name: "Choose Gift Recipient" })).toBeNull();
    expect(screen.getByText(/Have an opponent gain 3 life/)).toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: "Bob" }));
    expect(dispatch).toHaveBeenCalledWith({
      type: "ChooseGiftRecipient",
      data: { opponent: 2 },
    });
  });
});
