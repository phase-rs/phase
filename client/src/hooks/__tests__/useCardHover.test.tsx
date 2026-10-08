import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { useCardHover } from "../useCardHover.ts";
import { usePreferencesStore } from "../../stores/preferencesStore.ts";
import { useUiStore } from "../../stores/uiStore.ts";

const OBJECT_ID = 7;

function CardHoverHarness() {
  const { handlers } = useCardHover(OBJECT_ID);
  return <div data-testid="card" {...handlers} />;
}

// React derives onPointerEnter/onPointerLeave from pointerover/pointerout, which
// is what fireEvent.pointerEnter/pointerLeave fire — a hand-built
// `new PointerEvent("pointerenter")` would reach no handler at all.
describe("useCardHover", () => {
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

  // A pen genuinely hovers, so it is deliberately treated like a mouse; only
  // touch — where the enter is synthesized from a tap — is suppressed.
  for (const pointerType of ["mouse", "pen"] as const) {
    it(`opens the preview for a ${pointerType} pointer`, () => {
      render(<CardHoverHarness />);

      fireEvent.pointerEnter(screen.getByTestId("card"), { pointerType });

      expect(useUiStore.getState().inspectedObjectId).toBe(OBJECT_ID);
    });
  }

  it("closes the preview when a mouse pointer leaves", () => {
    render(<CardHoverHarness />);
    const card = screen.getByTestId("card");

    fireEvent.pointerEnter(card, { pointerType: "mouse" });
    fireEvent.pointerLeave(card, { pointerType: "mouse", relatedTarget: document.body });

    // The clear is deferred so a spurious leave from a layout shift can be
    // cancelled by a re-enter in the same frame.
    act(() => vi.advanceTimersByTime(50));
    expect(useUiStore.getState().inspectedObjectId).toBeNull();
  });

  it("ignores a touch-synthesized enter but still previews on long press", () => {
    render(<CardHoverHarness />);
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

  it("still cancels the long-press timer through the merged pointer-leave handler", () => {
    // useLongPress owns an onPointerLeave of its own. The hover handler wins the
    // key collision, so it must delegate — otherwise the timer survives a leave
    // and fires a sticky preview over whatever the pointer moved on to.
    render(<CardHoverHarness />);
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

  it("keeps the long-press preview open when the finger lifts", () => {
    // The leave handler's touch guard is load-bearing on a tablet: lifting the
    // finger fires a leave, which without the guard would immediately wipe the
    // sticky preview that the same gesture just opened.
    render(<CardHoverHarness />);
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

  it("previews on a host that reports no hover-capable input", () => {
    // The regression guard for the reported bug. A remote-desktop session
    // answers every hover capability query false while still delivering
    // `pointerType: "mouse"`; the preview must follow the event, not the
    // metadata. happy-dom would otherwise report `(any-hover: hover)` true,
    // so the RDP host has to be stubbed explicitly.
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

    render(<CardHoverHarness />);
    fireEvent.pointerEnter(screen.getByTestId("card"), { pointerType: "mouse" });

    expect(useUiStore.getState().inspectedObjectId).toBe(OBJECT_ID);
  });
});
