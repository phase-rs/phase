import { readFileSync } from "node:fs";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import {
  DIRECTORY_BODY_KEYS,
  DIRECTORY_ROW_KEYS,
  DIRECTORY_VERSION,
  DIRECTORY_TTL_MS,
  MAX_DIRECTORY_LOBBY_SOURCES,
  WIRE_SCORE_KEYS,
  projectDirectoryBody,
  projectDirectoryRow,
  refreshServerDirectory,
  type DirectoryRow,
} from "../serverDirectory";
import {
  lobbySources,
  useMultiplayerStore,
  userLobbySource,
} from "../../stores/multiplayerStore";
import { SERVER_PRESETS, parseWebSocketUrl } from "../serverDetection";
import {
  LOBBY_PROTOCOL_VERSION,
  PROTOCOL_VERSION,
} from "../../adapter/ws-adapter";

const repoRoot = resolve(dirname(fileURLToPath(import.meta.url)), "../../../..");

function row(overrides: Partial<DirectoryRow> & { url: string }): DirectoryRow {
  return {
    name: "example",
    mode: "LobbyOnly",
    server_version: "0.71.0",
    protocol_version: PROTOCOL_VERSION,
    lobby_protocol_version: LOBBY_PROTOCOL_VERSION,
    current_players: 0,
    first_seen_ms: 1_700_000_000_000,
    last_seen_ms: 1_700_000_060_000,
    score: null,
    ...overrides,
  };
}

function body(servers: unknown[], directoryVersion = DIRECTORY_VERSION) {
  return { directory_version: directoryVersion, servers };
}

/** Stub `fetch` with a queue of responses, resolving each `GET` in order and
 * repeating the last one after the queue drains. */
function stubFetch(...responses: (
  | { ok: true; json: unknown }
  | { ok: false; status: number }
  | { reject: true }
)[]) {
  let index = 0;
  const mock = vi.fn(async () => {
    const next = responses[Math.min(index, responses.length - 1)];
    index += 1;
    if ("reject" in next) throw new TypeError("Failed to fetch");
    if (!next.ok) return { ok: false, status: next.status, json: async () => ({}) };
    return { ok: true, status: 200, json: async () => next.json };
  });
  vi.stubGlobal("fetch", mock);
  return mock;
}

function directoryUrls(): string[] {
  return lobbySources(useMultiplayerStore.getState())
    .filter((source) => source.origin === "directory")
    .map((source) => source.url);
}

