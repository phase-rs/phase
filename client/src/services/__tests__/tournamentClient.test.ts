import { readFileSync } from "node:fs";
import { resolve } from "node:path";

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { repoRoot } from "../../adapter/__tests__/rustEnumVariants";
import type { TournamentSummary, TournamentView } from "../../adapter/types";
import type { PhaseSocket } from "../openPhaseSocket";
import type { ServerInfo } from "../../adapter/ws-adapter";
import {
  createTournamentOver,
  dropFromTournamentOver,
  endTournamentOver,
  getTournamentOver,
  joinTournamentOver,
  reportMatchResultOver,
  startTournamentRoundOver,
  subscribeTournamentsOver,
  type TournamentRpcResult,
} from "../tournamentClient";

/**
 * Copied from `brokerClient.test.ts:12-47` (the house convention is a
 * per-test-file harness, not a shared export), plus ONE local extension: a live
 * registration tally for `"message"` listeners, exposed as
 * {@link MockWebSocket.listenerCount}.
 *
 * The tally is deliberately `"message"`-only. `"close"` registrations are made
 * with `{ once: true }` AND explicitly removed in `cleanup()`, so the automatic
 * one-shot removal happens outside this override and a `"close"` tally would go
 * negative. Only `"message"` has exactly one add and one remove per request,
 * which is what makes it a meaningful leak detector for row 12a: once a promise
 * has settled it cannot visibly settle again, so a leaked listener is otherwise
 * undetectable.
 */
class MockWebSocket extends EventTarget {
  static OPEN = 1;
  readyState = MockWebSocket.OPEN;
  onopen: (() => void) | null = null;
  onmessage: ((event: { data: string }) => void) | null = null;
  onerror: (() => void) | null = null;
  onclose: (() => void) | null = null;
  send = vi.fn();
  close = vi.fn();

  private messageListeners = 0;

  override addEventListener(
    type: string,
    callback: EventListenerOrEventListenerObject | null,
    options?: AddEventListenerOptions | boolean,
  ): void {
    if (type === "message") this.messageListeners += 1;
    super.addEventListener(type, callback, options);
  }

  override removeEventListener(
    type: string,
    callback: EventListenerOrEventListenerObject | null,
    options?: EventListenerOptions | boolean,
  ): void {
    if (type === "message") this.messageListeners -= 1;
    super.removeEventListener(type, callback, options);
  }

  /** Live `"message"` registrations. See the class doc for why only this type. */
  listenerCount(type: "message"): number {
    return type === "message" ? this.messageListeners : 0;
  }

  deliver(data: string) {
    this.onmessage?.({ data });
    this.dispatchEvent(new MessageEvent("message", { data }));
  }
  fireClose() {
    this.onclose?.();
    this.dispatchEvent(new Event("close"));
  }
}

function makePhaseSocket(
  ws: MockWebSocket,
  serverInfo: Partial<ServerInfo> = {},
): PhaseSocket {
  return {
    ws: ws as unknown as WebSocket,
    serverInfo: {
      version: "0.0.0",
      buildCommit: "test",
      protocolVersion: 1,
      mode: "LobbyOnly",
      ...serverInfo,
    },
    close: () => ws.close(),
  };
}

beforeEach(() => {
  if (typeof MessageEvent === "undefined") {
    vi.stubGlobal("MessageEvent", class {
      constructor(public type: string, public init: { data: string }) {}
      get data() {
        return this.init.data;
      }
    });
  }
});

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

function makeView(status: TournamentView["summary"]["status"], playerCount: number): TournamentView {
  return {
    summary: {
      code: "AAA111",
      name: "Friday Night",
      arity: 2,
      bracket: "Swiss",
      status,
      player_count: playerCount,
      current_round: 1,
      total_rounds: 3,
      created_at: 1_700_000_000,
    },
    players: Array.from({ length: playerCount }, (_unused, index) => ({
      player_key: `key-${index}`,
      display_name: `Player ${index}`,
      dropped: false,
    })),
    pairings: [],
    standings: [],
  };
}

/** What *this* caller's own action would produce. */
const OWN_VIEW = makeView("Completed", 2);
/** What another actor's action on the same tournament produces. Distinguishable. */
const FOREIGN_VIEW = makeView("InProgress", 3);

const CODE = "AAA111";
const OTHER_CODE = "BBB222";

