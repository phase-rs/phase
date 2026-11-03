import { afterEach, beforeEach, describe, expect, spyOn, test } from "bun:test";
import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

import { BUILD_ENDPOINTS, type Build } from "../config";
import type { MessageApi } from "../discord";
import {
  CLOSED_POST_RETENTION_MS,
  type LobbyGameRow,
  type LobbyMirrorDeps,
  LOBBY_SETTLE_MS,
  LobbyPostStore,
  parseLobbyGames,
  syncLobbyPosts,
} from "../lobbyMirror";

/** Registration time of the test listings, in seconds; settled from SETTLED on. */
const CREATED_S = 1_700_000_000;
const SETTLED = CREATED_S * 1000 + LOBBY_SETTLE_MS;
const CHANNEL = "c-lobby";

function game(overrides: Partial<LobbyGameRow> = {}): LobbyGameRow {
  return {
    game_code: "AB12CD",
    host_name: "alice",
    created_at: CREATED_S,
    has_password: false,
    current_players: 1,
    max_players: 2,
    is_sandbox: false,
    format: "Standard",
    room_name: null,
    ...overrides,
  };
}

/** The lobby Worker's info document: what a Worker without `/games` answers. */
const INFO_DOCUMENT = { mode: "LobbyOnly", protocol_version: 76, lobby_protocol_version: 10, server_version: "lobby-rs" };

describe("parseLobbyGames", () => {
  test("a body without a games array is unreadable (null)", () => {
    for (const body of [null, "games", [game()], {}, { games: "x" }, { games: { 0: game() } }, INFO_DOCUMENT]) {
      expect(parseLobbyGames(body)).toBeNull();
    }
  });

  test("malformed rows are dropped; absent or null format and room_name are well-formed", () => {
    const good = [
      game(),
      game({ game_code: "NOFMT1", format: undefined, room_name: undefined }),
      game({ game_code: "ROOM01", format: null, room_name: "Friday pod" }),
    ];
    const malformed = [
      { ...game(), created_at: "1700000000" },
      { ...game(), current_players: null },
      { ...game(), has_password: undefined },
      { ...game(), is_sandbox: 0 },
      { ...game(), format: 3 },
      { ...game(), room_name: {} },
      { ...game(), game_code: undefined },
      null,
      "AB12CD",
    ];
    expect(parseLobbyGames({ games: [...malformed, ...good] })).toEqual(good);
  });
});

