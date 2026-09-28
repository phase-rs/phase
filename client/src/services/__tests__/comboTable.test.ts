import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

const wasm = vi.hoisted(() => ({
  init: vi.fn(),
  loadComboTable: vi.fn(),
}));

vi.mock("@wasm/engine", () => ({
  default: wasm.init,
  load_combo_table: wasm.loadComboTable,
}));

async function loadRuntime() {
  vi.resetModules();
  return import("../engineRuntime.ts");
}

beforeEach(() => {
  wasm.init.mockReset().mockResolvedValue(undefined);
  wasm.loadComboTable.mockReset();
  vi.stubGlobal("__ENGINE_WASM_URL__", undefined);
  vi.stubGlobal("__COMBO_TABLE_URL__", "/combo-table.json");
});

afterEach(() => {
  vi.unstubAllGlobals();
  vi.restoreAllMocks();
});

describe("ensureComboTable", () => {
  it("resolves unavailable for a missing artifact", async () => {
    const fetchMock = vi.fn().mockResolvedValue({ ok: false, status: 404 });
    vi.stubGlobal("fetch", fetchMock);
    const runtime = await loadRuntime();

    await expect(runtime.ensureComboTable()).resolves.toBe("unavailable");
    await expect(runtime.ensureComboTable()).resolves.toBe("unavailable");
    expect(fetchMock).toHaveBeenCalledOnce();
    expect(wasm.loadComboTable).not.toHaveBeenCalled();
  });

  it("retries after the WASM loader rejects malformed JSON", async () => {
    const fetchMock = vi
      .fn()
      .mockImplementation(() => Promise.resolve(new Response("not json", { status: 200 })));
    vi.stubGlobal("fetch", fetchMock);
    wasm.loadComboTable
      .mockRejectedValueOnce(new Error("invalid combo table"))
      .mockResolvedValueOnce(0);
    const runtime = await loadRuntime();

    await expect(runtime.ensureComboTable()).resolves.toBe("unavailable");
    await expect(runtime.ensureComboTable()).resolves.toBe("loaded");
    expect(fetchMock).toHaveBeenCalledTimes(2);
    expect(wasm.loadComboTable).toHaveBeenCalledTimes(2);
  });

  it("retries after transient WASM initialization failure", async () => {
    const fetchMock = vi.fn().mockResolvedValue(new Response("{\"entries\":[]}", { status: 200 }));
    vi.stubGlobal("fetch", fetchMock);
    wasm.init.mockRejectedValueOnce(new Error("WASM unavailable")).mockResolvedValueOnce(undefined);
    wasm.loadComboTable.mockResolvedValue(0);
    const runtime = await loadRuntime();

    await expect(runtime.ensureComboTable()).resolves.toBe("unavailable");
    await expect(runtime.ensureComboTable()).resolves.toBe("loaded");
    expect(wasm.init).toHaveBeenCalledTimes(2);
    expect(fetchMock).toHaveBeenCalledOnce();
  });

  it("loads once and memoizes success across callers", async () => {
    const fetchMock = vi.fn().mockResolvedValue(new Response("{\"entries\":[]}", { status: 200 }));
    vi.stubGlobal("fetch", fetchMock);
    wasm.loadComboTable.mockResolvedValue(0);
    const runtime = await loadRuntime();

    await expect(runtime.ensureComboTable()).resolves.toBe("loaded");
    await expect(runtime.ensureComboTable()).resolves.toBe("loaded");

    expect(fetchMock).toHaveBeenCalledOnce();
    expect(wasm.loadComboTable).toHaveBeenCalledOnce();
    expect(wasm.loadComboTable).toHaveBeenCalledWith("{\"entries\":[]}");
  });
});
