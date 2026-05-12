import { cleanup, render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";

import { BracketFilter } from "../BracketFilter";

describe("BracketFilter", () => {
  afterEach(cleanup);
  it("renders one toggle button per WotC bracket", () => {
    render(<BracketFilter selected={[]} onChange={() => {}} />);
    for (const tier of ["1 Exhibition", "2 Core", "3 Upgraded", "4 Optimized", "5 cEDH"]) {
      expect(screen.getByRole("button", { name: tier })).toBeInTheDocument();
    }
  });

  it("marks selected buttons with aria-pressed=true", () => {
    render(<BracketFilter selected={[2, 4]} onChange={() => {}} />);
    expect(screen.getByRole("button", { name: "2 Core" })).toHaveAttribute("aria-pressed", "true");
    expect(screen.getByRole("button", { name: "4 Optimized" })).toHaveAttribute("aria-pressed", "true");
    expect(screen.getByRole("button", { name: "1 Exhibition" })).toHaveAttribute("aria-pressed", "false");
  });

  it("toggling a chip adds it when absent", async () => {
    const user = userEvent.setup();
    const onChange = vi.fn();
    render(<BracketFilter selected={[2]} onChange={onChange} />);

    await user.click(screen.getByRole("button", { name: "4 Optimized" }));

    expect(onChange).toHaveBeenCalledWith([2, 4]);
  });

  it("toggling a chip removes it when present", async () => {
    const user = userEvent.setup();
    const onChange = vi.fn();
    render(<BracketFilter selected={[2, 4]} onChange={onChange} />);

    await user.click(screen.getByRole("button", { name: "2 Core" }));

    expect(onChange).toHaveBeenCalledWith([4]);
  });
});
