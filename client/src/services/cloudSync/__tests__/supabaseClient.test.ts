import { beforeEach, describe, expect, it, vi } from "vitest";

const { createClientMock } = vi.hoisted(() => ({
  createClientMock: vi.fn(),
}));

vi.mock("@supabase/supabase-js", () => ({
  createClient: createClientMock,
}));

function deferred<T>() {
  let resolve!: (value: T) => void;
  let reject!: (reason: unknown) => void;
  const promise = new Promise<T>((resolvePromise, rejectPromise) => {
    resolve = resolvePromise;
    reject = rejectPromise;
  });
  return { promise, resolve, reject };
}

async function flushAsyncWork() {
  await Promise.resolve();
  await Promise.resolve();
  await Promise.resolve();
  await Promise.resolve();
}

function fakeClient() {
  return {
    auth: {
      startAutoRefresh: vi.fn<() => Promise<void>>().mockResolvedValue(undefined),
      stopAutoRefresh: vi.fn<() => Promise<void>>().mockResolvedValue(undefined),
    },
    removeAllChannels: vi.fn<() => Promise<unknown[]>>().mockResolvedValue([]),
    removeChannel: vi.fn<() => Promise<string>>().mockResolvedValue("ok"),
    realtime: {
      disconnect: vi.fn<() => Promise<"ok" | "timeout">>().mockResolvedValue("ok"),
      getChannels: vi.fn<() => unknown[]>().mockReturnValue([]),
    },
  };
}

