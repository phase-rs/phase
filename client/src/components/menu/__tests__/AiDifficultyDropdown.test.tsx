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

  it("does not offer cEDH as a selectable difficulty (it is a table-wide toggle)", () => {
    render(<AiDifficultyDropdown difficulty="Medium" onChange={() => {}} />);
    const options = screen.queryAllByRole("option", { name: /cEDH/i });
    expect(options).toHaveLength(0);
  });
});
