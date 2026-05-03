import { describe, it } from "vitest";

describe("P2PDraftHost Bo3", () => {
  describe("BO3-02: sideboard timer auto-submit", () => {
    it.todo(
      "auto-submits current deck when 60s sideboard timer expires in Competitive mode",
    );
  });

  describe("BO3-03: no timer in Casual", () => {
    it.todo("does not start sideboard timer when podPolicy is Casual");
    it.todo("sends timerMs: 0 in sideboard prompt for Casual mode");
  });
});