type Invoke = (
  socket: PhaseSocket,
  opts?: { signal?: AbortSignal; timeoutMs?: number },
) => Promise<TournamentRpcResult<unknown>>;

interface HelperCase {
  /** Helper name, for the test title. */
  name: string;
  invoke: Invoke;
  /**
   * The exact bytes the helper must put on the wire — copied verbatim from
   * `every_client_variant_tag_is_known` in `crates/lobby-broker/src/protocol.rs:1143-1172`.
   */
  frame: string;
  /** A success frame this helper settles on. */
  reply: (view: TournamentView) => string;
}

const CREATED_REPLY = (view: TournamentView) =>
  JSON.stringify({
    type: "TournamentCreated",
    data: { code: "TOUR01", organizer_token: "tok", view },
  });
const JOINED_REPLY = (view: TournamentView) =>
  JSON.stringify({
    type: "TournamentJoined",
    data: { code: "TOUR01", player_token: "tok", view },
  });
const UPDATE_REPLY = (view: TournamentView) =>
  JSON.stringify({ type: "TournamentUpdate", data: { code: "TOUR01", view } });

const HELPERS: HelperCase[] = [
  {
    name: "createTournamentOver",
    invoke: (socket, opts) =>
      createTournamentOver(
        socket,
        {
          name: "Friday Night",
          arity: 2,
          scoring: { win_points: 3, draw_points: 1, loss_points: 0 },
          bracket: "Swiss",
          totalRounds: 3,
        },
        opts,
      ),
    frame:
      '{"type":"CreateTournament","data":{"name":"Friday Night","arity":2,"scoring":{"win_points":3,"draw_points":1,"loss_points":0},"bracket":"Swiss","total_rounds":3}}',
    reply: CREATED_REPLY,
  },
  {
    name: "joinTournamentOver",
    invoke: (socket, opts) => joinTournamentOver(socket, "TOUR01", "key-a", "Alice", opts),
    frame:
      '{"type":"JoinTournament","data":{"code":"TOUR01","player_key":"key-a","display_name":"Alice"}}',
    reply: JOINED_REPLY,
  },
  {
    name: "getTournamentOver",
    invoke: (socket, opts) => getTournamentOver(socket, "TOUR01", opts),
    frame: '{"type":"GetTournament","data":{"code":"TOUR01"}}',
    reply: UPDATE_REPLY,
  },
  {
    name: "startTournamentRoundOver",
    invoke: (socket, opts) => startTournamentRoundOver(socket, "TOUR01", "tok", opts),
    frame: '{"type":"StartTournamentRound","data":{"code":"TOUR01","organizer_token":"tok"}}',
    reply: UPDATE_REPLY,
  },
  {
    name: "reportMatchResultOver",
    invoke: (socket, opts) =>
      reportMatchResultOver(
        socket,
        "TOUR01",
        0,
        "tok",
        { Decisive: { winner: "key-a", game_wins: { "key-a": 2, "key-b": 1 } } },
        opts,
      ),
    frame:
      '{"type":"ReportMatchResult","data":{"code":"TOUR01","pairing_id":0,"player_token":"tok","outcome":{"Decisive":{"winner":"key-a","game_wins":{"key-a":2,"key-b":1}}}}}',
    reply: UPDATE_REPLY,
  },
  {
    name: "dropFromTournamentOver",
    invoke: (socket, opts) => dropFromTournamentOver(socket, "TOUR01", "tok", opts),
    frame: '{"type":"DropFromTournament","data":{"code":"TOUR01","player_token":"tok"}}',
    reply: UPDATE_REPLY,
  },
  {
    name: "endTournamentOver",
    invoke: (socket, opts) => endTournamentOver(socket, "TOUR01", "tok", opts),
    frame: '{"type":"EndTournament","data":{"code":"TOUR01","organizer_token":"tok"}}',
    reply: UPDATE_REPLY,
  },
];

// ---------------------------------------------------------------------------
// A. Request frames byte-match the Rust literals (matrix row 4)
// ---------------------------------------------------------------------------

