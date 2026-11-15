// @vitest-environment node
import { mkdtempSync, readFileSync, rmSync } from "node:fs";
import { createServer as createTcpServer, type AddressInfo } from "node:net";
import { tmpdir } from "node:os";
import path from "node:path";

import { createServer, loadConfigFromFile, resolveConfig } from "vite";
import { afterAll, describe, expect, it } from "vitest";

/**
 * Scope: the occurrences of the port that a process resolves at runtime — the
 * vite config, tauri.conf.json's devUrl, the Caddyfile upstreams, the Tiltfile
 * link. Prose mentions of the literal are out of scope.
 */
const DEV_PORT = 5173;
const REPO = path.resolve(__dirname, "../../../..");
const CLIENT = path.join(REPO, "client");

// Isolated so the harness never writes into the cache dir a live `pnpm dev`
// owns: createServer({configFile:false, root:CLIENT}) otherwise resolves
// cacheDir to client/node_modules/.vite and orphans a deps_temp_* dir per run.
const CACHE = mkdtempSync(path.join(tmpdir(), "vite-devport-"));
afterAll(() => rmSync(CACHE, { recursive: true, force: true }));

/** The `server` options this repo's config *declares*. */
async function repoServerOptions() {
  const loaded = await loadConfigFromFile(
    { command: "serve", mode: "development" },
    path.join(CLIENT, "vite.config.ts"),
    CLIENT,
    "silent",
  );
  if (!loaded) throw new Error("vite.config.ts did not load");
  return loaded.config.server ?? {};
}

function boundPort(address: string | AddressInfo | null | undefined): number {
  if (address === null || address === undefined || typeof address === "string") {
    throw new Error(`expected a TCP address, got ${String(address)}`);
  }
  return address.port;
}

function occupyEphemeralPort(): Promise<{ close: () => Promise<void>; port: number }> {
  return new Promise((resolve, reject) => {
    const held = createTcpServer();
    held.on("error", reject);
    held.listen(0, "127.0.0.1", () => {
      resolve({
        close: () => new Promise<void>((done) => held.close(() => done())),
        port: boundPort(held.address()),
      });
    });
  });
}

/** Resolves to the bound port, or rejects the way vite rejects. */
async function startVite(port: number, strictPort: boolean | undefined): Promise<number> {
  const dev = await createServer({
    configFile: false,
    root: CLIENT,
    cacheDir: CACHE,
    logLevel: "silent",
    server: { port, strictPort, host: "127.0.0.1" },
  });
  try {
    await dev.listen();
    return boundPort(dev.httpServer?.address());
  } finally {
    await dev.close();
  }
}

describe("dev server port", () => {
  it("declares the pinned port and refuses to drift", async () => {
    const server = await repoServerOptions();
    // Reach guard, first: allowedHosts is a value this config owns, so it is
    // what separates "read our config" from "read nothing". The port readings
    // cannot do that job — 5173 is also vite's own default.
    expect(server.allowedHosts).toContain("local.phase-rs.dev");
    expect(server.port).toBe(DEV_PORT);
    expect(server.strictPort).toBe(true);

    // What `pnpm dev` actually runs on: every plugin config() hook applied,
    // which loadConfigFromFile runs none of.
    const resolved = await resolveConfig(
      { root: CLIENT, cacheDir: CACHE, logLevel: "silent" },
      "serve",
      "development",
    );
    expect(resolved.server.allowedHosts).toContain("local.phase-rs.dev");
    expect(resolved.server.strictPort).toBe(true);
    // Catches a plugin config() hook moving the port out from under the pin,
    // which the declared reading above is blind to.
    expect(resolved.server.port).toBe(DEV_PORT);
  });

  it(
    "refuses the pinned port when taken, and would drift without strictPort",
    { timeout: 20_000 },
    async () => {
      const { strictPort } = await repoServerOptions();
      // Ephemeral, never 5173: this must not depend on 5173 being free, nor
      // steal it from a developer's running `pnpm dev`.
      const taken = await occupyEphemeralPort();
      try {
        await expect(startVite(taken.port, strictPort)).rejects.toThrow(/already in use/);
        // Paired positive control: same harness, same contended port,
        // strictPort off. Proves the refusal is decided by our config value and
        // not by an unrelated startup failure that would reject either way.
        expect(await startVite(taken.port, false)).toBeGreaterThan(taken.port);
      } finally {
        await taken.close();
      }
    },
  );

  it("agrees with the Tauri devUrl", () => {
    const conf = JSON.parse(
      readFileSync(path.join(REPO, "client/src-tauri/tauri.conf.json"), "utf8"),
    ) as { build?: { devUrl?: string } };
    expect(conf.build?.devUrl).toBeDefined();
    expect(new URL(String(conf.build?.devUrl)).port).toBe(String(DEV_PORT));
  });

  it("agrees with the Caddy and Tilt upstreams", () => {
    const proxies = [
      ...readFileSync(path.join(REPO, "Caddyfile"), "utf8").matchAll(
        /reverse_proxy localhost:(\d+)/g,
      ),
    ];
    expect(proxies.length).toBeGreaterThan(0);
    for (const [, port] of proxies) expect(port).toBe(String(DEV_PORT));

    const tiltPorts = [
      ...readFileSync(path.join(REPO, "Tiltfile"), "utf8").matchAll(/links\s*=\s*\[([^\]]*)\]/g),
    ].flatMap((m) => [...m[1].matchAll(/http:\/\/localhost:(\d+)/g)].map((p) => p[1]));
    expect(tiltPorts.length).toBeGreaterThan(0);
    // Membership, not every-capture: the Tiltfile legitimately links several
    // services, while every Caddy upstream is this one server.
    expect(tiltPorts).toContain(String(DEV_PORT));
  });
});
