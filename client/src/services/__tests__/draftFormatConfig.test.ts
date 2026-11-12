import { describe, expect, it } from "vitest";

import { FORMAT_DEFAULTS } from "../../stores/multiplayerStore";
import { limitedFormatConfigForSession, minimumDeckSizeOf } from "../draftFormatConfig";

// CR 100.2b + CR 702.139b: a draft match runs under the session's own minimum
// deck size, so the engine's Yorion condition reads it rather than 40.
describe("limitedFormatConfigForSession", () => {
  const base = FORMAT_DEFAULTS.Limited;

  it("carries a Cube's configured minimum into the match's format config", () => {
    const config = limitedFormatConfigForSession(base, 50);
    expect(config.deck_size).toEqual({ type: "Minimum", data: 50 });
    expect(minimumDeckSizeOf(config)).toBe(50);
    // Everything else about the format is untouched.
    expect({ ...config, deck_size: base.deck_size }).toEqual(base);
  });

  it("supports a minimum below 40", () => {
    expect(limitedFormatConfigForSession(base, 20).deck_size).toEqual({ type: "Minimum", data: 20 });
  });

  it("keeps the base config when the session minimum is the format's own or unknown", () => {
    expect(limitedFormatConfigForSession(base, 40)).toBe(base);
    expect(limitedFormatConfigForSession(base, undefined)).toBe(base);
    expect(limitedFormatConfigForSession(base, null)).toBe(base);
  });

  it("leaves a fixed-size rule alone", () => {
    const fixed = { ...base, deck_size: { type: "Exactly", data: 80 } } as typeof base;
    expect(limitedFormatConfigForSession(fixed, 50)).toBe(fixed);
    expect(minimumDeckSizeOf(fixed)).toBeUndefined();
  });
});