describe("syncLobbyPosts", () => {
  /** Each build's `/games` answer; a build left out answers 503. */
  let listings: Partial<Record<Build, () => Response>>;
  let creates: { channelId: string; body: unknown; nonce: string }[];
  /** How many of the next `create` calls reject. */
  let failNext: number;
  let edits: { channelId: string; messageId: string; body: unknown }[];
  /** How many of the next `edit` calls reject. */
  let failNextEdit: number;
  /** Message ids a moderator deleted: their edits answer "gone". */
  let deleted: Set<string>;
  /** Message ids the mirror removed, in order. */
  let removed: string[];
  /** How many of the next `delete` calls reject. */
  let failNextDelete: number;
  let clock: number;
  let errors: ReturnType<typeof spyOn>;
  let logs: ReturnType<typeof spyOn>;

  const listed = (...games: unknown[]) => () => Response.json({ games });

  const messages: MessageApi = {
    async create(channelId, body, nonce) {
      if (failNext > 0) {
        failNext--;
        throw new Error("discord down");
      }
      creates.push({ channelId, body, nonce });
      return `m-${creates.length}`;
    },
    async edit(channelId, messageId, body) {
      if (failNextEdit > 0) {
        failNextEdit--;
        throw new Error("discord down");
      }
      edits.push({ channelId, messageId, body });
      return deleted.has(messageId) ? "gone" : "edited";
    },
    async delete(channelId, messageId) {
      if (failNextDelete > 0) {
        failNextDelete--;
        throw new Error("discord down");
      }
      expect(channelId).toBe(CHANNEL);
      removed.push(messageId);
    },
  };

  function deps(posts = new LobbyPostStore(":memory:")): LobbyMirrorDeps {
    return {
      posts,
      messages,
      channelId: CHANNEL,
      fetchFn: async (url) => {
        for (const build of Object.keys(listings) as Build[]) {
          if (url === `${BUILD_ENDPOINTS[build].lobbyHttp}/games`) return listings[build]!();
        }
        return new Response("unavailable", { status: 503 });
      },
      readable: new Map(),
      now: () => clock,
    };
  }

  const titleOf = (body: unknown) => (body as { embeds: { title: string }[] }).embeds[0].title;
  const titles = () => creates.map((c) => titleOf(c.body));
  /** Each edit as `messageId: title`. */
  const edited = () => edits.map((e) => `${e.messageId}: ${titleOf(e.body)}`);

  beforeEach(() => {
    listings = {};
    creates = [];
    failNext = 0;
    edits = [];
    failNextEdit = 0;
    deleted = new Set();
    removed = [];
    failNextDelete = 0;
    clock = SETTLED;
    errors = spyOn(console, "error").mockImplementation(() => {});
    logs = spyOn(console, "log").mockImplementation(() => {});
  });
  afterEach(() => {
    errors.mockRestore();
    logs.mockRestore();
  });

  test("a room is posted once it has settled, then never again", async () => {
    listings.preview = listed(game());
    const d = deps();
    clock = SETTLED - 1;
    await syncLobbyPosts(d);
    expect(creates).toEqual([]);

    clock = SETTLED;
    await syncLobbyPosts(d);
    expect(creates).toHaveLength(1);
    expect(creates[0].channelId).toBe(CHANNEL);
    expect(creates[0].nonce).toBe(`pAB12CD${CREATED_S}`);
    expect(creates[0].nonce.length).toBeLessThanOrEqual(25);
    expect(titles()).toEqual(["Standard · 1/2 · PREVIEW"]);
    expect(d.posts.has("preview", "AB12CD", CREATED_S)).toBe(true);

    clock += 60_000;
    await syncLobbyPosts(d);
    expect(creates).toHaveLength(1);
  });

  test("password, sandbox and draft rooms are never posted", async () => {
    listings.release = listed(
      game({ game_code: "PASSWD", has_password: true }),
      game({ game_code: "SANDBX", is_sandbox: true }),
      { ...game({ game_code: "DRAFT1" }), draft_metadata: { setCode: "MKM", draftKind: "Quick" } },
      game({ game_code: "PLAIN1" }),
    );
    await syncLobbyPosts(deps());
    expect(creates.map((c) => c.nonce)).toEqual([`rPLAIN1${CREATED_S}`]);
  });

  test("a room that is full when it settles is not posted", async () => {
    listings.release = listed(game({ current_players: 2, max_players: 2 }));
    await syncLobbyPosts(deps());
    expect(creates).toEqual([]);
  });

  test("the same code re-registered later is a new listing and gets a new post", async () => {
    const d = deps();
    listings.release = listed(game());
    await syncLobbyPosts(d);
    listings.release = listed(game({ created_at: CREATED_S + 600 }));
    clock = (CREATED_S + 600) * 1000 + LOBBY_SETTLE_MS;
    await syncLobbyPosts(d);
    expect(creates.map((c) => c.nonce)).toEqual([`rAB12CD${CREATED_S}`, `rAB12CD${CREATED_S + 600}`]);
    // The first listing left the lobby, so its post is removed.
    expect(removed).toEqual(["m-1"]);
    expect(edits).toEqual([]);
    expect(d.posts.open("release").map((p) => p.messageId)).toEqual(["m-2"]);
  });

  test("an unreadable build is logged once while it stays unreadable, and again on recovery", async () => {
    const d = deps();
    const unreadable = () => errors.mock.calls.filter((c: unknown[]) => String(c[0]).includes("release /games unreadable"));
    const recovered = () => logs.mock.calls.filter((c: unknown[]) => String(c[0]).includes("release /games readable again"));

    listings.release = listed(game());
    await syncLobbyPosts(d); // readable from the start: nothing to report
    expect(recovered()).toHaveLength(0);

    listings.release = () => Response.json(INFO_DOCUMENT);
    await syncLobbyPosts(d);
    listings.release = () => new Response("down", { status: 502 });
    await syncLobbyPosts(d);
    expect(unreadable()).toHaveLength(1);
    expect(recovered()).toHaveLength(0);

    listings.release = listed();
    await syncLobbyPosts(d);
    await syncLobbyPosts(d);
    expect(recovered()).toHaveLength(1);

    listings.release = () => Response.json(INFO_DOCUMENT);
    await syncLobbyPosts(d);
    expect(unreadable()).toHaveLength(2);
  });

  test("an unreadable build posts nothing while the other build still posts", async () => {
    listings.release = () => Response.json(INFO_DOCUMENT);
    listings.preview = listed(game());
    const d = deps();
    await syncLobbyPosts(d);
    expect(titles()).toEqual(["Standard · 1/2 · PREVIEW"]);
    expect(d.readable).toEqual(new Map<Build, boolean>([["release", false], ["preview", true]]));
  });

  test("a failed create writes no row and is retried on the next pass", async () => {
    listings.release = listed(game());
    const d = deps();
    failNext = 1;
    await syncLobbyPosts(d);
    expect(creates).toEqual([]);
    expect(d.posts.has("release", "AB12CD", CREATED_S)).toBe(false);

    await syncLobbyPosts(d);
    expect(creates.map((c) => c.nonce)).toEqual([`rAB12CD${CREATED_S}`]);
    expect(d.posts.has("release", "AB12CD", CREATED_S)).toBe(true);
  });

  test("a restart (a new store over the same file) does not repost", async () => {
    const dir = mkdtempSync(join(tmpdir(), "lobby-mirror-"));
    const path = join(dir, "lfg.sqlite");
    try {
      listings.release = listed(game());
      await syncLobbyPosts(deps(new LobbyPostStore(path)));
      expect(creates).toHaveLength(1);
      await syncLobbyPosts(deps(new LobbyPostStore(path)));
      expect(creates).toHaveLength(1);
    } finally {
      rmSync(dir, { recursive: true, force: true });
    }
  });

  describe("editing posted rooms", () => {
    test("an unchanged room is not edited", async () => {
      listings.release = listed(game());
      const d = deps();
      await syncLobbyPosts(d);
      clock += 60_000;
      await syncLobbyPosts(d);
      await syncLobbyPosts(d);
      expect(creates).toHaveLength(1);
      expect(edits).toEqual([]);
    });

    test("a seat change edits the post once; a room that fills loses its Join button", async () => {
      listings.release = listed(game({ max_players: 3 }));
      const d = deps();
      await syncLobbyPosts(d);

      listings.release = listed(game({ current_players: 2, max_players: 3 }));
      await syncLobbyPosts(d);
      await syncLobbyPosts(d);
      expect(edited()).toEqual(["m-1: Standard · 2/3 · RELEASE"]);
      expect(edits[0].channelId).toBe(CHANNEL);
      expect((edits[0].body as { components: unknown[] }).components).toHaveLength(1);

      listings.release = listed(game({ current_players: 3, max_players: 3 }));
      await syncLobbyPosts(d);
      expect(edited()).toEqual(["m-1: Standard · 2/3 · RELEASE", "m-1: Standard · 3/3 · RELEASE"]);
      expect((edits[1].body as { components: unknown[] }).components).toEqual([]);
    });

    test("a room that leaves the lobby has its post removed, once", async () => {
      listings.release = listed(game());
      const d = deps();
      await syncLobbyPosts(d);

      listings.release = listed();
      await syncLobbyPosts(d);
      await syncLobbyPosts(d);
      expect(removed).toEqual(["m-1"]);
      expect(edits).toEqual([]);
      expect(d.posts.open("release")).toEqual([]);
      expect(creates).toHaveLength(1);
    });

    test("a room still listed but no longer eligible has its post removed", async () => {
      listings.release = listed(game());
      const d = deps();
      await syncLobbyPosts(d);
      listings.release = listed(game({ has_password: true }));
      await syncLobbyPosts(d);
      expect(removed).toEqual(["m-1"]);
      expect(edits).toEqual([]);
    });

    test("a failed removal leaves the post open and is retried on the next pass", async () => {
      listings.release = listed(game());
      const d = deps();
      await syncLobbyPosts(d);

      listings.release = listed();
      failNextDelete = 1;
      await syncLobbyPosts(d);
      expect(removed).toEqual([]);
      expect(d.posts.open("release")).toHaveLength(1);

      await syncLobbyPosts(d);
      expect(removed).toEqual(["m-1"]);
      expect(d.posts.open("release")).toEqual([]);
    });

    test("a failed seat-change edit keeps the old seats and is retried on the next pass", async () => {
      listings.release = listed(game({ max_players: 3 }));
      const d = deps();
      await syncLobbyPosts(d);

      listings.release = listed(game({ current_players: 2, max_players: 3 }));
      failNextEdit = 1;
      await syncLobbyPosts(d);
      expect(edits).toEqual([]);
      expect(d.posts.open("release").map((p) => p.room.current)).toEqual([1]);

      await syncLobbyPosts(d);
      expect(edited()).toEqual(["m-1: Standard · 2/3 · RELEASE"]);
      expect(d.posts.open("release").map((p) => p.room.current)).toEqual([2]);
    });

    test("a deleted post is closed, and its still-listed room is neither edited nor reposted", async () => {
      listings.release = listed(game({ max_players: 3 }));
      const d = deps();
      await syncLobbyPosts(d);

      deleted.add("m-1");
      listings.release = listed(game({ current_players: 2, max_players: 3 }));
      await syncLobbyPosts(d);
      expect(edits).toHaveLength(1);
      expect(d.posts.open("release")).toEqual([]);

      listings.release = listed(game({ max_players: 3 }));
      await syncLobbyPosts(d);
      expect(edits).toHaveLength(1);
      expect(creates).toHaveLength(1);
    });

    test("an unreadable build's open posts are left as they are", async () => {
      listings.release = listed(game());
      const d = deps();
      await syncLobbyPosts(d);

      listings.release = () => Response.json(INFO_DOCUMENT);
      await syncLobbyPosts(d);
      expect(edits).toEqual([]);
      expect(d.posts.open("release")).toHaveLength(1);
    });
  });

  describe("pruning closed posts", () => {
    /** Posts the room, then closes its post at `clock` by listing `after`. */
    async function closeAt(d: LobbyMirrorDeps, after: () => Response) {
      listings.release = listed(game());
      await syncLobbyPosts(d);
      listings.release = after;
      await syncLobbyPosts(d);
      expect(d.posts.open("release")).toEqual([]);
    }

    test("a closed post's row is deleted after the retention period once its room is gone", async () => {
      const d = deps();
      await closeAt(d, listed());
      const closedAt = clock;

      clock = closedAt + CLOSED_POST_RETENTION_MS;
      await syncLobbyPosts(d);
      expect(d.posts.has("release", "AB12CD", CREATED_S)).toBe(true);

      clock = closedAt + CLOSED_POST_RETENTION_MS + 1;
      await syncLobbyPosts(d);
      expect(d.posts.has("release", "AB12CD", CREATED_S)).toBe(false);
    });

    test("a deleted post's row is kept while its room is still listed, so it is never reposted", async () => {
      const d = deps();
      deleted.add("m-1");
      listings.release = listed(game({ max_players: 3 }));
      await syncLobbyPosts(d);
      listings.release = listed(game({ current_players: 2, max_players: 3 }));
      await syncLobbyPosts(d);
      expect(d.posts.open("release")).toEqual([]);

      listings.release = listed(game({ max_players: 3 }));
      clock += 2 * CLOSED_POST_RETENTION_MS;
      await syncLobbyPosts(d);
      await syncLobbyPosts(d);
      expect(d.posts.has("release", "AB12CD", CREATED_S)).toBe(true);
      expect(creates).toHaveLength(1);
    });

    test("an unreadable build's closed rows are not pruned", async () => {
      const d = deps();
      await closeAt(d, listed());

      listings.release = () => Response.json(INFO_DOCUMENT);
      clock += 2 * CLOSED_POST_RETENTION_MS;
      await syncLobbyPosts(d);
      expect(d.posts.has("release", "AB12CD", CREATED_S)).toBe(true);
    });
  });
});
