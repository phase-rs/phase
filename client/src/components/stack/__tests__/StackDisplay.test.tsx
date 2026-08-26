import { act, type CSSProperties, type ReactNode } from "react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen } from "@testing-library/react";

import { StackDisplay } from "../StackDisplay.tsx";
import { useGameStore } from "../../../stores/gameStore.ts";
import {
  buildGameState,
  buildStackEntry,
  targetSelectionWaitingForFactory,
} from "../../../test/factories/gameStateFactory.ts";

vi.mock("../StackEntry.tsx", () => ({
  StackEntry: ({
    entry,
    choiceObjectId,
    groupCount,
    onHoverChange,
    style,
  }: {
    entry: { id: number };
    choiceObjectId?: number;
    groupCount?: number;
    onHoverChange?: (hovered: boolean) => void;
    style?: CSSProperties;
  }) => (
    <button
      type="button"
      data-testid={`stack-entry-${entry.id}`}
      data-choice-object-id={choiceObjectId}
      data-group-count={groupCount}
      style={style}
      onMouseEnter={() => onHoverChange?.(true)}
      onMouseLeave={() => onHoverChange?.(false)}
    />
  ),
}));

const { stackTargetArcsMock } = vi.hoisted(() => ({ stackTargetArcsMock: vi.fn() }));
vi.mock("../StackTargetArcs.tsx", () => ({
  StackTargetArcs: (props: unknown) => {
    stackTargetArcsMock(props);
    return null;
  },
}));

vi.mock("../../flexlayout/DraggableWidget.tsx", () => ({
  DraggableWidget: ({ children }: { children: ReactNode }) => <div>{children}</div>,
}));

describe("StackDisplay", () => {
  beforeEach(() => {
    useGameStore.getState().reset();
    stackTargetArcsMock.mockClear();
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

  it("keeps a coalesced group compact and chooses its exact nonrepresentative legal member", () => {
    const representative = buildStackEntry({ id: 10 });
    const legalMember = buildStackEntry({ id: 11 });
    const gameState = buildGameState({
      stack: [representative, legalMember],
      derived: {
        stack_display_groups: [{ representative: 10, count: 2, member_ids: [10, 11] }],
      },
      waiting_for: targetSelectionWaitingForFactory
        .withData({ selection: { current_slot: 0, current_legal_targets: [{ Object: 11 }] } })
        .build(),
    });

    act(() => {
      useGameStore.setState({ gameState, waitingFor: gameState.waiting_for });
    });
    render(<StackDisplay effectiveMultiplayerBoardLayout="focused" />);

    expect(screen.getByTestId("stack-entry-10")).toHaveAttribute("data-choice-object-id", "11");
    expect(screen.getByTestId("stack-entry-10")).toHaveAttribute("data-group-count", "2");
    expect(screen.queryByTestId("stack-entry-11")).not.toBeInTheDocument();
    expect(stackTargetArcsMock).toHaveBeenLastCalledWith(
      expect.objectContaining({
        stackEntryRepresentatives: new Map([[10, 10], [11, 10]]),
      }),
    );
  });

  it("expands a coalesced group when multiple members are legal and preserves their identities", () => {
    const first = buildStackEntry({ id: 10 });
    const second = buildStackEntry({ id: 11 });
    const gameState = buildGameState({
      stack: [first, second],
      derived: {
        stack_display_groups: [{ representative: 10, count: 2, member_ids: [10, 11] }],
      },
      waiting_for: targetSelectionWaitingForFactory
        .withData({
          selection: {
            current_slot: 0,
            current_legal_targets: [{ Object: 10 }, { Object: 11 }],
          },
        })
        .build(),
    });

    act(() => {
      useGameStore.setState({ gameState, waitingFor: gameState.waiting_for });
    });
    render(<StackDisplay effectiveMultiplayerBoardLayout="focused" />);

    expect(screen.getByTestId("stack-entry-10")).toHaveAttribute("data-group-count", "1");
    expect(screen.getByTestId("stack-entry-11")).toHaveAttribute("data-group-count", "1");
    expect(screen.getByTestId("stack-entry-10")).not.toHaveAttribute("data-choice-object-id");
    expect(screen.getByTestId("stack-entry-11")).not.toHaveAttribute("data-choice-object-id");
    expect(stackTargetArcsMock).toHaveBeenLastCalledWith(
      expect.objectContaining({
        stackEntryRepresentatives: new Map([[10, 10], [11, 11]]),
      }),
    );
  });

  it("keeps a coalesced group compact and inert when none of its members are legal", () => {
    const representative = buildStackEntry({ id: 10 });
    const member = buildStackEntry({ id: 11 });
    const gameState = buildGameState({
      stack: [representative, member],
      derived: {
        stack_display_groups: [{ representative: 10, count: 2, member_ids: [10, 11] }],
      },
    });

    act(() => {
      useGameStore.setState({ gameState, waitingFor: gameState.waiting_for });
    });
    render(<StackDisplay effectiveMultiplayerBoardLayout="focused" />);

    expect(screen.getByTestId("stack-entry-10")).toHaveAttribute("data-group-count", "2");
    expect(screen.getByTestId("stack-entry-10")).not.toHaveAttribute("data-choice-object-id");
    expect(screen.queryByTestId("stack-entry-11")).not.toBeInTheDocument();
  });

  it("renders the raw stack directly when engine group data is unavailable", () => {
    const first = buildStackEntry({ id: 10 });
    const second = buildStackEntry({ id: 11 });
    const gameState = buildGameState({ stack: [first, second] });

    act(() => {
      useGameStore.setState({ gameState, waitingFor: gameState.waiting_for });
    });
    render(<StackDisplay effectiveMultiplayerBoardLayout="focused" />);

    expect(screen.getByTestId("stack-entry-10")).toHaveAttribute("data-group-count", "1");
    expect(screen.getByTestId("stack-entry-11")).toHaveAttribute("data-group-count", "1");
  });
});
