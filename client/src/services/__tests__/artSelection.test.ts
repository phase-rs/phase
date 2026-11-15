import { describe, expect, it } from "vitest";

import { applyChain, selectedPrinting } from "../artSelection.ts";
import type { PrintingEntry } from "../scryfall.ts";
import type { ArtChainEntry, CardArtOverride } from "../../stores/preferencesStore.ts";

function printing(fields: Partial<PrintingEntry> & { id: string; set: string }): PrintingEntry {
  return {
    set_name: fields.set.toUpperCase(),
    collector_number: "1",
    released_at: "2020-01-01",
    border_color: "black",
    frame_effects: [],
    full_art: false,
    faces: [{ normal: `https://img.example/${fields.id}.jpg`, art_crop: `https://img.example/${fields.id}-art.jpg` }],
    ...fields,
  };
}

// Deliberately not sorted by release date: `newest` is defined as the catalog's
// own first entry, while `oldest` re-sorts by `released_at`, so a fixture where
// those disagree is the only one that can tell them apart.
const NEWEST = printing({ id: "dmu", set: "dmu", collector_number: "137", released_at: "2022-09-09" });
const BORDERLESS = printing({
  id: "sld",
  set: "sld",
  border_color: "borderless",
  released_at: "2021-06-01",
});
const EXTENDED = printing({
  id: "znr",
  set: "znr",
  collector_number: "42",
  frame_effects: ["extendedart"],
  released_at: "2020-09-25",
});
const PRINTINGS = [NEWEST, BORDERLESS, EXTENDED];

const NO_OVERRIDES: Record<string, CardArtOverride> = {};

describe("applyChain", () => {
  it("returns null when the card has no printings at all", () => {
    expect(applyChain([{ type: "newest" }], [])).toBeNull();
  });

  it("resolves a set entry to that set's printing", () => {
    expect(applyChain([{ type: "set", setCode: "znr", label: "Zendikar Rising" }], PRINTINGS))
      .toBe(EXTENDED);
  });

  it("resolves a newest entry to the catalog's first printing", () => {
    expect(applyChain([{ type: "newest" }], PRINTINGS)).toBe(NEWEST);
  });

  it("resolves an oldest entry by release date, not catalog order", () => {
    expect(applyChain([{ type: "oldest" }], PRINTINGS)).toBe(EXTENDED);
  });

  it("resolves a prefer_borderless entry to the borderless printing", () => {
    expect(applyChain([{ type: "prefer_borderless" }], PRINTINGS)).toBe(BORDERLESS);
  });

  it("resolves a prefer_extended entry to the extended-art printing", () => {
    expect(applyChain([{ type: "prefer_extended" }], PRINTINGS)).toBe(EXTENDED);
  });

  it("resolves a source_printing entry to the deck's set and collector number", () => {
    expect(applyChain([{ type: "source_printing" }], PRINTINGS, { setCode: "DMU", collectorNumber: "137" }))
      .toBe(NEWEST);
  });

  it("matches a source_printing entry on the collector number, not the set alone", () => {
    expect(applyChain([{ type: "source_printing" }], PRINTINGS, { setCode: "DMU", collectorNumber: "999" }))
      .toBeNull();
  });

  it("skips a source_printing entry when the caller supplied no source", () => {
    expect(applyChain([{ type: "source_printing" }, { type: "newest" }], PRINTINGS)).toBe(NEWEST);
  });

  it("walks past entries that match nothing and applies the first that does", () => {
    const chain: ArtChainEntry[] = [
      { type: "set", setCode: "zzz", label: "Nonexistent Set" },
      { type: "prefer_borderless" },
      { type: "newest" },
    ];
    expect(applyChain(chain, PRINTINGS)).toBe(BORDERLESS);
  });

  it("returns null when no entry in the chain matches", () => {
    const chain: ArtChainEntry[] = [
      { type: "set", setCode: "zzz", label: "Nonexistent Set" },
      { type: "prefer_extended" },
    ];
    expect(applyChain(chain, [NEWEST, BORDERLESS])).toBeNull();
  });
});

