import { beforeEach, describe, expect, it } from "vitest";

import { applyBackup, buildBackup, importBackupFromFile, type PhaseBackupV1 } from "../backup";
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
