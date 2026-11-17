import { cleanup, render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";

import { ComboDeclarationPicker } from "../ComboDeclarationPicker";

afterEach(cleanup);

describe("ComboDeclarationPicker", () => {
  it("emits each primary declaration and tracks the active option", async () => {
    const user = userEvent.setup();
    const onChange = vi.fn();
    render(<ComboDeclarationPicker value={{ kind: "undeclared" }} onChange={onChange} />);

    expect(screen.getByRole("button", { name: "Not answered" })).toHaveAttribute(
      "aria-pressed",
      "true",
    );
    await user.click(screen.getByRole("button", { name: "None intended" }));
    await user.click(screen.getByRole("button", { name: "Intended" }));
    await user.click(screen.getByRole("button", { name: "Not answered" }));

    expect(onChange).toHaveBeenNthCalledWith(1, { kind: "none_intended" });
    expect(onChange).toHaveBeenNthCalledWith(2, { kind: "intended", window: null });
    expect(onChange).toHaveBeenNthCalledWith(3, { kind: "undeclared" });
  });

  it("shows window choices only for intended combos and emits early game", async () => {
    const user = userEvent.setup();
    const onChange = vi.fn();
    const { rerender } = render(
      <ComboDeclarationPicker value={{ kind: "none_intended" }} onChange={onChange} />,
    );
    expect(screen.queryByRole("group", { name: "When can it assemble?" })).toBeNull();

    await user.click(screen.getByRole("button", { name: "Intended" }));
    expect(onChange).toHaveBeenCalledWith({ kind: "intended", window: null });

    rerender(
      <ComboDeclarationPicker
        value={{ kind: "intended", window: null }}
        onChange={onChange}
      />,
    );
    expect(screen.getByRole("button", { name: "Unstated" })).toHaveAttribute(
      "aria-pressed",
      "true",
    );
    await user.click(screen.getByRole("button", { name: "Early game" }));
    expect(onChange).toHaveBeenCalledWith({ kind: "intended", window: "early_game" });
  });
});
