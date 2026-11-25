import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, expect, it, vi } from "vitest";

import { StepperRow } from "../StepperRow.tsx";

afterEach(cleanup);

it("renders caller-provided shortcuts in order with their accessible names and disabled states", () => {
  const lethal = vi.fn();
  const clear = vi.fn();
  render(
    <StepperRow
      value={1}
      decrementLabel="−"
      incrementLabel="+"
      onDecrement={vi.fn()}
      onIncrement={vi.fn()}
      decrementDisabled={false}
      incrementDisabled={true}
      shortcuts={[
        { key: "lethal", label: "Lethal", ariaLabel: "Assign lethal to Guard", onClick: lethal, disabled: true },
        { key: "clear", label: "Clear", ariaLabel: "Clear Guard", onClick: clear, disabled: false },
      ]}
    >
      Guard
    </StepperRow>,
  );

  const lethalButton = screen.getByRole("button", { name: "Assign lethal to Guard" });
  const clearButton = screen.getByRole("button", { name: "Clear Guard" });
  const buttons = screen.getAllByRole("button");
  expect(buttons).toEqual([
    screen.getByRole("button", { name: "−" }),
    screen.getByRole("button", { name: "+" }),
    lethalButton,
    clearButton,
  ]);
  for (const button of buttons) {
    expect(button).toHaveClass("!min-h-11", "min-w-11");
  }
  expect(lethalButton).toBeDisabled();
  expect(clearButton).toBeEnabled();
  fireEvent.click(lethalButton);
  fireEvent.click(clearButton);
  expect(lethal).not.toHaveBeenCalled();
  expect(clear).toHaveBeenCalledTimes(1);
});
