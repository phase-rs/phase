import { act, renderHook } from "@testing-library/react";
import type { ThreeEvent } from "@react-three/fiber";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import {
  TABLETOP_CARD_HOLD_DELAY_MS,
  useTabletopCardHold,
} from "../useTabletopCardHold.ts";

function pointerEvent(
  overrides: Partial<ThreeEvent<PointerEvent>> = {},
): ThreeEvent<PointerEvent> {
  const nativeEvent = {
    button: 0,
    clientX: 20,
    clientY: 30,
    isPrimary: true,
    pointerId: 7,
    pointerType: "touch",
    preventDefault: vi.fn(),
  } as unknown as PointerEvent;
  return {
    button: 0,
    clientX: 20,
    clientY: 30,
    isPrimary: true,
    nativeEvent,
    pointerId: 7,
    pointerType: "touch",
    preventDefault: vi.fn(),
    stopPropagation: vi.fn(),
    target: {
      releasePointerCapture: vi.fn(),
      setPointerCapture: vi.fn(),
    },
    ...overrides,
  } as unknown as ThreeEvent<PointerEvent>;
}

describe("useTabletopCardHold", () => {
  beforeEach(() => vi.useFakeTimers());

  afterEach(() => {
    vi.runOnlyPendingTimers();
    vi.useRealTimers();
  });

  it("uses the short card-preview hold threshold", () => {
    expect(TABLETOP_CARD_HOLD_DELAY_MS).toBe(300);
  });

  it("opens after the hold delay and consumes the follow-up click", () => {
    const onHold = vi.fn();
    const { result } = renderHook(() => useTabletopCardHold({ onHold }));

    act(() => result.current.handlers.onPointerDown(pointerEvent()));
    act(() => vi.advanceTimersByTime(TABLETOP_CARD_HOLD_DELAY_MS));

    expect(onHold).toHaveBeenCalledTimes(1);
    expect(result.current.consumeClick()).toBe(true);
    expect(result.current.consumeClick()).toBe(false);
  });

  it("accepts a touch press whose pointer fields are exposed only by nativeEvent", () => {
    const onHold = vi.fn();
    const { result } = renderHook(() => useTabletopCardHold({ onHold }));
    const event = pointerEvent({
      button: undefined,
      isPrimary: undefined,
      pointerType: undefined,
    });

    act(() => result.current.handlers.onPointerDown(event));
    act(() => vi.advanceTimersByTime(TABLETOP_CARD_HOLD_DELAY_MS));

    expect(onHold).toHaveBeenCalledTimes(1);
  });

  it("leaves a short press available to the card's normal click", () => {
    const onHold = vi.fn();
    const { result } = renderHook(() => useTabletopCardHold({ onHold }));
    const down = pointerEvent();

    act(() => result.current.handlers.onPointerDown(down));
    act(() => result.current.handlers.onPointerUp(pointerEvent()));
    act(() => vi.advanceTimersByTime(TABLETOP_CARD_HOLD_DELAY_MS));

    expect(onHold).not.toHaveBeenCalled();
    expect(result.current.consumeClick()).toBe(false);
  });

  it("keeps a completed hold consumable after pointer capture is released", () => {
    const onHold = vi.fn();
    const { result } = renderHook(() => useTabletopCardHold({ onHold }));
    const event = pointerEvent();

    act(() => result.current.handlers.onPointerDown(event));
    act(() => vi.advanceTimersByTime(TABLETOP_CARD_HOLD_DELAY_MS));
    act(() => result.current.handlers.onLostPointerCapture(event));

    expect(result.current.consumeClick()).toBe(true);
  });

  it("cancels when the pointer moves beyond the gesture threshold", () => {
    const onHold = vi.fn();
    const { result } = renderHook(() => useTabletopCardHold({ onHold }));

    act(() => result.current.handlers.onPointerDown(pointerEvent()));
    act(() =>
      result.current.handlers.onPointerMove(
        pointerEvent({ clientX: 40, clientY: 30 }),
      ),
    );
    act(() => vi.advanceTimersByTime(TABLETOP_CARD_HOLD_DELAY_MS));

    expect(onHold).not.toHaveBeenCalled();
    expect(result.current.consumeClick()).toBe(false);
  });
});
