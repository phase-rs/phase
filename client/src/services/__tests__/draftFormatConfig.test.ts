import { describe, expect, it } from "vitest";

import { FORMAT_DEFAULTS } from "../../stores/multiplayerStore";
import {
  limitedFormatConfigForSession,
  minimumDeckSizeOf,
  splitDraftSessionAuthority,
} from "../draftFormatConfig";

// CR 100.2b + CR 702.139b: a draft match runs under the session's own minimum
// deck size, so the engine's Yorion condition reads it rather than 40.
describe("limitedFormatConfigForSession", () => {
  const base = FORMAT_DEFAULTS.Limited;

  it("carries a Cube's configured minimum beside, never inside, the format payload", () => {
    const config = limitedFormatConfigForSession(base, 50);
    expect(minimumDeckSizeOf(config)).toBe(50);
    expect(config.draft_session_min_deck_size).toBe(50);
    // The payload keeps the registry rule and claims no provenance.
    expect(config.deck_size).toEqual(base.deck_size);
    expect(config.deck_size_origin).toBeUndefined();
  });

  it("supports a minimum below 40", () => {
    expect(minimumDeckSizeOf(limitedFormatConfigForSession(base, 20))).toBe(20);
  });

  it("lifts the carrier into the draft-match initializer parameter at the engine boundary", () => {
    const { formatConfig, draftSessionMinDeckSize } = splitDraftSessionAuthority(
      limitedFormatConfigForSession(base, 20),
    );
    expect(draftSessionMinDeckSize).toBe(20);
    expect(formatConfig).toEqual(base);
    expect(formatConfig).not.toHaveProperty("draft_session_min_deck_size");
    expect(splitDraftSessionAuthority(base)).toEqual({
      formatConfig: base,
      draftSessionMinDeckSize: undefined,
    });
    expect(splitDraftSessionAuthority(null)).toEqual({
      formatConfig: null,
      draftSessionMinDeckSize: undefined,
    });
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