describe("tournament request frames", () => {
  it.each(HELPERS)(
    "$name puts the exact protocol.rs literal on the wire",
    async ({ invoke, frame }) => {
      const ws = new MockWebSocket();
      const controller = new AbortController();
      const promise = invoke(makePhaseSocket(ws), { signal: controller.signal });

      // Positive reach-guard: exactly one frame, byte-identical to the Rust
      // literal `every_client_variant_tag_is_known` sends.
      expect(ws.send).toHaveBeenCalledTimes(1);
      expect(ws.send).toHaveBeenCalledWith(frame);

      controller.abort();
      await expect(promise).resolves.toMatchObject({ ok: false, reason: "aborted" });
    },
  );

  it("serializes an omitted total_rounds as an explicit null", async () => {
    const ws = new MockWebSocket();
    const controller = new AbortController();
    const promise = createTournamentOver(
      makePhaseSocket(ws),
      {
        name: "Friday Night",
        arity: 4,
        scoring: { win_points: 7, draw_points: 1, loss_points: 0 },
        bracket: "Swiss",
      },
      { signal: controller.signal },
    );

    // `Option<u32>` with `#[serde(default)]` and no `skip_serializing_if`.
    expect(ws.send).toHaveBeenCalledWith(
      '{"type":"CreateTournament","data":{"name":"Friday Night","arity":4,"scoring":{"win_points":7,"draw_points":1,"loss_points":0},"bracket":"Swiss","total_rounds":null}}',
    );

    controller.abort();
    await expect(promise).resolves.toMatchObject({ ok: false, reason: "aborted" });
  });

  it("serializes a pod draw outcome as the bare Draw unit variant", async () => {
    const ws = new MockWebSocket();
    const controller = new AbortController();
    const promise = reportMatchResultOver(
      makePhaseSocket(ws),
      "TOUR01",
      7,
      "tok",
      "Draw",
      { signal: controller.signal },
    );

    expect(ws.send).toHaveBeenCalledWith(
      '{"type":"ReportMatchResult","data":{"code":"TOUR01","pairing_id":7,"player_token":"tok","outcome":"Draw"}}',
    );

    controller.abort();
    await expect(promise).resolves.toMatchObject({ ok: false, reason: "aborted" });
  });
});

// ---------------------------------------------------------------------------
// B. Five settlement paths × seven helpers, socket never shut down
//    (matrix rows 5 and 6)
// ---------------------------------------------------------------------------

describe("tournament RPC settlement paths", () => {
  it.each(HELPERS)("$name settles ok on its success frame", async ({ invoke, reply }) => {
    const ws = new MockWebSocket();
    const promise = invoke(makePhaseSocket(ws));
    ws.deliver(reply(OWN_VIEW));

    const result = await promise;
    expect(result.ok).toBe(true);
    // Paired payload assertion, so "ok" cannot be satisfied by an empty value.
    if (result.ok) {
      expect((result.value as { view: TournamentView }).view).toEqual(OWN_VIEW);
    }
    expect(ws.close).not.toHaveBeenCalled();
  });

  it.each(HELPERS)("$name settles rejected on a server Error", async ({ invoke }) => {
    const ws = new MockWebSocket();
    const promise = invoke(makePhaseSocket(ws));
    ws.deliver(JSON.stringify({ type: "Error", data: { message: "Not the organizer" } }));

    await expect(promise).resolves.toEqual({
      ok: false,
      reason: "rejected",
      message: "Not the organizer",
    });
    expect(ws.close).not.toHaveBeenCalled();
  });

  it.each(HELPERS)("$name settles aborted when its signal fires", async ({ invoke }) => {
    const ws = new MockWebSocket();
    const controller = new AbortController();
    const promise = invoke(makePhaseSocket(ws), { signal: controller.signal });
    controller.abort();

    await expect(promise).resolves.toMatchObject({ ok: false, reason: "aborted" });
    expect(ws.close).not.toHaveBeenCalled();
  });

  it.each(HELPERS)("$name settles connection_lost when the socket drops", async ({ invoke }) => {
    const ws = new MockWebSocket();
    const promise = invoke(makePhaseSocket(ws));
    ws.fireClose();

    await expect(promise).resolves.toMatchObject({ ok: false, reason: "connection_lost" });
    // The drop came from the far end; this module still shut nothing down.
    expect(ws.close).not.toHaveBeenCalled();
  });

  describe("with fake timers", () => {
    beforeEach(() => {
      vi.useFakeTimers();
    });
    afterEach(() => {
      vi.useRealTimers();
    });

    it.each(HELPERS)("$name settles timeout when nothing answers", async ({ invoke }) => {
      const ws = new MockWebSocket();
      const promise = invoke(makePhaseSocket(ws));
      await vi.advanceTimersByTimeAsync(10_001);

      await expect(promise).resolves.toMatchObject({ ok: false, reason: "timeout" });
      expect(ws.close).not.toHaveBeenCalled();
    });
  });

  it("refuses to send on a socket that is not open, without shutting it down", async () => {
    const ws = new MockWebSocket();
    ws.readyState = 3;

    await expect(getTournamentOver(makePhaseSocket(ws), CODE)).resolves.toMatchObject({
      ok: false,
      reason: "connection_lost",
    });
    expect(ws.send).not.toHaveBeenCalled();
    expect(ws.close).not.toHaveBeenCalled();
  });

  it("settles aborted without sending when the signal is already aborted", async () => {
    const ws = new MockWebSocket();
    const controller = new AbortController();
    controller.abort();

    await expect(
      getTournamentOver(makePhaseSocket(ws), CODE, { signal: controller.signal }),
    ).resolves.toMatchObject({ ok: false, reason: "aborted" });
    expect(ws.send).not.toHaveBeenCalled();
    expect(ws.close).not.toHaveBeenCalled();
  });
});

