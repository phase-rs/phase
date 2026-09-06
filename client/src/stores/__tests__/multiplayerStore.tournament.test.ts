import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

const localStorageMock = vi.hoisted(() => {
  const items = new Map<string, string>();
  const setItem = vi.fn((key: string, value: string) => {
    items.set(key, value);
  });
  Object.defineProperty(globalThis, "localStorage", {
    configurable: true,
    value: {
      getItem: (key: string) => items.get(key) ?? null,
      setItem,
      removeItem: (key: string) => {
        items.delete(key);
      },
      clear: () => {
        items.clear();
      },
      key: (index: number) => [...items.keys()][index] ?? null,
      get length() {
        return items.size;
      },
    },
  });
  return { items, setItem };
});

import {
  MAX_TOURNAMENT_CREDENTIALS,
  rememberTournamentCredential,
  useMultiplayerStore,
  findLobbyGameByCode,
  type TournamentCredential,
} from "../multiplayerStore";
import { openPhaseSocket, withReconnect } from "../../services/openPhaseSocket";
import { SERVER_PRESETS } from "../../services/serverDetection";
import { LOBBY_PROTOCOL_VERSION } from "../../adapter/ws-adapter";
import type {
  TournamentSummary,
  TournamentView,
} from "../../adapter/types";

// Deliberately NOT mocked, and this is load-bearing rather than incidental:
//
//  - `../../services/brokerClient` owns the only `SubscribeLobby` /
//    `UnsubscribeLobby` `ws.send` calls in the client. `multiplayerStore.test.ts`
//    stubs that module down to `openBrokerClient` alone; under that stub
//    `subscribeLobbyOver` would not exist and every frame assertion in this file
//    would be vacuous.
//  - `../../services/tournamentClient` owns the request frames, the reply
//    correlation and the broadcast fan-out this suite is here to exercise.
//
// Precedent that the store's real import graph resolves under vitest with
// nothing stubbed: `multiplayerStore.visualAvatars.test.ts` has zero `vi.mock`
// calls. Only the socket *transport* is faked, at the `openPhaseSocket` seam.
vi.mock("../../services/openPhaseSocket", () => ({
  HandshakeError: class HandshakeError extends Error {
    kind: string;

    constructor(message: string, kind: string) {
      super(message);
      this.kind = kind;
    }
  },
  openPhaseSocket: vi.fn(),
  withReconnect: vi.fn(),
}));

// ── Harness ──────────────────────────────────────────────────────────────

type Listener = (event: unknown) => void;

function makeFakeSocket() {
  const listeners = new Map<string, Set<Listener>>();
  const send = vi.fn();
  const wsClose = vi.fn();
  const socketClose = vi.fn();
  const ws = {
    readyState: 1, // WebSocket.OPEN
    send,
    close: wsClose,
    addEventListener: vi.fn((type: string, fn: Listener) => {
      let bucket = listeners.get(type);
      if (!bucket) {
        bucket = new Set();
        listeners.set(type, bucket);
      }
      bucket.add(fn);
    }),
    removeEventListener: vi.fn((type: string, fn: Listener) => {
      listeners.get(type)?.delete(fn);
    }),
  };
  return {
    socket: {
      serverInfo: {
        version: "test",
        buildCommit: "test",
        mode: "LobbyOnly" as const,
        protocolVersion: 14,
        // Models a current-generation broker, so it tracks the client's own
        // export rather than a literal. Load-bearing: `tournamentClient`'s
        // capability gate treats an absent lobby version as unsupported, so
        // omitting this would make every gated store action in this file settle
        // `{ok:false, reason:"unsupported"}` the instant it sent — quietly
        // gutting the round-trip assertions instead of failing them.
        lobbyProtocolVersion: LOBBY_PROTOCOL_VERSION,
      },
      ws,
      close: socketClose,
    },
    ws,
    send,
    wsClose,
    socketClose,
    listenerCount: (type = "message") => listeners.get(type)?.size ?? 0,
    deliver: (type: string, data?: unknown) => {
      for (const fn of [...(listeners.get("message") ?? [])]) {
        fn({ data: JSON.stringify({ type, data }) });
      }
    },
    /**
     * Frame-tag tally over `send`, by EXACT PARSED TAG EQUALITY — not a regex.
     * `UnsubscribeLobby` has a lowercase `s`, so a naive
     * `(?:Un)?SubscribeLobby` pattern would miss it entirely. Equality on the
     * parsed `.type` cannot suffer that class of bug.
     */
    tally: (tag: string) =>
      send.mock.calls.filter(
        ([raw]) => (JSON.parse(raw as string) as { type: string }).type === tag,
      ).length,
    /** The parsed payload of the nth frame carrying `tag`. */
    frame: (tag: string, nth = 0) =>
      send.mock.calls
        .map(([raw]) => JSON.parse(raw as string) as { type: string; data?: unknown })
        .filter((f) => f.type === tag)[nth],
  };
}

/**
 * The correlator the wire layer minted for the nth `tag` frame this socket
 * sent.
 *
 * The four token-gated actions settle only on a `TournamentActionAck` /
 * `TournamentActionRejected` echoing this id, so a test that wants to settle
 * one has to read the id off the frame that actually went out. Asserting the
 * type here keeps that non-vacuous: a request that stopped carrying a
 * correlator fails here rather than settling on an `undefined === undefined`
 * match.
 */
function correlatorOf(fake: FakeSocket, tag: string, nth = 0): number {
  const data = fake.frame(tag, nth)?.data as { request_id?: number } | undefined;
  expect(typeof data?.request_id).toBe("number");
  return data?.request_id as number;
}

type FakeSocket = ReturnType<typeof makeFakeSocket>;

let driver: {
  setCurrent: (socket: unknown) => void;
  fire: (state: "open" | "reconnecting" | "offline") => void;
} | null = null;

