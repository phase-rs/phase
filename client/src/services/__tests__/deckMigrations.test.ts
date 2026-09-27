// @vitest-environment jsdom
// The storage-watcher tests below patch `Storage.prototype`, which the
// default happy-dom environment does not honor for `localStorage.setItem`
// (see `cloudSync/__tests__/storageWatcher.test.ts`'s own directive).
import { beforeEach, describe, expect, it, vi } from "vitest";

const mocks = vi.hoisted(() => ({
  canonicalCardNames: vi.fn(),
}));

vi.mock("../../adapter/wasm-adapter", () => ({
  getSharedAdapter: () => ({ canonicalCardNames: mocks.canonicalCardNames }),
}));

import { canonicalizeSavedDeckNames, migrateSavedDecks } from "../deckMigrations";
import {
  onSavedDeckRewritten,
  saveDeckOrigins,
  STORAGE_KEY_PREFIX,
  type SavedDeckRewrite,
} from "../../constants/storage";
import { watchUserStorage } from "../cloudSync/storageWatcher";

function renameTo(map: Record<string, string | null>) {
  return (names: string[]) => Promise.resolve(names.map((name) => map[name] ?? null));
}

beforeEach(() => {
  localStorage.clear();
  mocks.canonicalCardNames.mockReset();
});

describe("migrateSavedDecks — bare-slash names", () => {
  it("leaves a printed name containing a bare \"/\" untouched", () => {
    const raw = JSON.stringify({
      main: [{ count: 1, name: "Summon: Choco/Mog" }],
      sideboard: [],
    });
    localStorage.setItem(STORAGE_KEY_PREFIX + "Slash Deck", raw);

    migrateSavedDecks();

    expect(localStorage.getItem(STORAGE_KEY_PREFIX + "Slash Deck")).toBe(raw);
  });

  it("rewrites irregular \"//\" spacing in stored decks", () => {
    localStorage.setItem(
      STORAGE_KEY_PREFIX + "Spacing Deck",
      JSON.stringify({ main: [{ count: 1, name: "Fire// Ice" }], sideboard: [] }),
    );

    migrateSavedDecks();

    const stored = JSON.parse(localStorage.getItem(STORAGE_KEY_PREFIX + "Spacing Deck") ?? "{}");
    expect(stored.main).toEqual([{ count: 1, name: "Fire // Ice" }]);
  });
});

describe("migrateSavedDecks — malformed records", () => {
  it("skips malformed records stored before a well-formed deck that needs repair", () => {
    const malformed = [
      "{}",
      JSON.stringify(null),
      JSON.stringify({ main: "x", sideboard: [] }),
      JSON.stringify({ main: [], sideboard: [], companion: 5 }),
    ];
    malformed.forEach((raw, i) => localStorage.setItem(STORAGE_KEY_PREFIX + `Bad${i}`, raw));
    localStorage.setItem(
      STORAGE_KEY_PREFIX + "Spacing Deck",
      JSON.stringify({ main: [{ count: 1, name: "Fire// Ice" }], sideboard: [] }),
    );

    expect(() => migrateSavedDecks()).not.toThrow();

    const stored = JSON.parse(localStorage.getItem(STORAGE_KEY_PREFIX + "Spacing Deck") ?? "{}");
    expect(stored.main).toEqual([{ count: 1, name: "Fire // Ice" }]);
    malformed.forEach((raw, i) => expect(localStorage.getItem(STORAGE_KEY_PREFIX + `Bad${i}`)).toBe(raw));
  });

  it("still repairs a legacy deck whose commander and companion were stored as null", () => {
    localStorage.setItem(
      STORAGE_KEY_PREFIX + "Legacy Deck",
      JSON.stringify({ main: [{ count: 1, name: "Fire// Ice" }], sideboard: [], commander: null, companion: null }),
    );

    migrateSavedDecks();

    const stored = JSON.parse(localStorage.getItem(STORAGE_KEY_PREFIX + "Legacy Deck") ?? "{}");
    expect(stored.main).toEqual([{ count: 1, name: "Fire // Ice" }]);
  });
});

