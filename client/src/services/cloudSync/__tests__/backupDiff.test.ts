import { describe, expect, it } from "vitest";

import type { PhaseBackup } from "../../backup";
import { computeBackupDigest, summarizeBackupDiff } from "../backupDiff";

function makeBackup(over: Partial<PhaseBackup> = {}): PhaseBackup {
  return {
    version: 1,
    exportedAt: "2024-01-01T00:00:00.000Z",
    preferences: null,
    decks: {},
    deckMetadata: null,
    deckFolders: null,
    activeDeck: null,
    feedSubscriptions: null,
    feedDeckOrigins: null,
    ...over,
  };
}

describe("computeBackupDigest", () => {
  it("ignores the volatile exportedAt timestamp", async () => {
    const a = makeBackup({ exportedAt: "2024-01-01T00:00:00.000Z" });
    const b = makeBackup({ exportedAt: "2025-06-06T12:00:00.000Z" });
    expect(await computeBackupDigest(a)).toBe(await computeBackupDigest(b));
  });

  it("changes when deckFolders changes", async () => {
    const a = makeBackup({ deckFolders: '["A"]' });
    const b = makeBackup({ deckFolders: '["A","B"]' });
    expect(await computeBackupDigest(a)).not.toBe(await computeBackupDigest(b));
  });

  it("treats an omitted deckFolders the same as null", async () => {
    const withNull = makeBackup({ deckFolders: null });
    const omitted = makeBackup();
    delete (omitted as { deckFolders?: string | null }).deckFolders;
    expect(await computeBackupDigest(withNull)).toBe(
      await computeBackupDigest(omitted),
    );
  });

  it("ignores regenerated feed deck bodies, origins, and added-at metadata", async () => {
    const a = makeBackup({
      decks: { Personal: "same", "Feed A": "old" },
      deckMetadata: JSON.stringify({
        Personal: { addedAt: 10 },
        "Feed A": { addedAt: 20 },
      }),
      feedDeckOrigins: JSON.stringify({ "Feed A": "bundled-a" }),
    });
    const b = makeBackup({
      decks: { Personal: "same", "Feed B": "new" },
      deckMetadata: JSON.stringify({
        Personal: { addedAt: 10 },
        "Feed B": { addedAt: 30 },
      }),
      feedDeckOrigins: JSON.stringify({ "Feed B": "bundled-b" }),
    });

    expect(await computeBackupDigest(a)).toBe(await computeBackupDigest(b));
    expect(summarizeBackupDiff(a, b)).toEqual({
      decksAdded: 0,
      decksRemoved: 0,
      decksModified: 0,
      prefsChanged: false,
      feedsChanged: false,
      otherChanged: false,
    });
  });

  it("ignores all metadata attached to regenerated feed decks", async () => {
    const a = makeBackup({
      decks: { "Feed A": "same" },
      deckMetadata: JSON.stringify({ "Feed A": { addedAt: 20, starred: true } }),
      feedDeckOrigins: JSON.stringify({ "Feed A": "bundled-a" }),
    });
    const b = makeBackup({
      decks: { "Feed A": "changed upstream" },
      deckMetadata: JSON.stringify({ "Feed A": { addedAt: 30 } }),
      feedDeckOrigins: JSON.stringify({ "Feed A": "bundled-a" }),
    });

    expect(await computeBackupDigest(a)).toBe(await computeBackupDigest(b));
    expect(summarizeBackupDiff(a, b).otherChanged).toBe(false);
  });

  it("ignores per-device subscription timestamps and cache versions", async () => {
    const subscription = {
      sourceId: "feed-a",
      url: "/feeds/a.json",
      type: "bundled",
    };
    const a = makeBackup({
      feedSubscriptions: JSON.stringify([{
        ...subscription,
        subscribedAt: 1,
        lastRefreshedAt: 2,
        lastVersion: 1,
      }]),
    });
    const b = makeBackup({
      feedSubscriptions: JSON.stringify([{
        ...subscription,
        subscribedAt: 10,
        lastRefreshedAt: 20,
        lastVersion: 99,
        error: "local failure",
      }]),
    });

    expect(await computeBackupDigest(a)).toBe(await computeBackupDigest(b));
    expect(summarizeBackupDiff(a, b).feedsChanged).toBe(false);
  });
});

describe("summarizeBackupDiff", () => {
  it("flags a deckFolders-only change via otherChanged", () => {
    const local = makeBackup({ deckFolders: '["A"]' });
    const remote = makeBackup({ deckFolders: '["A","B"]' });
    expect(summarizeBackupDiff(local, remote).otherChanged).toBe(true);
  });

  it("reports no otherChanged when folders match", () => {
    const local = makeBackup({ deckFolders: '["A"]' });
    const remote = makeBackup({ deckFolders: '["A"]' });
    expect(summarizeBackupDiff(local, remote).otherChanged).toBe(false);
  });
});