// ---------------------------------------------------------------------------
// C. Reply correlation (matrix rows 10, 11, 12 and 12a)
// ---------------------------------------------------------------------------

/** Resolves to `"pending"` when `promise` has not settled within 20ms. */
async function settledOrPending(promise: Promise<unknown>): Promise<unknown> {
  return Promise.race([
    promise,
    new Promise((r) => setTimeout(() => r("pending"), 20)),
  ]);
}

describe("tournament reply correlation", () => {
  it("ignores a same-tag reply carrying a different tournament code", async () => {
    const ws = new MockWebSocket();
    const socket = makePhaseSocket(ws);
    const promise = getTournamentOver(socket, CODE);

    // Hostile fixture: this client holds two tournaments; the other one updates.
    ws.deliver(
      JSON.stringify({ type: "TournamentUpdate", data: { code: OTHER_CODE, view: FOREIGN_VIEW } }),
    );
    expect(await settledOrPending(promise)).toBe("pending");

    // Paired positive: the right code still settles it afterwards.
    ws.deliver(JSON.stringify({ type: "TournamentUpdate", data: { code: CODE, view: OWN_VIEW } }));
    const result = await promise;
    expect(result.ok).toBe(true);
    if (result.ok) expect(result.value.view).toEqual(OWN_VIEW);
  });

  it("correlates TournamentCreated on its tag alone, but still filters by tag", async () => {
    const ws = new MockWebSocket();
    const promise = createTournamentOver(makePhaseSocket(ws), {
      name: "Friday Night",
      arity: 2,
      scoring: { win_points: 3, draw_points: 1, loss_points: 0 },
      bracket: "Swiss",
    });

    // The broker mints the code in the reply, so there is nothing to correlate
    // on — but a different tag must still be ignored.
    ws.deliver(
      JSON.stringify({
        type: "TournamentJoined",
        data: { code: "ZZZ999", player_token: "tok", view: FOREIGN_VIEW },
      }),
    );
    expect(await settledOrPending(promise)).toBe("pending");

    ws.deliver(
      JSON.stringify({
        type: "TournamentCreated",
        data: { code: "ZZZ999", organizer_token: "tok", view: OWN_VIEW },
      }),
    );
    const result = await promise;
    expect(result.ok).toBe(true);
    if (result.ok) expect(result.value.organizer_token).toBe("tok");
  });

  it("settles every in-flight request on one uncorrelated Error frame", async () => {
    const ws = new MockWebSocket();
    const socket = makePhaseSocket(ws);
    const first = getTournamentOver(socket, CODE);
    const second = getTournamentOver(socket, OTHER_CODE);

    // `LobbyServerMessage::Error` carries no tournament code, so it cannot be
    // routed to one request. Both settle — positively, with the same message.
    ws.deliver(JSON.stringify({ type: "Error", data: { message: "Tournament not found" } }));

    await expect(first).resolves.toEqual({
      ok: false,
      reason: "rejected",
      message: "Tournament not found",
    });
    await expect(second).resolves.toEqual({
      ok: false,
      reason: "rejected",
      message: "Tournament not found",
    });
  });

  describe("a foreign same-code broadcast settles a gated helper (B6)", () => {
    it("settles with this caller's own view when no one else acts", async () => {
      // Positive reach-guard for the whole group: the real settlement path is
      // observed, so the tests below are not passing on "nothing ever settles".
      const ws = new MockWebSocket();
      const promise = endTournamentOver(makePhaseSocket(ws), CODE, "tok");
      ws.deliver(JSON.stringify({ type: "TournamentUpdate", data: { code: CODE, view: OWN_VIEW } }));

      const result = await promise;
      expect(result.ok).toBe(true);
      if (result.ok) expect(result.value.view).toEqual(OWN_VIEW);
      expect(ws.listenerCount("message")).toBe(0);
    });

    it.each([
      ["endTournamentOver", (socket: PhaseSocket) => endTournamentOver(socket, CODE, "tok")],
      [
        "reportMatchResultOver",
        (socket: PhaseSocket) => reportMatchResultOver(socket, CODE, 3, "tok", "Draw"),
      ],
    ] as const)(
      "%s settles with a foreign actor's view, and a later Error cannot re-settle it",
      async (_label, invoke) => {
        const ws = new MockWebSocket();
        const promise = invoke(makePhaseSocket(ws));

        // Byte-identical in shape to this caller's own would-be reply — the
        // `data.code === code` filter matches it and cannot discriminate.
        ws.deliver(
          JSON.stringify({ type: "TournamentUpdate", data: { code: CODE, view: FOREIGN_VIEW } }),
        );

        const result = await promise;
        expect(result.ok).toBe(true);
        if (result.ok) expect(result.value.view).toEqual(FOREIGN_VIEW);

        // The real rejection for OUR request arrives after cleanup ran. It is
        // dropped, and — the part that must not regress — no listener is left
        // behind holding a settled promise's closure.
        ws.deliver(JSON.stringify({ type: "Error", data: { message: "Not the organizer" } }));
        await expect(promise).resolves.toMatchObject({ ok: true });
        expect(await promise).toEqual(result);
        expect(ws.listenerCount("message")).toBe(0);
        expect(ws.close).not.toHaveBeenCalled();
      },
    );

    it("does not settle on a foreign broadcast for a different code", async () => {
      // Adjacent negative: the code conjunct still discriminates tournaments,
      // even though it cannot discriminate requests.
      const ws = new MockWebSocket();
      const promise = endTournamentOver(makePhaseSocket(ws), CODE, "tok");

      ws.deliver(
        JSON.stringify({
          type: "TournamentUpdate",
          data: { code: OTHER_CODE, view: FOREIGN_VIEW },
        }),
      );

      expect(await settledOrPending(promise)).toBe("pending");
      expect(ws.listenerCount("message")).toBe(1);
    });
  });
});

