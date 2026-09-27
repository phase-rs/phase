import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import type { WaitingFor } from "../../../adapter/types.ts";
import { DistributeAmongModal } from "../DistributeAmongModal.tsx";

const { dispatchMock } = vi.hoisted(() => ({ dispatchMock: vi.fn() }));

vi.mock("../../../hooks/useGameDispatch.ts", () => ({
  useGameDispatch: () => dispatchMock,
}));

type DistributeAmong = Extract<WaitingFor, { type: "DistributeAmong" }>;

function distributeAmongData(
  unit: DistributeAmong["data"]["unit"],
  scope?: DistributeAmong["data"]["scope"],
): DistributeAmong["data"] {
  return {
    player: 0,
    total: 2,
    targets: [{ Object: 101 }, { Object: 102 }],
    unit,
    ...(scope ? { scope } : {}),
  };
}

afterEach(() => {
  cleanup();
  dispatchMock.mockReset();
});

describe("DistributeAmongModal", () => {
  it("formats canonical counter names for display", () => {
    const { rerender } = render(
      <DistributeAmongModal data={distributeAmongData({ type: "Counters", data: "P1P1" })} />,
    );

    expect(screen.getByText("Distribute 2 +1/+1 counter")).toBeInTheDocument();
    expect(
      screen.getByText("Assign at least 1 +1/+1 counter to each target. Remaining: 2"),
    ).toBeInTheDocument();

    rerender(
      <DistributeAmongModal data={distributeAmongData({ type: "Counters", data: "M1M1" })} />,
    );

    expect(screen.getByText("Distribute 2 -1/-1 counter")).toBeInTheDocument();
  });

  it("leaves generic counter names readable", () => {
    render(<DistributeAmongModal data={distributeAmongData({ type: "Counters", data: "lore" })} />);

    expect(screen.getByText("Distribute 2 lore counter")).toBeInTheDocument();
  });

  it("allows a resolution-time division among any number of candidates", () => {
    render(
      <DistributeAmongModal
        data={distributeAmongData(
          { type: "Damage" },
          { type: "ResolutionCandidates", data: { pending_effect: {} } },
        )}
      />,
    );

    expect(
      screen.getByText(
        "Assign at least 1 damage to each one you choose, among any number of them. Remaining: 2",
      ),
    ).toBeInTheDocument();
    const confirm = screen.getByRole("button", { name: /confirm/i });
    expect(confirm).toBeDisabled();

    const [firstPlus] = screen.getAllByRole("button", { name: "+" });
    fireEvent.click(firstPlus);
    fireEvent.click(firstPlus);
    expect(confirm).toBeEnabled();

    fireEvent.click(confirm);
    expect(dispatchMock).toHaveBeenCalledTimes(1);
    expect(dispatchMock).toHaveBeenCalledWith({
      type: "DistributeAmong",
      data: { distribution: [[{ Object: 101 }, 2]] },
    });
  });

  it("still requires every announced target to receive a share", () => {
    render(<DistributeAmongModal data={distributeAmongData({ type: "Damage" })} />);

    const confirm = screen.getByRole("button", { name: /confirm/i });
    const [firstPlus] = screen.getAllByRole("button", { name: "+" });
    fireEvent.click(firstPlus);
    fireEvent.click(firstPlus);
    expect(confirm).toBeDisabled();
    fireEvent.click(confirm);
    expect(dispatchMock).not.toHaveBeenCalled();
  });
});
