import { afterEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen } from "@testing-library/react";

import { AmountInput, parseAmount } from "../AmountInput.tsx";

const LABELS = {
  input: "Enter amount",
  decrease: "Decrease amount",
  increase: "Increase amount",
};

// Building-block level (CLAUDE.md "test the building block, not the special case"): every row
// below exercises `parseAmount`/`AmountInput` across their input range, not one card's prompt.
describe("parseAmount", () => {
  // DISCRIMINATING rows — each is rejected by exactly one conjunct, named per row.
  // `/^\d+$/` guard rows: `Number()` alone lands every one of these INSIDE the window.
  it.each([
    ["", 0, 1000, 'Number("") === 0'],
    [" 7 ", 0, 1000, 'Number(" 7 ") === 7'],
    ["1.5", 0, 1000, 'Number("1.5") === 1.5'],
    ["1e3", 0, 1000, 'Number("1e3") === 1000'],
    ["+2", 0, 1000, 'Number("+2") === 2 — discriminates at min <= 2 ONLY'],
    ["0x10", 0, 100, 'Number("0x10") === 16'],
  ])(
    "rejects %j in [%i, %i] via the digit guard (%s)",
    (raw, min, max) => {
      expect(parseAmount(raw as string, min as number, max as number)).toBeNull();
    },
  );

  // DOMINATED rows — labelled so no later reader mistakes them for probes. `Number()` already
  // yields NaN (or a negative that `min >= 0` rejects), so deleting `/^\d+$/` leaves them green.
  it.each([
    ["12a", 0, 10, "NaN — DOMINATED"],
    ["abc", 0, 10, "NaN — DOMINATED"],
    ["Infinity", 0, 10, "Infinity > max — DOMINATED"],
    ["1_0", 0, 10, "NaN — DOMINATED"],
    ["-1", 0, 10, "min is u32-sourced so min >= 0 always — DOMINATED"],
    ["+2", 5, 1000, "2 < min=5, so the range conjunct already rejects — DOMINATED at min=5"],
  ])("rejects %j in [%i, %i] (%s)", (raw, min, max) => {
    expect(parseAmount(raw as string, min as number, max as number)).toBeNull();
  });

  // Range-conjunct rows.
  it("rejects a value above max", () => {
    expect(parseAmount("1001", 0, 1000)).toBeNull();
  });

  it("rejects a value below min", () => {
    expect(parseAmount("3", 5, 1000)).toBeNull();
  });

  // ACCEPTING rows — the positive controls that make every rejection above non-vacuous.
  it("accepts leading zeros as legal digits", () => {
    expect(parseAmount("007", 0, 10)).toBe(7);
  });

  it("accepts max itself (the window is upper-INCLUSIVE)", () => {
    expect(parseAmount("1000", 0, 1000)).toBe(1000);
  });

  it("accepts min itself (the window is lower-INCLUSIVE)", () => {
    expect(parseAmount("5", 5, 1000)).toBe(5);
  });
});

