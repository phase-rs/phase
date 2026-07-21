import { beforeEach, describe, expect, it, vi } from "vitest";

const { invokeMock, isBundledTauriOriginMock, isTauriMock } = vi.hoisted(() => ({
  invokeMock: vi.fn(),
  isBundledTauriOriginMock: vi.fn(),
  isTauriMock: vi.fn(),
}));

vi.mock("@tauri-apps/api/core", () => ({ invoke: invokeMock }));
vi.mock("../platform", () => ({
  isBundledTauriOrigin: isBundledTauriOriginMock,
  isTauri: isTauriMock,
}));
vi.mock("../cloudSync/sessionKey", () => ({
  getSupabaseSessionKey: () => "sb-project-auth-token",
}));

import { importLegacyStorage } from "../legacyMigration";
import { STORAGE_KEY_PREFIX } from "../../constants/storage";

const backup = {
  version: 1 as const,
  exportedAt: new Date(0).toISOString(),
  preferences: null,
  decks: {
    Existing: JSON.stringify({ source: "legacy" }),
    Migrated: JSON.stringify({ source: "legacy" }),
  },
  deckMetadata: null,
  deckFolders: null,
  activeDeck: null,
  feedSubscriptions: null,
  feedDeckOrigins: null,
};

beforeEach(() => {
  localStorage.clear();
  vi.clearAllMocks();
  isTauriMock.mockReturnValue(true);
  isBundledTauriOriginMock.mockReturnValue(false);
});

describe("importLegacyStorage", () => {
  it("merges the backup and imports an absent Supabase session before confirming", async () => {
    const localDeck = JSON.stringify({ source: "remote" });
    localStorage.setItem(STORAGE_KEY_PREFIX + "Existing", localDeck);
    invokeMock.mockImplementation((command: string) => {
      if (command === "take_legacy_storage") {
        return Promise.resolve(JSON.stringify({ backup, supabaseSession: "legacy-session" }));
      }
      return Promise.resolve(undefined);
    });

    await importLegacyStorage();

    expect(localStorage.getItem(STORAGE_KEY_PREFIX + "Existing")).toBe(localDeck);
    expect(localStorage.getItem(STORAGE_KEY_PREFIX + "Migrated")).toBe(
      JSON.stringify({ source: "legacy" }),
    );
    expect(localStorage.getItem("sb-project-auth-token")).toBe("legacy-session");
    expect(invokeMock).toHaveBeenNthCalledWith(1, "take_legacy_storage");
    expect(invokeMock).toHaveBeenNthCalledWith(2, "confirm_legacy_import");
  });

  it("preserves a session already established on the remote origin", async () => {
    localStorage.setItem("sb-project-auth-token", "remote-session");
    invokeMock.mockImplementation((command: string) => {
      if (command === "take_legacy_storage") {
        return Promise.resolve(JSON.stringify({ backup, supabaseSession: "legacy-session" }));
      }
      return Promise.resolve(undefined);
    });

    await importLegacyStorage();

    expect(localStorage.getItem("sb-project-auth-token")).toBe("remote-session");
  });

  it("silently leaves the stash alone when an older shell lacks the read command", async () => {
    invokeMock.mockRejectedValue(new Error("unknown command"));

    await expect(importLegacyStorage()).resolves.toBeUndefined();

    expect(invokeMock).toHaveBeenCalledTimes(1);
    expect(invokeMock).toHaveBeenCalledWith("take_legacy_storage");
  });
});
