import { act, cleanup, render } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { DeathShatter } from "../DeathShatter.tsx";

const POSITION = { x: 40, y: 600, width: 63, height: 88 };

/** happy-dom has no 2D context; the shatter only needs one that accepts draws. */
function stubContext() {
  const context = {
    clearRect: vi.fn(),
    save: vi.fn(),
    restore: vi.fn(),
    translate: vi.fn(),
    rotate: vi.fn(),
    drawImage: vi.fn(),
    fillRect: vi.fn(),
    globalAlpha: 1,
    globalCompositeOperation: "source-over",
    fillStyle: "",
  };
  vi.spyOn(HTMLCanvasElement.prototype, "getContext").mockReturnValue(context as never);
}

function shatter(timeScale?: number) {
  const onComplete = vi.fn();
  const image = document.createElement("img");
  render(
    <DeathShatter
      position={POSITION}
      image={image}
      timeScale={timeScale}
      onComplete={onComplete}
    />,
  );
  return onComplete;
}

beforeEach(() => {
  vi.useFakeTimers({
    toFake: ["setTimeout", "clearTimeout", "requestAnimationFrame", "cancelAnimationFrame", "performance"],
  });
  stubContext();
});

afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
  vi.useRealTimers();
});

describe("DeathShatter timeScale", () => {
  it("finishes in half the wall time at timeScale 0.5, with the same picture", () => {
    const onComplete = shatter(0.5);

    act(() => {
      vi.advanceTimersByTime(250);
    });
    expect(onComplete).not.toHaveBeenCalled();

    // At 0.5 the fragments have faded once 300 ms have elapsed; unscaled they
    // would still be flying well past 350 ms.
    act(() => {
      vi.advanceTimersByTime(100);
    });
    expect(onComplete).toHaveBeenCalledTimes(1);
  });

  it("completes at once at timeScale 0 without dividing by zero", () => {
    const onComplete = shatter(0);

    expect(onComplete).toHaveBeenCalledTimes(1);
    // No frame or safety timer is left to run later.
    act(() => {
      vi.advanceTimersByTime(5000);
    });
    expect(onComplete).toHaveBeenCalledTimes(1);
  });

  it("still completes from its scaled safety timer when frames never advance the shatter", () => {
    // A tab that never paints: only the safety timer can settle the effect.
    vi.stubGlobal("requestAnimationFrame", vi.fn(() => 0));
    const onComplete = shatter(0.25);

    // Just short of the scaled safety deadline (0.25 × 1100 ms); the unscaled
    // 1100 ms timer would not fire here at all.
    act(() => {
      vi.advanceTimersByTime(260);
    });
    expect(onComplete).not.toHaveBeenCalled();

    act(() => {
      vi.advanceTimersByTime(20);
    });
    expect(onComplete).toHaveBeenCalledTimes(1);
  });
});
