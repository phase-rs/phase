import { describe, expect, it } from "vitest";

import { isPodSelectionResult } from "../podSelection";

describe("isPodSelectionResult", () => {
  it("accepts an ok result and an err result", () => {
    expect(isPodSelectionResult({ ok: { seats: [], relaxations: [] } })).toBe(true);
    expect(isPodSelectionResult({ err: { too_many_seats: { seats: 5 } } })).toBe(true);
  });

  it("rejects a result with neither", () => {
    expect(isPodSelectionResult({ seats: [], relaxations: [] })).toBe(false);
  });
});
