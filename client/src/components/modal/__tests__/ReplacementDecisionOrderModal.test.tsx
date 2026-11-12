import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import type { GameAction, GameObject, WaitingFor } from "../../../adapter/types.ts";
import { isWaitingForHandled } from "../../../game/waitingForRegistry.ts";
import { ReplacementDecisionOrderModalContent } from "../ReplacementDecisionOrderModal.tsx";

type ReplacementDecisionOrderWaitingFor = Extract<
  WaitingFor,
  { type: "ReplacementDecisionOrder" }
>;

function decisionOrderWaitingFor(): ReplacementDecisionOrderWaitingFor {
  return {
    type: "ReplacementDecisionOrder",
    data: { player: 1, objects: [3, 7] },
  };
}

function named(name: string): GameObject {
  return { name } as unknown as GameObject;
}

afterEach(() => {
  cleanup();
});

describe("ReplacementDecisionOrderModalContent", () => {
  it("registers the waiting state as handled", () => {
    expect(isWaitingForHandled(decisionOrderWaitingFor())).toBe(true);
  });

  it("dispatches the object whose decision is made first", () => {
    const dispatch = vi.fn<(action: GameAction) => void>();
    render(
      <ReplacementDecisionOrderModalContent
        waitingFor={decisionOrderWaitingFor()}
        objects={{ "3": named("Grizzly Bears"), "7": named("Runeclaw Bear") }}
        dispatch={dispatch}
      />,
    );

    expect(
      screen.getByRole("heading", { name: "Choose Which Decision First" }),
    ).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "Runeclaw Bear" }));

    expect(dispatch).toHaveBeenCalledWith({
      type: "SelectCards",
      data: { cards: [7] },
    });
  });
});