describe("selectedPrinting", () => {
  it("prefers a per-card art override over the chain", () => {
    const overrides = { bolt: { scryfallId: "znr", setCode: "znr", collectorNumber: "42" } };
    expect(selectedPrinting("bolt", PRINTINGS, [{ type: "newest" }], overrides)).toBe(EXTENDED);
  });

  // A pinned printing becomes unresolvable the moment the printings data is
  // regenerated without that scryfallId — not an exotic case. The hook's
  // `else if (artOverrides[oracleId])` gate fires on key PRESENCE, so the stale
  // pin consumes the branch and never falls through to the chain.
  const STALE_OVERRIDE = {
    bolt: { scryfallId: "not-a-printing", setCode: "xxx", collectorNumber: "1" },
  };

  it("stops at an override naming a printing the card does not have", () => {
    expect(selectedPrinting("bolt", PRINTINGS, [{ type: "newest" }], STALE_OVERRIDE)).toBeNull();
  });

  // The renderer's async path is handed the source printing whenever the chain
  // is empty, regardless of whether the override branch was taken — so with a
  // stale pin and no chain it still displays the deck's printing. A planner
  // that returned null here would cache the canonical asset and the pack would
  // not contain what the app actually shows.
  it("falls back to the deck's source printing when a stale override has no chain behind it", () => {
    expect(selectedPrinting("bolt", PRINTINGS, [], STALE_OVERRIDE, { setCode: "SLD", collectorNumber: "1" }))
      .toBe(BORDERLESS);
  });

  it("ignores the source for a stale override once any chain is configured", () => {
    // With a non-empty chain the hook passes `undefined` for the source, so the
    // renderer falls through to canonical art and this must return null.
    expect(selectedPrinting("bolt", PRINTINGS, [{ type: "newest" }], STALE_OVERRIDE, { setCode: "SLD", collectorNumber: "1" }))
      .toBeNull();
  });

  it("returns null for a stale override with neither chain nor source", () => {
    expect(selectedPrinting("bolt", PRINTINGS, [], STALE_OVERRIDE)).toBeNull();
  });

  it("ignores an override belonging to a different card", () => {
    const overrides = { other: { scryfallId: "znr", setCode: "znr", collectorNumber: "42" } };
    expect(selectedPrinting("bolt", PRINTINGS, [{ type: "newest" }], overrides)).toBe(NEWEST);
  });

  it("applies the chain when no override applies", () => {
    expect(selectedPrinting("bolt", PRINTINGS, [{ type: "prefer_borderless" }], NO_OVERRIDES))
      .toBe(BORDERLESS);
  });

  it("consumes the source only through a source_printing entry", () => {
    const source = { setCode: "SLD", collectorNumber: "1" };
    const withEntry: ArtChainEntry[] = [{ type: "source_printing" }, { type: "newest" }];
    expect(selectedPrinting("bolt", PRINTINGS, withEntry, NO_OVERRIDES, source)).toBe(BORDERLESS);
    // A `source_printing` entry is the only reader of `source`, so a chain
    // without one is unaffected by it and its own first match wins — the deck's
    // printing must not leak into `newest`, `oldest` or the preference entries.
    expect(selectedPrinting("bolt", PRINTINGS, [{ type: "newest" }], NO_OVERRIDES, source)).toBe(NEWEST);
  });

  it("returns null when the chain matches nothing, falling back to canonical art", () => {
    const chain: ArtChainEntry[] = [{ type: "set", setCode: "zzz", label: "Nonexistent Set" }];
    expect(selectedPrinting("bolt", PRINTINGS, chain, NO_OVERRIDES)).toBeNull();
  });

  // Precedence branch 4 — the DEFAULT configuration. An empty chain plus a deck
  // list's `(SET) NUM` annotation still resolves to the deck's own printing, so
  // a caller modelling only the chain would disagree with the renderer for
  // every default-config user.
  it("resolves the deck's source printing when the chain is empty", () => {
    expect(selectedPrinting("bolt", PRINTINGS, [], NO_OVERRIDES, { setCode: "SLD", collectorNumber: "1" }))
      .toBe(BORDERLESS);
  });

  it("matches the source printing's set code case-insensitively", () => {
    expect(selectedPrinting("bolt", PRINTINGS, [], NO_OVERRIDES, { setCode: "ZNR", collectorNumber: "42" }))
      .toBe(EXTENDED);
  });

  it("returns null when the source printing is not among the card's printings", () => {
    expect(selectedPrinting("bolt", PRINTINGS, [], NO_OVERRIDES, { setCode: "ZZZ", collectorNumber: "1" }))
      .toBeNull();
  });

  it("returns null when no override, no chain and no source apply", () => {
    expect(selectedPrinting("bolt", PRINTINGS, [], NO_OVERRIDES)).toBeNull();
  });
});
