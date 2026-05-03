import { describe, it } from "vitest";

describe("multiplayerDraftStore Bo3", () => {
  describe("BO3-01: match result reporting", () => {
    it.todo("reports match result only when MatchPhase is Completed");
    it.todo("does not report match result after game 1 in Bo3");
  });

  describe("BO3-04: both-submitted gate", () => {
    it.todo("play/draw prompt fires only after both players submit sideboards");
    it.todo(
      "does not send play/draw prompt when only one player has submitted",
    );
  });

  describe("BO3-05: play/draw auto-choose", () => {
    it.todo("auto-chooses play on 10s timer expiry");
  });
});