describe("serverDirectory", () => {
  beforeEach(() => {
    useMultiplayerStore.setState({
      userLobbySources: [],
      sourceStatus: new Map(),
      directorySources: [],
      directoryFetchedAtMs: null,
      disabledDirectorySources: [],
    });
  });

  afterEach(() => {
    vi.unstubAllGlobals();
  });

  // ── U11 ─────────────────────────────────────────────────────────────────

  // V-U11a
  it("keeps the last good directory when a refresh fails", async () => {
    const fetchMock = stubFetch({ reject: true });
    await refreshServerDirectory();
    // A directory that has never been read lists nothing — presets and
    // hand-added sources only.
    expect(directoryUrls()).toEqual([]);
    expect(useMultiplayerStore.getState().directoryFetchedAtMs).toBeNull();

    // Reach-guard: the projection DOES run, so the failure below is measuring
    // last-good rather than a projection that never worked.
    fetchMock.mockImplementation(
      async () =>
        ({ ok: true, status: 200, json: async () => body([row({ url: "wss://a.example/ws" })]) }) as never,
    );
    await refreshServerDirectory();
    expect(directoryUrls()).toEqual(["wss://a.example/ws"]);

    useMultiplayerStore.setState({ directoryFetchedAtMs: Date.now() - DIRECTORY_TTL_MS - 1 });
    fetchMock.mockImplementation(async () => {
      throw new TypeError("Failed to fetch");
    });
    await refreshServerDirectory();
    expect(directoryUrls()).toHaveLength(1);
  });

  // V-U11b
  it("merges each row as a directory source carrying score.value and the row's mode", async () => {
    stubFetch({
      ok: true,
      json: body([
        row({
          url: "wss://scored.example/ws",
          mode: "Full",
          score: {
            value: 72,
            samples: 40,
            success_rate: 0.9,
            completion_rate: 0.8,
            median_rtt_ms: 60,
          },
        }),
        row({
          url: "wss://thin.example/ws",
          score: {
            value: null,
            samples: 3,
            success_rate: 1,
            completion_rate: 1,
            median_rtt_ms: null,
          },
        }),
        row({ url: "wss://silent.example/ws", score: null }),
      ]),
    });
    await refreshServerDirectory();

    const sources = lobbySources(useMultiplayerStore.getState()).filter(
      (s) => s.origin === "directory",
    );
    expect(sources.map((s) => [s.url, s.score])).toEqual([
      ["wss://scored.example/ws", 72],
      ["wss://thin.example/ws", undefined],
      ["wss://silent.example/ws", undefined],
    ]);
    expect(sources.map((s) => s.kind)).toEqual(["Full", "LobbyOnly", "LobbyOnly"]);

    // "No evidence" and "too little evidence" are different things, and the
    // WireScore object is retained whole for the consumer that tells them
    // apart — but it is never what `LobbySource.score` holds.
    const entries = useMultiplayerStore.getState().directorySources;
    expect(entries.map((e) => e.row.score?.samples ?? null)).toEqual([40, 3, null]);
    expect(entries[1].row.score).toEqual({
      value: null,
      samples: 3,
      success_rate: 1,
      completion_rate: 1,
      median_rtt_ms: null,
    });
  });

  // V-U11c
  it("ignores a body whose directory_version is not this client's", async () => {
    const fetchMock = stubFetch({ ok: true, json: body([row({ url: "wss://a.example/ws" })]) });
    await refreshServerDirectory();
    const afterFirst = useMultiplayerStore.getState().directorySources;
    expect(afterFirst.map((e) => e.source.url)).toEqual(["wss://a.example/ws"]);

    // Age the timestamp or the second refresh short-circuits on the TTL and
    // never reaches the envelope check at all.
    useMultiplayerStore.setState({ directoryFetchedAtMs: Date.now() - DIRECTORY_TTL_MS - 1 });
    fetchMock.mockImplementation(
      async () =>
        ({
          ok: true,
          status: 200,
          json: async () =>
            body([row({ url: "wss://b.example/ws" })], DIRECTORY_VERSION + 1),
        }) as never,
    );
    await refreshServerDirectory();
    // Reach-guard: the second read really happened.
    expect(fetchMock).toHaveBeenCalledTimes(2);
    expect(useMultiplayerStore.getState().directorySources).toEqual(afterFirst);

    // Paired positive: the same second body at the matching version DOES
    // replace the list, so the assertion above is about the version and not
    // about the second read being inert.
    useMultiplayerStore.setState({ directoryFetchedAtMs: Date.now() - DIRECTORY_TTL_MS - 1 });
    fetchMock.mockImplementation(
      async () =>
        ({
          ok: true,
          status: 200,
          json: async () => body([row({ url: "wss://b.example/ws" })]),
        }) as never,
    );
    await refreshServerDirectory();
    expect(directoryUrls()).toEqual(["wss://b.example/ws"]);
  });

  // V-U11d
  it("suppresses a second read within the TTL and re-reads after it expires", async () => {
    const fetchMock = stubFetch({ ok: true, json: body([row({ url: "wss://a.example/ws" })]) });
    await refreshServerDirectory();
    await refreshServerDirectory();
    expect(fetchMock).toHaveBeenCalledTimes(1);

    useMultiplayerStore.setState({ directoryFetchedAtMs: Date.now() - DIRECTORY_TTL_MS - 1 });
    await refreshServerDirectory();
    expect(fetchMock).toHaveBeenCalledTimes(2);
  });

  // V-U11e
  it("drops malformed rows individually but rejects a malformed envelope whole", () => {
    const projected = projectDirectoryBody(
      body([
        row({ url: "wss://good.example/ws" }),
        { ...row({ url: "wss://x.example/ws" }), url: "not a url" },
        row({ url: "ws://plain.example/ws" }),
        (() => {
          const bad = row({ url: "wss://nolobby.example/ws" }) as Partial<DirectoryRow>;
          delete bad.lobby_protocol_version;
          return bad;
        })(),
        null,
        { ...row({ url: "wss://shortscore.example/ws" }), score: { value: 1 } },
      ]),
    );
    // The surviving valid row in the SAME body is the paired positive: the
    // drops were per-row, not a whole-body reject.
    expect(projected?.map((e) => e.source.url)).toEqual(["wss://good.example/ws"]);

    expect(projectDirectoryBody({ directory_version: DIRECTORY_VERSION, servers: "nope" })).toBeNull();
    expect(projectDirectoryBody(null)).toBeNull();
    expect(projectDirectoryBody({ servers: [] })).toBeNull();
  });

  // V-U11f
  it("bounds the dialed set and keeps the best-scored rows", () => {
    const many = Array.from({ length: 20 }, (_, i) =>
      row({
        url: `wss://s${i}.example/ws`,
        score: {
          value: 100 - i,
          samples: 50,
          success_rate: 1,
          completion_rate: 1,
          median_rtt_ms: 10,
        },
      }),
    );
    const projected = projectDirectoryBody(body(many));
    expect(projected).toHaveLength(MAX_DIRECTORY_LOBBY_SOURCES);
    expect(projected?.map((e) => e.source.score)).toEqual([100, 99, 98, 97, 96, 95, 94, 93]);

    // Paired: a short body is not truncated, so the cap is a bound and not an
    // unconditional slice.
    expect(projectDirectoryBody(body(many.slice(0, 3)))).toHaveLength(3);
  });

  // V-U11g
  it("collapses the announced key onto one client source, whoever else claims it", () => {
    // (i) A pathless authority announces without a trailing slash and
    // canonicalises with one. Both spellings are kept, and they differ.
    const pathless = projectDirectoryRow(row({ url: "wss://a.example" }));
    expect(pathless?.source.url).toBe("wss://a.example/");
    expect(pathless?.row.url).toBe("wss://a.example");

    // (ii) User collision: a hand-added entry outranks a transient listing.
    useMultiplayerStore.setState({
      directorySources: projectDirectoryBody(body([row({ url: "wss://a.example" })]))!,
      userLobbySources: [userLobbySource("wss://a.example/")!],
    });
    const forHost = lobbySources(useMultiplayerStore.getState()).filter(
      (s) => s.url === "wss://a.example/",
    );
    expect(forHost).toHaveLength(1);
    expect(forHost[0].origin).toBe("user");

    // (iii) Preset collision: the preset wins and the directory contributes
    // no second row for it.
    const presetUrl = SERVER_PRESETS[0].url;
    useMultiplayerStore.setState({
      directorySources: projectDirectoryBody(body([row({ url: presetUrl })]))!,
      userLobbySources: [],
    });
    const presetRow = useMultiplayerStore.getState().directorySources[0];
    // States which spelling is being matched, rather than relying on the two
    // coinciding under today's defines.
    expect(SERVER_PRESETS.map((p) => parseWebSocketUrl(p.url)?.href)).toContain(
      presetRow.source.url,
    );
    const merged = lobbySources(useMultiplayerStore.getState());
    expect(merged.filter((s) => s.url === presetUrl)).toHaveLength(1);
    expect(merged.filter((s) => s.url === presetUrl)[0].origin).toBe("official");
    expect(merged.filter((s) => s.origin === "directory")).toEqual([]);

    // Paired control for both collisions: with nobody else claiming it, the
    // same row DOES yield one directory entry — so above it is the dedupe
    // collapsing, not the projection failing.
    useMultiplayerStore.setState({
      directorySources: projectDirectoryBody(body([row({ url: "wss://a.example" })]))!,
      userLobbySources: [],
    });
    expect(directoryUrls()).toEqual(["wss://a.example/"]);
  });

  // ── The mirror gate: this client's duplicate declaration vs the Worker's ──

  const workerDirectory = readFileSync(
    resolve(repoRoot, "lobby-worker/src/directory.ts"),
    "utf8",
  );
  const workerLobbyDo = readFileSync(
    resolve(repoRoot, "lobby-worker/src/lobby-do.ts"),
    "utf8",
  );

  /** Field names declared directly in one interface body. Stops at the first
   * column-0 `}`, so it reads exactly one declaration. */
  function interfaceFields(source: string, name: string): string[] {
    const start = source.match(new RegExp(`export interface ${name}[^{]*\\{`));
    if (!start?.index) return [];
    const bodyStart = start.index + start[0].length;
    const end = source.indexOf("\n}", bodyStart);
    if (end === -1) return [];
    return [...source.slice(bodyStart, end).matchAll(/^ {2}(\w+)\??:/gm)].map((m) => m[1]);
  }

  /** Column names of one `CREATE TABLE IF NOT EXISTS` statement. */
  function ddlColumns(source: string, table: string): string[] {
    const start = source.indexOf(`CREATE TABLE IF NOT EXISTS ${table} (`);
    if (start === -1) return [];
    const end = source.indexOf(")", start);
    if (end === -1) return [];
    return [
      ...source.slice(start, end).matchAll(/^\s+(\w+)\s+(?:TEXT|INTEGER|REAL|BLOB)/gm),
    ].map((m) => m[1]);
  }

  // V-M0 — guard the guard. An extractor that silently returned [] would make
  // every comparison below pass over nothing.
  it("extracts non-empty declarations from the Worker's source", () => {
    expect(interfaceFields(workerDirectory, "StoredServerRow")).toContain("url");
    expect(interfaceFields(workerDirectory, "DirectoryRow").length).toBeGreaterThan(0);
    expect(interfaceFields(workerDirectory, "WireScore").length).toBeGreaterThan(0);
    expect(interfaceFields(workerDirectory, "DirectoryBody").length).toBeGreaterThan(0);
    expect(ddlColumns(workerLobbyDo, "servers")).toContain("url");
  });

  // V-M1 — the client declares DirectoryRow flat; the Worker splits it across
  // `StoredServerRow` and `DirectoryRow extends StoredServerRow`, so the union
  // is explicit rather than assumed to live in one body.
  it("mirrors DirectoryRow field-for-field", () => {
    expect(new Set(DIRECTORY_ROW_KEYS)).toEqual(
      new Set([
        ...interfaceFields(workerDirectory, "StoredServerRow"),
        ...interfaceFields(workerDirectory, "DirectoryRow"),
      ]),
    );
  });

  // V-M2
  it("mirrors WireScore field-for-field", () => {
    expect(new Set(WIRE_SCORE_KEYS)).toEqual(
      new Set(interfaceFields(workerDirectory, "WireScore")),
    );
  });

  // V-M3 — the DO's `SELECT *` into `StoredServerRow` is typed, not
  // runtime-checked; this is the only gate on that projection.
  it("keeps the servers DDL and StoredServerRow in agreement", () => {
    expect(new Set(ddlColumns(workerLobbyDo, "servers"))).toEqual(
      new Set(interfaceFields(workerDirectory, "StoredServerRow")),
    );
  });

  // V-M4
  it("mirrors the DirectoryBody envelope field-for-field", () => {
    expect(new Set(DIRECTORY_BODY_KEYS)).toEqual(
      new Set(interfaceFields(workerDirectory, "DirectoryBody")),
    );
  });
});
