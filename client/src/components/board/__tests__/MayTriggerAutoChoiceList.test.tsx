import { act } from "react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen } from "@testing-library/react";

import { MayTriggerAutoChoiceList } from "../MayTriggerAutoChoiceList.tsx";
import type { MayTriggerAutoChoiceRecord } from "../../../adapter/types.ts";
import { useGameStore } from "../../../stores/gameStore.ts";
import { buildGameObject, buildObjectMap } from "../../../test/factories/gameObjectFactory.ts";
import { buildGameState } from "../../../test/factories/gameStateFactory.ts";

const { dispatchActionMock } = vi.hoisted(() => ({ dispatchActionMock: vi.fn() }));
vi.mock("../../../game/dispatch.ts", () => ({ dispatchAction: dispatchActionMock }));

function record(sourceId: number, accept: boolean): MayTriggerAutoChoiceRecord {
  return {
    key: {
      player: 0,
      source_id: sourceId,
      origin: { type: "Printed", trigger_index: 0 },
    },
    choice: { type: accept ? "Accept" : "Decline" },
  };
}

function seed(records: MayTriggerAutoChoiceRecord[]) {
  const gameState = buildGameState({
    objects: buildObjectMap(
      buildGameObject({ id: 50, card_id: 9, name: "Kodama of the East Tree", zone: "Battlefield" }),
    ),
    may_trigger_auto_choices: records,
  });
  act(() => {
    useGameStore.setState({ gameState, waitingFor: gameState.waiting_for });
  });
}

describe("MayTriggerAutoChoiceList", () => {
  beforeEach(() => {
    useGameStore.getState().reset();
    dispatchActionMock.mockClear();
  });

  afterEach(() => {
    cleanup();
  });

  it("renders nothing when the viewer holds no auto-choices", () => {
    seed([]);
    const { container } = render(<MayTriggerAutoChoiceList />);
    expect(container).toBeEmptyDOMElement();
  });

  it("collapses stored auto-choices into a single chip that shows the count", () => {
    seed([record(50, true), record(51, false)]);
    render(<MayTriggerAutoChoiceList />);

    expect(screen.getByText("2")).toBeInTheDocument();
    expect(screen.queryByText("Clear all")).not.toBeInTheDocument();
    expect(screen.queryByText("Remove")).not.toBeInTheDocument();
  });

  it("reveals the removable list only after the chip is opened, labeled with the stored decision", () => {
    seed([record(50, true)]);
    render(<MayTriggerAutoChoiceList />);

    fireEvent.click(screen.getByRole("button", { name: /auto-deciding/i }));

    expect(screen.getByText("Clear all")).toBeInTheDocument();
    // Source name plus the stored decision (Accept -> "Yes").
    expect(screen.getByText(/Kodama of the East Tree — Yes/)).toBeInTheDocument();
    expect(screen.getByText("Remove")).toBeInTheDocument();
  });

  it("dispatches a Remove that echoes the stored key verbatim", () => {
    seed([record(50, true)]);
    render(<MayTriggerAutoChoiceList />);

    fireEvent.click(screen.getByRole("button", { name: /auto-deciding/i }));
    fireEvent.click(screen.getByText("Remove"));

    expect(dispatchActionMock).toHaveBeenCalledWith({
      type: "SetMayTriggerAutoChoice",
      data: {
        op: {
          type: "Remove",
          data: {
            key: {
              player: 0,
              source_id: 50,
              origin: { type: "Printed", trigger_index: 0 },
            },
          },
        },
      },
    });
  });

  it("dispatches ClearAll and closes the popover when Clear all is chosen", () => {
    seed([record(50, true), record(51, false)]);
    render(<MayTriggerAutoChoiceList />);

    fireEvent.click(screen.getByRole("button", { name: /auto-deciding/i }));
    fireEvent.click(screen.getByText("Clear all"));

    expect(dispatchActionMock).toHaveBeenCalledWith({
      type: "SetMayTriggerAutoChoice",
      data: { op: { type: "ClearAll" } },
    });
    expect(screen.queryByText("Clear all")).not.toBeInTheDocument();
  });
});
