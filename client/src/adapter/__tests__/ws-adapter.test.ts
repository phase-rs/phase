import { describe, it } from "vitest";

describe("WebSocketAdapter", () => {
  describe("Bug C: stateChanged emission", () => {
    it.todo("emits stateChanged event when StateUpdate arrives without pendingResolve");
  });

  describe("Bug D: getAiAction no-op", () => {
    it.todo("getAiAction returns null without throwing");
  });

  describe("Bug E: activePlayerId from GameStarted", () => {
    it.todo("sets activePlayerId in multiplayerStore on GameStarted");
  });
});
