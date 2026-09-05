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

import { importLegacyStorage, markRemoteLoadOk } from "../legacyMigration";
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

describe("markRemoteLoadOk", () => {
  it("waits for remote IPC and does not reuse a sequential success", async () => {
    let resolveFirst!: () => void;
    invokeMock
      .mockImplementationOnce(() => new Promise<void>((resolve) => { resolveFirst = resolve; }))
      .mockRejectedValueOnce(new Error("write failed"));

    const first = markRemoteLoadOk();
    expect(invokeMock).toHaveBeenCalledWith("mark_remote_load_ok");
    resolveFirst();
    await expect(first).resolves.toBe(true);
    await expect(markRemoteLoadOk()).resolves.toBe(false);
    expect(invokeMock).toHaveBeenCalledTimes(2);
  });

  it("coalesces concurrent writes, resolves synchronous throws, and retries failures", async () => {
    let resolve!: () => void;
    invokeMock.mockImplementationOnce(() => new Promise<void>((done) => { resolve = done; }));

    const first = markRemoteLoadOk();
    const concurrent = markRemoteLoadOk();
    expect(invokeMock).toHaveBeenCalledTimes(1);
    resolve();
    await expect(Promise.all([first, concurrent])).resolves.toEqual([true, true]);

    invokeMock.mockImplementationOnce(() => { throw new Error("sync failure"); });
    await expect(markRemoteLoadOk()).resolves.toBe(false);
    invokeMock.mockResolvedValueOnce(undefined);
    await expect(markRemoteLoadOk()).resolves.toBe(true);
  });

  it("does not invoke IPC outside a remote Tauri shell", async () => {
    isTauriMock.mockReturnValue(false);
    await expect(markRemoteLoadOk()).resolves.toBe(true);

    isTauriMock.mockReturnValue(true);
    isBundledTauriOriginMock.mockReturnValue(true);
    await expect(markRemoteLoadOk()).resolves.toBe(true);
    expect(invokeMock).not.toHaveBeenCalled();
  });
});
