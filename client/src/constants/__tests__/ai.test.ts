import { describe, expect, it } from "vitest";

import { AI_DIFFICULTIES, isAiDifficulty } from "../ai";

describe("isAiDifficulty", () => {
  it("accepts every selectable difficulty and the table-wide CEDH value", () => {
    for (const { id } of AI_DIFFICULTIES) {
      expect(isAiDifficulty(id)).toBe(true);
    }
    expect(isAiDifficulty("CEDH")).toBe(true);
  });

  it("rejects strings outside the engine difficulty contract", () => {
    expect(isAiDifficulty("Impossible")).toBe(false);
    expect(isAiDifficulty("")).toBe(false);
  });
});
