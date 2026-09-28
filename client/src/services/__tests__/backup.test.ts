import { beforeEach, describe, expect, it } from "vitest";
import { testSavedDeckTxn } from "../../test/helpers/webLocks";

import {
  applyBackup,
  buildBackup,
  buildCloudBackup,
  importBackupFromFile,
  mergeDeckCollections,
  projectCloudBackup,
  type PhaseBackupV1,
} from "../backup";
import {
  ACTIVE_DECK_KEY,
  DECK_METADATA_KEY,
  DECK_FOLDERS_KEY,
  DRAFT_WORKSPACE_PREFERENCES_KEY,
  FEED_DECK_ORIGINS_KEY,
  FEED_SUBSCRIPTIONS_KEY,
  getDeckMeta,
  STORAGE_KEY_PREFIX,
} from "../../constants/storage";

beforeEach(() => {
  localStorage.clear();
});

describe("backup — cloud projection", () => {
  it("keeps subscription identity without device-local cache state or feed decks", () => {
    localStorage.setItem(STORAGE_KEY_PREFIX + "Personal", "personal");
    localStorage.setItem(STORAGE_KEY_PREFIX + "Generated", "generated");
    localStorage.setItem(FEED_DECK_ORIGINS_KEY, JSON.stringify({ Generated: "daily-feed" }));
    localStorage.setItem(DECK_METADATA_KEY, JSON.stringify({
      Personal: { addedAt: 1 },
      Generated: { addedAt: 2, starred: true },
    }));
    localStorage.setItem(ACTIVE_DECK_KEY, "Generated");
    localStorage.setItem(FEED_SUBSCRIPTIONS_KEY, JSON.stringify([{
      sourceId: "daily-feed",
      url: "/feeds/daily.json",
      type: "bundled",
      subscribedAt: 10,
      lastRefreshedAt: 20,
      lastVersion: 7,
      error: "device-only failure",
    }]));

    const cloud = buildCloudBackup();

    expect(cloud.decks).toEqual({ Personal: "personal" });
    expect(cloud.feedDeckOrigins).toBeNull();
    expect(cloud.activeDeck).toBeNull();
    expect(JSON.parse(cloud.deckMetadata ?? "{}")).toEqual({
      Personal: { addedAt: 1 },
    });
    expect(JSON.parse(cloud.feedSubscriptions ?? "[]")).toEqual([{
      sourceId: "daily-feed",
      url: "/feeds/daily.json",
      type: "bundled",
      subscribedAt: 0,
      lastRefreshedAt: 0,
      lastVersion: 0,
    }]);
  });

  it("is idempotent after legacy feed material has been stripped", () => {
    const projected = projectCloudBackup({
      ...backupWithoutWorkspacePreferences(),
      decks: { Generated: "generated" },
      deckMetadata: JSON.stringify({ Generated: { addedAt: 2, starred: true } }),
      feedSubscriptions: JSON.stringify([{
        sourceId: "daily-feed",
        url: "/feeds/daily.json",
        type: "bundled",
        subscribedAt: 10,
        lastRefreshedAt: 20,
        lastVersion: 1,
      }]),
      feedDeckOrigins: JSON.stringify({ Generated: "daily-feed" }),
    });

    expect(projectCloudBackup(projected)).toEqual(projected);
  });
});

const FOLDERS_JSON = JSON.stringify([{ id: "f1", name: "Control", order: 0 }]);

const backupWithoutWorkspacePreferences = (): PhaseBackupV1 => ({
  version: 1,
  exportedAt: new Date(0).toISOString(),
  preferences: null,
  decks: {},
  deckMetadata: null,
  activeDeck: null,
  feedSubscriptions: null,
  feedDeckOrigins: null,
});

