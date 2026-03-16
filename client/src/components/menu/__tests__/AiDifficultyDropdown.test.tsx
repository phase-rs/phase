import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";

import { AiDifficultyDropdown } from "../AiDifficultyDropdown";

describe("AiDifficultyDropdown", () => {
  it("emits the selected difficulty", async () => {
    const user = userEvent.setup();
    const onChange = vi.fn();

    render(<AiDifficultyDropdown difficulty="Medium" onChange={onChange} />);

    await user.selectOptions(screen.getByRole("combobox", { name: "AI difficulty: Medium" }), "Hard");

    expect(onChange).toHaveBeenCalledWith("Hard");
  });
});
