import { readdirSync, readFileSync } from "node:fs";
import { join } from "node:path";

import { createInstance } from "i18next";
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
}> = [];

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

const WORKSPACE_SHELL_KEYS = [
  "workspace.shell.label",
  "workspace.shell.title",
  "workspace.shell.show",
  "workspace.shell.hide",
  "workspace.view.label",
  "workspace.view.board",
  "workspace.view.compact",
  "workspace.preview.label",
  "workspace.preview.none",
  "workspace.preview.follow",
  "workspace.preview.side",
  "workspace.preview.shift",
  "pack.scale",
  "pack.scaleDecrease",
  "pack.scaleReset",
  "pack.scaleIncrease",
  "workspace.count.deck_one",
  "workspace.count.deck_few",
  "workspace.count.deck_many",
  "workspace.count.deck_other",
  "workspace.count.sideboard_one",
  "workspace.count.sideboard_few",
  "workspace.count.sideboard_many",
  "workspace.count.sideboard_other",
  "workspace.sideboard.expand_one",
  "workspace.sideboard.expand_few",
  "workspace.sideboard.expand_many",
  "workspace.sideboard.expand_other",
  "workspace.sideboard.collapse",
  "workspace.layout.label",
  "workspace.layout.columns",
  "workspace.layout.maxPerRow",
  "workspace.layout.decreaseMaxPerRow",
  "workspace.layout.increaseMaxPerRow",
  "workspace.pool.label",
  "workspace.pool.filterLabel",
  "workspace.pool.sortLabel",
  "workspace.pool.empty",
  "workspace.pool.filter.combined_one",
  "workspace.pool.filter.combined_few",
  "workspace.pool.filter.combined_many",
  "workspace.pool.filter.combined_other",
  "workspace.pool.filter.deck_one",
  "workspace.pool.filter.deck_few",
  "workspace.pool.filter.deck_many",
  "workspace.pool.filter.deck_other",
  "workspace.pool.filter.sideboard_one",
  "workspace.pool.filter.sideboard_few",
  "workspace.pool.filter.sideboard_many",
  "workspace.pool.filter.sideboard_other",
  "workspace.compact.sideboardRegion",
  "workspace.headers.accessible_one",
  "workspace.headers.accessible_few",
  "workspace.headers.accessible_many",
  "workspace.headers.accessible_other",
  "workspace.card.moveToZone",
  "workspace.drag.dispatchError",
  "limitedDeck.spellCount_one",
  "limitedDeck.spellCount_few",
  "limitedDeck.spellCount_many",
  "limitedDeck.spellCount_other",
  "limitedDeck.landCount_one",
  "limitedDeck.landCount_few",
  "limitedDeck.landCount_many",
  "limitedDeck.landCount_other",
] as const;

/**
 * Plural families that must carry all four Polish forms, paired with the
 * namespace file each stem lives in. Polish is the locale whose grammar needs
 * `_one`/`_few`/`_many`/`_other`, but i18next resolves plurals by looking up
 * SUFFIXED keys and the key-parity case below is exact in both directions — so
 * every catalog, English included, must carry all four. The loop runs over
 * `[SOURCE, ...locales]` precisely so English is pinned too: key parity is
 * English-driven and cannot catch an English family that was never authored.
 */
const FOUR_FORM_STEMS: ReadonlyArray<{ ns: string; stem: string }> = [
  { ns: "draft.json", stem: "intro.quantity.packsOpened" },
  { ns: "draft.json", stem: "intro.quantity.cardsContained" },
  { ns: "draft.json", stem: "intro.quantity.packSizeEntry" },
  { ns: "draft.json", stem: "intro.quantity.minimumDeckCards" },
  { ns: "draft.json", stem: "intro.packPassing" },
  { ns: "draft.json", stem: "sealedOpening.subtitle" },
  { ns: "draft.json", stem: "workspace.count.deck" },
  { ns: "draft.json", stem: "workspace.count.sideboard" },
  { ns: "draft.json", stem: "workspace.sideboard.expand" },
  { ns: "draft.json", stem: "workspace.pool.filter.combined" },
  { ns: "draft.json", stem: "workspace.pool.filter.deck" },
  { ns: "draft.json", stem: "workspace.pool.filter.sideboard" },
  { ns: "draft.json", stem: "workspace.headers.accessible" },
  { ns: "draft.json", stem: "limitedDeck.spellCount" },
  { ns: "draft.json", stem: "limitedDeck.landCount" },
  { ns: "draft.json", stem: "seat.activePackCount" },
  { ns: "tournament.json", stem: "list.entrants" },
] as const;

