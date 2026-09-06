import { cleanup, render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";

import { ConnectionModeSwitch } from "../ConnectionModeSwitch";

describe("ConnectionModeSwitch", () => {
  afterEach(cleanup);

  it("offers both modes and marks the active one as pressed", () => {
    render(<ConnectionModeSwitch value="server" onChange={vi.fn()} />);

    expect(
      screen.getByRole("button", { name: "Dedicated server" }),
    ).toHaveAttribute("aria-pressed", "true");
    expect(screen.getByRole("button", { name: "You host (P2P)" })).toHaveAttribute(
      "aria-pressed",
      "false",
    );
  });

  it("follows `value` rather than any state of its own", () => {
    const { rerender } = render(
      <ConnectionModeSwitch value="server" onChange={vi.fn()} />,
    );
    rerender(<ConnectionModeSwitch value="p2p" onChange={vi.fn()} />);

    expect(screen.getByRole("button", { name: "You host (P2P)" })).toHaveAttribute(
      "aria-pressed",
      "true",
    );
    expect(
      screen.getByRole("button", { name: "Dedicated server" }),
    ).toHaveAttribute("aria-pressed", "false");
  });

  it("reports the mode the user picked", async () => {
    const user = userEvent.setup();
    const onChange = vi.fn();
    render(<ConnectionModeSwitch value="server" onChange={onChange} />);

    await user.click(screen.getByRole("button", { name: "You host (P2P)" }));
    expect(onChange).toHaveBeenCalledWith("p2p");

    // Re-picking the active mode still reports it: the parent owns the value,
    // so the control must not decide a click is a no-op.
    await user.click(screen.getByRole("button", { name: "Dedicated server" }));
    expect(onChange).toHaveBeenLastCalledWith("server");
  });

  it("names the group so a surface can place it beside its own label", () => {
    render(<ConnectionModeSwitch value="p2p" onChange={vi.fn()} />);

    expect(screen.getByRole("group", { name: "Who hosts?" })).toBeInTheDocument();
  });
});
