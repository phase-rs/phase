import { act, type ReactNode } from "react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { cleanup, render, screen } from "@testing-library/react";

import { StackDisplay } from "../StackDisplay.tsx";
import { useGameStore } from "../../../stores/gameStore.ts";
import { buildGameState, buildStackEntry } from "../../../test/factories/gameStateFactory.ts";

vi.mock("../../../hooks/useCardImage.ts", () => ({
  useCardImage: () => ({ src: "/test-card.png", isLoading: false }),
}));

vi.mock("../StackTargetArcs.tsx", () => ({
  StackTargetArcs: () => null,
}));

vi.mock("../../flexlayout/DraggableWidget.tsx", () => ({
  DraggableWidget: ({ children }: { children: ReactNode }) => <div>{children}</div>,
}));

describe("StackDisplay revealed cards", () => {
  beforeEach(() => {
    useGameStore.getState().reset();
  });

  afterEach(() => {
    cleanup();
  });

  it("renders each entry's engine-authored revealed card when the engine keeps them ungrouped", () => {
    // CR 701.20a: two reflexives with the same target but different revealed
    // cards arrive as two engine groups; each renders its own public reveal.
    const reflexive = (id: number) =>
      buildStackEntry({
        id,
        source_id: 0,
        controller: 0,
        kind: {
          type: "TriggeredAbility",
          data: { source_id: 0, ability: { targets: [] }, source_name: "Calibrated Blast" },
        },
      });
    const gameState = buildGameState({
      objects: {},
      stack: [reflexive(6), reflexive(7)],
      derived: {
        stack_display_groups: [
          { representative: 6, count: 1, member_ids: [6] },
          { representative: 7, count: 1, member_ids: [7] },
        ],
        stack_revealed_cards: { "6": ["Two Drop"], "7": ["Three Drop"] },
      },
    });

    act(() => {
      useGameStore.setState({ gameState, waitingFor: gameState.waiting_for });
    });
    render(<StackDisplay effectiveMultiplayerBoardLayout="focused" />);

    expect(
      screen.getAllByTestId("stack-revealed-cards").map((chip) => chip.textContent),
    ).toEqual(expect.arrayContaining(["Revealed: Two Drop", "Revealed: Three Drop"]));
    expect(screen.getAllByTestId("stack-revealed-cards")).toHaveLength(2);
  });
});
