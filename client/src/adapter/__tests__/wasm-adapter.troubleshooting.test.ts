import { afterEach, describe, expect, it, vi } from "vitest";
import { WasmAdapter } from "../wasm-adapter";
import { getDiagnosticHistory, getDiagnosticSources } from "../../services/troubleshooting";
import { trackEvent } from "../../services/telemetry";

const worker = vi.hoisted(() => ({ initialize: vi.fn().mockResolvedValue(undefined), dispose: vi.fn(), ping: vi.fn().mockResolvedValue("pong") }));
vi.mock("../engine-worker-client", () => ({ EngineWorkerClient: class { initialize = worker.initialize; dispose = worker.dispose; ping = worker.ping; } }));
vi.mock("../../services/telemetry", () => ({ trackEvent: vi.fn() }));
const adapters: WasmAdapter[] = [];
afterEach(() => { adapters.splice(0).forEach((adapter) => adapter.dispose()); vi.clearAllMocks(); });
function adapter() { const value = new WasmAdapter(); adapters.push(value); return value; }

describe("WASM diagnostic lifecycle", () => {
  it("records a caught guard with static operation and unchanged exception", async () => {
    const value = adapter();
    await expect(value.getState()).rejects.toThrow("Adapter not initialized. Call initialize() first.");
    expect(getDiagnosticHistory().slice(-1)[0]).toMatchObject({ kind: "engine-not-initialized", operation: "getState", engine: { initializing: false, disposed: false } });
    expect(trackEvent).toHaveBeenCalledWith("wasm_not_initialized", expect.objectContaining({ operation: "getState", disposed: false }));
    value.dispose();
    await expect(value.getState()).rejects.toThrow();
    expect(getDiagnosticHistory().slice(-1)[0]).toMatchObject({ engine: { disposed: true } });
  });
  it("reports actual in-flight init and unregisters then re-registers on reuse", async () => {
    let finish!: () => void;
    worker.initialize.mockImplementationOnce(() => new Promise<void>((resolve) => { finish = resolve; }));
    const before = getDiagnosticSources().engines.length;
    const value = adapter();
    const source = getDiagnosticSources().engines.slice(-1)[0]!;
    expect(source.snapshot()).toMatchObject({ initialized: false, initializing: false });
    await expect(source.ping()).rejects.toThrow("Engine unavailable");
    expect(worker.initialize).not.toHaveBeenCalled();
    const pending = value.initialize();
    expect(source.snapshot().initializing).toBe(true);
    finish();
    await pending;
    expect(source.snapshot()).toMatchObject({ initialized: true, initializing: false });
    await expect(source.ping()).resolves.toBe("pong");
    value.dispose();
    expect(getDiagnosticSources().engines).toHaveLength(before);
    expect(source.snapshot().disposed).toBe(true);
    await value.initialize();
    expect(getDiagnosticSources().engines).toHaveLength(before + 1);
    expect(getDiagnosticSources().engines.slice(-1)[0]!.snapshot().disposed).toBe(false);
  });
});
