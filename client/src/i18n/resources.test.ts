import { readdirSync, readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

import { describe, expect, it } from "vitest";

import { detectInitialLanguage, resources, SUPPORTED_LNGS } from "./resources";

const LOCALES_DIR = join(dirname(fileURLToPath(import.meta.url)), "locales");

/** Every `<lng>/<ns>.json` catalog on disk, as absolute paths. Reads the dir
 *  tree directly (not the Vite glob) so encoding checks see raw bytes. */
function localeCatalogFiles(): string[] {
  return readdirSync(LOCALES_DIR, { withFileTypes: true })
    .filter((entry) => entry.isDirectory())
    .flatMap((dir) =>
      readdirSync(join(LOCALES_DIR, dir.name))
        .filter((file) => file.endsWith(".json"))
        .map((file) => join(LOCALES_DIR, dir.name, file)),
    );
}

/** Collect every leaf key path in a namespace tree, prefixed with the namespace
 *  (`game.modeChoice.confirm`). Recurses into nested objects; treats strings (and
 *  any non-object value) as leaves. */
function flattenLeafKeys(
  tree: Record<string, unknown>,
  prefix: string,
  out: Set<string>,
): void {
  for (const [key, value] of Object.entries(tree)) {
    const path = prefix ? `${prefix}.${key}` : key;
    if (value !== null && typeof value === "object" && !Array.isArray(value)) {
      flattenLeafKeys(value as Record<string, unknown>, path, out);
    } else {
      out.add(path);
    }
  }
}

/** The full namespace-prefixed leaf-key set for one locale, across every catalog
 *  the glob discovered for it. */
function localeKeySet(lng: string): Set<string> {
  const keys = new Set<string>();
  for (const [ns, tree] of Object.entries(resources[lng] ?? {})) {
    flattenLeafKeys(tree as Record<string, unknown>, ns, keys);
  }
  return keys;
}

// Gate test (plan §9 Phase 0 step 1): proves Vite's import.meta.glob runs under
// vitest's transform pipeline AND that the reshape yields { lng: { ns: {...} } }.
// Both the runtime catalogs and the "don't mock t, keep getByText" test strategy
// depend on this, so it must pass before anything else is built on the glob.
describe("i18n resources", () => {
  it("aggregates locale JSON into a { lng: { ns: {...} } } shape", () => {
    expect(resources.en).toBeDefined();
    expect(resources.en.common).toMatchObject({ actions: { cancel: "Cancel" } });
  });

  it("derives every populated locale into the resources map", () => {
    // Every glob-discovered locale directory must be a known supported language.
    for (const lng of Object.keys(resources)) {
      expect(SUPPORTED_LNGS as readonly string[]).toContain(lng);
    }
  });

  it("detects a supported language or falls back to en", () => {
    expect(SUPPORTED_LNGS as readonly string[]).toContain(detectInitialLanguage());
  });
});

// Key-parity gate: `en` is the typing oracle, so every other shipped locale must
// carry the exact same namespace-prefixed leaf keys — no missing translations and
// no orphaned keys. A namespace-prefixed set comparison catches both a single
// dropped leaf and a wholesale missing/extra catalog file in one diff. Strict
// equality includes plural suffixes (`_one`/`_other`); the catalogs mirror en's
// structure, so a new CLDR plural category surfacing here is a deliberate review
// signal, not a false failure.
describe("i18n locale key parity", () => {
  const enKeys = localeKeySet("en");

  it("en (the oracle) has a non-empty key set", () => {
    expect(enKeys.size).toBeGreaterThan(0);
  });

  for (const lng of SUPPORTED_LNGS) {
    if (lng === "en") continue;
    it(`${lng} has exactly the same keys as en`, () => {
      const localeKeys = localeKeySet(lng);
      const missing = [...enKeys].filter((k) => !localeKeys.has(k)).sort();
      const extra = [...localeKeys].filter((k) => !enKeys.has(k)).sort();
      // toEqual surfaces the offending keys directly in the failure diff.
      expect({ missing, extra }).toEqual({ missing: [], extra: [] });
    });
  }
});

// Encoding gate: catalogs use literal UTF-8 characters (not `\uXXXX` escapes) so
// translations stay human-readable and reviewable. The cost of literals is
// encoding drift — a file saved as Latin-1, or mojibake pasted in — so enforce
// that every catalog is valid, BOM-free UTF-8. This reads raw bytes; the parsed
// `resources` glob cannot see encoding because Vite already decoded it.
describe("i18n locale file encoding", () => {
  const files = localeCatalogFiles();

  it("discovers catalog files to validate", () => {
    expect(files.length).toBeGreaterThan(0);
  });

  for (const file of files) {
    const rel = file.slice(LOCALES_DIR.length + 1);
    it(`${rel} is valid, BOM-free UTF-8`, () => {
      const bytes = readFileSync(file);
      // A UTF-8 BOM (EF BB BF) is valid UTF-8 but trips some JSON tooling.
      expect([...bytes.subarray(0, 3)]).not.toEqual([0xef, 0xbb, 0xbf]);
      // fatal:true throws on any malformed UTF-8 byte sequence.
      expect(() =>
        new TextDecoder("utf-8", { fatal: true }).decode(bytes),
      ).not.toThrow();
      // A baked-in replacement char (U+FFFD) signals earlier corruption.
      const replacementChar = String.fromCharCode(0xfffd);
      expect(new TextDecoder("utf-8").decode(bytes)).not.toContain(replacementChar);
    });
  }
});

// #7692 makes the FEMININE noun reachable in it/pt for the first time: today an
// ordinary creature offer renders the masculine `nounNonlandPermanent`, and after
// the fix it correctly renders `nounCreature` (creatura / criatura), which the
// hard-coded masculine article disagrees with. The hedge copies the `un(a)` /
// `un(e)` form es and fr already ship. Neither locale gate compares VALUES, so
// without these assertions a revert of the hedge reds nothing.
describe("targeting article agreement (#7692)", () => {
  it("hedges the it/pt indefinite article so a feminine noun agrees", () => {
    expect(resources.it.game).toMatchObject({
      targeting: { one: "un(a) {{target}}", upToOne: "fino a un(a) {{target}}" },
    });
    expect(resources.pt.game).toMatchObject({
      targeting: { one: "um(a) {{target}}", upToOne: "até um(a) {{target}}" },
    });
  });

  // Both DECLINED, for different reasons, and neither decline means "correct".
  //
  // de: the it/pt bracket hedge has no analogue here — `eine` is feminine, not
  // ungendered, and three-way gender has no two-way bracket. So `one` disagrees
  // with every masculine or neuter noun: `nounPlaneswalker`, `nounPlayer`,
  // `nounSpell` and `nounTarget` all render "eine <masc/neut>". #7692 WIDENED
  // that cost rather than creating it — `nounPlaneswalker` was effectively
  // unreachable before, because the old client-side inference tested "no Land"
  // ahead of the planeswalker branch, so an ordinary planeswalker rendered
  // `nounNonlandPermanent`, which is feminine and agreed. Repairing German needs
  // case-aware phrasing (`one` and `upToOne` do not even govern the same case),
  // not a bracket. Not attempted here.
  //
  // pl: `one` carries no article and IS correct, which is the half pinned below.
  // `upToOne` is not — "do jednego" governs the genitive while every noun value
  // is nominative, so all of them are ungrammatical there. Deliberately left
  // unpinned: pinning a string that is known-wrong would record it as intended.
  //
  // Pinned so each decline is a decision in the tree, not an omission a later
  // reader repairs by guessing.
  it("leaves the de and pl articles untouched", () => {
    expect(resources.de.game).toMatchObject({ targeting: { one: "eine {{target}}" } });
    expect(resources.pl.game).toMatchObject({ targeting: { one: "{{target}}" } });
  });
});
