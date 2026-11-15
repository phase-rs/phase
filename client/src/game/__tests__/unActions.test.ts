import { afterEach, beforeEach, describe, expect, it } from "vitest";

import type { GameEvent } from "../../adapter/types";
import { useUnActionFxStore } from "../../stores/unActionFxStore";
import { flashUnActionEvents } from "../unActions";

beforeEach(() => useUnActionFxStore.getState().clear());
afterEach(() => useUnActionFxStore.getState().clear());

const kinds = () => useUnActionFxStore.getState().effects.map((fx) => fx.kind);

describe("flashUnActionEvents", () => {
  it.each([
    ["TouchFace", "touchFace"],
    ["TouchTable", "touchTable"],
    ["Laugh", "laugh"],
    ["FlickCards", "flickCards"],
  ] as const)("plays the %s gag for the performing seat", (tell, kind) => {
    flashUnActionEvents([
      { type: "UnTellPerformed", data: { player: 1, tell: { type: "Physical", data: tell } } },
    ]);
    expect(useUnActionFxStore.getState().effects).toMatchObject([{ kind, playerId: 1 }]);
  });

  it("shows spoken words and phrases in a speech bubble", () => {
    flashUnActionEvents([
      { type: "UnTellPerformed", data: { player: 1, tell: { type: "Said", data: { text: "seven" } } } },
      { type: "PhraseSaid", data: { player: 0, phrase: "Thank you, sir. May I have another?" } },
    ]);
    expect(useUnActionFxStore.getState().effects).toMatchObject([
      { kind: "saysWord", text: "seven", playerId: 1 },
      { kind: "phrase", text: "Thank you, sir. May I have another?", playerId: 0 },
    ]);
  });

  it("fires the Gotcha gag and ignores unrelated events", () => {
    const events: GameEvent[] = [
      { type: "GameStarted" },
      {
        type: "GotchaSaid",
        data: {
          player: 0,
          source: {
            reference: { object_id: 4, incarnation: 0 },
            expected_zone: "Graveyard",
          },
          returns: {
            reference: { object_id: 4, incarnation: 0 },
            expected_zone: "Graveyard",
          },
        },
      },
    ];
    flashUnActionEvents(events);
    expect(kinds()).toEqual(["gotcha"]);
  });
});
