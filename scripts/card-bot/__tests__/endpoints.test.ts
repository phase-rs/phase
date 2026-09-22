import { describe, expect, test } from "bun:test";
import { join } from "node:path";

import { resolveMultiplayerServerUrls } from "../../../client/src/config/multiplayerServerUrls";
import { BUILD_ENDPOINTS, BUILDS } from "../config";

const REPO = join(import.meta.dir, "../../..");
const readRepo = (path: string) => Bun.file(join(REPO, path)).text();

describe("BUILD_ENDPOINTS matches each build's client", () => {
  test("release lobby = the client's fallback OFFICIAL broker, which release.yml never overrides", async () => {
    expect(BUILD_ENDPOINTS.release.lobbyWs).toBe(resolveMultiplayerServerUrls(() => undefined).official);

    const releaseYml = await readRepo(".github/workflows/release.yml");
    expect(releaseYml).toContain("jobs:"); // reach guard: the real workflow was read
    expect(releaseYml).not.toContain("OFFICIAL_MULTIPLAYER_SERVER_URL");
  });

  test("preview lobby = deploy.yml's OFFICIAL_MULTIPLAYER_SERVER_URL", async () => {
    const deployYml = await readRepo(".github/workflows/deploy.yml");
    const matches = [...deployYml.matchAll(/OFFICIAL_MULTIPLAYER_SERVER_URL: "([^"]+)"/g)];
    expect(matches).toHaveLength(1);
    const env: Record<string, string> = { OFFICIAL_MULTIPLAYER_SERVER_URL: matches[0][1] };
    expect(BUILD_ENDPOINTS.preview.lobbyWs).toBe(resolveMultiplayerServerUrls((name) => env[name]).official);
  });

  test("lobby HTTP base is the lobby WebSocket's host over https", () => {
    for (const build of BUILDS) {
      const ws = new URL(BUILD_ENDPOINTS[build].lobbyWs);
      const http = new URL(BUILD_ENDPOINTS[build].lobbyHttp);
      expect(ws.protocol).toBe("wss:");
      expect(http.protocol).toBe("https:");
      expect(http.host).toBe(ws.host);
    }
  });

  test("site origins match the desktop shell's RELEASE_ORIGIN / PREVIEW_ORIGIN", async () => {
    const nativeEngine = await readRepo("client/src-tauri/src/native_engine.rs");
    const declared = (name: string) => {
      const matches = [
        ...nativeEngine.matchAll(new RegExp(`^const ${name}: &str = "([^"]+)";$`, "gm")),
      ];
      expect(matches).toHaveLength(1);
      return matches[0][1];
    };
    expect(BUILD_ENDPOINTS.release.site).toBe(declared("RELEASE_ORIGIN"));
    expect(BUILD_ENDPOINTS.preview.site).toBe(declared("PREVIEW_ORIGIN"));
  });
});
