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
    vi.stubGlobal("fetch", vi.fn().mockResolvedValue({ ok: false, status: 404 }));
    const runtime = await loadRuntime();

    await expect(runtime.ensureComboTable()).resolves.toBe("unavailable");
    expect(wasm.loadComboTable).not.toHaveBeenCalled();
  });

  it("resolves unavailable when the WASM loader rejects malformed JSON", async () => {
    vi.stubGlobal("fetch", vi.fn().mockResolvedValue(new Response("not json", { status: 200 })));
    wasm.loadComboTable.mockImplementation(() => {
      throw new Error("invalid combo table");
    });
    const runtime = await loadRuntime();

    await expect(runtime.ensureComboTable()).resolves.toBe("unavailable");
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
