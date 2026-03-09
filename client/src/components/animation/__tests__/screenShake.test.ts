import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { applyScreenShake, getShakeConfig } from "../ScreenShake";

describe("getShakeConfig", () => {
  it("returns correct config for light intensity", () => {
    expect(getShakeConfig("light")).toEqual({
      amplitude: 2,
      duration: 150,
      oscillations: 4,
    });
  });

  it("returns correct config for medium intensity", () => {
    expect(getShakeConfig("medium")).toEqual({
      amplitude: 4,
      duration: 250,
      oscillations: 5,
    });
  });

  it("returns correct config for heavy intensity", () => {
    expect(getShakeConfig("heavy")).toEqual({
      amplitude: 8,
      duration: 350,
      oscillations: 6,
    });
  });
});

describe("applyScreenShake", () => {
  let element: HTMLDivElement;
  let rafCallbacks: Array<(time: number) => void>;
  let startTime: number;

  beforeEach(() => {
    element = document.createElement("div");
    rafCallbacks = [];
    startTime = 1000;

    vi.spyOn(window, "requestAnimationFrame").mockImplementation((cb) => {
      rafCallbacks.push(cb);
      return rafCallbacks.length;
    });

    vi.spyOn(performance, "now").mockReturnValue(startTime);
  });

  afterEach(() => {
    vi.restoreAllMocks();
  });

  it("calls requestAnimationFrame", () => {
    applyScreenShake(element, "light", 1.0);
    expect(window.requestAnimationFrame).toHaveBeenCalled();
  });

  it("applies speedMultiplier to duration", () => {
    applyScreenShake(element, "medium", 0.5);

    // First rAF call sets up the animation — advance partway through
    // medium duration = 250ms, with 0.5 multiplier = 125ms
    // At 100ms into 125ms animation, transform should be non-empty
    expect(rafCallbacks.length).toBe(1);
    rafCallbacks[0](startTime + 100);
    expect(element.style.transform).not.toBe("");

    // At 130ms (past 125ms), animation should complete and reset
    rafCallbacks[1](startTime + 130);
    expect(element.style.transform).toBe("");
  });

  it("resets element.style.transform to empty string after completion", () => {
    applyScreenShake(element, "light", 1.0);

    // light duration = 150ms, advance past it
    rafCallbacks[0](startTime + 160);
    expect(element.style.transform).toBe("");
  });
});
