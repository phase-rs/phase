import { describe, expect, test } from "bun:test";
import { join } from "node:path";

import { resolveMultiplayerServerUrls } from "../../../client/src/config/multiplayerServerUrls";
import { BUILD_ENDPOINTS, BUILDS } from "../config";
import { findFormat } from "../formats";
import { desktopLink, hostLink } from "../lfgView";

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

  test("site origins match the desktop shell's channel origins", async () => {
    const channels = await readRepo("client/src-tauri/src/channels.rs");
    const declared = (name: string) => {
      const matches = [
        ...channels.matchAll(new RegExp(`^pub const ${name}: &str = "([^"]+)";$`, "gm")),
      ];
      expect(matches).toHaveLength(1);
      return matches[0][1];
    };
    expect(BUILD_ENDPOINTS.release.site).toBe(declared("RELEASE_ORIGIN"));
    expect(BUILD_ENDPOINTS.preview.site).toBe(declared("PREVIEW_ORIGIN"));
  });
});

describe("desktop link matches the desktop shell", () => {
  test("the phase:// scheme is the one tauri.conf.json registers", async () => {
    const conf = JSON.parse(await readRepo("client/src-tauri/tauri.conf.json"));
    const schemes: string[] = conf.plugins["deep-link"].desktop.schemes;
    expect(schemes).toHaveLength(1);
    const lfg = {
      id: "id",
      guildId: "guild",
      creatorId: "111",
      format: findFormat("Commander")!,
      seats: 4,
      mode: "p2p" as const,
      build: "release" as const,
      server: null,
      state: "ready" as const,
      code: "AB12CD",
      touchedMs: 0,
      seated: ["111", "222"],
      thread: null,
    };
    const to = new URL(new URL(desktopLink(lfg, hostLink(lfg))).searchParams.get("to")!);
    expect(schemes).toEqual([to.protocol.slice(0, -1)]);
  });
});