describe("AmountInput — rendered contract", () => {
  afterEach(() => {
    cleanup();
  });

  // No store reset: AmountInput is prop-driven and reads no store. `cleanup` IS mandatory —
  // there is no global RTL auto-cleanup in this repo, so a second mount would make
  // getByLabelText throw "Found multiple elements".

  it("A11Y/associate: announces the window and the validation message", () => {
    const onRawChange = vi.fn();
    const { rerender } = render(
      <AmountInput
        raw="abc"
        onRawChange={onRawChange}
        min={0}
        max={5}
        onSubmit={vi.fn()}
        labels={LABELS}
      />,
    );

    const box = screen.getByLabelText("Enter amount");
    expect(box).toHaveAttribute("aria-invalid", "true");

    // Both ids are associated while invalid, and both RESOLVE to the elements that carry the
    // hint and the validation copy — an id string alone would be a dangling reference.
    const describedBy = box.getAttribute("aria-describedby")?.split(" ") ?? [];
    expect(describedBy).toHaveLength(2);
    const described = describedBy.map((id) => document.getElementById(id));
    expect(described.map((el) => el?.textContent)).toEqual([
      "max 5",
      "Enter a whole number between 0 and 5",
    ]);

    expect(screen.getByLabelText("Decrease amount")).toBeInTheDocument();
    expect(screen.getByLabelText("Increase amount")).toBeInTheDocument();

    // A11Y/valid-state: the window stays ANNOUNCED once the entry is valid. This is the half
    // that reverting to `amount === null ? errorId : undefined` would red.
    rerender(
      <AmountInput
        raw="3"
        onRawChange={onRawChange}
        min={0}
        max={5}
        onSubmit={vi.fn()}
        labels={LABELS}
      />,
    );
    const validBox = screen.getByLabelText("Enter amount");
    expect(validBox).toHaveAttribute("aria-invalid", "false");
    const validIds = validBox.getAttribute("aria-describedby")?.split(" ") ?? [];
    expect(validIds).toHaveLength(1);
    expect(document.getElementById(validIds[0])?.textContent).toBe("max 5");
  });

  // A11Y/value: the three controls this box replaced announced their value natively
  // (`type="range"` ⇒ slider, `type="number"` ⇒ spinbutton). A bare `type="text"` announces
  // nothing, so stepping with +/− or the arrow keys would mutate a value exposed by NOTHING —
  // the live region below carries only the refusal, never the accepted amount.
  it("A11Y/value: the accepted amount is exposed, and withheld while out of range", () => {
    const { rerender } = render(
      <AmountInput
        raw="3"
        onRawChange={vi.fn()}
        min={0}
        max={5}
        onSubmit={vi.fn()}
        labels={LABELS}
      />,
    );

    const box = screen.getByLabelText("Enter amount");
    expect(box).toHaveAttribute("role", "spinbutton");
    expect(box).toHaveAttribute("aria-valuenow", "3");
    expect(box).toHaveAttribute("aria-valuemin", "0");
    expect(box).toHaveAttribute("aria-valuemax", "5");

    // Out of range: the value is WITHHELD rather than reported, so a screen reader is never told
    // a valuenow that contradicts valuemax. `aria-invalid` carries that state instead.
    rerender(
      <AmountInput
        raw="9"
        onRawChange={vi.fn()}
        min={0}
        max={5}
        onSubmit={vi.fn()}
        labels={LABELS}
      />,
    );
    const invalidBox = screen.getByLabelText("Enter amount");
    expect(invalidBox).not.toHaveAttribute("aria-valuenow");
    expect(invalidBox).toHaveAttribute("aria-invalid", "true");
  });

  // A11Y/live-region: the refusal must be ANNOUNCED, not merely associated. `aria-describedby`
  // is resolved by a screen reader when focus ARRIVES at the box, but the message appears while
  // focus is already inside it — so association alone is silent, and the player would have to tab
  // away and back to learn the entry was refused. The region must therefore PRE-EXIST its message
  // and only mutate its text; inserting the region (or inserting a node into one) is not
  // reliably announced.
  it("A11Y/live-region: the validation copy lives in a polite region that pre-exists it", () => {
    const { rerender } = render(
      <AmountInput
        raw="3"
        onRawChange={vi.fn()}
        min={0}
        max={5}
        onSubmit={vi.fn()}
        labels={LABELS}
      />,
    );

    // VALID first. This is the half that reds under a conditional mount, where the node does not
    // exist until the entry goes invalid.
    const region = document.querySelector('[role="status"]');
    expect(region).not.toBeNull();
    expect(region).toHaveAttribute("aria-live", "polite");
    expect(region).toBeEmptyDOMElement();

    rerender(
      <AmountInput
        raw="9"
        onRawChange={vi.fn()}
        min={0}
        max={5}
        onSubmit={vi.fn()}
        labels={LABELS}
      />,
    );

    // The SAME node, with mutated text — that identity is the property that makes it audible.
    // A different node here would mean the message was inserted rather than announced.
    expect(document.querySelector('[role="status"]')).toBe(region);
    expect(region).toHaveTextContent("Enter a whole number between 0 and 5");
  });

  it("A11Y/arrows: ArrowUp/ArrowDown step and clamp to the window", () => {
    const onRawChange = vi.fn();
    const { rerender } = render(
      <AmountInput
        raw="4"
        onRawChange={onRawChange}
        min={0}
        max={5}
        onSubmit={vi.fn()}
        labels={LABELS}
      />,
    );

    const box = screen.getByLabelText("Enter amount");
    fireEvent.keyDown(box, { key: "ArrowUp" });
    expect(onRawChange).toHaveBeenLastCalledWith("5");
    fireEvent.keyDown(box, { key: "ArrowDown" });
    expect(onRawChange).toHaveBeenLastCalledWith("3");

    // Clamped, not 6 — the distinct probe for `Math.min(…, max)`.
    rerender(
      <AmountInput
        raw="5"
        onRawChange={onRawChange}
        min={0}
        max={5}
        onSubmit={vi.fn()}
        labels={LABELS}
      />,
    );
    fireEvent.keyDown(screen.getByLabelText("Enter amount"), { key: "ArrowUp" });
    expect(onRawChange).toHaveBeenLastCalledWith("5");
  });

  it("renders the maximum shortcut only when opted in and writes the raw maximum", () => {
    const onRawChange = vi.fn();
    const { rerender } = render(
      <AmountInput
        raw="2"
        onRawChange={onRawChange}
        min={0}
        max={5}
        onSubmit={vi.fn()}
        labels={LABELS}
      />,
    );

    expect(screen.queryByRole("button", { name: "Set amount to maximum" })).toBeNull();

    rerender(
      <AmountInput
        raw="2"
        onRawChange={onRawChange}
        min={0}
        max={5}
        onSubmit={vi.fn()}
        labels={{ ...LABELS, setMaximum: "Set amount to maximum" }}
      />,
    );

    fireEvent.click(screen.getByRole("button", { name: "Set amount to maximum" }));
    expect(onRawChange).toHaveBeenLastCalledWith("5");
  });

  it("N5/recover-junk: steppers stay LIVE on an unreadable entry and re-enter the window", () => {
    const onRawChange = vi.fn();
    render(
      <AmountInput
        raw="abc"
        onRawChange={onRawChange}
        min={0}
        max={5}
        onSubmit={vi.fn()}
        labels={LABELS}
      />,
    );

    // The recovery anchor: with the slider deleted these are the only non-typing way out.
    expect(screen.getByLabelText("Decrease amount")).not.toBeDisabled();
    expect(screen.getByLabelText("Increase amount")).not.toBeDisabled();

    fireEvent.click(screen.getByLabelText("Increase amount"));
    expect(onRawChange).toHaveBeenLastCalledWith("1");
    fireEvent.click(screen.getByLabelText("Decrease amount"));
    expect(onRawChange).toHaveBeenLastCalledWith("0");
  });

  it("N5/recover-range: a READABLE out-of-range entry steps back to the bound, not to min", () => {
    const onRawChange = vi.fn();
    render(
      <AmountInput
        raw="1001"
        onRawChange={onRawChange}
        min={0}
        max={1000}
        onSubmit={vi.fn()}
        labels={LABELS}
      />,
    );

    // THE N5 discriminator. `step` reads the DIGITS of raw, so 1001 recovers to 1000. Under the
    // `(amount ?? min) + delta` implementation both of these would be "0"/"1" — and the "abc"
    // case above stays green under BOTH, which is why the two cases are separate tests.
    fireEvent.click(screen.getByLabelText("Decrease amount"));
    expect(onRawChange).toHaveBeenLastCalledWith("1000");
    fireEvent.click(screen.getByLabelText("Increase amount"));
    expect(onRawChange).toHaveBeenLastCalledWith("1000");
  });

  // The Enter route at the BUILDING-BLOCK level. The caller suites (T6b, AP/enter) assert only
  // that an invalid entry never reaches the engine — and that is satisfied EQUALLY by "onSubmit
  // was never called" and by "onSubmit was called and the caller's guard rejected it". Nothing
  // separated those two until this row, which is what turns the `onSubmit` prop comment ("MUST
  // itself reject an invalid amount — AmountInput deliberately does not re-guard") from an
  // assertion in prose into one the suite checks. The second half is the discriminating half:
  // re-adding a guard here is exactly the change the comment forbids, and it reds only this row.
  it("enter: Enter calls onSubmit, and calls it EVEN when the entry is out of range", () => {
    const onSubmit = vi.fn();
    const { rerender } = render(
      <AmountInput
        raw="3"
        onRawChange={vi.fn()}
        min={0}
        max={5}
        onSubmit={onSubmit}
        labels={LABELS}
      />,
    );
    fireEvent.keyDown(screen.getByLabelText("Enter amount"), { key: "Enter" });
    expect(onSubmit).toHaveBeenCalledTimes(1);

    rerender(
      <AmountInput
        raw="9"
        onRawChange={vi.fn()}
        min={0}
        max={5}
        onSubmit={onSubmit}
        labels={LABELS}
      />,
    );
    fireEvent.keyDown(screen.getByLabelText("Enter amount"), { key: "Enter" });
    expect(onSubmit).toHaveBeenCalledTimes(2);
  });

  // ponytail: no `min > 0` hint row here. A review asked for one on the premise that the branch
  // was uncovered; MEASURED, both legs are already dominated, so the row would detect nothing NEW.
  // Collapsing the ternary to the max-only string reds `ChooseXValueUI`'s "min 1 / max 10";
  // collapsing it to the two-sided string reds three rows (this file's A11Y/associate equality on
  // "max 5", the assist suite's "max 4", and a ChooseXValue row) because each asserts the hint's
  // FULL text, not a substring. A candidate row asserting both legs does red alongside them — it
  // is the two mutants' set of DETECTIONS it fails to grow, not their failure count. That is the
  // definition of dominated, and it is why the row is absent: it would report coverage it is not
  // supplying. Both mutants are one-line and re-runnable if you want to check the claim.
});
