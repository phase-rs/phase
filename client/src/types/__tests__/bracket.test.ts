import { describe, expect, it } from "vitest";

import { BRACKET_LABEL, COMMANDER_BRACKETS, type CommanderBracket } from "../bracket";

describe("CommanderBracket constants", () => {
  it("COMMANDER_BRACKETS lists 1..5 in order", () => {
    expect(COMMANDER_BRACKETS).toEqual([1, 2, 3, 4, 5]);
  });

  it("BRACKET_LABEL covers every bracket", () => {
    for (const b of COMMANDER_BRACKETS) {
      expect(BRACKET_LABEL[b]).toEqual(expect.stringMatching(/.+/));
    }
  });

  it("BRACKET_LABEL uses the WotC names", () => {
    const expected: Record<CommanderBracket, string> = {
      1: "Exhibition",
      2: "Core",
      3: "Upgraded",
      4: "Optimized",
      5: "cEDH",
    };
    expect(BRACKET_LABEL).toEqual(expected);
  });
});