describe("locale parity", () => {
  // Guards the guard: if the layout changes and these come back empty, every
  // assertion below passes over nothing.
  it("discovers the locales and namespaces it is meant to check", () => {
    expect(namespaces.length).toBeGreaterThan(0);
    expect(locales.length).toBeGreaterThan(0);
    expect(locales).toContain("de");
  });

  it("workspace_shell_and_compact_keys_are_nonempty_and_placeholder_exact", () => {
    const source = load(SOURCE, "draft.json");
    for (const locale of [SOURCE, ...locales]) {
      const target = load(locale, "draft.json");
      for (const key of WORKSPACE_SHELL_KEYS) {
        expect(target[key], `${locale}:${key}`).toEqual(expect.any(String));
        expect((target[key] as string).trim(), `${locale}:${key}`).not.toBe("");
        expect(placeholders(target[key]), `${locale}:${key}`).toEqual(placeholders(source[key]));
      }
    }
  });

  it("keeps_workspace_keys_in_phase_after_pin_and_actions_removal", () => {
    const obsolete = [
      "workspace.pin.pin",
      "workspace.pin.unpin",
      "workspace.resize.label",
      "workspace.resize.title",
      "workspace.resize.value",
      "workspace.resize.instructions",
      "workspace.card.actions",
      "workspace.card.actionsFor",
      "workspace.card.moveToColumn",
      "workspace.card.moveToStart",
      "workspace.card.moveToEnd",
      "workspace.card.moveBefore",
      "workspace.card.moveAfter",
    ];
    for (const locale of [SOURCE, ...locales]) {
      const target = load(locale, "draft.json");
      for (const key of obsolete) expect(target[key], `${locale}:${key}`).toBeUndefined();
    }
  });

  it("keeps_all_plural_families_complete_in_every_locale", () => {
    for (const locale of [SOURCE, ...locales]) {
      for (const { ns, stem } of FOUR_FORM_STEMS) {
        const target = load(locale, ns);
        for (const suffix of ["one", "few", "many", "other"]) {
          expect(target[`${stem}_${suffix}`], `${locale}:${ns}:${stem}_${suffix}`).toEqual(expect.any(String));
        }
      }
    }
  });

  it("resolves_polish_one_few_many_and_other_without_fallback", async () => {
    const resources = JSON.parse(readFileSync(join(LOCALES_DIR, "pl", "draft.json"), "utf8"));
    const instance = createInstance();
    await instance.init({ lng: "pl", fallbackLng: false, resources: { pl: { draft: resources } } });

    expect(instance.t("pack.cardsInPack", { ns: "draft", count: 1 })).toBe("1 karta w boosterze");
    expect(instance.t("pack.cardsInPack", { ns: "draft", count: 2 })).toBe("2 karty w boosterze");
    expect(instance.t("pack.cardsInPack", { ns: "draft", count: 5 })).toBe("5 kart w boosterze");
    expect(instance.t("pack.cardsInPack", { ns: "draft", count: 12 })).toBe("12 kart w boosterze");
    expect(instance.t("pack.cardsInPack", { ns: "draft", count: 1.5 })).toBe("1.5 karty w boosterze");

    const quantityFamilies = [
      ["intro.quantity.packsOpened", ["1 booster", "2 boostery", "5 boosterów", "12 boosterów", "1,5 boostera"]],
      ["intro.quantity.cardsContained", ["1 kartę", "2 karty", "5 kart", "12 kart", "1,5 karty"]],
      ["intro.quantity.packSizeEntry", ["1 karta", "2 karty", "5 kart", "12 kart", "1,5 karty"]],
      ["intro.quantity.minimumDeckCards", ["1 karty", "2 kart", "5 kart", "12 kart", "1,5 karty"]],
    ] as const;
    for (const [key, expected] of quantityFamilies) {
      expect([1, 2, 5, 12, 1.5].map((count) => instance.t(key, { ns: "draft", count }))).toEqual(expected);
    }

    const packs = instance.t("intro.quantity.packsOpened", { ns: "draft", count: 3 });
    const packSizes = [1, 2, 5].map((count) =>
      instance.t("intro.quantity.packSizeEntry", { ns: "draft", count }),
    );
    expect(instance.t("intro.quick.step1Mixed", { ns: "draft", packs, packSizes })).toBe(
      "Otworzysz 3 boostery o różnych rozmiarach, w tej kolejności: 1 karta, 2 karty i 5 kart",
    );
    expect(instance.t("intro.packPassing", { ns: "draft", count: 1 })).toBe(
      "Ten draft ma tylko jedną rundę boosterów, więc kierunek przekazywania się nie zmienia",
    );
    expect(instance.t("intro.packPassing", { ns: "draft", count: 3 })).toBe(
      "Kierunek przekazywania zmienia się w każdej rundzie",
    );

    const header = (count: number) => instance.t("workspace.headers.accessible", {
      ns: "draft", count, column: 3, labels: "Niebieskie",
    });
    expect(header(1)).toBe("Kolumna 3: Niebieskie, 1 karta");
    expect(header(2)).toBe("Kolumna 3: Niebieskie, 2 karty");
    expect(header(5)).toBe("Kolumna 3: Niebieskie, 5 kart");
    expect(header(12)).toBe("Kolumna 3: Niebieskie, 12 kart");
    expect(header(1.5)).toBe("Kolumna 3: Niebieskie, 1.5 karty");
    const families = [
      ["workspace.count.deck", ["Talia (1 karta)", "Talia (2 karty)", "Talia (5 kart)", "Talia (12 kart)", "Talia (1.5 karty)"]],
      ["workspace.count.sideboard", ["Sideboard (1 karta)", "Sideboard (2 karty)", "Sideboard (5 kart)", "Sideboard (12 kart)", "Sideboard (1.5 karty)"]],
      ["workspace.sideboard.expand", ["Pokaż sideboard (1 karta)", "Pokaż sideboard (2 karty)", "Pokaż sideboard (5 kart)", "Pokaż sideboard (12 kart)", "Pokaż sideboard (1.5 karty)"]],
      ["workspace.pool.filter.combined", ["Wszystkie (1)", "Wszystkie (2)", "Wszystkie (5)", "Wszystkie (12)", "Wszystkie (1.5)"]],
      ["workspace.pool.filter.deck", ["Talia (1)", "Talia (2)", "Talia (5)", "Talia (12)", "Talia (1.5)"]],
      ["workspace.pool.filter.sideboard", ["Sideboard (1)", "Sideboard (2)", "Sideboard (5)", "Sideboard (12)", "Sideboard (1.5)"]],
      ["limitedDeck.spellCount", ["1 czar", "2 czary", "5 czarów", "12 czarów", "1.5 czaru"]],
      ["limitedDeck.landCount", ["1 ziemia", "2 ziemie", "5 ziem", "12 ziem", "1.5 ziemi"]],
    ] as const;
    for (const [key, expected] of families) {
      expect([1, 2, 5, 12, 1.5].map((count) => instance.t(key, { ns: "draft", count }))).toEqual(expected);
    }
    expect([1, 2, 5, 12, 1.5].map((count) => instance.services.pluralResolver.getSuffix("pl", count))).toEqual([
      "_one", "_few", "_many", "_many", "_other",
    ]);
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