// ---------------------------------------------------------------------------
// D. Malformed frames (matrix row 13)
// ---------------------------------------------------------------------------

describe("tournament frame trust boundary", () => {
  it("ignores unparseable and payload-less frames, then settles on a valid one", async () => {
    const ws = new MockWebSocket();
    const promise = getTournamentOver(makePhaseSocket(ws), CODE);

    expect(() => ws.deliver("{not json")).not.toThrow();
    expect(() => ws.deliver(JSON.stringify({ type: "TournamentUpdate" }))).not.toThrow();
    expect(await settledOrPending(promise)).toBe("pending");

    ws.deliver(JSON.stringify({ type: "TournamentUpdate", data: { code: CODE, view: OWN_VIEW } }));
    await expect(promise).resolves.toMatchObject({ ok: true });
  });

  // The reply filter and the broadcast listener read the SAME `TournamentUpdate`
  // frames, so they must refuse the same malformed ones. The broadcast half is
  // already pinned in section E ("ignores malformed and payload-less broadcast
  // frames", `{ code: CODE }` with no view); these are the point-reply half.
  it("ignores a reply payload missing its view, exactly as the broadcast listener does", async () => {
    const ws = new MockWebSocket();
    const promise = getTournamentOver(makePhaseSocket(ws), CODE);

    // Right tag, right code, no `view`. Settling `{ok: true}` here would hand
    // the caller a `TournamentUpdateReply` whose `view` the wire never sent.
    ws.deliver(JSON.stringify({ type: "TournamentUpdate", data: { code: CODE } }));
    expect(await settledOrPending(promise)).toBe("pending");
    // The refusal consumed nothing: the request is still listening.
    expect(ws.listenerCount("message")).toBe(1);

    // Paired positive: the same tag and code WITH a view does settle it.
    ws.deliver(JSON.stringify({ type: "TournamentUpdate", data: { code: CODE, view: OWN_VIEW } }));
    const result = await promise;
    expect(result.ok).toBe(true);
    if (result.ok) expect(result.value.view).toEqual(OWN_VIEW);
  });

  it("ignores a TournamentCreated payload missing the code the broker mints", async () => {
    const ws = new MockWebSocket();
    const promise = createTournamentOver(makePhaseSocket(ws), {
      name: "Friday Night",
      arity: 2,
      scoring: { win_points: 3, draw_points: 1, loss_points: 0 },
      bracket: "Swiss",
    });

    // `TournamentCreated` correlates on its tag alone, so the presence check is
    // the only thing between a code-less payload and a caller navigating to the
    // tournament it just created with nothing to navigate to.
    ws.deliver(
      JSON.stringify({
        type: "TournamentCreated",
        data: { organizer_token: "tok", view: OWN_VIEW },
      }),
    );
    expect(await settledOrPending(promise)).toBe("pending");
    expect(ws.listenerCount("message")).toBe(1);

    ws.deliver(
      JSON.stringify({
        type: "TournamentCreated",
        data: { code: "TOUR01", organizer_token: "tok", view: OWN_VIEW },
      }),
    );
    const result = await promise;
    expect(result.ok).toBe(true);
    if (result.ok) expect(result.value.code).toBe("TOUR01");
  });

  it("falls back to a generic message when an Error frame carries no text", async () => {
    const ws = new MockWebSocket();
    const promise = getTournamentOver(makePhaseSocket(ws), CODE);
    ws.deliver(JSON.stringify({ type: "Error" }));

    const result = await promise;
    expect(result).toMatchObject({ ok: false, reason: "rejected" });
    if (!result.ok) expect(result.message.length).toBeGreaterThan(0);
  });
});

