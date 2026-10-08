import { beforeEach, describe, expect, it, vi } from "vitest";

// Mock the Supabase client so the provider's query-builder chain
// (getSupabaseClient().from(...).select(...).maybeSingle()) is fully captured
// without a live backend or the build-time __SUPABASE_*__ defines.
const {
  maybeSingleMock,
  selectMock,
  getClientMock,
  pauseSupabaseClientMock,
  resumeSupabaseClientMock,
} = vi.hoisted(() => {
  const maybeSingleMock = vi.fn();
  const selectMock = vi.fn(() => ({ maybeSingle: maybeSingleMock }));
  const fromMock = vi.fn(() => ({ select: selectMock }));
  const getSessionMock = vi.fn();
  const signOutMock = vi.fn();
  const setAuthMock = vi.fn();
  const getChannelsMock = vi.fn<() => unknown[]>().mockReturnValue([]);
  const disconnectMock = vi
    .fn<() => Promise<"ok" | "timeout">>()
    .mockResolvedValue("ok");
  const removeChannelMock = vi.fn();
  const subscribeMock = vi.fn();
  const onMock = vi.fn();
  const channel = { on: onMock, subscribe: subscribeMock };
  onMock.mockReturnValue(channel);
  subscribeMock.mockReturnValue(channel);
  const getClientMock = vi.fn(() => ({
    from: fromMock,
    auth: { getSession: getSessionMock, signOut: signOutMock },
    realtime: {
      setAuth: setAuthMock,
      getChannels: getChannelsMock,
      disconnect: disconnectMock,
    },
    channel: vi.fn(() => channel),
    removeChannel: removeChannelMock,
  }));
  const pauseSupabaseClientMock = vi.fn();
  const resumeSupabaseClientMock = vi.fn();
  return {
    maybeSingleMock,
    selectMock,
    getClientMock,
    pauseSupabaseClientMock,
    resumeSupabaseClientMock,
  };
});

vi.mock("../supabaseClient", async (importOriginal) => ({
  ...(await importOriginal<typeof import("../supabaseClient")>()),
  getSupabaseClient: getClientMock,
  isSupabaseConfigured: () => true,
  pauseSupabaseClient: pauseSupabaseClientMock,
  resumeSupabaseClient: resumeSupabaseClientMock,
}));

import { SupabaseSyncProvider } from "../supabaseProvider";

function deferred<T>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((resolvePromise) => {
    resolve = resolvePromise;
  });
  return { promise, resolve };
}

describe("SupabaseSyncProvider.pullMeta", () => {
  beforeEach(() => vi.clearAllMocks());

  it("reads only revision + updated_at, never the payload column", async () => {
    maybeSingleMock.mockResolvedValue({
      data: { revision: 6, updated_at: "t" },
      error: null,
    });

    await new SupabaseSyncProvider().pullMeta();

    // The egress win depends on the payload column being omitted from the wire.
    expect(selectMock).toHaveBeenCalledWith("revision, updated_at");
  });

  it("coerces a bigint-as-string revision to a number", async () => {
    // Postgres bigint can serialize as a JSON string; without coercion the
    // store's `meta.revision !== lastSyncedRevision` would see "6" !== 6 and
    // force an unnecessary full pull (re-introducing the egress) or a false
    // conflict.
    maybeSingleMock.mockResolvedValue({
      data: { revision: "6", updated_at: "t" },
      error: null,
    });

    const m = await new SupabaseSyncProvider().pullMeta();

    expect(m).toEqual({ revision: 6, updatedAt: "t" });
    expect(typeof m?.revision).toBe("number");
  });

  it("returns null when the account has never synced", async () => {
    maybeSingleMock.mockResolvedValue({ data: null, error: null });

    expect(await new SupabaseSyncProvider().pullMeta()).toBeNull();
  });
});

