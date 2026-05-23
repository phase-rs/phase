import { describe, expect, it } from "vitest";

import {
  anyAiOpponentIsCedh,
  applyCedhCascade,
  CEDH_BRACKET,
  isDeckCedhLegal,
} from "../cedhLock";
import type { AiSeatPref } from "../../stores/preferencesStore";
import type { CommanderBracket } from "../../types/bracket";

function seat(difficulty: string): AiSeatPref {
  return { difficulty: difficulty as AiSeatPref["difficulty"], deckId: "Random" };
}

describe("cedhLock", () => {
  describe("anyAiOpponentIsCedh", () => {
    it("returns false when no AI seat is cEDH", () => {
      expect(anyAiOpponentIsCedh([seat("Easy"), seat("Medium"), seat("Hard")])).toBe(false);
    });

    it("returns true when any AI seat is cEDH", () => {
      expect(anyAiOpponentIsCedh([seat("Easy"), seat("CEDH"), seat("Hard")])).toBe(true);
    });

    it("returns false for an empty seat list", () => {
      expect(anyAiOpponentIsCedh([])).toBe(false);
    });
  });

  describe("applyCedhCascade", () => {
    it("upgrades all AI seats to CEDH when one seat is CEDH", () => {
      const before = [seat("Easy"), seat("CEDH"), seat("Hard")];
      const after = applyCedhCascade(before);
      expect(after.every((s) => s.difficulty === "CEDH")).toBe(true);
    });

    it("is a no-op (returns original reference) when no AI seat is cEDH", () => {
      const before = [seat("Easy"), seat("Medium")];
      const after = applyCedhCascade(before);
      expect(after).toBe(before);
      expect(after.map((s) => s.difficulty)).toEqual(["Easy", "Medium"]);
    });

    it("never mutates the input array", () => {
      const before = [seat("Easy"), seat("CEDH")];
      const origDiff = before[0].difficulty;
      applyCedhCascade(before);
      expect(before[0].difficulty).toBe(origDiff);
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
