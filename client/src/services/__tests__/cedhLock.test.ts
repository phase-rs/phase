import { describe, expect, it } from "vitest";

import {
  CEDH_BRACKET,
  CEDH_DIFFICULTY,
  effectiveAiDifficulty,
  isDeckCedhLegal,
} from "../cedhLock";
import type { CommanderBracket } from "../../types/bracket";

describe("cedhLock", () => {
  describe("effectiveAiDifficulty", () => {
    it("returns the seat's own difficulty when cEDH mode is off", () => {
      expect(effectiveAiDifficulty("Easy", false)).toBe("Easy");
      expect(effectiveAiDifficulty("VeryHard", false)).toBe("VeryHard");
    });

    it("overrides every seat to CEDH when cEDH mode is on", () => {
      expect(effectiveAiDifficulty("Easy", true)).toBe(CEDH_DIFFICULTY);
      expect(effectiveAiDifficulty("VeryHard", true)).toBe(CEDH_DIFFICULTY);
    });
  });

  describe("isDeckCedhLegal", () => {
    it("returns true only for bracket 5 (cEDH)", () => {
      expect(isDeckCedhLegal(CEDH_BRACKET)).toBe(true);
    });

    it("returns false for null (no tag)", () => {
      expect(isDeckCedhLegal(null)).toBe(false);
    });

    it("returns false for each non-cEDH bracket", () => {
      const nonCedh: CommanderBracket[] = [1, 2, 3, 4];
      for (const b of nonCedh) {
        expect(isDeckCedhLegal(b)).toBe(false);
      }
    });
  });
});
