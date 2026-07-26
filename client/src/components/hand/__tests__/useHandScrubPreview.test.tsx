import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import { useRef } from "react";
import { afterEach, describe, expect, it, vi } from "vitest";

import { useUiStore } from "../../../stores/uiStore.ts";
import { useHandScrubPreview } from "../useHandScrubPreview.ts";

function rect(left: number, right: number): DOMRect {
  return {
    bottom: 180,
    height: 140,
    left,
    right,
    top: 40,
    width: right - left,
    x: left,
    y: 40,
    toJSON: () => ({}),
  } as DOMRect;
}

function ScrubHarness({
  onOpen,
  isPlayable,
  canReleaseToCast,
  onReleaseToCast,
}: {
  onOpen: () => void;
  isPlayable?: (objectId: number) => boolean;
  canReleaseToCast?: (objectId: number) => boolean;
  onReleaseToCast?: (objectId: number) => void;
}) {
  const ref = useRef<HTMLDivElement | null>(null);
  const { handlers, consumeClick } = useHandScrubPreview(ref, true, {
    isPlayable,
    canReleaseToCast,
    onReleaseToCast,
  });

  return (
    <div
      ref={ref}
      data-testid="hand"
      {...handlers}
      onClick={() => {
        if (!consumeClick()) onOpen();
      }}
    >
      <div data-testid="card-11" data-hand-card data-object-id="11" />
      <div data-testid="card-12" data-hand-card data-object-id="12" />
    </div>
  );
}

afterEach(() => {
  cleanup();
  vi.useRealTimers();
  useUiStore.getState().dismissPreview();
});

