import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { UnlistenFn } from "@tauri-apps/api/event";

const { isDesktopTauriMock, listenMock, eventModule } = vi.hoisted(() => ({
  isDesktopTauriMock: vi.fn(),
  listenMock: vi.fn(),
  eventModule: { reads: 0, failRead: false },
}));

vi.mock("../platform", () => ({ isDesktopTauri: isDesktopTauriMock }));
// A getter, not a plain property: the mock factory runs once per test FILE, so
// a counter inside it cannot tell "never imported" from "imported by an earlier
// test". Reading `listen` is what every consumer of this module does.
vi.mock("@tauri-apps/api/event", () => ({
  get listen() {
    eventModule.reads += 1;
    // Stands in for a dynamic import whose module never loads: a failed chunk
    // load rejects the import() itself while this throws from the .then, and
    // both land in the same .catch, which is the fallback under test.
    if (eventModule.failRead) throw new Error("failed to fetch dynamically imported module");
    return listenMock;
  },
}));

interface ShellDownload {
  url: string;
  path: string | null;
  outcome: "saved" | "unknown" | "failed";
}

describe("downloadBlob", () => {
  /** Every listen/click in the order it happened — the race is the point. */
  let calls: string[] = [];
  let emit: ((payload: ShellDownload) => void) | null = null;
  let unlisten = vi.fn();

  async function fileDownload() {
    return import("../fileDownload.ts");
  }

  beforeEach(() => {
    vi.resetModules();
    calls = [];
    emit = null;
    eventModule.reads = 0;
    eventModule.failRead = false;
    unlisten = vi.fn();
    isDesktopTauriMock.mockReturnValue(false);
    listenMock.mockImplementation(
      async (_event: string, handler: (event: { payload: ShellDownload }) => void) => {
        calls.push("listen");
        emit = (payload) => handler({ payload });
        return unlisten as unknown as UnlistenFn;
      },
    );
  });

  afterEach(() => {
    vi.useRealTimers();
    vi.restoreAllMocks();
    Reflect.deleteProperty(window, "showSaveFilePicker");
  });

  function stubAnchorDownload(onClick?: () => void) {
    let downloadedBlob: Blob | null = null;
    let downloadedName: string | null = null;
    vi.spyOn(URL, "createObjectURL").mockImplementation((blob) => {
      downloadedBlob = blob as Blob;
      return "blob:mock-url";
    });
    vi.spyOn(URL, "revokeObjectURL").mockImplementation(() => {});
    vi.spyOn(HTMLAnchorElement.prototype, "click").mockImplementation(function (
      this: HTMLAnchorElement,
    ) {
      calls.push("click");
      downloadedName = this.download;
      onClick?.();
    });
    return {
      blob: () => downloadedBlob,
      filename: () => downloadedName,
    };
  }

  /** The shell reports the finished download the moment the click lands. */
  function shellFinishes(payload: Omit<ShellDownload, "url">) {
    return () => {
      if (!emit) throw new Error("clicked before subscribing to shell-download");
      emit({ url: "blob:mock-url", ...payload });
    };
  }

  it("writes through the save picker when it succeeds", async () => {
    const write = vi.fn(async () => {});
    const close = vi.fn(async () => {});
    const showSaveFilePicker = vi.fn(async () => ({
      createWritable: async () => ({ write, close }),
    }));
    Object.defineProperty(window, "showSaveFilePicker", {
      configurable: true,
      value: showSaveFilePicker,
    });
    const blob = new Blob(["data"], { type: "text/plain" });
    const { downloadBlob } = await fileDownload();

    const result = await downloadBlob("notes.txt", blob, [
      { description: "Text", accept: { "text/plain": [".txt"] } },
    ]);

    // The picker writes wherever the user pointed it and never says where.
    expect(result).toStrictEqual({ kind: "saved", filename: "notes.txt" });
    expect(showSaveFilePicker).toHaveBeenCalledWith({
      suggestedName: "notes.txt",
      types: [{ description: "Text", accept: { "text/plain": [".txt"] } }],
    });
    expect(write).toHaveBeenCalledWith(blob);
    expect(close).toHaveBeenCalledOnce();
  });

  it("falls back to an anchor download when the picker fails", async () => {
    // Chrome exposes showSaveFilePicker but the picker path can fail there;
    // the download must then degrade to the plain anchor path Firefox uses.
    Object.defineProperty(window, "showSaveFilePicker", {
      configurable: true,
      value: vi.fn(async () => {
        throw new DOMException("The picker is unavailable", "SecurityError");
      }),
    });
    const anchor = stubAnchorDownload();
    const blob = new Blob(["data"], { type: "text/plain" });
    const { downloadBlob } = await fileDownload();

    const result = await downloadBlob("notes.txt", blob);

    expect(result).toStrictEqual({ kind: "saved", filename: "notes.txt" });
    expect(anchor.filename()).toBe("notes.txt");
    expect(anchor.blob()).toBe(blob);
  });

  it("reports an anchor download in a browser without waiting on any shell", async () => {
    const anchor = stubAnchorDownload();
    const { downloadBlob } = await fileDownload();

    const result = await downloadBlob("notes.txt", new Blob(["data"]));

    // No path: a browser cannot learn one, and claiming one would be a lie.
    expect(result).toStrictEqual({ kind: "saved", filename: "notes.txt" });
    expect(anchor.filename()).toBe("notes.txt");
    // Paired with the shell cases below, which do read it: off the shell the
    // tauri event module must not be pulled in at all.
    expect(eventModule.reads).toBe(0);
    expect(listenMock).not.toHaveBeenCalled();
  });

  it("subscribes to the shell's download event before clicking", async () => {
    // A small download can finish before a listener registered after the click
    // would exist, so the order here is the whole mechanism.
    isDesktopTauriMock.mockReturnValue(true);
    stubAnchorDownload(shellFinishes({ path: "~/Downloads/notes.txt", outcome: "saved" }));
    const { downloadBlob } = await fileDownload();

    await downloadBlob("notes.txt", new Blob(["data"]));

    expect(calls).toEqual(["listen", "click"]);
    expect(listenMock).toHaveBeenCalledWith("shell-download", expect.any(Function));
    expect(eventModule.reads).toBe(1);
  });

  it("reports the path the shell gives when the download succeeds", async () => {
    isDesktopTauriMock.mockReturnValue(true);
    stubAnchorDownload(shellFinishes({ path: "~/Downloads/notes.txt", outcome: "saved" }));
    const { downloadBlob } = await fileDownload();

    const result = await downloadBlob("notes.txt", new Blob(["data"]));

    expect(result).toStrictEqual({
      kind: "saved",
      filename: "notes.txt",
      path: "~/Downloads/notes.txt",
    });
    expect(unlisten).toHaveBeenCalledOnce();
  });

  it("reports a failure when the shell says the download failed", async () => {
    isDesktopTauriMock.mockReturnValue(true);
    stubAnchorDownload(shellFinishes({ path: "~/Downloads/notes.txt", outcome: "failed" }));
    const { downloadBlob } = await fileDownload();

    const result = await downloadBlob("notes.txt", new Blob(["data"]));

    expect(result).toStrictEqual({
      kind: "failed",
      filename: "notes.txt",
      path: "~/Downloads/notes.txt",
    });
    expect(unlisten).toHaveBeenCalledOnce();
  });

  it("does not claim a save the shell could not confirm", async () => {
    // A file at the destination after a reported failure is equally a stale
    // latched failure flag and a write that died mid-file, so the shell says
    // "unknown" and the page must not turn that into a saved file.
    isDesktopTauriMock.mockReturnValue(true);
    stubAnchorDownload(shellFinishes({ path: "~/Downloads/notes.txt", outcome: "unknown" }));
    const { downloadBlob } = await fileDownload();

    const result = await downloadBlob("notes.txt", new Blob(["data"]));

    expect(result).toStrictEqual({ kind: "requested", filename: "notes.txt" });
    expect(unlisten).toHaveBeenCalledOnce();
  });

  it("ignores a shell report for another download and waits for its own", async () => {
    // Two exports can overlap inside the 10 s wait, and the log dump in
    // DebugPanel is another shell download entirely. Settling on whoever
    // reports first would print the other file's path.
    isDesktopTauriMock.mockReturnValue(true);
    stubAnchorDownload(() => {
      if (!emit) throw new Error("clicked before subscribing to shell-download");
      emit({ url: "blob:another-export", path: "~/Downloads/other.zip", outcome: "failed" });
      emit({ url: "blob:mock-url", path: "~/Downloads/notes.txt", outcome: "saved" });
    });
    const { downloadBlob } = await fileDownload();

    const result = await downloadBlob("notes.txt", new Blob(["data"]));

    expect(result).toStrictEqual({
      kind: "saved",
      filename: "notes.txt",
      path: "~/Downloads/notes.txt",
    });
  });

  it("keeps a pathless shell report honest", async () => {
    isDesktopTauriMock.mockReturnValue(true);
    stubAnchorDownload(shellFinishes({ path: null, outcome: "saved" }));
    const { downloadBlob } = await fileDownload();

    const result = await downloadBlob("notes.txt", new Blob(["data"]));

    expect(result).toStrictEqual({ kind: "saved", filename: "notes.txt", path: undefined });
  });

  it("falls back to 'requested' when the shell reports nothing in time", async () => {
    isDesktopTauriMock.mockReturnValue(true);
    vi.useFakeTimers();
    stubAnchorDownload();
    const { downloadBlob } = await fileDownload();

    const pending = downloadBlob("notes.txt", new Blob(["data"]));
    await vi.advanceTimersByTimeAsync(10_000);

    // Nothing was heard, so the message must not claim the file was saved.
    expect(await pending).toStrictEqual({ kind: "requested", filename: "notes.txt" });
    expect(calls).toEqual(["listen", "click"]);
    expect(unlisten).toHaveBeenCalledOnce();
  });

  it("does not resolve before the shell reports or the wait elapses", async () => {
    isDesktopTauriMock.mockReturnValue(true);
    vi.useFakeTimers();
    stubAnchorDownload();
    const { downloadBlob } = await fileDownload();
    const settled = vi.fn();

    void downloadBlob("notes.txt", new Blob(["data"])).then(settled);
    await vi.advanceTimersByTimeAsync(9_000);

    // Guards the timeout test above: without this, a result that resolved
    // immediately would still look like a "timeout".
    expect(settled).not.toHaveBeenCalled();
    expect(calls).toEqual(["listen", "click"]);

    await vi.advanceTimersByTimeAsync(1_000);
    expect(settled).toHaveBeenCalledWith({ kind: "requested", filename: "notes.txt" });
  });

  it("still downloads when the tauri event module cannot be loaded", async () => {
    // Setup runs before the click, so a failure here would otherwise cost the
    // user the export itself — the exact shape this whole change exists to fix.
    isDesktopTauriMock.mockReturnValue(true);
    eventModule.failRead = true;
    const anchor = stubAnchorDownload();
    const { downloadBlob } = await fileDownload();

    const result = await downloadBlob("notes.txt", new Blob(["data"]));

    expect(result).toStrictEqual({ kind: "requested", filename: "notes.txt" });
    expect(calls).toEqual(["click"]);
    expect(anchor.filename()).toBe("notes.txt");
    expect(unlisten).not.toHaveBeenCalled();
  });

  it("still downloads when subscribing to the shell event is refused", async () => {
    isDesktopTauriMock.mockReturnValue(true);
    listenMock.mockRejectedValueOnce(new Error("event.listen not allowed"));
    const anchor = stubAnchorDownload();
    const { downloadBlob } = await fileDownload();

    const result = await downloadBlob("notes.txt", new Blob(["data"]));

    expect(result).toStrictEqual({ kind: "requested", filename: "notes.txt" });
    // Exactly one click, and no listener left behind to unsubscribe.
    expect(calls).toEqual(["click"]);
    expect(anchor.filename()).toBe("notes.txt");
    expect(unlisten).not.toHaveBeenCalled();
  });

  it("does not download when the user cancels the picker", async () => {
    Object.defineProperty(window, "showSaveFilePicker", {
      configurable: true,
      value: vi.fn(async () => {
        throw new DOMException("The user aborted a request", "AbortError");
      }),
    });
    const clickSpy = vi
      .spyOn(HTMLAnchorElement.prototype, "click")
      .mockImplementation(() => {});
    const { downloadBlob } = await fileDownload();

    const err = await downloadBlob("notes.txt", new Blob(["data"])).catch((e: unknown) => e);

    expect(err).toBeInstanceOf(DOMException);
    expect((err as DOMException).name).toBe("AbortError");
    expect(clickSpy).not.toHaveBeenCalled();
  });

  it.each(["createWritable", "write", "close"] as const)(
    "rejects without an anchor download when %s fails on the picked file",
    async (failingStep) => {
      // Once a destination is chosen it may already be empty or partial, so a
      // failed write must surface as a failure, not as a fallback "success".
      const streamError = new DOMException("Write failed", "NoModificationAllowedError");
      const steps = {
        createWritable: vi.fn(async () => {}),
        write: vi.fn(async () => {}),
        close: vi.fn(async () => {}),
      };
      steps[failingStep].mockRejectedValueOnce(streamError);
      Object.defineProperty(window, "showSaveFilePicker", {
        configurable: true,
        value: vi.fn(async () => ({
          createWritable: async () => {
            await steps.createWritable();
            return { write: steps.write, close: steps.close };
          },
        })),
      });
      const anchor = stubAnchorDownload();
      const { downloadBlob } = await fileDownload();

      await expect(downloadBlob("notes.txt", new Blob(["data"]))).rejects.toBe(streamError);

      expect(steps[failingStep]).toHaveBeenCalledOnce();
      expect(anchor.filename()).toBeNull();
    },
  );
});