/** Wires the transport mocks so `ensureSubscriptionSocket` resolves `fake`. */
function primeSocket(fake: FakeSocket): void {
  vi.mocked(openPhaseSocket).mockResolvedValue(
    fake.socket as unknown as Awaited<ReturnType<typeof openPhaseSocket>>,
  );
  vi.mocked(withReconnect).mockImplementation((factory, opts) => {
    let current: Awaited<ReturnType<typeof factory>> | null = null;
    // The real implementation notifies from an async continuation, after it
    // has returned the handle the store stores. Reproduce that ordering —
    // notifying synchronously would find no handle to read `current()` from.
    void (async () => {
      current = await factory(0);
      opts?.onStateChange?.("open");
    })();
    driver = {
      setCurrent: (socket) => {
        current = socket as Awaited<ReturnType<typeof factory>>;
      },
      fire: (state) => {
        opts?.onStateChange?.(state);
      },
    };
    return { current: () => current, close: vi.fn() };
  });
}

/** Lets pending microtasks and the store's async continuations settle. */
const flush = () => new Promise<void>((resolve) => setTimeout(resolve, 0));

function makeHandlers() {
  return {
    onListUpdate: vi.fn(),
    onTournamentUpdate: vi.fn(),
    onTournamentRemoved: vi.fn(),
  };
}

function summaryFor(code: string): TournamentSummary {
  return {
    code,
    name: `Event ${code}`,
    arity: 2,
    bracket: "Swiss",
    status: "Registration",
    player_count: 0,
    current_round: 0,
    total_rounds: 3,
    created_at: 0,
  };
}

function viewFor(code: string): TournamentView {
  return {
    summary: summaryFor(code),
    players: [],
    pairings: [],
    standings: [],
  };
}

const store = () => useMultiplayerStore.getState();

/** Codes `prefix + zero-padded n`, for `from..=to`. */
function paddedCodes(from: number, to: number): string[] {
  const out: string[] = [];
  for (let n = from; n <= to; n += 1) out.push(`T${String(n).padStart(2, "0")}`);
  return out;
}

function heldMap(
  codes: string[],
  updatedAt: (code: string, index: number) => number,
): Record<string, TournamentCredential> {
  const map: Record<string, TournamentCredential> = {};
  codes.forEach((code, index) => {
    map[code] = { organizerToken: `org-${code}`, updatedAt: updatedAt(code, index) };
  });
  return map;
}

beforeEach(() => {
  vi.clearAllMocks();
  vi.mocked(openPhaseSocket).mockReset();
  vi.mocked(withReconnect).mockReset();
  driver = null;
  // Module-level subscription state (subscriber sets, attach handles, cached
  // snapshots) lives outside the store, so `setState` alone cannot reset it.
  store().closeSubscriptionSocket();
  localStorageMock.items.clear();
  useMultiplayerStore.setState({
    tournamentCredentials: {},
    // The hosting server IS the one browsed source here, so the single fake
    // socket backs both the lobby channel and the tournament channel — see
    // `tournamentBroadcastUrl`. Pointing them at different URLs would open two
    // channels onto one fake and double every frame tally.
    hostingServer: SERVER_PRESETS[0].url,
    // Without these four a source added or listed by an earlier case leaks
    // into every following one, and `subscribeLobby` dials it too.
    userLobbySources: [],
    sourceStatus: new Map(),
    directorySources: [],
    disabledDirectorySources: [],
    displayName: "",
  });
});

afterEach(() => {
  store().closeSubscriptionSocket();
});

// ── A. tournament credentials (R1, R2, R6, R10, R11, R11b) ───────────────

