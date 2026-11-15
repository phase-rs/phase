import { describe, expect, it } from "vitest";

import de from "./locales/de/draft.json";
import en from "./locales/en/draft.json";
import es from "./locales/es/draft.json";
import fr from "./locales/fr/draft.json";
import itCatalog from "./locales/it/draft.json";
import ja from "./locales/ja/draft.json";
import pl from "./locales/pl/draft.json";
import pt from "./locales/pt/draft.json";
import zh from "./locales/zh/draft.json";
import { SUPPORTED_LNGS } from "./resources";

const REQUIRED_LOCALES = ["de", "en", "es", "fr", "it", "ja", "pl", "pt", "zh"] as const;
type RequiredLocale = (typeof REQUIRED_LOCALES)[number];

// This is deliberately a fixed contract, not a catalog intersection: deleting
// one key from every catalog must still make this test fail.
const REQUIRED_LEAF_PATHS = [
  "pack.waitingNext",
  "podError.incompatible",
  "podError.retry",
  "hostControls.pauseDraft",
  "hostControls.resumeDraft",
] as const;

const CATALOGS: Record<RequiredLocale, Record<string, unknown>> = {
  de,
  en,
  es,
  fr,
  it: itCatalog,
  ja,
  pl,
  pt,
  zh,
};

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function readLeaf(catalog: Record<string, unknown>, path: string): unknown {
  let value: unknown = catalog;
  for (const segment of path.split(".")) {
    if (!isRecord(value)) return undefined;
    value = value[segment];
  }
  return value;
}

function placeholders(value: string): string[] {
  return [...value.matchAll(/\{\{\s*([A-Za-z_][A-Za-z0-9_]*)\s*(?:,[^}]*)?\}\}/gu)]
    .map((match) => match[1]!)
    .sort();
}

describe("draft compatibility vocabulary", () => {
  it("ships exactly the explicitly supported draft catalogs", () => {
    expect(Object.keys(CATALOGS).sort()).toEqual([...REQUIRED_LOCALES]);
    expect([...SUPPORTED_LNGS].sort()).toEqual([...REQUIRED_LOCALES]);
  });

  it("keeps every fixed compatibility leaf nonblank in every catalog", () => {
    for (const locale of REQUIRED_LOCALES) {
      for (const path of REQUIRED_LEAF_PATHS) {
        const value = readLeaf(CATALOGS[locale], path);
        expect(typeof value, `${locale}.${path}`).toBe("string");
        expect((value as string).trim(), `${locale}.${path}`).not.toBe("");
      }
    }
  });

  it("keeps interpolation placeholders aligned with the English oracle", () => {
    for (const path of REQUIRED_LEAF_PATHS) {
      const english = readLeaf(CATALOGS.en, path);
      expect(typeof english, `en.${path}`).toBe("string");
      const expected = placeholders(english as string);
      for (const locale of REQUIRED_LOCALES) {
        const value = readLeaf(CATALOGS[locale], path);
        expect(typeof value, `${locale}.${path}`).toBe("string");
        expect(placeholders(value as string), `${locale}.${path}`).toEqual(expected);
      }
    }
  });
});