describe("backup — draft workspace preferences", () => {
  it("round-trips valid noncanonical JSON bytes through V1 build and apply", () => {
    const raw = `{ "schemaVersion" : 1, "explicitView" : null }\n`;
    localStorage.setItem(DRAFT_WORKSPACE_PREFERENCES_KEY, raw);

    const backup = buildBackup();
    expect(backup.version).toBe(1);
    expect(backup.draftWorkspacePreferences).toBe(raw);

    localStorage.clear();
    applyBackup(testSavedDeckTxn, backup, "overwrite");
    expect(localStorage.getItem(DRAFT_WORKSPACE_PREFERENCES_KEY)).toBe(raw);
  });

  it("preserves merge state and clears overwrite state when an old V1 omits the field", () => {
    const raw = JSON.stringify({ schemaVersion: 1 });
    const oldBackup = backupWithoutWorkspacePreferences();
    localStorage.setItem(DRAFT_WORKSPACE_PREFERENCES_KEY, raw);

    applyBackup(testSavedDeckTxn, oldBackup, "merge");
    expect(localStorage.getItem(DRAFT_WORKSPACE_PREFERENCES_KEY)).toBe(raw);

    applyBackup(testSavedDeckTxn, oldBackup, "overwrite");
    expect(localStorage.getItem(DRAFT_WORKSPACE_PREFERENCES_KEY)).toBeNull();
  });

  it("keeps the local profile value when cloud merge input omits the field", () => {
    const local = backupWithoutWorkspacePreferences();
    local.draftWorkspacePreferences = JSON.stringify({ schemaVersion: 1, explicitView: "board" });
    const merged = mergeDeckCollections(local, backupWithoutWorkspacePreferences());

    expect(merged.draftWorkspacePreferences).toBe(local.draftWorkspacePreferences);
  });

  it.each(["merge", "overwrite"] as const)(
    "reports malformed preferences without writing them in %s mode",
    (mode) => {
      const prior = JSON.stringify({ schemaVersion: 1 });
      const backup = {
        ...backupWithoutWorkspacePreferences(),
        draftWorkspacePreferences: "{ malformed",
      };
      localStorage.setItem(DRAFT_WORKSPACE_PREFERENCES_KEY, prior);

      const result = applyBackup(testSavedDeckTxn, backup, mode);

      expect(result.malformedKeys).toContain(DRAFT_WORKSPACE_PREFERENCES_KEY);
      expect(localStorage.getItem(DRAFT_WORKSPACE_PREFERENCES_KEY))
        .toBe(mode === "merge" ? prior : null);
    },
  );

  it("validates an old V1 file that omits the optional field", async () => {
    const file = new File(
      [JSON.stringify(backupWithoutWorkspacePreferences())],
      "phase-backup.json",
      { type: "application/json" },
    );

    const result = await importBackupFromFile(file, "merge");
    expect(result.malformedKeys).toEqual([]);
  });
});

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
    applyBackup(testSavedDeckTxn, backup, "overwrite");
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

    applyBackup(testSavedDeckTxn, oldBackup, "overwrite");

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

  it("does not merge malformed cloud folder or deck metadata entries", () => {
    const local = backup({ Local: "local" });
    local.deckMetadata = JSON.stringify({ Local: { addedAt: 1 } });
    local.deckFolders = JSON.stringify([{ id: "local-folder", name: "Local", order: 0 }]);
    const cloud = backup({ Remote: "remote" });
    cloud.deckMetadata = JSON.stringify({ Remote: null });
    cloud.deckFolders = JSON.stringify([{ id: 42, name: "Invalid", order: 0 }]);

    const merged = mergeDeckCollections(local, cloud);

    expect(merged.deckMetadata).toBe(local.deckMetadata);
    expect(merged.deckFolders).toBe(local.deckFolders);
  });

  it("never lets a cloud entry confer autosave ownership onto a deck the local profile already held", () => {
    const local = backup({ X: "same-bytes" });
    local.deckMetadata = null;
    const cloud = backup({ X: "same-bytes" });
    cloud.deckMetadata = JSON.stringify({ X: { addedAt: 2, autosaveSlot: "Sealed" } });

    const merged = mergeDeckCollections(local, cloud);

    expect(JSON.parse(merged.deckMetadata ?? "{}").X.autosaveSlot).toBeUndefined();
  });

  it("keeps a cloud-only deck's autosave marker", () => {
    const local = backup({});
    local.deckMetadata = null;
    const cloud = backup({ Y: "cloud-only" });
    cloud.deckMetadata = JSON.stringify({ Y: { addedAt: 2, autosaveSlot: "Sealed" } });

    const merged = mergeDeckCollections(local, cloud);

    expect(JSON.parse(merged.deckMetadata ?? "{}").Y.autosaveSlot).toBe("Sealed");
  });

  it("rejects an unknown autosave slot in cloud metadata like any other malformed field", () => {
    const local = backup({ Local: "local" });
    local.deckMetadata = JSON.stringify({ Local: { addedAt: 1 } });
    const cloud = backup({ Remote: "remote" });
    cloud.deckMetadata = JSON.stringify({ Remote: { addedAt: 2, autosaveSlot: "Bogus" } });

    const merged = mergeDeckCollections(local, cloud);

    expect(merged.deckMetadata).toBe(local.deckMetadata);
  });
});