describe("tournament credentials", () => {
  it("persists tournament credentials across a rehydrate", async () => {
    useMultiplayerStore.setState({
      tournamentCredentials: {
        AAA: { organizerToken: "org-a", updatedAt: 10 },
        BBB: { playerToken: "ply-b", playerKey: "key-b", updatedAt: 20 },
      },
    });

    const raw = localStorage.getItem("phase-multiplayer");
    expect(raw).not.toBeNull();
    const blob = JSON.parse(raw as string) as { state: Record<string, unknown> };
    expect(blob.state.tournamentCredentials).toEqual({
      AAA: { organizerToken: "org-a", updatedAt: 10 },
      BBB: { playerToken: "ply-b", playerKey: "key-b", updatedAt: 20 },
    });
    // Positive reach-guard: the credentials rode the SAME partition as the
    // other persisted keys, so this is not a coincidentally-present blob.
    expect(blob.state.playerId).toBe(store().playerId);

    // Wipe in memory, restore the blob (the wipe re-persisted an empty map),
    // and hydrate through `merge` + the normalizer.
    useMultiplayerStore.setState({ tournamentCredentials: {} });
    localStorage.setItem("phase-multiplayer", raw as string);
    await useMultiplayerStore.persist.rehydrate();

    expect(store().tournamentCredentials).toEqual({
      AAA: { organizerToken: "org-a", updatedAt: 10 },
      BBB: { playerToken: "ply-b", playerKey: "key-b", updatedAt: 20 },
    });
  });

  it("hydrates a pre-phase-2 blob with an empty credential map", async () => {
    localStorage.setItem(
      "phase-multiplayer",
      JSON.stringify({
        state: {
          playerId: "legacy-player",
          displayName: "Legacy",
          serverAddress: "ws://localhost:8787",
          lastHostConfig: null,
        },
        version: 5,
      }),
    );

    await useMultiplayerStore.persist.rehydrate();

    // `{}`, not `undefined`: a blob written before this key existed must still
    // give every consumer an indexable map.
    expect(store().tournamentCredentials).toEqual({});
    expect(store().displayName).toBe("Legacy");
  });

  it("drops malformed persisted credentials and enforces the cap on hydrate", async () => {
    const hydrateWith = async (credentials: unknown) => {
      localStorage.setItem(
        "phase-multiplayer",
        JSON.stringify({
          state: {
            playerId: "p",
            serverAddress: "ws://localhost:8787",
            tournamentCredentials: credentials,
          },
          version: 5,
        }),
      );
      await useMultiplayerStore.persist.rehydrate();
      return store().tournamentCredentials;
    };

    // Part 1 — malformed entries, well under the cap so eviction cannot be
    // confused with rejection.
    const cleaned = await hydrateWith({
      AAA: { organizerToken: "t", updatedAt: 5 },
      BBB: { playerToken: 42 }, // non-string token, no other authority
      CCC: "not-an-object",
      DDD: { playerKey: "only-a-key", updatedAt: 1 }, // no authority at all
      EEE: { playerToken: "p", playerKey: "k", updatedAt: "nope" },
    });

    // Positive reach-guard: a normalizer that returned `{}` unconditionally
    // would fail this line.
    expect(cleaned.AAA).toEqual({ organizerToken: "t", updatedAt: 5 });
    // A non-numeric `updatedAt` degrades to 0 rather than poisoning the sort.
    expect(cleaned.EEE).toEqual({ playerToken: "p", playerKey: "k", updatedAt: 0 });
    expect("BBB" in cleaned).toBe(false);
    expect("CCC" in cleaned).toBe(false);
    expect("DDD" in cleaned).toBe(false);
    expect(Object.keys(cleaned)).toHaveLength(2);

    // Part 2 — a blob written by a build with a larger cap is trimmed on
    // hydrate, oldest-first.
    const overflowing: Record<string, unknown> = {};
    paddedCodes(1, 40).forEach((code, index) => {
      overflowing[code] = { organizerToken: `org-${code}`, updatedAt: 1000 + index };
    });
    const capped = await hydrateWith(overflowing);

    expect(Object.keys(capped)).toHaveLength(MAX_TOURNAMENT_CREDENTIALS);
    expect("T40" in capped).toBe(true); // newest survives
    expect("T01" in capped).toBe(false); // oldest evicted
  });

  it("merges a later join into an existing organizer credential for the same code", () => {
    // Create-then-join.
    const created = rememberTournamentCredential(
      {},
      "AAA",
      { organizerToken: "org-a" },
      100,
    );
    const joined = rememberTournamentCredential(
      created,
      "AAA",
      { playerToken: "ply-a", playerKey: "key-a" },
      200,
    );
    expect(Object.keys(joined)).toEqual(["AAA"]);
    expect(joined.AAA).toEqual({
      organizerToken: "org-a",
      playerToken: "ply-a",
      playerKey: "key-a",
      updatedAt: 200,
    });

    // Join-then-create — the reverse order must accumulate identically.
    const joinedFirst = rememberTournamentCredential(
      {},
      "AAA",
      { playerToken: "ply-a", playerKey: "key-a" },
      100,
    );
    const createdSecond = rememberTournamentCredential(
      joinedFirst,
      "AAA",
      { organizerToken: "org-a" },
      200,
    );
    expect(createdSecond.AAA).toEqual({
      organizerToken: "org-a",
      playerToken: "ply-a",
      playerKey: "key-a",
      updatedAt: 200,
    });
  });

  it("evicts the least-recently-written credential and never the newest", () => {
    const FROZEN = 1_700_000_000_000;
    // 32 held codes "T02".."T33", every entry on ONE frozen timestamp, then a
    // 33rd write for "T01" — which sorts lexicographically BEFORE all of them.
    // That inversion is the point: with `protect` the victim is "T02" (the
    // lexicographic minimum of the eligible set); without it "T01" is the
    // minimum of all 33 and evicts itself. Both asserted halves flip.
    const existing = heldMap(paddedCodes(2, 33), () => FROZEN);
    expect(Object.keys(existing)).toHaveLength(MAX_TOURNAMENT_CREDENTIALS);

    const result = rememberTournamentCredential(
      existing,
      "T01",
      { organizerToken: "org-T01" },
      FROZEN,
    );

    expect(result.T01).toEqual({ organizerToken: "org-T01", updatedAt: FROZEN });
    expect("T02" in result).toBe(false);
    // Named survivor: an implementation evicting more than `overflow` fails here.
    expect("T03" in result).toBe(true);
    expect(Object.keys(result)).toHaveLength(MAX_TOURNAMENT_CREDENTIALS);
  });

  it("evicts by write time even when the tournament codes sort the other way", () => {
    const BASE = 1_700_000_000_000;
    // The ONLY case with distinct timestamps. "TNN" is written at
    // BASE + (33 - NN), so "T01" is the NEWEST and "T32" the OLDEST — exactly
    // inverting lexicographic order. With the `updatedAt` term the victim is
    // "T32"; sorting by code alone would take "T01" instead.
    const codes = paddedCodes(1, 32);
    const existing = heldMap(codes, (code) => BASE + (33 - Number(code.slice(1))));
    expect(Object.keys(existing)).toHaveLength(MAX_TOURNAMENT_CREDENTIALS);

    const result = rememberTournamentCredential(
      existing,
      "T99",
      { organizerToken: "org-T99" },
      BASE + 100,
    );

    expect("T32" in result).toBe(false);
    expect("T01" in result).toBe(true);
    expect(result.T99).toEqual({ organizerToken: "org-T99", updatedAt: BASE + 100 });
    expect(Object.keys(result)).toHaveLength(MAX_TOURNAMENT_CREDENTIALS);
  });

  it("orders eviction deterministically for all-digit tournament codes", () => {
    const FROZEN = 1_700_000_000_000;
    // Unpadded CANONICAL ARRAY INDEX codes "9".."40" (32 of them). JS
    // enumerates such keys in ascending NUMERIC order ahead of insertion
    // order, so `Object.keys` yields "8","9","10",…,"40" while the
    // `(updatedAt, code)` sort yields the lexicographic minimum "10".
    // Zero-padded codes ("0001") do not round-trip `ToString(ToUint32(k))`
    // and so could not exhibit this hazard at all.
    const digits: string[] = [];
    for (let n = 9; n <= 40; n += 1) digits.push(String(n));
    const existing = heldMap(digits, () => FROZEN);
    expect(Object.keys(existing)).toHaveLength(MAX_TOURNAMENT_CREDENTIALS);

    const result = rememberTournamentCredential(
      existing,
      "8",
      { organizerToken: "org-8" },
      FROZEN,
    );

    expect("10" in result).toBe(false); // the sorted victim
    expect("9" in result).toBe(true); // the key-order victim, which must survive
    expect(result["8"]).toEqual({ organizerToken: "org-8", updatedAt: FROZEN });
    expect(Object.keys(result)).toHaveLength(MAX_TOURNAMENT_CREDENTIALS);
  });

  it("seeds a late tournament subscriber with the pre-removal list after a TournamentRemoved", async () => {
    const fake = makeFakeSocket();
    primeSocket(fake);
    useMultiplayerStore.setState({
      tournamentCredentials: { BBB: { organizerToken: "org-b", updatedAt: 1 } },
    });

    const first = makeHandlers();
    await store().subscribeTournaments(first);
    fake.deliver("TournamentListUpdate", {
      tournaments: [summaryFor("AAA"), summaryFor("BBB")],
    });

    // Anchor on the array the STORE actually observed, not on the literal this
    // test constructed: `deliver` stringifies and the client re-parses, so the
    // two are never the same object.
    const observed = first.onListUpdate.mock.calls[0][0] as TournamentSummary[];
    expect(observed.map((t) => t.code)).toEqual(["AAA", "BBB"]);

    fake.deliver("TournamentRemoved", { code: "BBB" });

    // Reach-guards: the frame genuinely reached the fan-out, so the
    // "snapshot unchanged" assertion below cannot pass vacuously.
    expect(first.onTournamentRemoved).toHaveBeenCalledWith("BBB");
    expect("BBB" in store().tournamentCredentials).toBe(false);

    const late = makeHandlers();
    await store().subscribeTournaments(late);
    expect(late.onListUpdate).toHaveBeenCalledTimes(1);
    const seeded = late.onListUpdate.mock.calls[0][0] as TournamentSummary[];
    // Identity against the observed array: folding the removal into the cache
    // would produce a new, shorter array and break both halves.
    expect(seeded).toBe(observed);
    expect(seeded.map((t) => t.code)).toEqual(["AAA", "BBB"]);

    // Only a subsequent list push may replace the snapshot — assert that too.
    fake.deliver("TournamentListUpdate", { tournaments: [summaryFor("AAA")] });
    const third = makeHandlers();
    await store().subscribeTournaments(third);
    expect(
      (third.onListUpdate.mock.calls[0][0] as TournamentSummary[]).map((t) => t.code),
    ).toEqual(["AAA"]);
  });

  it("forgets credentials when the broker removes the tournament", async () => {
    const fake = makeFakeSocket();
    primeSocket(fake);
    useMultiplayerStore.setState({
      tournamentCredentials: {
        AAA: { organizerToken: "org-a", updatedAt: 1 },
        BBB: { playerToken: "ply-b", updatedAt: 2 },
      },
    });

    const handlers = makeHandlers();
    await store().subscribeTournaments(handlers);
    fake.deliver("TournamentRemoved", { code: "AAA" });

    expect(handlers.onTournamentRemoved).toHaveBeenCalledWith("AAA");
    expect("AAA" in store().tournamentCredentials).toBe(false);
    // The unrelated code is untouched.
    expect(store().tournamentCredentials.BBB).toEqual({
      playerToken: "ply-b",
      updatedAt: 2,
    });
  });

  it("leaves credentials untouched for a TournamentRemoved it holds nothing for", async () => {
    const fake = makeFakeSocket();
    primeSocket(fake);
    useMultiplayerStore.setState({
      tournamentCredentials: { AAA: { organizerToken: "org-a", updatedAt: 1 } },
    });

    const handlers = makeHandlers();
    await store().subscribeTournaments(handlers);

    const before = store().tournamentCredentials;
    const writesBefore = localStorageMock.setItem.mock.calls.length;

    fake.deliver("TournamentRemoved", { code: "ZZZ" });

    // Reach-guard: the frame really did reach the fan-out.
    expect(handlers.onTournamentRemoved).toHaveBeenCalledWith("ZZZ");
    expect(store().tournamentCredentials).toBe(before);
    // The discriminating half: returning `{}` from inside the zustand updater
    // keeps the reference but still runs `set`, and `persist` still writes.
    expect(localStorageMock.setItem.mock.calls.length).toBe(writesBefore);
  });

  it("files a created tournament's organizer token under the reply's code", async () => {
    const fake = makeFakeSocket();
    primeSocket(fake);

    const pending = store().createTournament({
      name: "Friday Night",
      arity: 2,
      scoring: { win_points: 3, draw_points: 1, loss_points: 0 },
      bracket: "Swiss",
    });
    await flush();
    expect(fake.tally("CreateTournament")).toBe(1);

    // The broker mints the code; the caller never chose one.
    fake.deliver("TournamentCreated", {
      code: "ZZZ",
      organizer_token: "org-zzz",
      view: viewFor("ZZZ"),
    });
    const result = await pending;

    expect(result.ok).toBe(true);
    expect(store().tournamentCredentials.ZZZ?.organizerToken).toBe("org-zzz");
    expect(Object.keys(store().tournamentCredentials)).toEqual(["ZZZ"]);
  });

  it("files a join's player token and the player key it actually sent", async () => {
    const fake = makeFakeSocket();
    primeSocket(fake);

    const pending = store().joinTournament("AAA", "Rajah");
    await flush();
    const sent = fake.frame("JoinTournament")?.data as {
      code: string;
      player_key: string;
      display_name: string;
    };
    expect(sent.code).toBe("AAA");
    expect(sent.display_name).toBe("Rajah");

    fake.deliver("TournamentJoined", {
      code: "AAA",
      player_token: "ply-a",
      view: viewFor("AAA"),
    });
    const result = await pending;

    expect(result.ok).toBe(true);
    expect(store().tournamentCredentials.AAA).toEqual({
      playerToken: "ply-a",
      playerKey: sent.player_key,
      updatedAt: expect.any(Number) as unknown as number,
    });
    expect(sent.player_key).toBe(store().playerId);
  });
});

