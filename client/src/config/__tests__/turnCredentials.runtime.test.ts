import path from "node:path";

import { createServer, loadConfigFromFile } from "vite";
import { afterEach, describe, expect, it, vi } from "vitest";

const CONFIGURED_ENDPOINT = "https://turn.example.test/credentials";

describe("configured TURN endpoint runtime boundary", () => {
  afterEach(() => {
    vi.unstubAllGlobals();
  });

  it("carries a custom build value through both configs to the runtime fetch", async () => {
    const previousEndpoint = process.env.TURN_CREDENTIALS_URL;
    process.env.TURN_CREDENTIALS_URL = CONFIGURED_ENDPOINT;
    const root = process.cwd();
    let server: Awaited<ReturnType<typeof createServer>> | undefined;

    try {
      const [viteConfig, vitestConfig] = await Promise.all([
        loadConfigFromFile({ command: "build", mode: "test" }, path.join(root, "vite.config.ts")),
        loadConfigFromFile({ command: "serve", mode: "test" }, path.join(root, "vitest.config.ts")),
      ]);
      expect(viteConfig?.config.define?.__TURN_CREDENTIALS_URL__).toBe(JSON.stringify(CONFIGURED_ENDPOINT));
      expect(vitestConfig?.config.define?.__TURN_CREDENTIALS_URL__).toBe(JSON.stringify(CONFIGURED_ENDPOINT));

      server = await createServer({
        root,
        define: vitestConfig?.config.define,
        server: { middlewareMode: true, hmr: false, ws: false },
        optimizeDeps: { noDiscovery: true },
        appType: "custom",
      });
      const connection = await server.ssrLoadModule("/src/network/connection.ts?turn-runtime-test");
      const fetcher = vi.fn().mockResolvedValue(new Response(JSON.stringify({
        iceServers: [{ urls: "turn:example.org:3478", username: "user", credential: "credential" }],
      })));
      vi.stubGlobal("fetch", fetcher);

      expect(connection.TURN_CREDENTIALS_URL).toBe(CONFIGURED_ENDPOINT);
      await connection.fetchFreshTurnConfig();
      expect(fetcher).toHaveBeenCalledWith(
        CONFIGURED_ENDPOINT,
        expect.objectContaining({ cache: "no-store", credentials: "omit" }),
      );
    } finally {
      if (server) await server.close();
      if (previousEndpoint === undefined) delete process.env.TURN_CREDENTIALS_URL;
      else process.env.TURN_CREDENTIALS_URL = previousEndpoint;
    }
  });
});