describe("applyBackup — draft autosave ownership (merge mode)", () => {
  it("strips ownership only from decks the import loop skipped as already held locally", () => {
    localStorage.setItem(STORAGE_KEY_PREFIX + "X", "local-bytes");
    const backup: PhaseBackupV1 = {
      version: 1,
      exportedAt: new Date(0).toISOString(),
      preferences: null,
      decks: { X: JSON.stringify({ main: [], sideboard: [] }), Y: JSON.stringify({ main: [], sideboard: [] }) },
      deckMetadata: JSON.stringify({
        X: { addedAt: 1, autosaveSlot: "Sealed" },
        Y: { addedAt: 1, autosaveSlot: "Quick" },
      }),
      activeDeck: null,
      feedSubscriptions: null,
      feedDeckOrigins: null,
    };

    const result = applyBackup(testSavedDeckTxn, backup, "merge");

    expect(result.decksImported).toBe(1);
    expect(localStorage.getItem(STORAGE_KEY_PREFIX + "X")).toBe("local-bytes");
    expect(getDeckMeta("X")?.autosaveSlot).toBeUndefined();
    expect(getDeckMeta("Y")?.autosaveSlot).toBe("Quick");
  });

  it("strips ownership from a local deck whose backup metadata names it even when the backup's decks lack it", () => {
    // Orphaned/foreign metadata: the backup's metadata mentions a name the
    // backup's own `decks` does not carry.
    localStorage.setItem(STORAGE_KEY_PREFIX + "[Autosave] Sealed", JSON.stringify({ main: [{ name: "Mine", count: 1 }], sideboard: [] }));
    const backup: PhaseBackupV1 = {
      version: 1,
      exportedAt: new Date(0).toISOString(),
      preferences: null,
      decks: { Other: "other-bytes" },
      deckMetadata: JSON.stringify({
        "[Autosave] Sealed": { addedAt: 1, autosaveSlot: "Sealed" },
        Other: { addedAt: 2 },
      }),
      activeDeck: null,
      feedSubscriptions: null,
      feedDeckOrigins: null,
    };

    applyBackup(testSavedDeckTxn, backup, "merge");

    expect(getDeckMeta("[Autosave] Sealed")?.autosaveSlot).toBeUndefined();
    // The backup's other metadata entry still wrote through: this proves the
    // stripping ran on the imported metadata, not that nothing was written.
    expect(getDeckMeta("Other")).not.toBeNull();
  });

  it("strips ownership from every locally-held name regardless of other entries' validity", () => {
    localStorage.setItem(STORAGE_KEY_PREFIX + "X", "local-bytes");
    localStorage.setItem(STORAGE_KEY_PREFIX + "W", "local-bytes-2");
    const backup: PhaseBackupV1 = {
      version: 1,
      exportedAt: new Date(0).toISOString(),
      preferences: null,
      decks: {},
      deckMetadata: JSON.stringify({
        X: { addedAt: 1, autosaveSlot: "Sealed" },
        W: { addedAt: 1, autosaveSlot: "Bogus" },
        Z: { addedAt: "not a number" },
      }),
      activeDeck: null,
      feedSubscriptions: null,
      feedDeckOrigins: null,
    };

    applyBackup(testSavedDeckTxn, backup, "merge");

    const raw = JSON.parse(localStorage.getItem(DECK_METADATA_KEY) ?? "{}");
    expect("autosaveSlot" in raw.X).toBe(false);
    expect("autosaveSlot" in raw.W).toBe(false);
    // The invalid sibling entry still made it through unmodified: proves the
    // backup's metadata was actually written, not silently rejected.
    expect(raw.Z).toEqual({ addedAt: "not a number" });
  });

  it("keeps the backup's marker on an import with nothing held locally", () => {
    const backup: PhaseBackupV1 = {
      version: 1,
      exportedAt: new Date(0).toISOString(),
      preferences: null,
      decks: { Y: JSON.stringify({ main: [], sideboard: [] }) },
      deckMetadata: JSON.stringify({ Y: { addedAt: 1, autosaveSlot: "Quick" } }),
      activeDeck: null,
      feedSubscriptions: null,
      feedDeckOrigins: null,
    };

    applyBackup(testSavedDeckTxn, backup, "merge");

    expect(getDeckMeta("Y")?.autosaveSlot).toBe("Quick");
  });
});
