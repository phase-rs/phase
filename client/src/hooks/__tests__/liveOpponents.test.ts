import { describe, expect, it } from "vitest";

import { countLiveOpponents } from "../liveOpponents";

describe("countLiveOpponents", () => {
  it("counts the live seats other than the local human", () => {
    expect(
      countLiveOpponents([
        { id: 0, is_eliminated: false },
        { id: 1, is_eliminated: false },
        { id: 2, is_eliminated: false },
      ]),
    ).toBe(2);
  });

  it("does not subtract the local seat when seat 0 is eliminated", () => {
    // Regression: the old `Math.max(0, liveCount - 1)` undercounted this to 1.
    expect(
      countLiveOpponents([
        { id: 0, is_eliminated: true },
        { id: 1, is_eliminated: false },
        { id: 2, is_eliminated: false },
      ]),
    ).toBe(2);
  });

  it("excludes eliminated opponents", () => {
    expect(
      countLiveOpponents([
        { id: 0, is_eliminated: false },
        { id: 1, is_eliminated: true },
        { id: 2, is_eliminated: false },
      ]),
    ).toBe(1);
  });

  it("is zero when only the local human remains", () => {
    expect(countLiveOpponents([{ id: 0, is_eliminated: false }])).toBe(0);
  });

  it("is zero when everyone is eliminated", () => {
    expect(
      countLiveOpponents([
        { id: 0, is_eliminated: true },
        { id: 1, is_eliminated: true },
      ]),
    ).toBe(0);
  });
});