describe("canonicalizeSavedDeckNames", () => {
  it("rewrites saved decks to the engine's canonical names", async () => {
    localStorage.setItem(
      STORAGE_KEY_PREFIX + "A",
      JSON.stringify({
        main: [
          { count: 2, name: "Revival/Revenge" },
          { count: 1, name: "Summon: Choco // Mog" },
          { count: 1, name: "Revival // Revenge" },
        ],
        sideboard: [],
        format: "Modern",
      }),
    );
    mocks.canonicalCardNames.mockImplementation(
      renameTo({
        "Revival/Revenge": "Revival // Revenge",
        "Summon: Choco // Mog": "Summon: Choco/Mog",
        "Revival // Revenge": "Revival // Revenge",
      }),
    );

    await canonicalizeSavedDeckNames();

    const stored = JSON.parse(localStorage.getItem(STORAGE_KEY_PREFIX + "A") ?? "{}");
    expect(stored.main).toEqual([
      { name: "Revival // Revenge", count: 3 },
      { name: "Summon: Choco/Mog", count: 1 },
    ]);
    expect(stored.format).toBe("Modern");
  });

  it("leaves a deck with nothing to rename byte-identical", async () => {
    const rawA = JSON.stringify({
      main: [{ count: 1, name: "Revival/Revenge" }],
      sideboard: [],
    });
    const rawB = JSON.stringify({
      main: [
        { count: 1, name: "Lightning Bolt" },
        { count: 1, name: "Lightning Bolt" },
      ],
      sideboard: [],
    });
    localStorage.setItem(STORAGE_KEY_PREFIX + "A", rawA);
    localStorage.setItem(STORAGE_KEY_PREFIX + "B", rawB);
    mocks.canonicalCardNames.mockImplementation(
      renameTo({ "Revival/Revenge": "Revival // Revenge" }),
    );

    await canonicalizeSavedDeckNames();

    expect(localStorage.getItem(STORAGE_KEY_PREFIX + "A")).not.toBe(rawA);
    expect(localStorage.getItem(STORAGE_KEY_PREFIX + "B")).toBe(rawB);
  });

  it("skips feed-owned decks", async () => {
    localStorage.setItem(
      STORAGE_KEY_PREFIX + "F",
      JSON.stringify({ main: [{ count: 1, name: "Revival/Revenge" }], sideboard: [] }),
    );
    localStorage.setItem(
      STORAGE_KEY_PREFIX + "A",
      JSON.stringify({ main: [{ count: 1, name: "Revival/Revenge" }], sideboard: [] }),
    );
    saveDeckOrigins({ F: "feed-1" });
    mocks.canonicalCardNames.mockImplementation(
      renameTo({ "Revival/Revenge": "Revival // Revenge" }),
    );
    const rawF = localStorage.getItem(STORAGE_KEY_PREFIX + "F");

    await canonicalizeSavedDeckNames();

    expect(localStorage.getItem(STORAGE_KEY_PREFIX + "F")).toBe(rawF);
    const storedA = JSON.parse(localStorage.getItem(STORAGE_KEY_PREFIX + "A") ?? "{}");
    expect(storedA.main).toEqual([{ count: 1, name: "Revival // Revenge" }]);
  });

  it("does not overwrite a deck changed while the engine answered", async () => {
    localStorage.setItem(
      STORAGE_KEY_PREFIX + "A",
      JSON.stringify({ main: [{ count: 1, name: "Revival/Revenge" }], sideboard: [] }),
    );
    localStorage.setItem(
      STORAGE_KEY_PREFIX + "C",
      JSON.stringify({ main: [{ count: 1, name: "Revival/Revenge" }], sideboard: [] }),
    );
    const externalWrite = JSON.stringify({ main: [{ count: 9, name: "Sol Ring" }], sideboard: [] });
    mocks.canonicalCardNames.mockImplementation(async (names: string[]) => {
      localStorage.setItem(STORAGE_KEY_PREFIX + "A", externalWrite);
      return names.map((name) => (name === "Revival/Revenge" ? "Revival // Revenge" : null));
    });

    const rewrites: SavedDeckRewrite[] = [];
    const unsubscribe = onSavedDeckRewritten((rewrite) => rewrites.push(rewrite));
    const rawCBefore = localStorage.getItem(STORAGE_KEY_PREFIX + "C");

    await canonicalizeSavedDeckNames();
    unsubscribe();

    expect(localStorage.getItem(STORAGE_KEY_PREFIX + "A")).toBe(externalWrite);
    const storedC = JSON.parse(localStorage.getItem(STORAGE_KEY_PREFIX + "C") ?? "{}");
    expect(storedC.main).toEqual([{ count: 1, name: "Revival // Revenge" }]);
    expect(rewrites).toEqual([
      {
        name: "C",
        previousRaw: rawCBefore,
        raw: localStorage.getItem(STORAGE_KEY_PREFIX + "C"),
      },
    ]);
  });

  it("writes without notifying the storage watcher", async () => {
    localStorage.setItem(
      STORAGE_KEY_PREFIX + "A",
      JSON.stringify({ main: [{ count: 1, name: "Revival/Revenge" }], sideboard: [] }),
    );
    mocks.canonicalCardNames.mockImplementation(
      renameTo({ "Revival/Revenge": "Revival // Revenge" }),
    );
    const spy = vi.fn();
    const uninstall = watchUserStorage(spy);

    await canonicalizeSavedDeckNames();
    expect(spy).not.toHaveBeenCalled();

    localStorage.setItem(STORAGE_KEY_PREFIX + "Z", "{}");
    expect(spy).toHaveBeenCalled();

    uninstall();
  });

  it("writes nothing when the engine cannot answer", async () => {
    const raw = JSON.stringify({ main: [{ count: 1, name: "Revival/Revenge" }], sideboard: [] });
    localStorage.setItem(STORAGE_KEY_PREFIX + "A", raw);
    mocks.canonicalCardNames.mockRejectedValueOnce(new Error("worker unavailable"));

    await expect(canonicalizeSavedDeckNames()).rejects.toThrow("worker unavailable");

    expect(localStorage.getItem(STORAGE_KEY_PREFIX + "A")).toBe(raw);
  });

  it("skips malformed stored records and still repairs every well-formed deck", async () => {
    const goodRaw = JSON.stringify({
      main: [{ count: 1, name: "Summon: Choco // Mog" }],
      sideboard: [],
    });
    const missingMainRaw = JSON.stringify({ sideboard: [] });
    const mainNotArrayRaw = JSON.stringify({ main: "not-an-array", sideboard: [] });
    const nullRaw = JSON.stringify(null);
    const missingSideboardRaw = JSON.stringify({ main: [] });
    const commanderStringRaw = JSON.stringify({
      main: [{ count: 1, name: "Lightning Bolt" }],
      sideboard: [],
      commander: "Not An Array",
    });
    localStorage.setItem(STORAGE_KEY_PREFIX + "Good", goodRaw);
    localStorage.setItem(STORAGE_KEY_PREFIX + "MissingMain", missingMainRaw);
    localStorage.setItem(STORAGE_KEY_PREFIX + "MainNotArray", mainNotArrayRaw);
    localStorage.setItem(STORAGE_KEY_PREFIX + "Null", nullRaw);
    localStorage.setItem(STORAGE_KEY_PREFIX + "MissingSideboard", missingSideboardRaw);
    localStorage.setItem(STORAGE_KEY_PREFIX + "CommanderString", commanderStringRaw);
    mocks.canonicalCardNames.mockImplementation(
      renameTo({ "Summon: Choco // Mog": "Summon: Choco/Mog" }),
    );

    await canonicalizeSavedDeckNames();

    const storedGood = JSON.parse(localStorage.getItem(STORAGE_KEY_PREFIX + "Good") ?? "{}");
    expect(storedGood.main).toEqual([{ count: 1, name: "Summon: Choco/Mog" }]);
    expect(localStorage.getItem(STORAGE_KEY_PREFIX + "MissingMain")).toBe(missingMainRaw);
    expect(localStorage.getItem(STORAGE_KEY_PREFIX + "MainNotArray")).toBe(mainNotArrayRaw);
    expect(localStorage.getItem(STORAGE_KEY_PREFIX + "Null")).toBe(nullRaw);
    expect(localStorage.getItem(STORAGE_KEY_PREFIX + "MissingSideboard")).toBe(missingSideboardRaw);
    expect(localStorage.getItem(STORAGE_KEY_PREFIX + "CommanderString")).toBe(commanderStringRaw);
  });
});
