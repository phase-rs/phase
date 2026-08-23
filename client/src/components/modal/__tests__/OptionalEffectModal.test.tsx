import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import type { GameAction, WaitingFor } from "../../../adapter/types.ts";
import { OptionalEffectModalContent } from "../OptionalEffectModal.tsx";

type OptionalEffectWaitingFor = Extract<WaitingFor, { type: "OptionalEffectChoice" }>;

function optionalWaitingFor(
  mayTriggerKey?: OptionalEffectWaitingFor["data"]["may_trigger_key"],
  sameCardAvailable = false,
): OptionalEffectWaitingFor {
  return {
    type: "OptionalEffectChoice",
    data: {
      player: 0,
      source_id: 100,
      description: "You may gain 1 life.",
      may_trigger_key: mayTriggerKey,
      same_card_may_trigger_choice_available: sameCardAvailable,
    },
  };
}

function renderModal(waitingFor: OptionalEffectWaitingFor) {
  const dispatch = vi.fn<(action: GameAction) => void>();
  render(<OptionalEffectModalContent waitingFor={waitingFor} dispatch={dispatch} />);
  return dispatch;
}

afterEach(() => {
  cleanup();
});

describe("OptionalEffectModalContent", () => {
  it("hides remember control for unkeyed prompts and dispatches the existing action", () => {
    const dispatch = renderModal(optionalWaitingFor());

    expect(screen.queryByLabelText("Don't ask again this game")).not.toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "Yes" }));

    expect(dispatch).toHaveBeenCalledWith({
      type: "DecideOptionalEffect",
      data: { accept: true },
    });
  });

  it("keeps keyed prompts on the existing action while remember is unchecked", () => {
    const dispatch = renderModal(
      optionalWaitingFor({
        player: 0,
        source_id: 100,
        origin: { type: "Printed", trigger_index: 0 },
      }),
    );

    expect(screen.getByLabelText("Don't ask again this game")).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "No" }));

    expect(dispatch).toHaveBeenCalledWith({
      type: "DecideOptionalEffect",
      data: { accept: false },
    });
  });

  it("dispatches remembered accept and decline actions when checked", () => {
    const dispatch = renderModal(
      optionalWaitingFor({
        player: 0,
        source_id: 100,
        origin: { type: "Printed", trigger_index: 0 },
      }),
    );

    fireEvent.click(screen.getByLabelText("Don't ask again this game"));
    fireEvent.click(screen.getByRole("button", { name: "Yes" }));

    expect(dispatch).toHaveBeenCalledWith({
      type: "DecideOptionalEffectAndRemember",
      data: { choice: { type: "Accept" }, scope: { type: "ExactInstance" } },
    });

    cleanup();
    const declineDispatch = renderModal(
      optionalWaitingFor({
        player: 0,
        source_id: 100,
        origin: { type: "Printed", trigger_index: 0 },
      }),
    );

    fireEvent.click(screen.getByLabelText("Don't ask again this game"));
    fireEvent.click(screen.getByRole("button", { name: "No" }));

    expect(declineDispatch).toHaveBeenCalledWith({
      type: "DecideOptionalEffectAndRemember",
      data: { choice: { type: "Decline" }, scope: { type: "ExactInstance" } },
    });
  });

  it("shows the engine-gated same-card checkbox and dispatches that scope", () => {
    const dispatch = renderModal(
      optionalWaitingFor(
        {
          player: 0,
          source_id: 100,
          origin: { type: "Printed", trigger_index: 0 },
        },
        true,
      ),
    );

    fireEvent.click(screen.getByLabelText("Apply this to every copy of this card"));
    fireEvent.click(screen.getByRole("button", { name: "Yes" }));

    expect(dispatch).toHaveBeenCalledWith({
      type: "DecideOptionalEffectAndRemember",
      data: { choice: { type: "Accept" }, scope: { type: "SameCard" } },
    });
  });

  it("resets the same-card checkbox for each prompt", () => {
    const keyed = {
      player: 0,
      source_id: 100,
      origin: { type: "Printed", trigger_index: 0 },
    };
    const { rerender } = render(
      <OptionalEffectModalContent waitingFor={optionalWaitingFor(keyed, true)} dispatch={vi.fn()} />,
    );

    fireEvent.click(screen.getByLabelText("Apply this to every copy of this card"));
    expect(screen.getByLabelText("Apply this to every copy of this card")).toBeChecked();

    rerender(
      <OptionalEffectModalContent
        waitingFor={{ ...optionalWaitingFor(keyed, true), data: { ...optionalWaitingFor(keyed, true).data, source_id: 101 } }}
        dispatch={vi.fn()}
      />,
    );
    expect(screen.getByLabelText("Apply this to every copy of this card")).not.toBeChecked();
  });
});
