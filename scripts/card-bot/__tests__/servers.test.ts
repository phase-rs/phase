import { afterAll, beforeAll, describe, expect, spyOn, test } from "bun:test";
import { join } from "node:path";

import { BUILD_ENDPOINTS, type Build } from "../config";
import {
  brokerSupportsBotGames,
  DIRECTORY_VERSION,
  type DirectoryServer,
  eligibleServers,
  type FetchFn,
  type LobbyInfo,
  ServerCache,
} from "../servers";

const BROKER: LobbyInfo = { protocol_version: 76, lobby_protocol_version: 10 };

function row(overrides: Partial<DirectoryServer> = {}): DirectoryServer {
  return {
    url: "wss://a.example/ws",
    name: "A",
    mode: "Full",
    protocol_version: 76,
    lobby_protocol_version: 10,
    current_players: 3,
    score: { value: 90 },
    ...overrides,
  };
}

const urls = (rows: readonly DirectoryServer[]) => rows.map((r) => r.url);

describe("availability gate", () => {
  test("a broker honors bot games from lobby protocol 10", () => {
    expect(brokerSupportsBotGames({ protocol_version: 76, lobby_protocol_version: 9 })).toBe(false);
    expect(brokerSupportsBotGames({ protocol_version: 76, lobby_protocol_version: 10 })).toBe(true);
  });

  test("eligibleServers keeps Full / lobby ≥ 10 / same protocol / printable-ASCII url ≤ 100 only", () => {
    const good = row({ url: "wss://good.example/ws" });
    const cases: [string, DirectoryServer][] = [
      ["LobbyOnly", row({ url: "wss://lobbyonly.example/ws", mode: "LobbyOnly" })],
      ["lobby 9", row({ url: "wss://lobby9.example/ws", lobby_protocol_version: 9 })],
      ["protocol -1", row({ url: "wss://older.example/ws", protocol_version: 75 })],
      ["protocol +1", row({ url: "wss://newer.example/ws", protocol_version: 77 })],
      ["url > 100", row({ url: `wss://${"x".repeat(95)}.example/ws` })],
      // 100 chars, so only the charset excludes it: percent-encoded, it overflows
      // Discord's 512-char link-button URL.
      ["non-ASCII url", row({ url: `wss://${"é".repeat(100 - "wss://.example/ws".length)}.example/ws` })],
      ["url with whitespace", row({ url: "wss://a b.example/ws" })],
    ];
    for (const [, bad] of cases) {
      expect(urls(eligibleServers(BROKER, [bad, good]))).toEqual([good.url]);
    }
    // Boundary: a 100-char URL is still offered.
    const exactly100 = row({ url: `wss://${"y".repeat(100 - "wss://.example/ws".length)}.example/ws` });
    expect(exactly100.url).toHaveLength(100);
    expect(urls(eligibleServers(BROKER, [exactly100]))).toEqual([exactly100.url]);
  });

  test("sorted by score descending (null last), then url ascending", () => {
    const rows = [
      row({ url: "wss://n.example/ws", score: null }),
      row({ url: "wss://b50.example/ws", score: { value: 50 } }),
      row({ url: "wss://v-null.example/ws", score: { value: null } }),
      row({ url: "wss://z80.example/ws", score: { value: 80 } }),
      row({ url: "wss://a80.example/ws", score: { value: 80 } }),
    ];
    expect(urls(eligibleServers(BROKER, rows))).toEqual([
      "wss://a80.example/ws",
      "wss://z80.example/ws",
      "wss://b50.example/ws",
      "wss://n.example/ws",
      "wss://v-null.example/ws",
    ]);
  });

  test("the default pick ([0]) is the best-scored row, not the first listed or the lowest URL", () => {
    // The lower-scored row is listed first AND has the alphabetically earlier URL,
    // so neither "first unsorted" nor "sort by url" picks the score-80 row.
    const low = row({ url: "wss://a-low.example/ws", score: { value: 50 } });
    const high = row({ url: "wss://b-high.example/ws", score: { value: 80 } });
    expect(eligibleServers(BROKER, [low, high])[0].url).toBe(high.url);
  });
});

type Route = () => Response | Promise<Response>;

/** A fetch stub answering `<lobbyHttp>/health` and `/servers` per build; a build
 *  with no routes answers 503. */
function stubFetch(routes: Partial<Record<Build, { health: Route; servers: Route }>>): FetchFn {
  return async (url) => {
    for (const build of Object.keys(routes) as Build[]) {
      const base = BUILD_ENDPOINTS[build].lobbyHttp;
      if (url === `${base}/health`) return routes[build]!.health();
      if (url === `${base}/servers`) return routes[build]!.servers();
    }
    return new Response("unavailable", { status: 503 });
  };
}

