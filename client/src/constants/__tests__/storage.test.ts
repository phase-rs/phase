import { beforeEach, describe, expect, it } from "vitest";

import {
  createFolder,
  deleteFolder,
  getDeckMeta,
  listFolders,
  loadSavedDeck,
  loadSavedDeckBracket,
  migrateDeckMeta,
  renameFolder,
  saveSavedDeckBracket,
  setDeckFolder,
  stampDeckMeta,
  toggleDeckStar,
  touchDeckPlayed,
  STORAGE_KEY_PREFIX,
} from "../storage";
import { expandParsedDeck } from "../../services/deckParser";

beforeEach(() => {
  localStorage.clear();
});

describe("saved-deck bracket sidecar", () => {
  it("preserves sticker sheets when loading and expanding a saved deck", () => {
    localStorage.setItem(
      STORAGE_KEY_PREFIX + "Sticker Deck",
      JSON.stringify({
        main: [{ count: 1, name: "Sol Ring" }],
        sideboard: [],
        sticker_sheets: ["sheet-1", "sheet-2", "sheet-3"],
      }),
    );

    const loaded = loadSavedDeck("Sticker Deck");

    expect(loaded?.sticker_sheets).toEqual(["sheet-1", "sheet-2", "sheet-3"]);
    expect(loaded && expandParsedDeck(loaded).sticker_sheets).toEqual(["sheet-1", "sheet-2", "sheet-3"]);
  });

  it("preserves planar decks when loading and expanding a saved deck", () => {
    localStorage.setItem(
      STORAGE_KEY_PREFIX + "Planar Deck",
      JSON.stringify({
        main: [{ count: 1, name: "Sol Ring" }],
        sideboard: [],
        planar_deck: ["The Aether Flues", "Spatial Merging"],
      }),
    );

    const loaded = loadSavedDeck("Planar Deck");

    expect(loaded?.planar_deck).toEqual(["The Aether Flues", "Spatial Merging"]);
    expect(loaded && expandParsedDeck(loaded).planar_deck).toEqual(["The Aether Flues", "Spatial Merging"]);
  });

  it("returns null when the deck does not exist", () => {
    expect(loadSavedDeckBracket("Missing Deck")).toBeNull();
  });

  it("returns null when the persisted JSON has no bracket field", () => {
    localStorage.setItem(
      STORAGE_KEY_PREFIX + "Untagged",
      JSON.stringify({ main: [], sideboard: [], format: "Commander" }),
    );
    expect(loadSavedDeckBracket("Untagged")).toBeNull();
  });

  it("returns the bracket when persisted", () => {
    localStorage.setItem(
      STORAGE_KEY_PREFIX + "Tagged",
      JSON.stringify({ main: [], sideboard: [], format: "Commander", bracket: 3 }),
    );
    expect(loadSavedDeckBracket("Tagged")).toBe(3);
  });

  it("returns null when the persisted bracket is invalid (e.g. 0 or 'x')", () => {
    localStorage.setItem(
      STORAGE_KEY_PREFIX + "Bad",
      JSON.stringify({ main: [], sideboard: [], format: "Commander", bracket: 0 }),
    );
    expect(loadSavedDeckBracket("Bad")).toBeNull();
  });

  it("saveSavedDeckBracket merges the bracket into the existing persisted JSON", () => {
    localStorage.setItem(
      STORAGE_KEY_PREFIX + "Existing",
      JSON.stringify({ main: [{ count: 1, name: "Sol Ring" }], sideboard: [], format: "Commander" }),
    );
    saveSavedDeckBracket("Existing", 4);
    const raw = localStorage.getItem(STORAGE_KEY_PREFIX + "Existing")!;
    const parsed = JSON.parse(raw);
    expect(parsed.bracket).toBe(4);
    // Pre-existing fields must be preserved.
    expect(parsed.main).toEqual([{ count: 1, name: "Sol Ring" }]);
    expect(parsed.format).toBe("Commander");
  });

  it("saveSavedDeckBracket with null removes any existing bracket field", () => {
    localStorage.setItem(
      STORAGE_KEY_PREFIX + "Existing",
      JSON.stringify({ main: [], sideboard: [], format: "Commander", bracket: 4 }),
    );
    saveSavedDeckBracket("Existing", null);
    const parsed = JSON.parse(localStorage.getItem(STORAGE_KEY_PREFIX + "Existing")!);
    expect("bracket" in parsed).toBe(false);
  });

  it("saveSavedDeckBracket is a no-op when the deck does not exist", () => {
    saveSavedDeckBracket("Missing", 3);
    expect(localStorage.getItem(STORAGE_KEY_PREFIX + "Missing")).toBeNull();
  });
});

