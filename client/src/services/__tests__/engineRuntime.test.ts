import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

type EngineModuleMock = Record<string, unknown>;
type EngineModuleMockFactory = () => EngineModuleMock | Promise<EngineModuleMock>;

const engineModuleMock = vi.hoisted(() => ({
  factory: null as EngineModuleMockFactory | null,
  modulePromise: null as Promise<EngineModuleMock> | null,
  rejectImport: false,
}));

async function invokeEngineExport(name: string, args: unknown[]) {
  if (!engineModuleMock.modulePromise) {
    const factory = engineModuleMock.factory;
    if (!factory) throw new Error("engine module mock was not configured");
    engineModuleMock.modulePromise = Promise.resolve().then(factory);
  }
  const engineModule = await engineModuleMock.modulePromise;
  const exported = engineModule[name];
  if (typeof exported !== "function") {
    throw new Error(`engine module mock is missing ${name}`);
  }
  return exported(...args);
}

vi.mock("@wasm/engine", () => {
  if (!engineModuleMock.factory) throw new Error("engine module mock was not configured");
  if (engineModuleMock.rejectImport) return engineModuleMock.factory();
  return {
    default: (...args: unknown[]) => invokeEngineExport("default", args),
    load_card_database: (text: string) => invokeEngineExport("load_card_database", [text]),
  };
});

async function loadRuntime(
  engineFactory: EngineModuleMockFactory,
  { rejectImport = false }: { rejectImport?: boolean } = {},
) {
  engineModuleMock.factory = engineFactory;
  engineModuleMock.modulePromise = null;
  engineModuleMock.rejectImport = rejectImport;
  vi.resetModules();
  return import("../engineRuntime.ts");
}

beforeEach(() => {
  engineModuleMock.factory = null;
  engineModuleMock.modulePromise = null;
  engineModuleMock.rejectImport = false;
});

afterEach(() => {
  vi.unstubAllGlobals();
  vi.restoreAllMocks();
});

describe("engine runtime initialization", () => {
  it("wraps an engine module import failure as reload-required for the document lifetime", async () => {
    const importFailure = new Error("module unavailable");
    const engineFactory = vi.fn(async () => {
      throw importFailure;
    });
    const runtime = await loadRuntime(engineFactory, { rejectImport: true });

    const firstError = await runtime.ensureWasmInit().catch((error: unknown) => error);
    const secondError = await runtime.ensureWasmInit().catch((error: unknown) => error);

    expect(firstError).toBeInstanceOf(runtime.EngineModuleReloadRequiredError);
    expect(secondError).toBe(firstError);
    expect(engineFactory).toHaveBeenCalledOnce();
  });

  it("retries WASM initialization before loading the card database without reimporting", async () => {
    const initialize = vi.fn()
      .mockRejectedValueOnce(new Error("temporary initialization failure"))
      .mockResolvedValue(undefined);
    const loadCardDatabase = vi.fn().mockResolvedValue(2);
    const engineFactory = vi.fn(() => ({
      default: initialize,
      load_card_database: loadCardDatabase,
    }));
    vi.stubGlobal("fetch", vi.fn().mockResolvedValue(new Response("{}", { status: 200 })));

    const runtime = await loadRuntime(engineFactory);
    await expect(runtime.ensureCardDatabase()).rejects.toThrow("temporary initialization failure");
    await expect(runtime.ensureCardDatabase()).resolves.toBe(2);

    expect(engineFactory).toHaveBeenCalledOnce();
    expect(initialize).toHaveBeenCalledTimes(2);
    expect(loadCardDatabase).toHaveBeenCalledOnce();
    await expect(runtime.ensureCardDatabase()).resolves.toBe(2);
    expect(global.fetch).toHaveBeenCalledOnce();
  });

  it("retries failed card-data attempts while sharing the replacement attempt", async () => {
    const initialize = vi.fn().mockResolvedValue(undefined);
    const loadCardDatabase = vi.fn().mockResolvedValue(3);
    vi.stubGlobal("fetch", vi.fn()
      .mockResolvedValueOnce({ ok: false, status: 503 } as Response)
      .mockImplementation(() => Promise.resolve(new Response("{}", { status: 200 }))));

    const runtime = await loadRuntime(() => ({
      default: initialize,
      load_card_database: loadCardDatabase,
    }));
    await expect(runtime.ensureCardDatabase()).rejects.toThrow("503");

    const first = runtime.ensureCardDatabase();
    const second = runtime.ensureCardDatabase();
    await expect(first).resolves.toBe(3);
    await expect(second).resolves.toBe(3);
    expect(global.fetch).toHaveBeenCalledTimes(2);
    expect(loadCardDatabase).toHaveBeenCalledOnce();
  });

  it("retries a throwing card-database load without reinitializing WASM", async () => {
    const initialize = vi.fn().mockResolvedValue(undefined);
    const loadCardDatabase = vi.fn()
      .mockRejectedValueOnce(new Error("temporary card-data failure"))
      .mockResolvedValueOnce(4);
    vi.stubGlobal("fetch", vi.fn().mockImplementation(() =>
      Promise.resolve(new Response("{}", { status: 200 }))));

    const runtime = await loadRuntime(() => ({
      default: initialize,
      load_card_database: loadCardDatabase,
    }));
    await expect(runtime.ensureCardDatabase()).rejects.toThrow("temporary card-data failure");
    await expect(runtime.ensureCardDatabase()).resolves.toBe(4);

    expect(initialize).toHaveBeenCalledOnce();
    expect(loadCardDatabase).toHaveBeenCalledTimes(2);
    expect(global.fetch).toHaveBeenCalledTimes(2);
  });

  it("rejects a non-positive card database result and retries it", async () => {
    const loadCardDatabase = vi.fn().mockResolvedValueOnce(0).mockResolvedValueOnce(1);
    vi.stubGlobal("fetch", vi.fn().mockImplementation(() =>
      Promise.resolve(new Response("{}", { status: 200 }))));

    const runtime = await loadRuntime(() => ({
      default: vi.fn().mockResolvedValue(undefined),
      load_card_database: loadCardDatabase,
    }));
    await expect(runtime.ensureCardDatabase()).rejects.toThrow("no cards loaded");
    await expect(runtime.ensureCardDatabase()).resolves.toBe(1);
  });

  it("shares one in-flight WASM initialization attempt", async () => {
    let resolveInitialization!: () => void;
    const initialize = vi.fn(() => new Promise<void>((resolve) => {
      resolveInitialization = resolve;
    }));
    const runtime = await loadRuntime(() => ({ default: initialize }));

    const first = runtime.ensureWasmInit();
    const second = runtime.ensureWasmInit();
    await vi.waitFor(() => expect(initialize).toHaveBeenCalledOnce());

    resolveInitialization();
    await expect(first).resolves.toBeUndefined();
    await expect(second).resolves.toBeUndefined();
  });
});