const healthOk: Route = () => Response.json({ mode: "LobbyOnly", ...BROKER, server_version: "lobby-rs" });
const envelope = (servers: unknown, directory_version: unknown = DIRECTORY_VERSION) =>
  () => Response.json({ directory_version, servers });
const status = (code: number): Route => () => new Response("nope", { status: code });

async function refreshed(routes: Parameters<typeof stubFetch>[0]): Promise<ServerCache> {
  const cache = new ServerCache(stubFetch(routes));
  await cache.refresh();
  return cache;
}

describe("ServerCache.refresh", () => {
  // refresh() logs every failed lobby request; the failures here are deliberate.
  let quiet: ReturnType<typeof spyOn>;
  beforeAll(() => {
    quiet = spyOn(console, "error").mockImplementation(() => {});
  });
  afterAll(() => quiet.mockRestore());

  test("an eligible row in the envelope is cached with the broker", async () => {
    const cache = await refreshed({ release: { health: healthOk, servers: envelope([row()]) } });
    expect(cache.get("release")).toEqual({ broker: BROKER, servers: [row()] });
  });

  test("a failing /servers leaves the broker set with no servers", async () => {
    const failures: [string, Route][] = [
      // A non-2xx body is never read, even when it looks like a good envelope.
      ["500 (with an envelope body)", () =>
        Response.json({ directory_version: DIRECTORY_VERSION, servers: [row()] }, { status: 500 })],
      ["throws (timeout abort / network error)", () => {
        throw new DOMException("The operation timed out.", "TimeoutError");
      }],
      ["200 with a non-JSON body", () => new Response("<html>not json</html>")],
      ["wrong directory_version", envelope([row()], 2)],
      ["absent directory_version", () => Response.json({ servers: [row()] })],
      ["bare array (pre-envelope shape)", () => Response.json([row()])],
      ["servers not an array", envelope({ url: "wss://a.example/ws" })],
    ];
    for (const [label, servers] of failures) {
      const cache = await refreshed({ release: { health: healthOk, servers } });
      expect({ label, entry: cache.get("release") }).toEqual({
        label,
        entry: { broker: BROKER, servers: [] },
      });
    }
  });

  test("a failing /health makes the build unknown (null)", async () => {
    const failures: Route[] = [
      status(500),
      () => new Response("ok"), // a Full server's plain-text /health is not a version document
      () => Response.json({ protocol_version: 76 }),
      () => {
        throw new Error("network down");
      },
    ];
    for (const health of failures) {
      const cache = await refreshed({ release: { health, servers: envelope([row()]) } });
      expect(cache.get("release")).toBeNull();
    }
  });

  test("a malformed row beside a good one keeps only the good one", async () => {
    const malformed = [
      { ...row({ url: "wss://bad1.example/ws" }), protocol_version: "76" },
      { ...row({ url: "wss://bad2.example/ws" }), score: 80 },
      { ...row({ url: "wss://bad3.example/ws" }), score: { value: "80" } },
      { ...row({ url: "wss://bad4.example/ws" }), name: undefined },
      null,
      "wss://bad5.example/ws",
    ];
    const cache = await refreshed({
      release: { health: healthOk, servers: envelope([...malformed, row()]) },
    });
    expect(urls(cache.get("release")!.servers)).toEqual([row().url]);
  });

  test("builds are independent: release failing does not null preview", async () => {
    // Release fails only after preview has settled, so a failure that clobbered
    // other builds' entries could not be hidden by preview writing last.
    const slowFailure: Route = async () => {
      await Bun.sleep(5);
      return status(503)();
    };
    const cache = await refreshed({
      release: { health: slowFailure, servers: status(503) },
      preview: { health: healthOk, servers: envelope([row()]) },
    });
    expect(cache.get("release")).toBeNull();
    expect(cache.get("preview")).toEqual({ broker: BROKER, servers: [row()] });
  });

  test("an entry is exactly the last refresh (a server that left stops being offered)", async () => {
    let servers = [row()];
    const cache = new ServerCache(
      stubFetch({ release: { health: healthOk, servers: () => Response.json({ directory_version: 1, servers }) } }),
    );
    await cache.refresh();
    expect(cache.get("release")!.servers).toHaveLength(1);
    servers = [];
    await cache.refresh();
    expect(cache.get("release")!.servers).toEqual([]);
  });

  test("never refreshed → null", () => {
    expect(new ServerCache(stubFetch({})).get("preview")).toBeNull();
  });
});

test("DIRECTORY_VERSION equals lobby_broker::directory::DIRECTORY_VERSION", async () => {
  const rust = await Bun.file(
    join(import.meta.dir, "../../../crates/lobby-broker/src/directory.rs"),
  ).text();
  const matches = [...rust.matchAll(/pub const DIRECTORY_VERSION: u32 = (\d+);/g)];
  expect(matches).toHaveLength(1);
  expect(DIRECTORY_VERSION).toBe(Number(matches[0][1]));
});
