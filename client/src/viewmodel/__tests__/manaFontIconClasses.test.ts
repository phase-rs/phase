import { readFileSync } from "node:fs";
import { createRequire } from "node:module";

import { describe, expect, it } from "vitest";

import { COUNTER_ICON_CLASS } from "../cardProps.ts";
import { loyaltyIconClasses, loyaltyStartIconClasses } from "../costLabel.ts";
import { KEYWORD_ICON_CLASS } from "../keywordProps.ts";
import { CARD_TYPE_ICON_CLASS } from "../typeIcons.ts";

// Guardrail: every mana-font class emitted by our mapping tables MUST exist as
// a selector in the shipped mana.css. This fails CI on an upstream rename or a
// typo in a table, catching silently-broken icons before they reach the board.
const require = createRequire(import.meta.url);
const MANA_CSS = readFileSync(require.resolve("mana-font/css/mana.css"), "utf-8");

/** True if `.<cls>` appears as a selector in mana.css (class-name boundary). */
function hasSelector(cls: string): boolean {
  // Escape regex metacharacters, then require a non-[word/hyphen] char after so
  // "ms-loyalty-2" doesn't match inside "ms-loyalty-20".
  const escaped = cls.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
  return new RegExp(`\\.${escaped}(?![\\w-])`).test(MANA_CSS);
}

/** All space-separated class tokens across a set of icon-class strings. */
function tokensOf(classStrings: Iterable<string>): Set<string> {
  const tokens = new Set<string>();
  for (const cs of classStrings) {
    for (const token of cs.split(/\s+/).filter(Boolean)) tokens.add(token);
  }
  return tokens;
}

// mana-font ships loyalty numerals for these magnitudes only.
const LOYALTY_MAGNITUDES = [...Array.from({ length: 21 }, (_, i) => i), 25];

describe("mana-font icon class tables", () => {
  it("sanity-checks the CSS fixture loaded", () => {
    expect(hasSelector("ms")).toBe(true);
    expect(hasSelector("ms-ability-flying")).toBe(true);
    expect(hasSelector("ms-nonexistent-glyph-xyz")).toBe(false);
  });

  it("keyword icon classes all exist in mana.css", () => {
    for (const cls of tokensOf(Object.values(KEYWORD_ICON_CLASS))) {
      expect(hasSelector(cls), cls).toBe(true);
    }
  });

  it("card-type icon classes all exist in mana.css", () => {
    for (const cls of tokensOf(Object.values(CARD_TYPE_ICON_CLASS))) {
      expect(hasSelector(cls), cls).toBe(true);
    }
  });

  it("counter icon classes all exist in mana.css", () => {
    for (const cls of tokensOf(Object.values(COUNTER_ICON_CLASS))) {
      expect(hasSelector(cls), cls).toBe(true);
    }
  });

  it("loyalty cost icon classes exist across the full valid range", () => {
    const emitted = new Set<string>();
    // Zero.
    emitted.add(loyaltyIconClasses(0)!);
    // Positive and negative magnitudes with a shipped numeral.
    for (const n of LOYALTY_MAGNITUDES) {
      if (n === 0) continue;
      emitted.add(loyaltyIconClasses(n)!);
      emitted.add(loyaltyIconClasses(-n)!);
    }
    for (const cls of emitted) expect(cls, "expected non-null loyalty classes").not.toBeNull();
    for (const cls of tokensOf(emitted)) {
      expect(hasSelector(cls), cls).toBe(true);
    }
    // Magnitudes with no glyph return null (caller falls back to text).
    expect(loyaltyIconClasses(21)).toBeNull();
    expect(loyaltyIconClasses(-24)).toBeNull();
  });

  it("loyalty shield (start) icon classes exist across the full valid range", () => {
    const emitted = new Set<string>();
    for (const n of LOYALTY_MAGNITUDES) emitted.add(loyaltyStartIconClasses(n)!);
    for (const cls of emitted) expect(cls, "expected non-null shield classes").not.toBeNull();
    for (const cls of tokensOf(emitted)) {
      expect(hasSelector(cls), cls).toBe(true);
    }
    expect(loyaltyStartIconClasses(21)).toBeNull();
  });
});