describe("SupabaseSyncProvider.pull", () => {
  beforeEach(() => vi.clearAllMocks());

  it("coerces a bigint-as-string revision to a number", async () => {
    maybeSingleMock.mockResolvedValue({
      data: { payload: { version: 1 }, revision: "6", updated_at: "t" },
      error: null,
    });

    const snap = await new SupabaseSyncProvider().pull();

    expect(snap?.meta.revision).toBe(6);
    expect(typeof snap?.meta.revision).toBe("number");
  });
});

describe("SupabaseSyncProvider lifecycle", () => {
  beforeEach(() => vi.clearAllMocks());

  it("delegates lifecycle calls to the Supabase transport authority", async () => {
    const provider = new SupabaseSyncProvider();

    await provider.resume();
    await provider.pause();

    expect(resumeSupabaseClientMock).toHaveBeenCalledTimes(1);
    expect(pauseSupabaseClientMock).toHaveBeenCalledTimes(1);
  });
});

function session() {
  return {
    access_token: "jwt",
    user: { id: "u1", email: "test@example.com", user_metadata: {} },
  };
}

describe("SupabaseSyncProvider auth settlement", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    getClientMock().realtime.setAuth.mockResolvedValue(undefined);
  });

  it("throws a returned restore error before realtime auth or identity adoption", async () => {
    const provider = new SupabaseSyncProvider();
    getClientMock().auth.getSession.mockResolvedValue({
      data: { session: session() },
      error: null,
    });
    await provider.restoreSession();
    const error = new Error("restore failed");
    getClientMock().auth.getSession.mockResolvedValue({
      data: { session: null },
      error,
    });
    getClientMock().realtime.setAuth.mockClear();

    await expect(provider.restoreSession()).rejects.toBe(error);

    expect(provider.identity()).toEqual({
      userId: "u1",
      label: "test@example.com",
      avatarUrl: undefined,
    });
    expect(getClientMock().realtime.setAuth).not.toHaveBeenCalled();
  });

  it("retains identity and skips realtime auth clearing after a returned sign-out error", async () => {
    const provider = new SupabaseSyncProvider();
    getClientMock().auth.getSession.mockResolvedValue({
      data: { session: session() },
      error: null,
    });
    await provider.restoreSession();
    const error = new Error("sign-out failed");
    getClientMock().auth.signOut.mockResolvedValue({ error });
    getClientMock().realtime.setAuth.mockClear();

    await expect(provider.signOut()).rejects.toBe(error);

    expect(provider.identity()).not.toBeNull();
    expect(getClientMock().realtime.setAuth).not.toHaveBeenCalled();
  });

  it("clears identity after confirmed sign-out even when realtime auth clearing rejects", async () => {
    const provider = new SupabaseSyncProvider();
    getClientMock().auth.getSession.mockResolvedValue({
      data: { session: session() },
      error: null,
    });
    await provider.restoreSession();
    const error = new Error("realtime auth failed");
    getClientMock().auth.signOut.mockResolvedValue({ error: null });
    getClientMock().realtime.setAuth.mockRejectedValue(error);

    await expect(provider.signOut()).rejects.toBe(error);

    expect(provider.identity()).toBeNull();
    expect(getClientMock().realtime.setAuth).toHaveBeenCalledWith();
  });
});