// ── B. unified SubscribeLobby refcount (R3, R4, R5, R14, R16) ────────────

describe("unified SubscribeLobby refcount", () => {
  it("sends SubscribeLobby exactly once for the first tournament subscriber with no lobby subscribers", async () => {
    const fake = makeFakeSocket();
    primeSocket(fake);

    const handlers = makeHandlers();
    const detach = await store().subscribeTournaments(handlers);

    expect(detach).not.toBeNull();
    expect(fake.tally("SubscribeLobby")).toBe(1);
    // Delivery reach-guard: a store that sent the frame but wired no listener
    // would otherwise satisfy the tally vacuously.
    fake.deliver("TournamentListUpdate", { tournaments: [summaryFor("AAA")] });
    expect(handlers.onListUpdate).toHaveBeenCalledTimes(1);
  });

  it("does not send a second SubscribeLobby for a second tournament subscriber", async () => {
    const fake = makeFakeSocket();
    primeSocket(fake);

    const first = makeHandlers();
    const second = makeHandlers();
    await store().subscribeTournaments(first);
    await store().subscribeTournaments(second);

    expect(fake.tally("SubscribeLobby")).toBe(1);
    fake.deliver("TournamentListUpdate", { tournaments: [summaryFor("AAA")] });
    expect(first.onListUpdate).toHaveBeenCalledTimes(1);
    expect(second.onListUpdate).toHaveBeenCalledTimes(1);
  });

  it("does not send UnsubscribeLobby while a tournament subscriber is still live", async () => {
    const fake = makeFakeSocket();
    primeSocket(fake);

    const lobbyCb = vi.fn();
    const detachLobby = await store().subscribeLobby(lobbyCb);
    const handlers = makeHandlers();
    const detachTournament = await store().subscribeTournaments(handlers);
    expect(fake.tally("SubscribeLobby")).toBe(1);

    detachLobby?.();
    expect(fake.tally("UnsubscribeLobby")).toBe(0);
    // Reach-guard: the surviving subscription still delivers.
    fake.deliver("TournamentListUpdate", { tournaments: [summaryFor("AAA")] });
    expect(handlers.onListUpdate).toHaveBeenCalledTimes(1);

    detachTournament?.();
    expect(fake.tally("UnsubscribeLobby")).toBe(1);
  });

  it("does not send UnsubscribeLobby while a lobby subscriber is still live", async () => {
    const fake = makeFakeSocket();
    primeSocket(fake);

    const handlers = makeHandlers();
    const detachTournament = await store().subscribeTournaments(handlers);
    const lobbyCb = vi.fn();
    const detachLobby = await store().subscribeLobby(lobbyCb);
    expect(fake.tally("SubscribeLobby")).toBe(1);

    detachTournament?.();
    expect(fake.tally("UnsubscribeLobby")).toBe(0);
    fake.deliver("LobbyUpdate", { games: [] });
    expect(lobbyCb).toHaveBeenCalledTimes(1);

    detachLobby?.();
    expect(fake.tally("UnsubscribeLobby")).toBe(1);
  });

  it("is idempotent when the same detach runs twice", async () => {
    const fake = makeFakeSocket();
    primeSocket(fake);

    const handlers = makeHandlers();
    const detach = await store().subscribeTournaments(handlers);
    detach?.();
    expect(() => detach?.()).not.toThrow();
    expect(fake.tally("UnsubscribeLobby")).toBe(1);
    expect(fake.listenerCount()).toBe(0);
  });

  it("seeds a late tournament subscriber from the cached list push", async () => {
    const fake = makeFakeSocket();
    primeSocket(fake);

    // A LOBBY subscriber acquires the shared subscription first. The broker's
    // one-shot `ToSelf(TournamentListUpdate)` arrives before any tournament
    // subscriber exists — attaching the tournament listener later would lose
    // it permanently, since nothing re-fetches the list.
    const lobbyCb = vi.fn();
    await store().subscribeLobby(lobbyCb);
    fake.deliver("TournamentListUpdate", {
      tournaments: [summaryFor("AAA"), summaryFor("BBB")],
    });

    const late = makeHandlers();
    await store().subscribeTournaments(late);
    expect(late.onListUpdate).toHaveBeenCalledTimes(1);
    const seeded = late.onListUpdate.mock.calls[0][0] as TournamentSummary[];
    expect(seeded.map((t) => t.code)).toEqual(["AAA", "BBB"]);

    // The cache is one array shared by every seed, not a per-subscriber copy.
    const later = makeHandlers();
    await store().subscribeTournaments(later);
    expect(later.onListUpdate.mock.calls[0][0]).toBe(seeded);
  });

  it("re-establishes the shared subscription for a tournament-only subscriber after a reconnect", async () => {
    const first = makeFakeSocket();
    primeSocket(first);

    const handlers = makeHandlers();
    await store().subscribeTournaments(handlers);
    expect(first.tally("SubscribeLobby")).toBe(1);
    expect(useMultiplayerStore.getState().tournamentCredentials).toEqual({});

    const second = makeFakeSocket();
    driver?.fire("reconnecting");
    driver?.setCurrent(second.socket);
    driver?.fire("open");

    // Zero lobby subscribers exist — a `lobbySubscribers.size > 0` gate would
    // leave this page silently dead on the new socket.
    expect(second.tally("SubscribeLobby")).toBe(1);
    // Delivery reach-guard on the NEW socket.
    second.deliver("TournamentListUpdate", { tournaments: [summaryFor("AAA")] });
    expect(handlers.onListUpdate).toHaveBeenCalledTimes(1);
  });

  it("clears the cached snapshots on a reconnect so no subscriber is seeded from a pre-drop list", async () => {
    const first = makeFakeSocket();
    primeSocket(first);

    const handlers = makeHandlers();
    await store().subscribeTournaments(handlers);
    first.deliver("TournamentListUpdate", { tournaments: [summaryFor("AAA")] });
    first.deliver("LobbyUpdate", { games: [] });
    expect(handlers.onListUpdate).toHaveBeenCalledTimes(1);

    const second = makeFakeSocket();
    driver?.fire("reconnecting");
    driver?.setCurrent(second.socket);
    driver?.fire("open");

    const late = makeHandlers();
    await store().subscribeTournaments(late);
    // Not seeded: the pre-drop list is not authoritative on the new socket.
    expect(late.onListUpdate).not.toHaveBeenCalled();
    // Reach-guard: the new socket's own push does reach it.
    second.deliver("TournamentListUpdate", { tournaments: [summaryFor("BBB")] });
    expect(late.onListUpdate).toHaveBeenCalledTimes(1);
  });

  it("still sends exactly one SubscribeLobby / UnsubscribeLobby pair for a lobby-only cycle", async () => {
    const fake = makeFakeSocket();
    primeSocket(fake);

    const firstCb = vi.fn();
    const secondCb = vi.fn();
    const detachFirst = await store().subscribeLobby(firstCb);
    const detachSecond = await store().subscribeLobby(secondCb);
    expect(fake.tally("SubscribeLobby")).toBe(1);

    detachFirst?.();
    expect(fake.tally("UnsubscribeLobby")).toBe(0);
    fake.deliver("LobbyUpdate", {
      games: [{ game_code: "ABCDE" } as unknown as Record<string, unknown>],
    });
    expect(secondCb).toHaveBeenCalledTimes(1);
    // The lobby snapshot still backs `findLobbyGameByCode`.
    expect(findLobbyGameByCode("abcde")?.game.game_code).toBe("ABCDE");

    detachSecond?.();
    expect(fake.tally("UnsubscribeLobby")).toBe(1);
    expect(fake.tally("SubscribeLobby")).toBe(1);
  });
});

