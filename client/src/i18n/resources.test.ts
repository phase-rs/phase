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

// #7692 could not repair the article, because `t("targeting.one", { target })`
// concatenates an article onto a noun at runtime and an article's form depends
// on the noun's gender, its initial sound, and the case the frame governs — none
// of which the catalog can see through a placeholder. It shipped a bracket hedge
// (`un(a)`) in it/pt and DECLINED de and pl outright.
//
// Whole-phrase keys retire the composition, and with it BOTH declines. This
// suite is REWRITTEN rather than deleted: the declines were decisions with
// reasons, and deleting them would erase the record that they were decisions
// rather than oversights. It now pins the RESOLVED state, and each retired
// decline carries the evidence that retired it.
//
// RESOLVED, de. The decline's stated reason was that repairing German needs
// case-aware phrasing because "`one` and `upToOne` do not even govern the same
// case". That parenthetical is FALSE and is withdrawn. German "bis zu" has two
// uses: as a PREPOSITION meaning "up to a limit" it takes dative, but as a
// QUANTIFIER modifying a numeral it is transparent, and MTG uses the second. A
// form attested in Wizards' German card text is impossible under a
// dative-governing "bis zu": "bis zu einen" is masculine ACCUSATIVE only, and it
// appears in 16 cards of client/public/card-data.de.json — cards whose
// case-folded `oracle_text` contains that phrase with `einen` as a complete
// word, which is also its raw-substring count, so the figure does not move with
// the method. One would be a typo; sixteen is a grammar. A corroborating count
// for "bis zu ein" used to stand here and is REMOVED rather than relabelled: it
// is method-sensitive by a factor of six, because `ein` is a prefix of `eine`,
// `einen` and `einer`, so it read as corroboration while being an artifact. The
// shipped `upToOne.creature` value is attested far more widely and needs no
// inference: "bis zu eine Kreatur" appears as a complete phrase in 228 cards
// (262 by raw substring — 34 of those are "bis zu eine Kreaturenkarte", a
// different noun sharing the prefix, which is the same raw-vs-bounded artifact
// that retired the "bis zu ein" count). Feminine dative is `einer`, so `eine` is
// accusative there too, in Wizards' own text. Both
// frames are accusative, so the real German defect was the hard-coded FEMININE
// `eine` alone — wrong for Spieler, Zauberspruch and Planeswalker (m) and for
// Ziel and Permanent (n), in both frames.
//
// RESOLVED, pl. The decline was RIGHT: "do jednego" governs the genitive while
// every noun value was nominative. Every `upToOne` value is now genitive.
//
// FLAGGED, not settled, in both de and pl. Case here is assigned by a governing
// verb and this caption HAS none, so no corpus can settle it. The caption is a
// bare noun phrase, and the corpus offers no near-miss: the verbless shape the
// counterexamples below take — a bare activation cost plus a P/T delta — occurs
// zero times among the 1205 texts carrying the QUANTIFIER use, "bis zu" before a
// numeral. That is a measured absence of ONE shape, not a proof that all 1205
// carry a governing verb; a verb-list sweep for the stronger claim was tried and
// discarded as too contaminated to cite. Two earlier drafts of this sentence
// overreached on the same axis, each stating at "every card" scope what the
// evidence supports only at "every card carrying THIS construction" scope.
// "Every printed German card is a sentence" — FALSE: 222 have single-token
// oracle text, bare keywords like "Fliegend", against 29,535 multi-word.
// "Every bis-zu text has a governing verb" — FALSE: six cards' entire text
// is the verbless "{U}: +1/+0 bis zum Ende des Zuges", where
// "bis zum" is the dative PREPOSITION, not the quantifier. Neither counterexample
// class carries the quantifier use, so the conclusion is unaffected. Resolved to
// accusative because this slot's own fallback is an imperative ("Wähle ein
// Ziel", TargetingOverlay.tsx) and the repo's choice-prompt register is
// imperative throughout. MTG has never been printed in Polish — MTGJSON's
// foreignData carries no Polish at all — so there is no authority to defer to
// and no corpus to check. Both locales ship flagged for native review.
describe("targeting phrase agreement (#7692 resolved)", () => {
  const TARGETING_FRAMES = ["one", "upToOne"] as const;

  /** Every rendered targeting phrase value for one locale, as [key, value],
   *  read from the shipped catalog rather than a list written here. */
  function targetingPhrases(lng: string): [string, string][] {
    const targeting = (resources[lng].game as { targeting: Record<string, Record<string, string>> })
      .targeting;
    return TARGETING_FRAMES.flatMap((frame) =>
      Object.entries(targeting[frame]).map(
        ([slug, value]) => [`${lng}.targeting.${frame}.${slug}`, value] as [string, string],
      ),
    );
  }

  it("carries 16 non-empty whole-phrase keys in every locale", () => {
    for (const lng of SUPPORTED_LNGS) {
      const phrases = targetingPhrases(lng);
      // toEqual over an object keeps the offending locale in the failure diff.
      expect({ lng, count: phrases.length }).toEqual({ lng, count: 16 });
      const blank = phrases.filter(([, value]) => value.trim().length === 0).map(([key]) => key);
      expect(blank).toEqual([]);
    }
  });

  // The exact logical negation of #7692's pinned `un(a)`, which is why both
  // cannot coexist: a bracketed article renders LITERALLY to the player
  // ("un(a) criatura"). It was the best available while a placeholder hid the
  // noun's gender, and it is unavailable now that each phrase is authored whole,
  // so its return would be a regression rather than a stopgap.
  //
  // Recorded for the Italian values it frees, because nothing gates it: Italian
  // requires "uno" before s+consonant, z, gn, ps, x and y, and "un'" before a
  // vowel-initial feminine. Neither bites TODAY — none of the six Italian nouns
  // qualifies (the pl- of planeswalker is muta cum liquida, so "un planeswalker"
  // is right) and there is no vowel-initial feminine. A future noun can change
  // that, and no test here would see it.
  it("ships no bracketed article hedge in any targeting phrase", () => {
    const hedged = SUPPORTED_LNGS.flatMap((lng) =>
      targetingPhrases(lng)
        .filter(([, value]) => /\(\w+\)/u.test(value))
        .map(([key]) => key),
    );
    expect(hedged).toEqual([]);
  });

  // Accusative is VISIBLE on exactly these three nouns — masculine "einen"
  // against nominative "ein". The other four German nouns are feminine or
  // neuter, where accusative and nominative are identical, so they cannot
  // evidence the case at all and pinning them would look like coverage without
  // being it.
  it("inflects the German masculine nouns to the accusative in both frames", () => {
    expect(resources.de.game).toMatchObject({
      targeting: {
        one: {
          player: "einen Spieler",
          spell: "einen Zauberspruch",
          planeswalker: "einen Planeswalker",
          orPlayer: "{{noun}} oder einen Spieler",
        },
        upToOne: {
          player: "bis zu einen Spieler",
          spell: "bis zu einen Zauberspruch",
          planeswalker: "bis zu einen Planeswalker",
          orPlayer: "{{noun}} oder einen Spieler",
        },
      },
    });
  });

  // The other four German nouns are feminine (Kreatur) or neuter (Ziel,
  // Permanent), where accusative and nominative are IDENTICAL, so no assertion
  // over them can evidence the case — that is why the test above stops at the
  // masculines. What they CAN evidence is the article, and unpinned they were
  // the gap that let this change's own argument be violated in silence:
  // "bis zu eine Kreatur" -> "bis zu einer Kreatur" is the dative reading the
  // German paragraph above withdraws, and it passed every other assertion in
  // this file. So pin the ARTICLE FORM — `eine` for the feminine and `ein` for
  // the neuters, in both frames. A gender regression (`ein Kreatur`) and a case
  // regression (`einer Kreatur`, `einem Ziel`) each red here; the case one reds
  // because the dative article is a different STRING, not because this test can
  // see case.
  it("pins the German article for the feminine and neuter nouns in both frames", () => {
    expect(resources.de.game).toMatchObject({
      targeting: {
        one: {
          creature: "eine Kreatur",
          nonlandPermanent: "ein Nichtland-Permanent",
          targetPermanent: "ein Permanent deiner Wahl",
          target: "ein Ziel",
        },
        upToOne: {
          creature: "bis zu eine Kreatur",
          nonlandPermanent: "bis zu ein Nichtland-Permanent",
          targetPermanent: "bis zu ein Permanent deiner Wahl",
          target: "bis zu ein Ziel",
        },
      },
    });
  });

  // es, fr and it were checked only for existence, count and absence of a
  // bracket hedge, so "un criatura" would have shipped green. Pin one MASCULINE
  // and one FEMININE noun per locale — the minimum that can red on a gender
  // regression in either direction — in both frames, since each frame carries
  // its own copy of the article after the "up to one" marker.
  it.each([
    ["es", "un jugador", "hasta un jugador", "una criatura", "hasta una criatura"],
    ["fr", "un joueur", "jusqu'à un joueur", "une créature", "jusqu'à une créature"],
    ["it", "un giocatore", "fino a un giocatore", "una creatura", "fino a una creatura"],
  ])("agrees the %s article with each noun's gender", (lng, mOne, mUpTo, fOne, fUpTo) => {
    expect(resources[lng].game).toMatchObject({
      targeting: {
        one: { player: mOne, creature: fOne },
        upToOne: { player: mUpTo, creature: fUpTo },
      },
    });
  });

  // Portuguese `permanente` is FEMININE, and it is the one noun whose gender
  // diverges from the other Romance locales (es/fr/it are all masculine here),
  // so the unit of verification is locale x noun, never locale. Measured over
  // Wizards' Portuguese card text in client/public/card-data.pt.json, searching
  // each card's case-folded `oracle_text`: "um permanente" appears in 0 cards
  // under BOTH a word-bounded and a raw-substring count, while "uma permanente"
  // is nonzero in the same corpus under the same count. That same-noun control
  // is what makes the zero a gender fact rather than a failed search, and it is
  // recorded as a relation rather than a magnitude on purpose — the absolute
  // counts drift with each card-data regeneration, so a number here would go
  // stale silently where the relation does not.
  //
  // `mágica` is feminine too, and replaces "feitiço", which is a
  // MISTRANSLATION with gameplay consequence rather than a register choice:
  // "Feitiço" IS the card type SORCERY, so the shipped string told a Brazilian
  // player to pick a sorcery when any spell was legal.
  it("agrees the Portuguese article with each noun's gender", () => {
    expect(resources.pt.game).toMatchObject({
      targeting: {
        one: {
          player: "um jogador",
          spell: "uma mágica",
          creature: "uma criatura",
          planeswalker: "um planeswalker",
          nonlandPermanent: "uma permanente não de terreno",
          targetPermanent: "uma permanente alvo",
          target: "um alvo",
        },
        upToOne: {
          player: "até um jogador",
          spell: "até uma mágica",
          creature: "até uma criatura",
          planeswalker: "até um planeswalker",
          nonlandPermanent: "até uma permanente não de terreno",
          targetPermanent: "até uma permanente alvo",
          target: "até um alvo",
        },
      },
    });
  });

  // Polish carries no article, so the defect was never the article — it was
  // CASE. `upToOne` is genitive throughout, governed by "do", which sits inside
  // the string and so is unaffected by the elided-verb question above. `one` is
  // accusative, visible only on the three masculine ANIMATES (gracz -> gracza,
  // stwór -> stwora with its o/ó alternation, wędrowiec -> wędrowca with
  // e-deletion); the four masculine inanimates have accusative == nominative and
  // are unchanged, which is why czar/cel/trwały are pinned in their bare form.
  //
  // TWO FRAGILE INVARIANTS, documented here because no gate can see either:
  //  1. "do jednego" agrees with the FIRST conjunct's gender. All seven Polish
  //     nouns are masculine today, so the numeral is invariant — adding one
  //     FEMININE category silently renders "do jednego <fem>" where Polish needs
  //     "do jednej".
  //  2. `orPlayer` is ONE string across BOTH frames only because `gracz` is
  //     SYNCRETIC: its genitive and its animate accusative are both `gracza`. A
  //     single string is carrying two cases by coincidence, and it breaks the
  //     moment the second conjunct is any noun but `gracz`.
  it("inflects Polish for case: accusative in one, genitive in upToOne", () => {
    expect(resources.pl.game).toMatchObject({
      targeting: {
        one: {
          player: "gracza",
          creature: "stwora",
          planeswalker: "wędrowca",
          spell: "czar",
          target: "cel",
          targetPermanent: "docelowy trwały",
          // Inanimate, so accusative == nominative: this pins the FORM, not the
          // case. Included because leaving the one unpinned slot in an otherwise
          // total block reads as an omission rather than a decision.
          nonlandPermanent: "trwały niebędący ziemią",
          orPlayer: "{{noun}} lub gracza",
        },
        upToOne: {
          player: "do jednego gracza",
          creature: "do jednego stwora",
          planeswalker: "do jednego wędrowca",
          spell: "do jednego czaru",
          target: "do jednego celu",
          targetPermanent: "do jednego docelowego trwałego",
          nonlandPermanent: "do jednego trwałego niebędącego ziemią",
          orPlayer: "{{noun}} lub gracza",
        },
      },
    });
  });
});