describe("useHandScrubPreview", () => {
  it("holds to preview, scrubs adjacent cards, and suppresses the release click", () => {
    vi.useFakeTimers();
    const onOpen = vi.fn();
    render(<ScrubHarness onOpen={onOpen} />);
    const hand = screen.getByTestId("hand");
    const first = screen.getByTestId("card-11");
    const second = screen.getByTestId("card-12");
    first.getBoundingClientRect = () => rect(0, 100);
    second.getBoundingClientRect = () => rect(70, 170);

    fireEvent.pointerDown(hand, {
      button: 0,
      clientX: 40,
      clientY: 100,
      isPrimary: true,
      pointerId: 7,
      pointerType: "touch",
    });
    act(() => vi.advanceTimersByTime(400));

    expect(useUiStore.getState().inspectedObjectId).toBe(11);
    expect(useUiStore.getState().previewSticky).toBe(true);
    expect(first).toHaveAttribute("data-hand-touch-active", "true");

    fireEvent.pointerMove(hand, {
      clientX: 140,
      clientY: 100,
      isPrimary: true,
      pointerId: 7,
      pointerType: "touch",
    });

    expect(useUiStore.getState().inspectedObjectId).toBe(12);
    expect(first).not.toHaveAttribute("data-hand-touch-active");
    expect(second).toHaveAttribute("data-hand-touch-active", "true");

    fireEvent.pointerUp(hand, {
      button: 0,
      clientX: 140,
      clientY: 100,
      isPrimary: true,
      pointerId: 7,
      pointerType: "touch",
    });
    act(() => vi.advanceTimersByTime(100));
    fireEvent.click(hand);

    expect(useUiStore.getState().inspectedObjectId).toBeNull();
    expect(second).not.toHaveAttribute("data-hand-touch-active");
    expect(onOpen).not.toHaveBeenCalled();
  });

  it("allows a deliberate tap after consuming a held preview's delayed click", () => {
    vi.useFakeTimers();
    const onOpen = vi.fn();
    render(<ScrubHarness onOpen={onOpen} />);
    const hand = screen.getByTestId("hand");
    const first = screen.getByTestId("card-11");
    first.getBoundingClientRect = () => rect(0, 100);

    fireEvent.pointerDown(hand, {
      button: 0,
      clientX: 40,
      clientY: 100,
      isPrimary: true,
      pointerId: 10,
      pointerType: "touch",
    });
    act(() => vi.advanceTimersByTime(400));
    fireEvent.pointerUp(hand, {
      button: 0,
      clientX: 40,
      clientY: 100,
      isPrimary: true,
      pointerId: 10,
      pointerType: "touch",
    });
    act(() => vi.advanceTimersByTime(100));
    fireEvent.click(hand);

    fireEvent.pointerDown(hand, {
      button: 0,
      clientX: 40,
      clientY: 100,
      isPrimary: true,
      pointerId: 11,
      pointerType: "touch",
    });
    fireEvent.pointerUp(hand, {
      button: 0,
      clientX: 40,
      clientY: 100,
      isPrimary: true,
      pointerId: 11,
      pointerType: "touch",
    });
    fireEvent.click(hand);

    expect(onOpen).toHaveBeenCalledOnce();
  });

  it("moves a playable held preview and casts only after it enters the gold range", () => {
    vi.useFakeTimers();
    const onOpen = vi.fn();
    const onReleaseToCast = vi.fn();
    render(
      <ScrubHarness
        onOpen={onOpen}
        isPlayable={(objectId) => objectId === 11}
        canReleaseToCast={(objectId) => objectId === 11}
        onReleaseToCast={onReleaseToCast}
      />,
    );
    const hand = screen.getByTestId("hand");
    const first = screen.getByTestId("card-11");
    hand.getBoundingClientRect = () => rect(0, 200);
    first.getBoundingClientRect = () => rect(0, 100);

    fireEvent.pointerDown(hand, {
      button: 0,
      clientX: 40,
      clientY: 100,
      isPrimary: true,
      pointerId: 15,
      pointerType: "touch",
    });
    act(() => vi.advanceTimersByTime(400));

    expect(useUiStore.getState().mobileHandGesture).toMatchObject({
      objectId: 11,
      phase: "preview",
      sourceOrigin: {
        bottom: 180,
        centerX: 50,
        height: 140,
        rotation: 0,
        top: 40,
        width: 100,
      },
      offsetX: 0,
      offsetY: 0,
      playable: true,
      castReady: false,
    });

    fireEvent.pointerMove(hand, {
      clientX: 45,
      clientY: 88,
      isPrimary: true,
      pointerId: 15,
      pointerType: "touch",
    });

    expect(useUiStore.getState().mobileHandGesture).toMatchObject({
      objectId: 11,
      phase: "preview",
      castReady: false,
    });

    // Crossing above the hand is not sufficient by itself: the card remains
    // blue until the larger hand-specific cast distance is also crossed.
    fireEvent.pointerMove(hand, {
      clientX: 48,
      clientY: 39,
      isPrimary: true,
      pointerId: 15,
      pointerType: "touch",
    });

    expect(useUiStore.getState().mobileHandGesture).toMatchObject({
      objectId: 11,
      phase: "drag",
      castReady: false,
    });

    fireEvent.pointerMove(hand, {
      clientX: 52,
      clientY: 10,
      isPrimary: true,
      pointerId: 15,
      pointerType: "touch",
    });

    expect(useUiStore.getState().mobileHandGesture).toMatchObject({
      objectId: 11,
      phase: "drag",
      offsetX: 12,
      offsetY: -90,
      playable: true,
      castReady: true,
    });

    fireEvent.pointerUp(hand, {
      button: 0,
      clientX: 52,
      clientY: 10,
      isPrimary: true,
      pointerId: 15,
      pointerType: "touch",
    });

    expect(onReleaseToCast).toHaveBeenCalledOnce();
    expect(onReleaseToCast).toHaveBeenCalledWith(11);
    expect(useUiStore.getState().mobileHandGesture).toBeNull();
    expect(onOpen).not.toHaveBeenCalled();
  });

  it("disarms casting after returning toward the hand, including at pointer release", () => {
    vi.useFakeTimers();
    const onReleaseToCast = vi.fn();
    render(
      <ScrubHarness
        onOpen={vi.fn()}
        isPlayable={(objectId) => objectId === 11}
        canReleaseToCast={(objectId) => objectId === 11}
        onReleaseToCast={onReleaseToCast}
      />,
    );
    const hand = screen.getByTestId("hand");
    const first = screen.getByTestId("card-11");
    hand.getBoundingClientRect = () => rect(0, 200);
    first.getBoundingClientRect = () => rect(0, 100);

    fireEvent.pointerDown(hand, {
      button: 0,
      clientX: 40,
      clientY: 100,
      isPrimary: true,
      pointerId: 16,
      pointerType: "touch",
    });
    act(() => vi.advanceTimersByTime(400));

    fireEvent.pointerMove(hand, {
      clientX: 52,
      clientY: 10,
      isPrimary: true,
      pointerId: 16,
      pointerType: "touch",
    });
    expect(useUiStore.getState().mobileHandGesture).toMatchObject({
      phase: "drag",
      castReady: true,
    });

    fireEvent.pointerMove(hand, {
      clientX: 52,
      clientY: 90,
      isPrimary: true,
      pointerId: 16,
      pointerType: "touch",
    });
    expect(useUiStore.getState().mobileHandGesture).toMatchObject({
      phase: "drag",
      castReady: false,
    });

    // Arm it again, then release back near the hand without a final move event.
    // The pointerup coordinates must be authoritative rather than the stale
    // gold state from the preceding pointermove.
    fireEvent.pointerMove(hand, {
      clientX: 52,
      clientY: 10,
      isPrimary: true,
      pointerId: 16,
      pointerType: "touch",
    });
    fireEvent.pointerUp(hand, {
      button: 0,
      clientX: 52,
      clientY: 90,
      isPrimary: true,
      pointerId: 16,
      pointerType: "touch",
    });

    expect(onReleaseToCast).not.toHaveBeenCalled();
    expect(useUiStore.getState().mobileHandGesture).toBeNull();
  });

  it("keeps a short tap available for opening the hand drawer", () => {
    vi.useFakeTimers();
    const onOpen = vi.fn();
    render(<ScrubHarness onOpen={onOpen} />);
    const hand = screen.getByTestId("hand");

    fireEvent.pointerDown(hand, {
      button: 0,
      clientX: 40,
      clientY: 100,
      isPrimary: true,
      pointerId: 8,
      pointerType: "touch",
    });
    fireEvent.pointerUp(hand, {
      button: 0,
      clientX: 40,
      clientY: 100,
      isPrimary: true,
      pointerId: 8,
      pointerType: "touch",
    });
    fireEvent.click(hand);

    expect(onOpen).toHaveBeenCalledOnce();
    expect(useUiStore.getState().inspectedObjectId).toBeNull();
  });

  it("claims native touch movement on the hand without consuming a short tap", () => {
    vi.useFakeTimers();
    const onOpen = vi.fn();
    render(<ScrubHarness onOpen={onOpen} />);
    const hand = screen.getByTestId("hand");

    fireEvent.pointerDown(hand, {
      button: 0,
      clientX: 40,
      clientY: 100,
      isPrimary: true,
      pointerId: 13,
      pointerType: "touch",
    });

    const touchMove = new Event("touchmove", { bubbles: true, cancelable: true });
    hand.dispatchEvent(touchMove);
    expect(touchMove.defaultPrevented).toBe(true);

    fireEvent.pointerUp(hand, {
      button: 0,
      clientX: 42,
      clientY: 100,
      isPrimary: true,
      pointerId: 13,
      pointerType: "touch",
    });
    fireEvent.click(hand);

    expect(onOpen).toHaveBeenCalledOnce();
  });

  it("does not consume a long tap that starts outside a card", () => {
    vi.useFakeTimers();
    const onOpen = vi.fn();
    render(<ScrubHarness onOpen={onOpen} />);
    const hand = screen.getByTestId("hand");
    const first = screen.getByTestId("card-11");
    const second = screen.getByTestId("card-12");
    first.getBoundingClientRect = () => rect(0, 100);
    second.getBoundingClientRect = () => rect(70, 170);

    fireEvent.pointerDown(hand, {
      button: 0,
      clientX: 240,
      clientY: 100,
      isPrimary: true,
      pointerId: 9,
      pointerType: "touch",
    });
    act(() => vi.advanceTimersByTime(400));
    fireEvent.pointerUp(hand, {
      button: 0,
      clientX: 240,
      clientY: 100,
      isPrimary: true,
      pointerId: 9,
      pointerType: "touch",
    });
    fireEvent.click(hand);

    expect(onOpen).toHaveBeenCalledOnce();
    expect(useUiStore.getState().inspectedObjectId).toBeNull();
  });
});
