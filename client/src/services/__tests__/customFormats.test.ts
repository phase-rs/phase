import { beforeEach, describe, expect, it, vi } from "vitest";

// A self-contained `localStorage`, installed before the module under test is
// imported. The shared test environment's stub is partial (no `clear`), and
// these tests need to script storage failure as well as read it back.
const storageItems = vi.hoisted(() => {
  const items = new Map<string, string>();
  const store = {
    getItem: (key: string) => items.get(key) ?? null,
    setItem: (key: string, value: string) => {
      items.set(key, value);
    },
    removeItem: (key: string) => {
      items.delete(key);
    },
    clear: () => {
      items.clear();
    },
    key: (index: number) => [...items.keys()][index] ?? null,
    get length() {
      return items.size;
    },
  };
  Object.defineProperty(globalThis, "localStorage", {
    configurable: true,
    value: store,
  });
  return { items, store };
});

import {
  deleteSavedCustomFormat,
  findSavedCustomFormat,
  loadSavedCustomFormats,
  saveCustomFormat,
} from "../customFormats";
import type { CustomFormatDef } from "../../adapter/types";

const STORAGE_KEY = "phase-custom-formats";

function def(label = "House Rules"): CustomFormatDef {
  return {
    rules: {
      id: 0,
      structural: {
        starting_life: 20,
        min_players: 2,
        max_players: 4,
        deck_size: { type: "Minimum", data: 60 },
        singleton: false,
        command_zone_mode: "Disabled",
        range_of_influence: null,
        team_based: false,
        sideboard_policy: { type: "Limited", data: 15 },
        default_deck_copy_limit: { type: "UpTo", data: 4 },
      },
      legality: {
        legal_sets: null,
        banned: [],
        restricted: [],
        legacy: {
          mana_burn: "Modern",
          damage_timing: "Modern",
          wish_scope: "PostM10SideboardOnly",
          legend_rule_scope: "Modern",
        },
      },
    },
    label,
    short_label: "HOU",
    description: "60-card minimum, 2–4 players, 20 life",
    reprint_policy: null,
    printing_fidelity: "NotApplicable",
  };
}

describe("customFormats", () => {
  beforeEach(() => {
    storageItems.items.clear();
    vi.restoreAllMocks();
  });

  it("round-trips a saved definition under a client-generated id", () => {
    const saved = saveCustomFormat("House Rules", def());

    expect(saved.id).toBeTruthy();
    expect(findSavedCustomFormat(saved.id)).toEqual(saved);
    expect(loadSavedCustomFormats()).toEqual([saved]);
  });

  it("gives two saves distinct ids even though both carry the engine sentinel", () => {
    // The whole reason `SavedCustomFormat.id` exists: every Axis-A save carries
    // `LOBBY_SAVE_CUSTOM_FORMAT_ID` (`CustomFormatId(0)`) by design, so
    // `rules.id` is 0 for both and cannot tell them apart.
    const a = saveCustomFormat("First", def("First"));
    const b = saveCustomFormat("Second", def("Second"));

    expect(a.def.rules.id).toBe(0);
    expect(b.def.rules.id).toBe(0);
    expect(a.id).not.toBe(b.id);
    expect(findSavedCustomFormat(a.id)?.name).toBe("First");
    expect(findSavedCustomFormat(b.id)?.name).toBe("Second");
  });

  it("deletes only the named save", () => {
    const a = saveCustomFormat("First", def("First"));
    const b = saveCustomFormat("Second", def("Second"));

    deleteSavedCustomFormat(a.id);

    expect(findSavedCustomFormat(a.id)).toBeNull();
    expect(findSavedCustomFormat(b.id)).toEqual(b);
  });

  it("returns null for an absent or empty id without touching storage shape", () => {
    expect(findSavedCustomFormat(null)).toBeNull();
    expect(findSavedCustomFormat("")).toBeNull();
    expect(findSavedCustomFormat("nope")).toBeNull();
  });

  it("drops individually malformed entries but keeps the good ones", () => {
    const good = { id: "good", name: "Good", savedAt: 2, def: def() };
    localStorage.setItem(
      STORAGE_KEY,
      JSON.stringify([
        { id: "no-def", name: "Broken", savedAt: 1 },
        { id: "bad-rules", name: "Broken", savedAt: 1, def: { ...def(), rules: { id: "0" } } },
        { name: "No id", savedAt: 1, def: def() },
        "not an object",
        good,
      ]),
    );

    // One unreadable save from an older build must not erase the rest.
    expect(loadSavedCustomFormats()).toEqual([good]);
  });

  it("treats non-array and unparseable storage as empty", () => {
    localStorage.setItem(STORAGE_KEY, "{}");
    expect(loadSavedCustomFormats()).toEqual([]);

    localStorage.setItem(STORAGE_KEY, "not json");
    expect(loadSavedCustomFormats()).toEqual([]);
  });

  it("survives storage being unavailable", () => {
    // Private-mode browsers throw from `getItem` rather than returning null.
    vi.spyOn(storageItems.store, "getItem").mockImplementation(() => {
      throw new Error("disabled");
    });
    expect(loadSavedCustomFormats()).toEqual([]);
  });

  it("orders saves oldest-first regardless of stored order", () => {
    localStorage.setItem(
      STORAGE_KEY,
      JSON.stringify([
        { id: "newer", name: "Newer", savedAt: 20, def: def() },
        { id: "older", name: "Older", savedAt: 10, def: def() },
      ]),
    );
    expect(loadSavedCustomFormats().map((s) => s.id)).toEqual(["older", "newer"]);
  });
});
