import { cleanup, render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";

import { AiDifficultyDropdown } from "../AiDifficultyDropdown";

afterEach(cleanup);

describe("AiDifficultyDropdown", () => {
  it("emits the selected difficulty", async () => {
    const user = userEvent.setup();
    const onChange = vi.fn();

    render(<AiDifficultyDropdown difficulty="Medium" onChange={onChange} />);

    await user.selectOptions(screen.getByRole("combobox", { name: "AI difficulty: Medium" }), "Hard");

    expect(onChange).toHaveBeenCalledWith("Hard");
  });
});

describe("AiDifficultyDropdown — cEDH", () => {
  it("renders the cEDH option in the select", () => {
    render(<AiDifficultyDropdown difficulty="Medium" onChange={() => {}} />);
    // The option label is "cEDH (B5 lock)" from AI_DIFFICULTIES; query all in
    // case the select renders an option for each ID including "CEDH".
    const options = screen.getAllByRole("option", { name: /cEDH/i });
    expect(options.length).toBeGreaterThan(0);
    expect(options[0]).toBeInTheDocument();
  });

  it("renders the B5 badge when difficulty is CEDH", () => {
    render(<AiDifficultyDropdown difficulty="CEDH" onChange={() => {}} />);
    const badge = screen.getByLabelText("B5 lock");
    expect(badge).toBeInTheDocument();
  });

  it("does not render the B5 badge when difficulty is not CEDH", () => {
    const { container } = render(<AiDifficultyDropdown difficulty="Hard" onChange={() => {}} />);
    const badge = container.querySelector('[aria-label="B5 lock"]');
    expect(badge).not.toBeInTheDocument();
  });
});
