import { describe, expect, it } from "vitest";

import {
  advanceTripleTap,
  initialTripleTapState,
  type TripleTapState,
} from "../tripleTap";

/** Feed a sequence of (touchCount, now) touchstarts; return the tap index that triggered, or -1. */
function run(events: Array<[number, number]>): number {
  let state: TripleTapState = initialTripleTapState();
  for (let i = 0; i < events.length; i++) {
    const [count, now] = events[i];
    const result = advanceTripleTap(state, count, now);
    state = result.state;
    if (result.triggered) return i;
  }
  return -1;
}

describe("advanceTripleTap", () => {
  it("triggers only on the third 3-finger tap, not the second", () => {
    // Three 3-finger taps within the window.
    expect(run([[3, 0], [3, 100], [3, 200]])).toBe(2);
  });

  it("does not let the 1- and 2-finger touchstarts of a gesture reset progress", () => {
    // Each real 3-finger tap arrives as 1-, then 2-, then 3-finger touchstarts.
    const events: Array<[number, number]> = [
      [1, 0], [2, 5], [3, 10], // tap 1
      [1, 100], [2, 105], [3, 110], // tap 2
      [1, 200], [2, 205], [3, 210], // tap 3 -> trigger
    ];
    expect(run(events)).toBe(8);
  });

  it("does not trigger on only two 3-finger taps", () => {
    expect(run([[3, 0], [3, 100]])).toBe(-1);
  });

  it("restarts the sequence when a tap falls outside the window", () => {
    // 3rd tap is >500ms after the 2nd, so it counts as a fresh first tap.
    expect(run([[3, 0], [3, 100], [3, 700]])).toBe(-1);
  });

  it("resets after a successful trigger so the next gesture starts fresh", () => {
    let state = initialTripleTapState();
    for (const now of [0, 100, 200]) {
      state = advanceTripleTap(state, 3, now).state;
    }
    expect(state.count).toBe(0);
  });
});