// ---------------------------------------------------------------------------
// E. The subscription sends nothing, ever (matrix rows 7 and 8)
// ---------------------------------------------------------------------------

describe("subscribeTournamentsOver", () => {
  it("sends nothing across a full attach → deliver → detach cycle", () => {
    const ws = new MockWebSocket();
    const lists: TournamentSummary[][] = [];
    const updates: Array<[string, TournamentView]> = [];
    const removed: string[] = [];

    const detach = subscribeTournamentsOver(makePhaseSocket(ws), {
      onListUpdate: (tournaments) => lists.push(tournaments),
      onTournamentUpdate: (code, view) => updates.push([code, view]),
      onTournamentRemoved: (code) => removed.push(code),
    });

    // Checkpoint 1 — attach. `subscribeLobbyOver` sends `SubscribeLobby` here;
    // this one must not, because the shared refcount belongs to the store.
    expect(ws.send).not.toHaveBeenCalled();

    ws.deliver(
      JSON.stringify({ type: "TournamentListUpdate", data: { tournaments: [OWN_VIEW.summary] } }),
    );
    ws.deliver(JSON.stringify({ type: "TournamentUpdate", data: { code: CODE, view: OWN_VIEW } }));
    ws.deliver(JSON.stringify({ type: "TournamentRemoved", data: { code: OTHER_CODE } }));

    // Checkpoint 2 — after inbound traffic.
    expect(ws.send).not.toHaveBeenCalled();

    // Paired positive reach-guard: all three handlers actually fired with the
    // parsed shapes, so the zero-send assertion is not vacuously satisfied by a
    // helper that never wired anything up.
    expect(lists).toEqual([[OWN_VIEW.summary]]);
    expect(updates).toEqual([[CODE, OWN_VIEW]]);
    expect(removed).toEqual([OTHER_CODE]);

    detach();

    // Checkpoint 3 — detach, while the socket is still OPEN. This is exactly
    // where `subscribeLobbyOver` DOES send `UnsubscribeLobby`.
    expect(ws.readyState).toBe(MockWebSocket.OPEN);
    expect(ws.send).not.toHaveBeenCalled();
    expect(ws.close).not.toHaveBeenCalled();
  });

  it("stops delivering after detach, and tolerates a double detach", () => {
    const ws = new MockWebSocket();
    const updates: string[] = [];
    const detach = subscribeTournamentsOver(makePhaseSocket(ws), {
      onTournamentUpdate: (code) => updates.push(code),
    });

    ws.deliver(JSON.stringify({ type: "TournamentUpdate", data: { code: CODE, view: OWN_VIEW } }));
    // Pre-detach increment proves the counter is live.
    expect(updates).toEqual([CODE]);

    detach();
    detach();

    ws.deliver(JSON.stringify({ type: "TournamentUpdate", data: { code: CODE, view: OWN_VIEW } }));
    expect(updates).toEqual([CODE]);
    expect(ws.send).not.toHaveBeenCalled();
  });

  it("ignores malformed and payload-less broadcast frames", () => {
    const ws = new MockWebSocket();
    let calls = 0;
    const detach = subscribeTournamentsOver(makePhaseSocket(ws), {
      onListUpdate: () => {
        calls += 1;
      },
      onTournamentUpdate: () => {
        calls += 1;
      },
      onTournamentRemoved: () => {
        calls += 1;
      },
    });

    expect(() => ws.deliver("{not json")).not.toThrow();
    ws.deliver(JSON.stringify({ type: "TournamentListUpdate", data: {} }));
    ws.deliver(JSON.stringify({ type: "TournamentUpdate", data: { code: CODE } }));
    ws.deliver(JSON.stringify({ type: "TournamentRemoved" }));
    ws.deliver(JSON.stringify({ type: "LobbyUpdate", data: { games: [] } }));
    expect(calls).toBe(0);

    // Positive reach-guard: a well-formed frame still gets through.
    ws.deliver(JSON.stringify({ type: "TournamentRemoved", data: { code: CODE } }));
    expect(calls).toBe(1);

    detach();
  });
});

