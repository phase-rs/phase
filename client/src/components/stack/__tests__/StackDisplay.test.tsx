import { act, type CSSProperties, type ReactNode } from "react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen } from "@testing-library/react";

import { StackDisplay } from "../StackDisplay.tsx";
import { useGameStore } from "../../../stores/gameStore.ts";
import { buildGameState, buildStackEntry } from "../../../test/factories/gameStateFactory.ts";

vi.mock("../StackEntry.tsx", () => ({
  StackEntry: ({
    entry,
    onHoverChange,
    style,
  }: {
    entry: { id: number };
    onHoverChange?: (hovered: boolean) => void;
    style?: CSSProperties;
  }) => (
    <button
      type="button"
      data-testid={`stack-entry-${entry.id}`}
      style={style}
      onMouseEnter={() => onHoverChange?.(true)}
      onMouseLeave={() => onHoverChange?.(false)}
    />
  ),
}));

vi.mock("../StackTargetArcs.tsx", () => ({ StackTargetArcs: () => null }));

vi.mock("../../flexlayout/DraggableWidget.tsx", () => ({
  DraggableWidget: ({ children }: { children: ReactNode }) => <div>{children}</div>,
}));

describe("StackDisplay", () => {
  beforeEach(() => {
    useGameStore.getState().reset();
  });

  afterEach(() => {
    cleanup();
  });

  it("raises the hovered entry above every other card in the pile", () => {
    const bottomEntry = buildStackEntry({ id: 10 });
    const topEntry = buildStackEntry({ id: 20 });
    const gameState = buildGameState({ stack: [bottomEntry, topEntry] });

    act(() => {
      useGameStore.setState({ gameState, waitingFor: gameState.waiting_for });
    });

    render(<StackDisplay effectiveMultiplayerBoardLayout="focused" />);

    const bottomCard = screen.getByTestId("stack-entry-10");
    const topCard = screen.getByTestId("stack-entry-20");
    expect(bottomCard).toHaveStyle({ zIndex: 1 });
    expect(topCard).toHaveStyle({ zIndex: 2 });

    fireEvent.mouseEnter(bottomCard);

    expect(bottomCard).toHaveStyle({ zIndex: 3 });
    expect(topCard).toHaveStyle({ zIndex: 2 });
  });
});
