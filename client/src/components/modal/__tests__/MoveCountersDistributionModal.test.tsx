import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { WaitingFor } from "../../../adapter/types.ts";
import { MoveCountersDistributionModal } from "../MoveCountersDistributionModal.tsx";

const dispatchMock = vi.fn();

vi.mock("../../../hooks/useGameDispatch.ts", () => ({
  useGameDispatch: () => dispatchMock,
}));

type RemoveCountersChoice = Extract<WaitingFor, { type: "RemoveCountersChoice" }>;

function removal(maxTotal: number | null): RemoveCountersChoice {
  return {
    type: "RemoveCountersChoice",
    data: {
      player: 0,
      source_id: 7,
      counter_type: null,
      available: [
        ["P1P1", 2],
        ["M1M1", 1],
      ],
      max_total: maxTotal,
      pending_effect: {},
    },
  };
}

function plusButtons(): HTMLElement[] {
  return screen.getAllByRole("button", { name: "+" });
}

afterEach(() => {
  cleanup();
});

describe("MoveCountersDistributionModal in removal mode", () => {
  beforeEach(() => {
    dispatchMock.mockReset();
  });

  // CR 608.2d: "remove up to N counters" (Hex Parasite, Glissa Sunslayer) is
  // capped at the engine-published total across every kind.
  it("stops adding counters at the engine's max_total across kinds", () => {
    render(<MoveCountersDistributionModal waitingFor={removal(1)} />);

    expect(screen.getByText(/Remaining: 1/)).toBeInTheDocument();
    fireEvent.click(plusButtons()[0]);

    expect(screen.getByText(/Remaining: 0/)).toBeInTheDocument();
    for (const button of plusButtons()) {
      expect(button).toBeDisabled();
    }

    fireEvent.click(screen.getByRole("button", { name: "Confirm" }));
    expect(dispatchMock).toHaveBeenCalledWith({
      type: "ChooseCountersToRemove",
      data: { selections: [{ counter_type: "P1P1", count: 1 }] },
    });
  });

  // CR 107.1c: "any number" carries no cap; every available counter may go.
  it("lets every available counter be removed without a cap", () => {
    render(<MoveCountersDistributionModal waitingFor={removal(null)} />);

    expect(screen.getByText(/Remaining: 3/)).toBeInTheDocument();
    fireEvent.click(plusButtons()[0]);
    fireEvent.click(plusButtons()[0]);
    fireEvent.click(plusButtons()[1]);

    expect(screen.getByText(/Remaining: 0/)).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "Confirm" }));
    expect(dispatchMock).toHaveBeenCalledWith({
      type: "ChooseCountersToRemove",
      data: {
        selections: [
          { counter_type: "P1P1", count: 2 },
          { counter_type: "M1M1", count: 1 },
        ],
      },
    });
  });
});