// ── C. tournament store actions (R7, R8, R9, R12, R18) ───────────────────

describe("tournament store actions", () => {
  it("sends the matching code's organizer token when two tournaments are held", async () => {
    const fake = makeFakeSocket();
    primeSocket(fake);
    useMultiplayerStore.setState({
      tournamentCredentials: {
        AAA: { organizerToken: "org-a", updatedAt: 1 },
        BBB: { organizerToken: "org-b", updatedAt: 2 },
      },
    });

    const pending = store().startTournamentRound("BBB");
    await flush();
    const sent = fake.frame("StartTournamentRound")?.data as {
      code: string;
      organizer_token: string;
    };
    // Exact rather than partial: an extra field on a gated request would be a
    // wire change, and the correlator is the only one there should be.
    expect(sent).toEqual({
      code: "BBB",
      organizer_token: "org-b",
      request_id: expect.any(Number),
    });

    // The gated actions settle on their own correlated ack, not on the
    // same-code broadcast any participant's action produces.
    fake.deliver("TournamentActionAck", {
      request_id: correlatorOf(fake, "StartTournamentRound"),
      code: "BBB",
      view: viewFor("BBB"),
    });
    expect((await pending).ok).toBe(true);

    // A third code this browser holds nothing for is refused locally.
    const refused = await store().startTournamentRound("CCC");
    expect(refused).toEqual({
      ok: false,
      reason: "not_authorized",
      role: "organizer",
      message: "You are not the organizer of this tournament.",
    });
    expect(fake.tally("StartTournamentRound")).toBe(1);
  });

  it("rejects a gated action with no held token without opening a socket or sending a frame", async () => {
    // Deliberately NO socket primed and none live: the gate must return before
    // `ensureSubscriptionSocket` is ever reached.
    const result = await store().startTournamentRound("AAA");

    expect(result.ok).toBe(false);
    expect(result).toMatchObject({ reason: "not_authorized", role: "organizer" });
    expect(openPhaseSocket).not.toHaveBeenCalled();
    expect(withReconnect).not.toHaveBeenCalled();
  });

  it("does not let an organizer token authorize a player-gated action", async () => {
    const fake = makeFakeSocket();
    primeSocket(fake);
    useMultiplayerStore.setState({
      tournamentCredentials: { AAA: { organizerToken: "org-a", updatedAt: 1 } },
    });

    const refused = await store().reportMatchResult("AAA", 7, "Draw");

    // `role` reads "player" even though an organizer token IS held for this
    // code — that is what distinguishes an arm-correct switch from a bare
    // map-presence check.
    expect(refused).toMatchObject({
      ok: false,
      reason: "not_authorized",
      role: "player",
    });
    expect(fake.send).not.toHaveBeenCalled();
  });

  it("reaches the wire for a player-gated action once a player token is held", async () => {
    const fake = makeFakeSocket();
    primeSocket(fake);
    useMultiplayerStore.setState({
      tournamentCredentials: {
        AAA: { organizerToken: "org-a", playerToken: "ply-a", updatedAt: 1 },
      },
    });

    const pending = store().reportMatchResult("AAA", 7, "Draw");
    await flush();
    const sent = fake.frame("ReportMatchResult")?.data as {
      code: string;
      pairing_id: number;
      player_token: string;
      outcome: unknown;
    };
    expect(sent).toEqual({
      code: "AAA",
      pairing_id: 7,
      player_token: "ply-a",
      outcome: "Draw",
      request_id: expect.any(Number),
    });
    // Never the organizer token, even though one is held for the same code.
    expect(sent.player_token).not.toBe("org-a");

    fake.deliver("TournamentActionAck", {
      request_id: correlatorOf(fake, "ReportMatchResult"),
      code: "AAA",
      view: viewFor("AAA"),
    });
    expect((await pending).ok).toBe(true);
  });

  const gatedCases = [
    {
      label: "startTournamentRound",
      role: "organizer" as const,
      credential: { organizerToken: "org-a", updatedAt: 1 },
      frame: "StartTournamentRound",
      run: () => store().startTournamentRound("AAA"),
    },
    {
      label: "reportMatchResult",
      role: "player" as const,
      credential: { playerToken: "ply-a", updatedAt: 1 },
      frame: "ReportMatchResult",
      run: () => store().reportMatchResult("AAA", 7, "Draw"),
    },
  ];

  it.each(gatedCases)(
    "classifies a local refusal as not_authorized and a server rejection as rejected ($label)",
    async ({ role, credential, frame, run }) => {
      const fake = makeFakeSocket();
      primeSocket(fake);
      // A live socket exists, so "nothing went on the wire" is a real
      // measurement rather than a consequence of having no transport.
      await store().subscribeLobby(vi.fn());
      const framesBefore = fake.send.mock.calls.length;

      // (a) Local refusal — no credential held.
      const local = await run();
      expect(local.ok).toBe(false);
      expect(local).toMatchObject({ reason: "not_authorized", role });
      expect(fake.send.mock.calls.length).toBe(framesBefore);

      // (b) Genuine server rejection — credential held, frame goes out, the
      // broker refuses THIS request by correlator. For a gated action that is
      // now the only thing that produces `"rejected"`: a bare `Error` carries
      // no correlator and is deliberately ignored, which is what makes this a
      // reliable "the server refused me" signal rather than a guess.
      useMultiplayerStore.setState({ tournamentCredentials: { AAA: credential } });
      const pending = run();
      await flush();
      expect(fake.tally(frame)).toBe(1);
      fake.deliver("TournamentActionRejected", {
        request_id: correlatorOf(fake, frame),
        message: "Tournament is not in progress",
      });
      const wire = await pending;

      expect(wire.ok).toBe(false);
      expect(wire).toMatchObject({
        reason: "rejected",
        message: "Tournament is not in progress",
      });
      if (!wire.ok) expect(wire.reason).not.toBe("not_authorized");
    },
  );

  it("leaves credentials untouched when a gated action is rejected", async () => {
    const fake = makeFakeSocket();
    primeSocket(fake);
    useMultiplayerStore.setState({
      tournamentCredentials: { AAA: { organizerToken: "org-a", updatedAt: 1 } },
    });
    const before = structuredClone(store().tournamentCredentials);

    // (a) Local refusal on a code with no matching authority.
    const local = await store().endTournament("BBB");
    expect(local.ok).toBe(false);
    expect(store().tournamentCredentials).toEqual(before);

    // (b) Server rejection on a fully-credentialed call — the request frame
    // really did go out, so this is a completed round-trip, not an early return.
    const pending = store().endTournament("AAA");
    await flush();
    expect(fake.tally("EndTournament")).toBe(1);
    fake.deliver("TournamentActionRejected", {
      request_id: correlatorOf(fake, "EndTournament"),
      message: "nope",
    });
    expect((await pending).ok).toBe(false);
    expect(store().tournamentCredentials).toEqual(before);
  });

  it("aborts an in-flight tournament RPC on the reconnecting transition", async () => {
    const fake = makeFakeSocket();
    primeSocket(fake);

    const pending = store().getTournament("AAA");
    await flush();
    expect(fake.tally("GetTournament")).toBe(1);

    driver?.fire("reconnecting");
    const result = await pending;

    expect(result).toMatchObject({ ok: false, reason: "aborted" });
    expect(fake.wsClose).not.toHaveBeenCalled();
    expect(fake.socketClose).not.toHaveBeenCalled();
  });

  it("uses a fresh controller for an RPC started after a reconnect", async () => {
    const first = makeFakeSocket();
    primeSocket(first);

    const aborted = store().getTournament("AAA");
    await flush();
    driver?.fire("reconnecting");
    expect((await aborted).ok).toBe(false);

    const second = makeFakeSocket();
    driver?.setCurrent(second.socket);
    driver?.fire("open");

    const pending = store().getTournament("AAA");
    await flush();
    expect(second.tally("GetTournament")).toBe(1);
    second.deliver("TournamentUpdate", { code: "AAA", view: viewFor("AAA") });

    // The second RPC settles normally, proving the abort was scoped to the
    // controllers live at transition time.
    expect((await pending).ok).toBe(true);
  });

  it("never closes the borrowed socket on any action path", async () => {
    const fake = makeFakeSocket();
    primeSocket(fake);
    useMultiplayerStore.setState({
      tournamentCredentials: {
        AAA: { organizerToken: "org-a", playerToken: "ply-a", updatedAt: 1 },
      },
    });

    const pending = [
      store().getTournament("AAA"),
      store().startTournamentRound("AAA"),
      store().dropFromTournament("AAA"),
      store().endTournament("AAA"),
      store().reportMatchResult("AAA", 1, "Draw"),
      store().startTournamentRound("NOPE"), // local refusal path too
    ];
    await flush();
    // Each gated call settles only on its OWN correlated refusal; the
    // uncorrelated `getTournament` still settles on the bare `Error`, and the
    // local refusal never reached the wire at all. Three settlement routes in
    // one fixture, which is what makes the "socket untouched" claim total.
    for (const tag of [
      "StartTournamentRound",
      "DropFromTournament",
      "EndTournament",
      "ReportMatchResult",
    ]) {
      fake.deliver("TournamentActionRejected", {
        request_id: correlatorOf(fake, tag),
        message: "done",
      });
    }
    fake.deliver("Error", { message: "done" });
    const results = await Promise.all(pending);

    // Positive reach-guard: every call actually settled.
    expect(results).toHaveLength(6);
    expect(results.every((r) => r.ok === false)).toBe(true);
    expect(fake.wsClose).not.toHaveBeenCalled();
    expect(fake.socketClose).not.toHaveBeenCalled();
  });
});

