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
