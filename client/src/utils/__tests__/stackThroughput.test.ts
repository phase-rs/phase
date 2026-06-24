import { afterEach, describe, expect, it } from "vitest";

import {
  effectiveStackPressure,
  RATE_PRESSURE_ELEVATED,
  RATE_PRESSURE_RAPID,
  recordStackResolutions,
  resetStackThroughput,
  stackPressureFromRate,
  THROUGHPUT_WINDOW_MS,
} from "../stackThroughput";

// The tracker is a module-level singleton; reset between cases so windows don't
// bleed across tests. Timestamps are injected (never `performance.now()`) to
// keep the sliding-window math deterministic.
afterEach(() => resetStackThroughput());

describe("stackPressureFromRate", () => {
  it("is Normal with no recent resolutions", () => {
    expect(stackPressureFromRate(1000)).toBe("Normal");
  });

  it("escalates to Elevated then Rapid as in-window resolutions accumulate", () => {
    recordStackResolutions(RATE_PRESSURE_ELEVATED, 1000);
    expect(stackPressureFromRate(1000)).toBe("Elevated");

    recordStackResolutions(RATE_PRESSURE_RAPID - RATE_PRESSURE_ELEVATED, 1000);
    expect(stackPressureFromRate(1000)).toBe("Rapid");
  });

  it("never reports Instant from rate alone — that stays a depth-only verdict", () => {
    recordStackResolutions(RATE_PRESSURE_RAPID * 10, 1000);
    expect(stackPressureFromRate(1000)).toBe("Rapid");
  });

  it("decays back to Normal once resolutions age out of the window", () => {
    recordStackResolutions(RATE_PRESSURE_RAPID, 1000);
    expect(stackPressureFromRate(1000)).toBe("Rapid");

    // Just inside the window: still hot.
    expect(stackPressureFromRate(1000 + THROUGHPUT_WINDOW_MS - 1)).toBe("Rapid");
    // Just past the window: every timestamp has aged out.
    expect(stackPressureFromRate(1000 + THROUGHPUT_WINDOW_MS + 1)).toBe("Normal");
  });

  it("ignores non-positive counts as a no-op", () => {
    recordStackResolutions(0, 1000);
    expect(stackPressureFromRate(1000)).toBe("Normal");
  });

  it("resetStackThroughput clears accumulated churn immediately", () => {
    // Guards the new-game-boundary fix: a fast-churning prior game must not bleed
    // rate into the next game's opening pacing, even within the window.
    recordStackResolutions(RATE_PRESSURE_RAPID, 1000);
    expect(stackPressureFromRate(1000)).toBe("Rapid");
    resetStackThroughput();
    expect(stackPressureFromRate(1000)).toBe("Normal");
  });
});

describe("effectiveStackPressure", () => {
  it("takes the hotter of depth and rate", () => {
    // Depth quiet, rate hot → rate wins (the oscillating-loop case).
    recordStackResolutions(RATE_PRESSURE_RAPID, 1000);
    expect(effectiveStackPressure(1, 1000)).toBe("Rapid");
  });

  it("lets depth win when it outranks the rate axis (a true storm)", () => {
    // No churn recorded, but the stack is 100+ deep → depth says Instant, which
    // the rate axis (capped at Rapid) can never reach.
    expect(effectiveStackPressure(100, 1000)).toBe("Instant");
  });

  it("relaxes to Normal only when both axes are quiet", () => {
    expect(effectiveStackPressure(0, 1000)).toBe("Normal");
  });
});
