import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";

import { CubeSetupPanel } from "../CubeSetupPanel";

describe("CubeSetupPanel minimum deck size", () => {
  afterEach(cleanup);

  it("preserves its raw value across floor changes and submits the current maximum", async () => {
    const user = userEvent.setup();
    const onStart = vi.fn();
    const { rerender } = render(
      <CubeSetupPanel onStart={onStart} minimumDeckSize={73} />,
    );

    const minimum = screen.getByRole("spinbutton", { name: "Min Deck" });
    expect(minimum).toHaveValue(73);
    fireEvent.change(minimum, { target: { value: "89" } });
    expect(minimum).toHaveValue(89);

    rerender(<CubeSetupPanel onStart={onStart} minimumDeckSize={97} />);
    expect(minimum).toHaveValue(97);
    rerender(<CubeSetupPanel onStart={onStart} minimumDeckSize={61} />);
    expect(minimum).toHaveValue(89);

    await user.type(
      screen.getByPlaceholderText(/1 Lightning Bolt/),
      "1 Lightning Bolt",
    );
    await user.click(screen.getByRole("button", { name: "Start Cube Draft" }));

    expect(onStart).toHaveBeenCalledWith(expect.objectContaining({
      settings: expect.objectContaining({ min_deck_size: 89 }),
    }));
  });

  it("defaults to a positive floor and disables submission while pending", async () => {
    const user = userEvent.setup();
    const onStart = vi.fn();
    const { rerender } = render(<CubeSetupPanel onStart={onStart} />);

    const minimum = screen.getByRole("spinbutton", { name: "Min Deck" });
    fireEvent.change(minimum, { target: { value: "0" } });
    expect(minimum).toHaveValue(1);
    await user.type(screen.getByPlaceholderText(/1 Lightning Bolt/), "1 Opt");

    rerender(<CubeSetupPanel onStart={onStart} disabled />);
    expect(screen.getByRole("button", { name: "Start Cube Draft" })).toBeDisabled();
  });
});