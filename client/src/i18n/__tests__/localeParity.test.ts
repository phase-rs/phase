import { readdirSync, readFileSync } from "node:fs";
import { join } from "node:path";

import { describe, expect, it } from "vitest";

/**
 * Every locale must carry the same keys as the English source, with the same
 * interpolation placeholders.
 *
 * The test suite renders in English only (`test-setup.ts` loads `en`), so a key
 * added to `en` and forgotten elsewhere, or a translation whose `{{placeholder}}`
 * was dropped or renamed, produces no failing test — it produces a raw key or a
 * missing value in front of a player who does not read English, which nobody
 * running the suite will see. This closes that gap.
 *
 * The placeholder half is the one that catches real damage: a translation that
 * drops `{{min}}` still renders as fluent prose, so it reads as correct while
 * silently omitting the value the sentence exists to communicate.
 */

const LOCALES_DIR = join(__dirname, "..", "locales");
const SOURCE = "en";

/**
 * Known pre-existing divergences, each with the reason it is tolerated.
 *
 * This is a list of DEFECTS, not of exemptions: an entry here means the string
 * is wrong and has not been fixed yet, so keep it short and remove entries as
 * they are fixed rather than adding to it.
 */
const KNOWN_PLACEHOLDER_GAPS: ReadonlyArray<{
  ns: string;
  key: string;
  why: string;
}> = [
  {
    ns: "draft.json",
    key: "intro.quick.step1",
    why:
      "All six translations hard-code the default '3 packs of 14 cards' instead " +
      "of interpolating {{packCount}}/{{cardsPerPack}}, so a non-default draft " +
      "shows wrong numbers. Pre-existing; tracked separately.",
  },
];

type Flat = Record<string, unknown>;

function flatten(value: unknown, prefix = "", out: Flat = {}): Flat {
  if (value && typeof value === "object" && !Array.isArray(value)) {
    for (const [k, v] of Object.entries(value as Record<string, unknown>)) {
      flatten(v, prefix ? `${prefix}.${k}` : k, out);
    }
  } else {
    out[prefix] = value;
  }
  return out;
}

function load(locale: string, ns: string): Flat {
  return flatten(JSON.parse(readFileSync(join(LOCALES_DIR, locale, ns), "utf8")));
}

/** The `{{name}}` placeholders a string interpolates, sorted for comparison. */
function placeholders(value: unknown): string[] {
  if (typeof value !== "string") return [];
  return [...value.matchAll(/\{\{\s*([\w.]+)/g)].map((m) => m[1]).sort();
}

const namespaces = readdirSync(join(LOCALES_DIR, SOURCE)).filter((f) =>
  f.endsWith(".json"),
);
const locales = readdirSync(LOCALES_DIR).filter((d) => d !== SOURCE);

const isKnownGap = (ns: string, key: string) =>
  KNOWN_PLACEHOLDER_GAPS.some((g) => g.ns === ns && g.key === key);

describe("locale parity", () => {
  // Guards the guard: if the layout changes and these come back empty, every
  // assertion below passes over nothing.
  it("discovers the locales and namespaces it is meant to check", () => {
    expect(namespaces.length).toBeGreaterThan(0);
    expect(locales.length).toBeGreaterThan(0);
    expect(locales).toContain("de");
  });

  describe.each(locales)("%s", (locale) => {
    it.each(namespaces)("%s has exactly the English key set", (ns) => {
      const source = load(SOURCE, ns);
      const target = load(locale, ns);

      expect(Object.keys(source).filter((k) => !(k in target))).toEqual([]);
      // Extra keys are dead weight: nothing reads them, and they hide the fact
      // that the English source dropped a string.
      expect(Object.keys(target).filter((k) => !(k in source))).toEqual([]);
    });

    it.each(namespaces)("%s interpolates the same placeholders", (ns) => {
      const source = load(SOURCE, ns);
      const target = load(locale, ns);

      const diverged = Object.keys(source)
        .filter((k) => k in target && !isKnownGap(ns, k))
        .filter(
          (k) =>
            placeholders(source[k]).join() !== placeholders(target[k]).join(),
        )
        .map(
          (k) =>
            `${k}: en=[${placeholders(source[k])}] ${locale}=[${placeholders(target[k])}]`,
        );

      expect(diverged).toEqual([]);
    });
  });

  // Without this, a fixed defect could sit in the list forever, quietly
  // exempting a key that no longer needs it.
  it("has no stale entries in the known-gap list", () => {
    const stale = KNOWN_PLACEHOLDER_GAPS.filter(({ ns, key }) => {
      const source = load(SOURCE, ns);
      return locales.every(
        (locale) =>
          placeholders(source[key]).join() ===
          placeholders(load(locale, ns)[key]).join(),
      );
    }).map(({ ns, key }) => `${ns}:${key}`);

    expect(stale).toEqual([]);
  });
});