// ---------------------------------------------------------------------------
// F. Static source assertions (matrix row 9)
// ---------------------------------------------------------------------------

describe("tournamentClient source-level boundaries", () => {
  const SOURCE = readFileSync(
    resolve(repoRoot(), "client/src/services/tournamentClient.ts"),
    "utf8",
  );

  // These three run against RAW file text and are comment-unaware: prose that
  // happened to match would read as a genuine boundary violation. Each is
  // therefore scoped to CALL SITES rather than the whole file, and
  // `tournamentClient.ts`'s module header carries a matching wording constraint
  // so the explanation seam S3 positively wants stays legal. Comment-stripping
  // first was deliberately not chosen: no `stripComments`-style helper exists
  // anywhere under `client/src`, and inventing one is out of scope here.
  // Note the alternation spells both tags out rather than using an optional
  // `Un` prefix: the wire tag is `UnsubscribeLobby` with a LOWERCASE `s`, so
  // `(?:Un)?SubscribeLobby` matches only half of what it appears to. The
  // `UnsubscribeLobby` positive control below is what catches that.
  const SUBSCRIBE_FRAME_SEND = /\bsend\s*\([^)]*(?:Subscribe|Unsubscribe)Lobby/g;
  const SOCKET_SHUTDOWN_CALL = /\.close\s*\(/g;
  const SOCKET_FACTORY_CALL = /\bopenPhaseSocket\s*\(/g;

  it("never sends SubscribeLobby or UnsubscribeLobby (seam S3)", () => {
    expect(SOURCE.match(SUBSCRIBE_FRAME_SEND)).toBeNull();

    // Positive control — a regex that silently matches nothing cannot pass.
    expect(
      'ws.send(JSON.stringify({ type: "SubscribeLobby" }));'.match(SUBSCRIBE_FRAME_SEND),
    ).not.toBeNull();
    expect(
      'ws.send(JSON.stringify({ type: "UnsubscribeLobby" }));'.match(SUBSCRIBE_FRAME_SEND),
    ).not.toBeNull();

    // The explanatory prose the seam wants must remain legal.
    expect(SOURCE).toContain("UnsubscribeLobby");
  });

  it("never ends the borrowed socket's life", () => {
    expect(SOURCE.match(SOCKET_SHUTDOWN_CALL)).toBeNull();
    expect("socket.close();".match(SOCKET_SHUTDOWN_CALL)).not.toBeNull();
    expect("ws.close()".match(SOCKET_SHUTDOWN_CALL)).not.toBeNull();
  });

  it("never acquires a socket of its own", () => {
    expect(SOURCE.match(SOCKET_FACTORY_CALL)).toBeNull();
    expect('const s = await openPhaseSocket("ws://x");'.match(SOCKET_FACTORY_CALL)).not.toBeNull();

    // Positive control for the scoping itself: the module DOES reference
    // `openPhaseSocket` — as a type-only import — and that must stay allowed,
    // which is why the assertion above is call-scoped rather than whole-file.
    expect(SOURCE).toMatch(/import type \{ PhaseSocket \} from "\.\/openPhaseSocket";/);
  });
});
