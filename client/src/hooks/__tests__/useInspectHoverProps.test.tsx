import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { useInspectHoverProps } from "../useInspectHoverProps.ts";
import { usePreferencesStore } from "../../stores/preferencesStore.ts";
import { useUiStore } from "../../stores/uiStore.ts";

const OBJECT_ID = 7;

function HoverPropsHarness() {
  const hoverProps = useInspectHoverProps();
  return <div data-testid="card" {...hoverProps(OBJECT_ID)} />;
}

// React derives onPointerEnter/onPointerLeave from pointerover/pointerout, which
// is what fireEvent.pointerEnter/pointerLeave fire — a hand-built
// `new PointerEvent("pointerenter")` would reach no handler at all.
describe("useInspectHoverProps", () => {
  beforeEach(() => {
    vi.useFakeTimers();
    Object.defineProperty(window, "innerWidth", { configurable: true, value: 1920 });
    usePreferencesStore.setState({ cardPreviewMode: "follow", cardPreviewHoverDelayMs: 0 });
    useUiStore.setState({ inspectedObjectId: null, hoveredObjectId: null, previewSticky: false });
  });

  afterEach(() => {
    vi.useRealTimers();
    vi.restoreAllMocks();
    cleanup();
  });

  it("previews on a host that reports no hover-capable input", () => {
    // The regression guard for the reported bug. A remote-desktop session
    // answers every hover capability query false while still delivering
    // `pointerType: "mouse"`; the preview must follow the event, not the
    // metadata. happy-dom would otherwise report `(any-hover: hover)` true, so
    // the RDP host has to be stubbed explicitly.
    vi.spyOn(window, "matchMedia").mockImplementation((media: string) => ({
      matches: false,
      media,
      onchange: null,
      addEventListener: vi.fn(),
      removeEventListener: vi.fn(),
      addListener: vi.fn(),
      removeListener: vi.fn(),
      dispatchEvent: vi.fn(),
    }) as unknown as MediaQueryList);

    render(<HoverPropsHarness />);
    fireEvent.pointerEnter(screen.getByTestId("card"), { pointerType: "mouse" });

    expect(useUiStore.getState().inspectedObjectId).toBe(OBJECT_ID);
  });

  // A pen genuinely hovers, so it is deliberately treated like a mouse; only
  // touch — where the enter is synthesized from a tap — is suppressed.
  for (const pointerType of ["mouse", "pen"] as const) {
    it(`opens and closes the preview for a ${pointerType} pointer`, () => {
      render(<HoverPropsHarness />);
      const card = screen.getByTestId("card");

      fireEvent.pointerEnter(card, { pointerType });
      expect(useUiStore.getState().inspectedObjectId).toBe(OBJECT_ID);

      fireEvent.pointerLeave(card, { pointerType, relatedTarget: document.body });
      // The clear is deferred so a spurious leave from a layout shift can be
      // cancelled by a re-enter in the same frame.
      act(() => vi.advanceTimersByTime(50));
      expect(useUiStore.getState().inspectedObjectId).toBeNull();
    });
  }

  it("ignores a touch-synthesized enter but still previews on long press", () => {
    render(<HoverPropsHarness />);
    const card = screen.getByTestId("card");

    fireEvent.pointerEnter(card, { pointerType: "touch" });
    expect(useUiStore.getState().inspectedObjectId).toBeNull();

    fireEvent.pointerDown(card, {
      button: 0,
      clientX: 10,
      clientY: 10,
      isPrimary: true,
      pointerId: 1,
      pointerType: "touch",
    });
    act(() => vi.advanceTimersByTime(500));

    expect(useUiStore.getState().inspectedObjectId).toBe(OBJECT_ID);
    expect(useUiStore.getState().previewSticky).toBe(true);
  });

  it("does not arm the long press for a mouse", () => {
    // Long-press stands in for hover, so a mouse must never arm it. If it did,
    // any 500ms hold would open a sticky preview and the onClickCapture guard
    // would then swallow the click that ended it — on a choice modal that reads
    // as a card that cannot be picked.
    render(<HoverPropsHarness />);

    fireEvent.pointerDown(screen.getByTestId("card"), {
      button: 0,
      clientX: 10,
      clientY: 10,
      isPrimary: true,
      pointerId: 1,
      pointerType: "mouse",
    });
    act(() => vi.advanceTimersByTime(500));

    expect(useUiStore.getState().inspectedObjectId).toBeNull();
    expect(useUiStore.getState().previewSticky).toBe(false);
  });

  it("keeps the long-press preview open when the finger lifts", () => {
    // The leave handler's touch guard is load-bearing on a tablet: lifting the
    // finger fires a leave, which without the guard would immediately wipe the
    // sticky preview that the same gesture just opened.
    render(<HoverPropsHarness />);
    const card = screen.getByTestId("card");

    fireEvent.pointerDown(card, {
      button: 0,
      clientX: 10,
      clientY: 10,
      isPrimary: true,
      pointerId: 1,
      pointerType: "touch",
    });
    act(() => vi.advanceTimersByTime(500));
    expect(useUiStore.getState().inspectedObjectId).toBe(OBJECT_ID);

    fireEvent.pointerLeave(card, { pointerId: 1, pointerType: "touch", relatedTarget: document.body });
    act(() => vi.advanceTimersByTime(50));

    expect(useUiStore.getState().inspectedObjectId).toBe(OBJECT_ID);
    expect(useUiStore.getState().previewSticky).toBe(true);
  });

  it("cancels the long-press timer when the finger slides off the card", () => {
    // The composed onPointerLeave must still delegate to useLongPress's own
    // handler; dropping that delegation leaves the timer running and fires a
    // sticky preview over whatever the pointer moved on to.
    render(<HoverPropsHarness />);
    const card = screen.getByTestId("card");

    fireEvent.pointerDown(card, {
      button: 0,
      clientX: 10,
      clientY: 10,
      isPrimary: true,
      pointerId: 1,
      pointerType: "touch",
    });
    fireEvent.pointerLeave(card, { pointerId: 1, pointerType: "touch", relatedTarget: document.body });
    act(() => vi.advanceTimersByTime(500));

    expect(useUiStore.getState().inspectedObjectId).toBeNull();
    expect(useUiStore.getState().previewSticky).toBe(false);
  });
});
