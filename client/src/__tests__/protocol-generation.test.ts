// @vitest-environment node
import { execFileSync } from "node:child_process";
import { resolve } from "node:path";
import { createLogger, createServer } from "vite";
import { afterEach, expect, it, vi } from "vitest";
import { generateProtocolVersions, protocolGenerationPlugin } from "../../protocol-generation";

vi.mock("node:child_process", async (importOriginal) => {
  const original = await importOriginal<typeof import("node:child_process")>();
  return { ...original, execFileSync: vi.fn(original.execFileSync) };
});

afterEach(() => vi.mocked(execFileSync).mockClear());

it("reports watcher failures and regenerates on the next valid event", async () => {
  const logger = createLogger("silent");
  const error = vi.spyOn(logger, "error");
  const server = await createServer({
    configFile: false,
    customLogger: logger,
    plugins: [protocolGenerationPlugin()],
    server: { watch: null },
  });
  try {
    const baseline = resolve(import.meta.dirname, "../../../protocol/baseline.json");
    const failure = new Error("partial protocol JSON");
    vi.mocked(execFileSync).mockImplementationOnce(() => { throw failure; });
    expect(() => server.watcher.emit("all", "change", baseline)).not.toThrow();
    expect(error).toHaveBeenCalledWith("protocol-generation failed: Error: partial protocol JSON");
    expect(execFileSync).toHaveBeenCalledTimes(1);
    server.watcher.emit("all", "change", baseline);
    expect(execFileSync).toHaveBeenCalledTimes(2);
    expect(error).toHaveBeenCalledTimes(1);
    server.watcher.emit("all", "change", resolve(import.meta.dirname, "unrelated.json"));
    expect(execFileSync).toHaveBeenCalledTimes(2);
  } finally {
    await server.close();
  }
});

it("propagates generator failures during config startup", () => {
  const failure = new Error("invalid protocol JSON");
  vi.mocked(execFileSync).mockImplementationOnce(() => { throw failure; });
  expect(() => generateProtocolVersions()).toThrow(failure);
});
