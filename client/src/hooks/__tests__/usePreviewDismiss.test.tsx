import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { usePreviewDismiss } from "../usePreviewDismiss.ts";
import { useUiStore } from "../../stores/uiStore.ts";

function PreviewDismissHarness() {
  usePreviewDismiss();
  return (
    <>
      <div data-card-hover="true" data-testid="card" />
      <div data-card-preview="true" data-testid="preview" />
      <div data-testid="outside" />
    </>
  );
}

describe("usePreviewDismiss", () => {
  beforeEach(() => {
    vi.useFakeTimers();
    useUiStore.setState({ inspectedObjectId: 9, previewSticky: true, hoveredObjectId: 9 });
  });

  afterEach(() => {
    vi.useRealTimers();
    cleanup();
  });

  for (const pointerType of ["mouse", "pen", "touch"] as const) {
    it(`dismisses an outside ${pointerType} pointerdown after the delayed listener is installed`, () => {
      render(<PreviewDismissHarness />);
      const outside = screen.getByTestId("outside");
      vi.spyOn(document, "elementFromPoint").mockReturnValue(outside);

      act(() => vi.runOnlyPendingTimers());
      fireEvent.pointerDown(document, { clientX: 200, clientY: 200, pointerType });

      expect(useUiStore.getState().inspectedObjectId).toBeNull();
      expect(useUiStore.getState().hoveredObjectId).toBeNull();
    });
  }

  it("keeps a pointerdown over a card or a pointer-events-none preview", () => {
    render(<PreviewDismissHarness />);
    const card = screen.getByTestId("card");
    const preview = screen.getByTestId("preview");
    vi.spyOn(preview, "getBoundingClientRect").mockReturnValue({
      x: 10,
      y: 10,
      top: 10,
      left: 10,
      right: 110,
      bottom: 110,
      width: 100,
      height: 100,
      toJSON: () => ({}),
    });
    const elementFromPoint = vi.spyOn(document, "elementFromPoint");
    act(() => vi.runOnlyPendingTimers());

    elementFromPoint.mockReturnValue(card);
    fireEvent.pointerDown(document, { clientX: 1, clientY: 1, pointerType: "touch" });
    expect(useUiStore.getState().inspectedObjectId).toBe(9);

    // A desktop preview intentionally has pointer-events: none, so hit testing
    // returns the element behind it; geometry must still preserve the preview.
    elementFromPoint.mockReturnValue(screen.getByTestId("outside"));
    fireEvent.pointerDown(document, { clientX: 50, clientY: 50, pointerType: "mouse" });
    expect(useUiStore.getState().inspectedObjectId).toBe(9);
  });
});
