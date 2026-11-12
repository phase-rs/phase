import { describe, expect, it } from "vitest";

import { FORMAT_DEFAULTS } from "../../stores/multiplayerStore";
import {
  limitedFormatConfigForSession,
  minimumDeckSizeOf,
  draftSessionAuthorityOf,
} from "../draftFormatConfig";

// CR 100.2b + CR 702.139b: a draft match runs under the session's own minimum
// deck size, so the engine's Yorion condition reads it rather than 40.
describe("limitedFormatConfigForSession", () => {
  const base = FORMAT_DEFAULTS.Limited;

  it("issues a Cube's minimum against the exact launch config, never inside the payload", () => {
    const config = limitedFormatConfigForSession(base, 50);
    expect(minimumDeckSizeOf(config)).toBe(50);
    expect(draftSessionAuthorityOf(config)).toBe(50);
    // The payload keeps the registry rule and claims no provenance.
    expect(config.deck_size).toEqual(base.deck_size);
    expect(config.deck_size_origin).toBeUndefined();
    expect(JSON.stringify(config)).toBe(JSON.stringify(base));
  });

  it("supports a minimum below 40", () => {
    expect(minimumDeckSizeOf(limitedFormatConfigForSession(base, 20))).toBe(20);
  });

  it("carries no authority through a copy or a hand-edited config", () => {
    const issued = limitedFormatConfigForSession(base, 20);
    expect(draftSessionAuthorityOf({ ...issued })).toBeUndefined();
    expect(draftSessionAuthorityOf(structuredClone(issued))).toBeUndefined();
    expect(draftSessionAuthorityOf({ ...base, draft_session_min_deck_size: 20 } as never)).toBeUndefined();
    expect(draftSessionAuthorityOf(base)).toBeUndefined();
    expect(draftSessionAuthorityOf(null)).toBeUndefined();
  });

  it("keeps the base config when the session minimum is the format's own or unknown", () => {
    expect(limitedFormatConfigForSession(base, 40)).toBe(base);
    expect(base.deck_size_origin).toBeUndefined();
    expect(limitedFormatConfigForSession(base, undefined)).toBe(base);
    expect(limitedFormatConfigForSession(base, null)).toBe(base);
  });

  it("leaves a fixed-size rule alone", () => {
    const fixed = { ...base, deck_size: { type: "Exactly", data: 80 } } as typeof base;
    expect(limitedFormatConfigForSession(fixed, 50)).toBe(fixed);
    expect(minimumDeckSizeOf(fixed)).toBeUndefined();
  });
});