// ── D. subscription teardown (R13, R15) ──────────────────────────────────

describe("subscription teardown", () => {
  it("tears down both listeners, both subscriber sets and both snapshots on closeSubscriptionSocket", async () => {
    const fake = makeFakeSocket();
    primeSocket(fake);

    const lobbyCb = vi.fn();
    const handlers = makeHandlers();
    await store().subscribeLobby(lobbyCb);
    await store().subscribeTournaments(handlers);
    fake.deliver("LobbyUpdate", {
      games: [{ game_code: "ABCDE" } as unknown as Record<string, unknown>],
    });
    fake.deliver("TournamentListUpdate", { tournaments: [summaryFor("AAA")] });
    // Three, not two: a channel carries the lobby, ambient and tournament
    // listeners, and all three are bound together by `attachLobbyListener`.
    // The number is asserted so the teardown's `toBe(0)` below cannot pass
    // against a socket that never had them attached.
    expect(fake.listenerCount()).toBe(3);
    expect(findLobbyGameByCode("ABCDE")).toBeDefined();

    const inflight = store().getTournament("AAA");
    await flush();

    store().closeSubscriptionSocket();

    expect(fake.listenerCount()).toBe(0);
    expect(fake.tally("UnsubscribeLobby")).toBe(1);
    expect(findLobbyGameByCode("ABCDE")).toBeUndefined();
    expect((await inflight).ok).toBe(false);

    // Calling twice must not throw and must not double-send.
    expect(() => store().closeSubscriptionSocket()).not.toThrow();
    expect(fake.tally("UnsubscribeLobby")).toBe(1);

    // Both subscriber sets were cleared and both snapshots dropped: a fresh
    // subscription re-sends `SubscribeLobby` on the new socket and seeds nobody.
    const next = makeFakeSocket();
    primeSocket(next);
    const late = makeHandlers();
    await store().subscribeTournaments(late);
    expect(next.tally("SubscribeLobby")).toBe(1);
    expect(late.onListUpdate).not.toHaveBeenCalled();
    // The cleared lobby set means the old callback receives nothing more.
    next.deliver("LobbyUpdate", { games: [] });
    expect(lobbyCb).toHaveBeenCalledTimes(1); // only the pre-teardown push
  });

  it("drops the tournament stream when the hosting server changes", async () => {
    const fake = makeFakeSocket();
    primeSocket(fake);

    await store().subscribeLobby(vi.fn());
    const handlers = makeHandlers();
    await store().subscribeTournaments(handlers);
    const attached = fake.listenerCount();

    store().setHostingServer("ws://elsewhere:9999");

    // Exactly one listener goes: the tournament stream, which is the only one
    // bound to the HOSTING authority. The lobby and ambient listeners follow
    // every browsed source and are unaffected by where games are registered.
    expect(fake.listenerCount()).toBe(attached - 1);
    fake.deliver("TournamentListUpdate", { tournaments: [summaryFor("AAA")] });
    expect(handlers.onListUpdate).not.toHaveBeenCalled();
  });

  it("a caller that detaches a late-resolving subscription leaves nothing attached", async () => {
    const fake = makeFakeSocket();
    primeSocket(fake);

    // The `LobbyView.tsx` idiom: the consumer unmounts before the promise
    // resolves, then runs the detach it finally receives.
    let cancelled = false;
    const handlers = makeHandlers();
    const promise = store().subscribeTournaments(handlers);
    cancelled = true;
    const detach = await promise;

    // Positive reach-guard: the handler IS reachable before the detach runs,
    // so the post-detach silence below is a real change of state.
    fake.deliver("TournamentListUpdate", { tournaments: [summaryFor("AAA")] });
    expect(handlers.onListUpdate).toHaveBeenCalledTimes(1);

    if (cancelled) detach?.();

    expect(fake.tally("UnsubscribeLobby")).toBe(1);
    expect(fake.listenerCount()).toBe(0);
    handlers.onListUpdate.mockClear();
    fake.deliver("TournamentListUpdate", { tournaments: [summaryFor("BBB")] });
    expect(handlers.onListUpdate).not.toHaveBeenCalled();

    // Double-detach (React strict-mode cleanup) must stay a no-op.
    expect(() => detach?.()).not.toThrow();
    expect(fake.tally("UnsubscribeLobby")).toBe(1);
  });

  it("no hosting authority makes subscribeTournaments resolve null and its caller's detach a safe no-op", async () => {
    // Direct-codes mode. `hostingServer` is validated at every write
    // (`setHostingServer`, `merge`), so "unusable address" is no longer a
    // representable string here — `null` is the whole of that condition.
    useMultiplayerStore.setState({ hostingServer: null });

    const handlers = makeHandlers();
    const detach = await store().subscribeTournaments(handlers);

    expect(detach).toBeNull();
    expect(openPhaseSocket).not.toHaveBeenCalled();
    expect(() => detach?.()).not.toThrow();
  });
});