describe("Supabase transport lifecycle", () => {
  let client: ReturnType<typeof fakeClient>;

  beforeEach(() => {
    vi.resetModules();
    vi.clearAllMocks();
    client = fakeClient();
    createClientMock.mockReturnValue(client);
  });

  it("constructs lazily with automatic token refresh disabled and no app auth listener", async () => {
    const supabase = await import("../supabaseClient");

    await supabase.resumeSupabaseClient();

    expect(createClientMock).toHaveBeenCalledWith("", "", {
      auth: {
        persistSession: true,
        autoRefreshToken: false,
        detectSessionInUrl: true,
      },
    });
    expect(client.auth.startAutoRefresh).toHaveBeenCalledTimes(1);
  });

  it("leaves a cold client unconstructed when paused", async () => {
    const supabase = await import("../supabaseClient");

    await supabase.pauseSupabaseClient();

    expect(createClientMock).not.toHaveBeenCalled();
  });

  it("cleans up a client constructed while paused, then keeps settled pauses inert", async () => {
    const supabase = await import("../supabaseClient");

    supabase.getSupabaseClient();
    await supabase.pauseSupabaseClient();

    expect(client.auth.startAutoRefresh).not.toHaveBeenCalled();
    expect(client.auth.stopAutoRefresh).toHaveBeenCalledTimes(1);
    expect(client.removeAllChannels).toHaveBeenCalledTimes(1);
    await supabase.pauseSupabaseClient();
    expect(client.auth.stopAutoRefresh).toHaveBeenCalledTimes(1);
    expect(client.removeAllChannels).toHaveBeenCalledTimes(1);
  });

  it("honors resume requested during an initial no-op pause", async () => {
    const supabase = await import("../supabaseClient");

    await Promise.all([
      supabase.pauseSupabaseClient(),
      supabase.resumeSupabaseClient(),
    ]);

    expect(client.auth.startAutoRefresh).toHaveBeenCalledTimes(1);
  });

  it("uses the SDK-owned refresh and channel cleanup APIs after resume", async () => {
    const supabase = await import("../supabaseClient");

    await supabase.resumeSupabaseClient();
    await supabase.pauseSupabaseClient();

    expect(client.auth.startAutoRefresh).toHaveBeenCalledTimes(1);
    expect(client.auth.stopAutoRefresh).toHaveBeenCalledTimes(1);
    expect(client.removeAllChannels).toHaveBeenCalledTimes(1);
  });

  it("recovers registered channels after bulk cleanup before pause settles", async () => {
    const supabase = await import("../supabaseClient");
    const firstChannel = {};
    const secondChannel = {};
    const retry = deferred<string>();
    const events: string[] = [];
    client.removeAllChannels.mockImplementation(async () => {
      events.push("removeAll");
      return [];
    });
    client.realtime.disconnect.mockImplementation(async () => {
      events.push("disconnect");
      return "ok";
    });
    client.removeChannel.mockImplementation(() => {
      events.push("removeChannel");
      return retry.promise;
    });
    client.realtime.getChannels
      .mockReturnValueOnce([firstChannel, secondChannel])
      .mockReturnValueOnce([]);

    await supabase.resumeSupabaseClient();
    let settled = false;
    const pause = supabase.pauseSupabaseClient().then(() => {
      settled = true;
    });
    await flushAsyncWork();

    expect(events).toEqual(["removeAll", "disconnect", "removeChannel"]);
    expect(settled).toBe(false);
    retry.resolve("ok");
    await pause;

    expect(client.realtime.getChannels).toHaveBeenCalledTimes(3);
    expect(client.removeChannel).toHaveBeenCalledWith(firstChannel);
    expect(client.removeChannel).toHaveBeenCalledWith(secondChannel);
  });

  it("settles pause after a disconnect timeout and non-ok retry when the registry is empty", async () => {
    const supabase = await import("../supabaseClient");
    const channel = {};
    client.realtime.disconnect.mockResolvedValue("timeout");
    client.removeChannel.mockResolvedValue("error");
    client.realtime.getChannels
      .mockReturnValueOnce([channel])
      .mockReturnValueOnce([])
      .mockReturnValueOnce([]);

    await supabase.resumeSupabaseClient();
    await expect(supabase.pauseSupabaseClient()).resolves.toBeUndefined();

    expect(client.realtime.disconnect).toHaveBeenCalledTimes(1);
    expect(client.removeChannel).toHaveBeenCalledWith(channel);
  });

  it("tries every captured channel before rejecting retained registry entries", async () => {
    const supabase = await import("../supabaseClient");
    const firstChannel = {};
    const secondChannel = {};
    client.realtime.getChannels
      .mockReturnValueOnce([firstChannel, secondChannel])
      .mockReturnValueOnce([secondChannel]);
    client.removeChannel
      .mockResolvedValueOnce("error")
      .mockRejectedValueOnce(new Error("remove failed"));

    await supabase.resumeSupabaseClient();
    await expect(supabase.pauseSupabaseClient()).rejects.toThrow(
      "remain registered after recovery",
    );

    expect(client.removeChannel).toHaveBeenCalledWith(firstChannel);
    expect(client.removeChannel).toHaveBeenCalledWith(secondChannel);
  });

  it("leaves pause unapplied and retryable when registered channels remain", async () => {
    const supabase = await import("../supabaseClient");
    const channel = {};
    client.realtime.getChannels.mockReturnValue([channel]);

    await supabase.resumeSupabaseClient();
    await expect(supabase.pauseSupabaseClient()).rejects.toThrow(
      "remain registered",
    );

    expect(client.realtime.disconnect).toHaveBeenCalledTimes(1);
    client.realtime.getChannels.mockReturnValue([]);
    await supabase.pauseSupabaseClient();

    expect(client.removeAllChannels).toHaveBeenCalledTimes(2);
  });

  it("restarts refresh after channel removal fails during pause", async () => {
    const supabase = await import("../supabaseClient");
    await supabase.resumeSupabaseClient();
    client.auth.startAutoRefresh.mockClear();
    client.removeAllChannels.mockRejectedValueOnce(new Error("remove failed"));

    await expect(supabase.pauseSupabaseClient()).rejects.toThrow("remove failed");
    await supabase.resumeSupabaseClient();

    expect(client.auth.startAutoRefresh).toHaveBeenCalledTimes(1);
  });

  it("retries outstanding channel cleanup after a removal failure", async () => {
    const supabase = await import("../supabaseClient");
    await supabase.resumeSupabaseClient();
    client.auth.stopAutoRefresh.mockClear();
    client.removeAllChannels.mockClear();
    client.removeAllChannels
      .mockRejectedValueOnce(new Error("remove failed"))
      .mockResolvedValueOnce([]);

    await expect(supabase.pauseSupabaseClient()).rejects.toThrow("remove failed");
    await supabase.pauseSupabaseClient();

    expect(client.auth.stopAutoRefresh).toHaveBeenCalledTimes(1);
    expect(client.removeAllChannels).toHaveBeenCalledTimes(2);
  });

  it("coalesces concurrent resume calls", async () => {
    const supabase = await import("../supabaseClient");
    const start = deferred<void>();
    client.auth.startAutoRefresh.mockReturnValue(start.promise);

    const first = supabase.resumeSupabaseClient();
    const second = supabase.resumeSupabaseClient();

    await flushAsyncWork();

    expect(client.auth.startAutoRefresh).toHaveBeenCalledTimes(1);
    start.resolve();
    await Promise.all([first, second]);

    expect(client.auth.startAutoRefresh).toHaveBeenCalledTimes(1);
  });

  it("coalesces concurrent pause calls", async () => {
    const supabase = await import("../supabaseClient");
    await supabase.resumeSupabaseClient();
    client.auth.stopAutoRefresh.mockClear();
    client.removeAllChannels.mockClear();
    const stop = deferred<void>();
    client.auth.stopAutoRefresh.mockReturnValue(stop.promise);

    const first = supabase.pauseSupabaseClient();
    const second = supabase.pauseSupabaseClient();

    await flushAsyncWork();

    expect(client.auth.stopAutoRefresh).toHaveBeenCalledTimes(1);
    stop.resolve();
    await Promise.all([first, second]);

    expect(client.removeAllChannels).toHaveBeenCalledTimes(1);
  });

  it("finishes a superseding pause after start refresh rejects", async () => {
    const supabase = await import("../supabaseClient");
    const start = deferred<void>();
    client.auth.startAutoRefresh.mockReturnValue(start.promise);

    const resume = supabase.resumeSupabaseClient();
    await flushAsyncWork();
    const pause = supabase.pauseSupabaseClient();
    start.reject(new Error("start failed"));

    await expect(Promise.all([resume, pause])).resolves.toEqual([
      undefined,
      undefined,
    ]);

    expect(client.auth.stopAutoRefresh).toHaveBeenCalledTimes(1);
    expect(client.removeAllChannels).toHaveBeenCalledTimes(1);
  });

  it("re-establishes refresh after stop rejects while resume supersedes pause", async () => {
    const supabase = await import("../supabaseClient");
    await supabase.resumeSupabaseClient();
    client.auth.startAutoRefresh.mockClear();
    const stop = deferred<void>();
    client.auth.stopAutoRefresh.mockReturnValue(stop.promise);

    const pause = supabase.pauseSupabaseClient();
    await flushAsyncWork();
    const resume = supabase.resumeSupabaseClient();
    stop.reject(new Error("stop failed"));

    await expect(Promise.all([pause, resume])).resolves.toEqual([
      undefined,
      undefined,
    ]);

    expect(client.auth.startAutoRefresh).toHaveBeenCalledTimes(1);
    expect(client.removeAllChannels).not.toHaveBeenCalled();
  });

  it("restarts refresh after channel cleanup rejects while resume supersedes pause", async () => {
    const supabase = await import("../supabaseClient");
    await supabase.resumeSupabaseClient();
    client.auth.startAutoRefresh.mockClear();
    const remove = deferred<unknown[]>();
    client.removeAllChannels.mockReturnValue(remove.promise);

    const pause = supabase.pauseSupabaseClient();
    await flushAsyncWork();
    const resume = supabase.resumeSupabaseClient();
    remove.reject(new Error("remove failed"));

    await expect(Promise.all([pause, resume])).resolves.toEqual([
      undefined,
      undefined,
    ]);

    expect(client.auth.startAutoRefresh).toHaveBeenCalledTimes(1);
  });

  it("settles resume then pause callers only after the final pause", async () => {
    const supabase = await import("../supabaseClient");
    const events: string[] = [];
    const start = deferred<void>();
    const remove = deferred<unknown[]>();
    client.auth.startAutoRefresh.mockImplementation(() => {
      events.push("start");
      return start.promise;
    });
    client.auth.stopAutoRefresh.mockImplementation(async () => {
      events.push("stop");
    });
    client.removeAllChannels.mockImplementation(() => {
      events.push("remove");
      return remove.promise;
    });

    let firstSettled = false;
    let secondSettled = false;
    const first = supabase.resumeSupabaseClient().then(() => {
      firstSettled = true;
    });
    await flushAsyncWork();
    const second = supabase.pauseSupabaseClient().then(() => {
      secondSettled = true;
    });

    start.resolve();
    await flushAsyncWork();

    expect(events).toEqual(["start", "stop", "remove"]);
    expect(firstSettled).toBe(false);
    expect(secondSettled).toBe(false);
    remove.resolve([]);
    await Promise.all([first, second]);
  });

  it("starts a new drain for a pause requested after the prior drain settles", async () => {
    const supabase = await import("../supabaseClient");
    const start = deferred<void>();
    const remove = deferred<unknown[]>();
    client.auth.startAutoRefresh.mockReturnValue(start.promise);
    client.removeAllChannels.mockReturnValue(remove.promise);

    const resume = supabase.resumeSupabaseClient();
    let trailingPause: Promise<void> | undefined;
    const pauseRequested = start.promise.then(() => {
      // This handler runs after the drain's final equality check, but before
      // the old `.finally()` cleanup would have cleared its shared promise.
      trailingPause = supabase.pauseSupabaseClient();
    });

    start.resolve();
    await pauseRequested;

    let pauseSettled = false;
    const pauseComplete = trailingPause!.then(() => {
      pauseSettled = true;
    });
    expect(client.auth.stopAutoRefresh).toHaveBeenCalledTimes(1);
    expect(client.removeAllChannels).toHaveBeenCalledTimes(1);
    expect(pauseSettled).toBe(false);
    remove.resolve([]);
    await Promise.all([resume, pauseComplete]);
  });

  it("waits for channel removal before restarting after pause then resume", async () => {
    const supabase = await import("../supabaseClient");
    const events: string[] = [];
    const remove = deferred<unknown[]>();
    client.auth.startAutoRefresh.mockImplementation(async () => {
      events.push("start");
    });
    client.auth.stopAutoRefresh.mockImplementation(async () => {
      events.push("stop");
    });
    client.removeAllChannels.mockImplementation(() => {
      events.push("remove");
      return remove.promise;
    });
    await supabase.resumeSupabaseClient();
    events.length = 0;

    const first = supabase.pauseSupabaseClient();
    await flushAsyncWork();
    const second = supabase.resumeSupabaseClient();

    expect(events).toEqual(["stop", "remove"]);
    remove.resolve([]);
    await Promise.all([first, second]);

    expect(events).toEqual(["stop", "remove", "start"]);
  });
});