describe("SupabaseSyncProvider subscription disposal", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    getClientMock().realtime.setAuth.mockResolvedValue(undefined);
    getClientMock().auth.getSession.mockResolvedValue({
      data: { session: session() },
      error: null,
    });
  });

  function subscribedProvider() {
    const provider = new SupabaseSyncProvider();
    return provider.restoreSession().then(() => provider);
  }

  it("waits for a successful removal and confirms registry absence", async () => {
    const provider = await subscribedProvider();
    const removal = deferred<string>();
    getClientMock().removeChannel.mockReturnValue(removal.promise);
    getClientMock().realtime.getChannels.mockReturnValue([]);

    let settled = false;
    const dispose = provider.subscribe(() => {})();
    void dispose.then(() => {
      settled = true;
    });
    await Promise.resolve();

    expect(settled).toBe(false);
    removal.resolve("ok");
    await dispose;
    expect(getClientMock().realtime.disconnect).not.toHaveBeenCalled();
  });

  it.each([
    ["non-ok", () => getClientMock().removeChannel.mockResolvedValueOnce("error")],
    ["rejection", () => getClientMock().removeChannel.mockRejectedValueOnce(new Error("remove failed"))],
  ])("recovers a %s removal through disconnect and retry", async (_name, setup) => {
    const provider = await subscribedProvider();
    setup();
    getClientMock().removeChannel.mockResolvedValueOnce("ok");
    getClientMock().realtime.getChannels.mockReturnValue([]);

    await provider.subscribe(() => {})();

    expect(getClientMock().realtime.disconnect).toHaveBeenCalledTimes(1);
    expect(getClientMock().removeChannel).toHaveBeenCalledTimes(2);
  });

  it("recovers an ok response that leaves the channel registered", async () => {
    const provider = await subscribedProvider();
    const channel = getClientMock().channel();
    getClientMock().removeChannel.mockResolvedValue("ok");
    getClientMock().realtime.getChannels
      .mockReturnValueOnce([channel])
      .mockReturnValueOnce([]);

    await provider.subscribe(() => {})();

    expect(getClientMock().realtime.disconnect).toHaveBeenCalledTimes(1);
    expect(getClientMock().removeChannel).toHaveBeenCalledTimes(2);
  });

  it("rejects while retention persists and allows the same disposer to retry", async () => {
    const provider = await subscribedProvider();
    const channel = getClientMock().channel();
    getClientMock().removeChannel.mockResolvedValue("ok");
    getClientMock().realtime.getChannels
      .mockReturnValueOnce([channel])
      .mockReturnValueOnce([channel])
      .mockReturnValueOnce([]);
    const dispose = provider.subscribe(() => {});

    await expect(dispose()).rejects.toThrow("remain registered");
    await expect(dispose()).resolves.toBeUndefined();

    expect(getClientMock().realtime.disconnect).toHaveBeenCalledTimes(1);
  });
});

describe("pauseCloudSyncProvider", () => {
  it("does not resolve a provider before pausing and delegates after resolution", async () => {
    vi.resetModules();
    const constructed = vi.fn();
    const pause = vi.fn().mockResolvedValue(undefined);
    vi.doMock("../supabaseProvider", () => ({
      SupabaseSyncProvider: class {
        constructor() {
          constructed();
        }

        isConfigured() {
          return true;
        }

        pause() {
          return pause();
        }
      },
    }));

    const { getCloudSyncProvider, pauseCloudSyncProvider } = await import("../index");

    await pauseCloudSyncProvider();
    expect(constructed).not.toHaveBeenCalled();

    expect(getCloudSyncProvider()).not.toBeNull();
    await pauseCloudSyncProvider();
    expect(constructed).toHaveBeenCalledTimes(1);
    expect(pause).toHaveBeenCalledTimes(1);
  });
});

describe("isCloudSyncConfigured", () => {
  it("returns build configuration without constructing a provider or client", async () => {
    for (const configured of [false, true]) {
      vi.resetModules();
      const constructed = vi.fn();
      const getClient = vi.fn();
      const isConfigured = vi.fn(() => configured);
      vi.doMock("../supabaseProvider", () => ({
        SupabaseSyncProvider: class {
          constructor() {
            constructed();
          }
        },
      }));
      vi.doMock("../supabaseClient", () => ({
        getSupabaseClient: getClient,
        isSupabaseConfigured: isConfigured,
      }));

      const { isCloudSyncConfigured } = await import("../index");

      expect(isCloudSyncConfigured()).toBe(configured);
      expect(constructed).not.toHaveBeenCalled();
      expect(getClient).not.toHaveBeenCalled();
    }
  });
});
