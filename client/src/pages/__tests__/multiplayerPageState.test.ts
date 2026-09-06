import { describe, expect, it } from "vitest";

import type { DeckCompatibilityResult } from "../../services/deckCompatibility";
import { classifyCompatResult } from "../multiplayerPageState";

function makeResult(
  overrides: Partial<DeckCompatibilityResult> = {},
): DeckCompatibilityResult {
  return {
    standard: { compatible: true, reasons: [] },
    commander: { compatible: true, reasons: [] },
    bo3_ready: true,
    unknown_cards: [],
    selected_format_reasons: [],
    color_identity: [],
    color_distribution: [],
    ...overrides,
  };
}

describe("classifyCompatResult", () => {
  it("returns legal when the engine confirms compatibility", () => {
    const out = classifyCompatResult(
      "Standard",
      makeResult({ selected_format_compatible: true }),
    );
    expect(out).toEqual({ status: "legal", format: "Standard" });
  });

  it("returns illegal with engine-provided reasons when false", () => {
    const out = classifyCompatResult(
      "Commander",
      makeResult({
        selected_format_compatible: false,
        selected_format_reasons: ["Missing commander", "Deck size below 100"],
      }),
    );
    expect(out).toEqual({
      status: "illegal",
      format: "Commander",
      reasons: ["Missing commander", "Deck size below 100"],
    });
  });

  // The key regression guard: an indeterminate engine response (null or
  // undefined) must NOT be treated as "legal". A false-positive green chip
  // would mislead the user into thinking their deck was validated when the
  // engine explicitly declined to make a judgment.
  it("returns idle when the engine can't determine legality (null)", () => {
    const out = classifyCompatResult(
      "Pioneer",
      makeResult({ selected_format_compatible: null }),
    );
    expect(out).toEqual({ status: "idle" });
  });

  it("returns idle when selected_format_compatible is omitted", () => {
    const out = classifyCompatResult("Pauper", makeResult());
    expect(out).toEqual({ status: "idle" });
  });
});
