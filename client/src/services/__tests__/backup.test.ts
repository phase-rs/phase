import { beforeEach, describe, expect, it } from "vitest";

import {
  applyBackup,
  buildBackup,
  importBackupFromFile,
  mergeDeckCollections,
  type PhaseBackupV1,
} from "../backup";
import { DECK_FOLDERS_KEY, STORAGE_KEY_PREFIX } from "../../constants/storage";

beforeEach(() => {
  localStorage.clear();
});

const FOLDERS_JSON = JSON.stringify([{ id: "f1", name: "Control", order: 0 }]);

describe("backup — deck folders", () => {
  it("round-trips the folder registry through build + apply", () => {
    localStorage.setItem(DECK_FOLDERS_KEY, FOLDERS_JSON);
    localStorage.setItem(
      STORAGE_KEY_PREFIX + "Deck A",
      JSON.stringify({ main: [], sideboard: [] }),
    );

    const backup = buildBackup();
    expect(backup.deckFolders).toBe(FOLDERS_JSON);

    localStorage.clear();
    applyBackup(backup, "overwrite");
    expect(localStorage.getItem(DECK_FOLDERS_KEY)).toBe(FOLDERS_JSON);
  });

  it("overwrite-importing a pre-folders backup clears the local folder registry", () => {
    // An old backup object predates the feature: no `deckFolders` field.
    const oldBackup: PhaseBackupV1 = {
      version: 1,
      exportedAt: new Date(0).toISOString(),
      preferences: null,
      decks: {},
      deckMetadata: null,
      activeDeck: null,
      feedSubscriptions: null,
      feedDeckOrigins: null,
    };
    localStorage.setItem(DECK_FOLDERS_KEY, JSON.stringify([{ id: "stale", name: "Stale", order: 0 }]));

    applyBackup(oldBackup, "overwrite");

    // Cleared by the overwrite sweep; the absent field writes nothing back.
    expect(localStorage.getItem(DECK_FOLDERS_KEY)).toBeNull();
  });

  it("validates a pre-folders backup file that omits deckFolders entirely", async () => {
    const json = JSON.stringify({
      version: 1,
      exportedAt: new Date(0).toISOString(),
      preferences: null,
      decks: { "Deck A": JSON.stringify({ main: [], sideboard: [] }) },
      deckMetadata: null,
      activeDeck: null,
      feedSubscriptions: null,
      feedDeckOrigins: null,
    });
    const file = new File([json], "phase-backup.json", { type: "application/json" });

    const result = await importBackupFromFile(file, "merge");
    expect(result.decksImported).toBe(1);
  });
});

describe("mergeDeckCollections", () => {
  const backup = (decks: Record<string, string>): PhaseBackupV1 => ({
    version: 1,
    exportedAt: new Date(0).toISOString(),
    preferences: "local preferences",
    decks,
    deckMetadata: "local metadata",
    deckFolders: "local folders",
    activeDeck: "Local Deck",
    feedSubscriptions: "local feeds",
    feedDeckOrigins: "local origins",
  });

  it("keeps both conflicting decks with a unique cloud name", () => {
    const merged = mergeDeckCollections(
      backup({ Shared: "local", "Shared (Cloud)": "prior cloud copy" }),
      backup({ Shared: "cloud", Remote: "remote" }),
    );

    expect(merged.decks).toEqual({
      Shared: "local",
      "Shared (Cloud)": "prior cloud copy",
      "Shared (Cloud 2)": "cloud",
      Remote: "remote",
    });
    expect(merged.preferences).toBe("local preferences");
  });

  it("deduplicates cloud decks whose contents already match", () => {
    const merged = mergeDeckCollections(
      backup({ Shared: "same" }),
      backup({ Shared: "same" }),
    );

    expect(merged.decks).toEqual({ Shared: "same" });
  });

  it("keeps metadata, origins, and folders for renamed cloud decks", () => {
    const local = backup({ Shared: "local" });
    local.deckMetadata = JSON.stringify({ Shared: { addedAt: 1, folderId: "local-folder" } });
    local.deckFolders = JSON.stringify([{ id: "local-folder", name: "Local", order: 0 }]);
    local.feedDeckOrigins = JSON.stringify({ Shared: "local-feed" });
    const cloud = backup({ Shared: "cloud" });
    cloud.deckMetadata = JSON.stringify({ Shared: { addedAt: 2, starred: true, folderId: "cloud-folder" } });
    cloud.deckFolders = JSON.stringify([{ id: "cloud-folder", name: "Cloud", order: 0 }]);
    cloud.feedDeckOrigins = JSON.stringify({ Shared: "cloud-feed" });

    const merged = mergeDeckCollections(local, cloud);

    expect(JSON.parse(merged.deckMetadata ?? "{}")).toMatchObject({
      Shared: { folderId: "local-folder" },
      "Shared (Cloud)": { folderId: "cloud-folder", starred: true },
    });
    expect(JSON.parse(merged.feedDeckOrigins ?? "{}")).toMatchObject({
      Shared: "local-feed",
      "Shared (Cloud)": "cloud-feed",
    });
    expect(JSON.parse(merged.deckFolders ?? "[]")).toEqual([
      { id: "local-folder", name: "Local", order: 0 },
      { id: "cloud-folder", name: "Cloud", order: 0 },
    ]);
  });
});
