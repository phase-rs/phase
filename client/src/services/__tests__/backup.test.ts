import { beforeEach, describe, expect, it } from "vitest";

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
    applyBackup(backup, "overwrite");
    expect(localStorage.getItem(DRAFT_WORKSPACE_PREFERENCES_KEY)).toBe(raw);
  });

  it("preserves merge state and clears overwrite state when an old V1 omits the field", () => {
    const raw = JSON.stringify({ schemaVersion: 1 });
    const oldBackup = backupWithoutWorkspacePreferences();
    localStorage.setItem(DRAFT_WORKSPACE_PREFERENCES_KEY, raw);

    applyBackup(oldBackup, "merge");
    expect(localStorage.getItem(DRAFT_WORKSPACE_PREFERENCES_KEY)).toBe(raw);

    applyBackup(oldBackup, "overwrite");
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

      const result = applyBackup(backup, mode);

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
});
