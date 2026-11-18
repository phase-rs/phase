import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { GameplayTooltip } from "../GameplayTooltip.tsx";

// happy-dom matches `:focus-visible` for every focused element. A browser
// matches it mainly for keyboard focus, and not for the focus a tap or a click
// gives a button, so each test decides which kind of focus it is delivering.
let focusVisible = false;

function renderTooltip() {
  render(
    <button type="button" className="group relative">
      Resolve
      <GameplayTooltip>Pass priority.</GameplayTooltip>
    </button>,
  );
  return {
    trigger: screen.getByRole("button", { name: "Resolve" }),
    tooltip: screen.getByRole("tooltip", { hidden: true }),
  };
}

describe("GameplayTooltip", () => {
  beforeEach(() => {
    focusVisible = false;
    const matches = Element.prototype.matches;
    vi.spyOn(Element.prototype, "matches").mockImplementation(function (this: Element, selector: string) {
      return selector === ":focus-visible" ? focusVisible : matches.call(this, selector);
    });
  });

  afterEach(() => {
    cleanup();
    vi.restoreAllMocks();
  });

  it.each(["mouse", "pen"])("opens when a %s pointer moves over its trigger", (pointerType) => {
    const { trigger, tooltip } = renderTooltip();
    expect(tooltip).not.toBeVisible();

    fireEvent.pointerMove(trigger, { pointerType });

    expect(tooltip).toBeVisible();
  });

  it("stays closed when its trigger appears under a pointer that has not moved", () => {
    const { trigger, tooltip } = renderTooltip();

    fireEvent.pointerOver(trigger, { pointerType: "mouse" });
    fireEvent.pointerEnter(trigger, { pointerType: "mouse" });
    expect(tooltip).not.toBeVisible();

    fireEvent.pointerMove(trigger, { pointerType: "mouse" });
    expect(tooltip).toBeVisible();
  });

  it("ignores a touch pointer moving over its trigger", () => {
    const { trigger, tooltip } = renderTooltip();

    fireEvent.pointerMove(trigger, { pointerType: "touch" });
    expect(tooltip).not.toBeVisible();

    fireEvent.pointerMove(trigger, { pointerType: "mouse" });
    expect(tooltip).toBeVisible();
  });

  it("stays closed through a touch tap, even though the tap focuses its trigger", () => {
    const { trigger, tooltip } = renderTooltip();

    fireEvent.pointerEnter(trigger, { pointerType: "touch" });
    fireEvent.pointerDown(trigger, { pointerType: "touch" });
    fireEvent.pointerMove(trigger, { pointerType: "touch" });
    expect(tooltip).not.toBeVisible();
    fireEvent.pointerUp(trigger, { pointerType: "touch" });
    fireEvent.pointerLeave(trigger, { pointerType: "touch" });
    act(() => trigger.focus());
    fireEvent.click(trigger);

    expect(trigger).toHaveFocus();
    expect(tooltip).not.toBeVisible();

    // The same focus path opens it once the browser marks the focus visible,
    // so the tap is refused by the check rather than never reaching it.
    act(() => trigger.blur());
    focusVisible = true;
    act(() => trigger.focus());
    expect(tooltip).toBeVisible();
  });

  it("stays closed when a script focuses its trigger after pointer input", () => {
    const { trigger, tooltip } = renderTooltip();

    act(() => trigger.focus());

    expect(trigger).toHaveFocus();
    expect(tooltip).not.toBeVisible();

    act(() => trigger.blur());
    focusVisible = true;
    act(() => trigger.focus());
    expect(tooltip).toBeVisible();
  });

  it("opens on keyboard focus", () => {
    const { trigger, tooltip } = renderTooltip();
    focusVisible = true;

    act(() => trigger.focus());

    expect(tooltip).toBeVisible();
  });

  it("closes when the pointer leaves its trigger", () => {
    const { trigger, tooltip } = renderTooltip();
    fireEvent.pointerMove(trigger, { pointerType: "mouse" });
    expect(tooltip).toBeVisible();

    fireEvent.pointerLeave(trigger, { pointerType: "mouse" });

    expect(tooltip).not.toBeVisible();
  });

  it("closes when focus leaves its trigger", () => {
    const { trigger, tooltip } = renderTooltip();
    focusVisible = true;
    act(() => trigger.focus());
    expect(tooltip).toBeVisible();

    act(() => trigger.blur());

    expect(tooltip).not.toBeVisible();
  });
});
