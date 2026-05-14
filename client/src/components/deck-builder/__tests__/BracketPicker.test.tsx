import { cleanup, render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";

import { BracketPicker } from "../BracketPicker";

afterEach(cleanup);

describe("BracketPicker", () => {
  it("renders an Unrated chip plus 1..5", () => {
    render(<BracketPicker value={null} onChange={() => {}} />);
    expect(screen.getByRole("button", { name: "Unrated" })).toBeInTheDocument();
    for (const tier of ["1 Exhibition", "2 Core", "3 Upgraded", "4 Optimized", "5 cEDH"]) {
      expect(screen.getByRole("button", { name: tier })).toBeInTheDocument();
    }
  });

  it("marks the active chip with aria-pressed=true (Unrated when value is null)", () => {
    render(<BracketPicker value={null} onChange={() => {}} />);
    expect(screen.getByRole("button", { name: "Unrated" })).toHaveAttribute("aria-pressed", "true");
    expect(screen.getByRole("button", { name: "3 Upgraded" })).toHaveAttribute("aria-pressed", "false");
  });

  it("marks the active chip with aria-pressed=true (numeric when value is set)", () => {
    render(<BracketPicker value={3} onChange={() => {}} />);
    expect(screen.getByRole("button", { name: "Unrated" })).toHaveAttribute("aria-pressed", "false");
    expect(screen.getByRole("button", { name: "3 Upgraded" })).toHaveAttribute("aria-pressed", "true");
  });

  it("clicking a numeric chip emits that bracket", async () => {
    const user = userEvent.setup();
    const onChange = vi.fn();
    render(<BracketPicker value={null} onChange={onChange} />);

    await user.click(screen.getByRole("button", { name: "4 Optimized" }));

    expect(onChange).toHaveBeenCalledWith(4);
  });

  it("clicking Unrated emits null", async () => {
    const user = userEvent.setup();
    const onChange = vi.fn();
    render(<BracketPicker value={3} onChange={onChange} />);

    await user.click(screen.getByRole("button", { name: "Unrated" }));

    expect(onChange).toHaveBeenCalledWith(null);
  });
});