describe("folder registry", () => {
  it("createFolder appends with an incrementing order and returns the folder", () => {
    const a = createFolder("Control");
    const b = createFolder("Aggro");
    expect(a).not.toBeNull();
    expect(a?.name).toBe("Control");
    expect(a?.order).toBe(0);
    expect(b?.order).toBe(1);
    expect(a?.id).not.toBe(b?.id);
    expect(listFolders().map((f) => f.name)).toEqual(["Control", "Aggro"]);
  });

  it("createFolder trims, caps length, and rejects blank names", () => {
    expect(createFolder("   ")).toBeNull();
    const folder = createFolder(`  ${"x".repeat(60)}  `);
    expect(folder?.name).toHaveLength(40);
  });

  it("listFolders sorts by order then name", () => {
    createFolder("Zed"); // order 0
    createFolder("Alpha"); // order 1
    // Same order value sorts by name as a tiebreak.
    localStorage.setItem(
      "phase-deck-folders",
      JSON.stringify([
        { id: "1", name: "Zed", order: 5 },
        { id: "2", name: "Alpha", order: 5 },
      ]),
    );
    expect(listFolders().map((f) => f.name)).toEqual(["Alpha", "Zed"]);
  });

  it("renameFolder updates the name and ignores unknown ids / blanks", () => {
    const folder = createFolder("Old")!;
    renameFolder(folder.id, "New");
    expect(listFolders()[0].name).toBe("New");
    renameFolder(folder.id, "  ");
    expect(listFolders()[0].name).toBe("New");
    renameFolder("nonexistent", "Ghost");
    expect(listFolders()).toHaveLength(1);
  });

  it("deleteFolder removes the folder and reassigns its decks to Unfiled", () => {
    const folder = createFolder("Brews")!;
    stampDeckMeta("Deck A");
    setDeckFolder("Deck A", folder.id);
    expect(getDeckMeta("Deck A")?.folderId).toBe(folder.id);

    deleteFolder(folder.id);

    expect(listFolders()).toHaveLength(0);
    // Deck survives; only its folder membership is cleared.
    expect(getDeckMeta("Deck A")?.folderId).toBeUndefined();
  });
});

describe("deck membership + stars", () => {
  it("setDeckFolder assigns and clears membership", () => {
    const folder = createFolder("Commander")!;
    stampDeckMeta("Atraxa");
    setDeckFolder("Atraxa", folder.id);
    expect(getDeckMeta("Atraxa")?.folderId).toBe(folder.id);
    setDeckFolder("Atraxa", null);
    expect(getDeckMeta("Atraxa")?.folderId).toBeUndefined();
  });

  it("setDeckFolder seeds metadata for a deck that was never stamped", () => {
    const folder = createFolder("Imported")!;
    setDeckFolder("Fresh Import", folder.id);
    const meta = getDeckMeta("Fresh Import");
    expect(meta?.folderId).toBe(folder.id);
    expect(typeof meta?.addedAt).toBe("number");
  });

  it("toggleDeckStar flips and returns the resulting state", () => {
    stampDeckMeta("Burn");
    expect(toggleDeckStar("Burn")).toBe(true);
    expect(getDeckMeta("Burn")?.starred).toBe(true);
    expect(toggleDeckStar("Burn")).toBe(false);
    expect(getDeckMeta("Burn")?.starred).toBeUndefined();
  });
});

describe("metadata migration on rename", () => {
  it("migrateDeckMeta carries folder, star, and timestamps to the new name", () => {
    const folder = createFolder("Modern")!;
    stampDeckMeta("Old Name", 1000);
    setDeckFolder("Old Name", folder.id);
    toggleDeckStar("Old Name");
    touchDeckPlayed("Old Name");
    const before = getDeckMeta("Old Name")!;

    migrateDeckMeta("Old Name", "New Name");

    expect(getDeckMeta("Old Name")).toBeNull();
    const after = getDeckMeta("New Name")!;
    expect(after.folderId).toBe(folder.id);
    expect(after.starred).toBe(true);
    expect(after.addedAt).toBe(1000);
    expect(after.lastPlayedAt).toBe(before.lastPlayedAt);
  });

  it("migrateDeckMeta is a no-op when the source has no metadata", () => {
    migrateDeckMeta("Never Stamped", "New Name");
    expect(getDeckMeta("New Name")).toBeNull();
  });

  it("migrateDeckMeta is a no-op when source and target names match", () => {
    stampDeckMeta("Same", 500);
    migrateDeckMeta("Same", "Same");
    expect(getDeckMeta("Same")?.addedAt).toBe(500);
  });
});

describe("touchDeckPlayed preserves organization", () => {
  it("keeps folderId and starred when stamping lastPlayedAt", () => {
    const folder = createFolder("Pauper")!;
    stampDeckMeta("Affinity");
    setDeckFolder("Affinity", folder.id);
    toggleDeckStar("Affinity");

    touchDeckPlayed("Affinity");

    const meta = getDeckMeta("Affinity")!;
    expect(meta.folderId).toBe(folder.id);
    expect(meta.starred).toBe(true);
    expect(typeof meta.lastPlayedAt).toBe("number");
  });
});
