/**
 * IntegerField — the commit contract.
 *
 * The component exists because a per-keystroke clamp corrupts what the user
 * typed (see the component docstring). These tests pin both halves of the
 * contract it replaced that clamp with: a reading is committed only when it is
 * a whole number at or above `min`, and anything else leaves the last
 * committed value standing while the box keeps showing what was typed.
 */
import { cleanup, render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";
import { useState } from "react";

import { IntegerField } from "../IntegerField";

/** Mirrors a real call site: the committed value is owned by the parent. */
function Harness({ initial, min = 1, onCommit }: {
  initial: number;
  min?: number;
  onCommit?: (next: number) => void;
}) {
  const [value, setValue] = useState(initial);
  return (
    <IntegerField
      value={value}
      min={min}
      ariaLabel="Starting Life"
      onCommit={(next) => {
        onCommit?.(next);
        setValue(next);
      }}
    />
  );
}

afterEach(cleanup);

describe("IntegerField", () => {
  it("commits a value typed after clearing the box", async () => {
    const user = userEvent.setup();
    const onCommit = vi.fn();
    render(<Harness initial={40} onCommit={onCommit} />);

    const field = screen.getByLabelText("Starting Life");
    await user.clear(field);
    await user.type(field, "25");

    // The clamp this component replaced turned this exact sequence into 125.
    expect(onCommit).toHaveBeenLastCalledWith(25);
  });

  it("leaves the last committed value standing while the box is empty", async () => {
    const user = userEvent.setup();
    const onCommit = vi.fn();
    render(<Harness initial={40} onCommit={onCommit} />);

    const field = screen.getByLabelText("Starting Life");
    await user.clear(field);

    expect(onCommit).not.toHaveBeenCalled();
    expect(field).toHaveValue(null); // the box is empty, mid-edit
  });

  it("re-syncs the display to the committed value on blur", async () => {
    const user = userEvent.setup();
    render(<Harness initial={40} />);

    const field = screen.getByLabelText("Starting Life");
    await user.clear(field);
    await user.tab();

    expect(field).toHaveValue(40);
  });

  // `type="number"` accepts decimal and exponent text, so a value can arrive
  // whole — pasted, or restored by the browser — without passing through the
  // intermediate whole-number states that typing produces. That single arrival
  // is where a `parseInt` reading would commit a number the box never showed.
  //
  // Only the decimal half of that is testable here. Exponent text ("1e2", which
  // `parseInt` reads as 1 and the box means as 100) would be the other half, but
  // happy-dom normalizes a pasted "1e2" to the literal "100", so `parseInt`
  // reaches the same answer and the test would stay green against the very bug
  // it claimed to catch. Browsers only blank *invalid* input, leaving "1e2"
  // standing, so the divergence is real in production and unobservable here.
  it("rejects a pasted decimal instead of committing its integer part", async () => {
    const user = userEvent.setup();
    const onCommit = vi.fn();
    render(<Harness initial={40} onCommit={onCommit} />);

    const field = screen.getByLabelText("Starting Life");
    await user.clear(field);
    await user.click(field);
    await user.paste("20.5");

    // `parseInt("20.5")` is 20 — committing that silently disagrees with the
    // 20.5 the box is showing. Nothing whole was ever entered, so nothing is
    // committed and the previous value stands.
    expect(onCommit).not.toHaveBeenCalled();
    expect(field).toHaveValue(20.5);
  });

  it("does not commit a reading below min", async () => {
    const user = userEvent.setup();
    const onCommit = vi.fn();
    render(<Harness initial={40} min={10} onCommit={onCommit} />);

    const field = screen.getByLabelText("Starting Life");
    await user.clear(field);
    await user.type(field, "5");

    expect(onCommit).not.toHaveBeenCalled();
  });
});
