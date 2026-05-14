import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import type { WaitingFor } from "../../../adapter/types.ts";
import { NamedChoiceModal } from "../NamedChoiceModal.tsx";

const dispatchMock = vi.fn();

vi.mock("../../../hooks/useGameDispatch.ts", () => ({
  useGameDispatch: () => dispatchMock,
}));

type NamedChoiceData = Extract<WaitingFor, { type: "NamedChoice" }>["data"];

afterEach(() => {
  cleanup();
  dispatchMock.mockReset();
});

describe("NamedChoiceModal", () => {
  it("renders engine-provided restricted color options", () => {
    const data: NamedChoiceData = {
      player: 0,
      choice_type: { Color: { excluded: ["White"] } },
      options: ["Blue", "Black", "Red", "Green"],
    };

    render(<NamedChoiceModal data={data} />);

    expect(screen.getByRole("heading", { name: "Choose a Color" })).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "White" })).not.toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: "Blue" }));
    fireEvent.click(screen.getByRole("button", { name: "Confirm" }));

    expect(dispatchMock).toHaveBeenCalledWith({
      type: "ChooseOption",
      data: { choice: "Blue" },
    });
  });
});
