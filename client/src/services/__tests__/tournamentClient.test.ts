import { readFileSync } from "node:fs";
import { resolve } from "node:path";

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { repoRoot } from "../../adapter/__tests__/rustEnumVariants";
import type { TournamentSummary, TournamentView } from "../../adapter/types";
import type { PhaseSocket } from "../openPhaseSocket";
import {
  LOBBY_PROTOCOL_VERSION,
  MIN_LOBBY_PROTOCOL_FOR_TOURNAMENT_ACK,
  type ServerInfo,
} from "../../adapter/ws-adapter";
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

/**
 * The default `serverInfo` models a CURRENT-generation broker, so
 * `lobbyProtocolVersion` tracks the client's own `LOBBY_PROTOCOL_VERSION`
 * export rather than a literal that would drift from it at the next bump.
 *
 * This default is load-bearing rather than cosmetic. `tournamentClient`'s
 * capability gate treats an absent `lobbyProtocolVersion` as unsupported, so
 * leaving the field off would make every gated-helper test in this file settle
 * `{ok:false, reason:"unsupported"}` the moment it sent — quietly gutting the
 * pending-request assertions rather than failing them. The old-broker path is
 * reached only by tests that ask for it through the `Partial<ServerInfo>`
 * override below.
 *
 * NOTE the deliberate asymmetry with the gate itself, which reads the frozen
 * floor `MIN_LOBBY_PROTOCOL_FOR_TOURNAMENT_ACK`. Both constants are 5 today and
 * diverge at the next lobby bump; each site is written against the one that
 * answers its own question.
 */
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
      lobbyProtocolVersion: LOBBY_PROTOCOL_VERSION,
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
/**
 * The tournament code every entry in {@link HELPERS} acts on, and the code the
 * reply builders below mint their frames for. Named because the V6 group's
 * whole premise is that the foreign broadcast it delivers carries THIS code —
 * a different one would silently turn that row into a restatement of the
 * adjacent different-code negative.
 */
const HELPER_CODE = "TOUR01";

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
   * `every_client_variant_tag_is_known` in `crates/lobby-broker/src/protocol.rs`.
   *
   * For a {@link HelperCase.gated} helper this is the frame **without** its
   * `request_id`: the module mints that correlator from a private counter, so
   * no test can predict its value. {@link sentFrameWithoutCorrelator} strips it
   * back off for the byte comparison, and {@link sentCorrelator} asserts it was
   * there and is a number.
   */
  frame: string;
  /**
   * Whether this helper correlates its request. The four token-gated actions
   * settle on `TournamentActionAck` / `TournamentActionRejected` carrying their
   * own correlator; the other three keep tag + code matching and today's bare
   * `Error` behavior.
   */
  gated: boolean;
  /** A success frame this helper settles on, for the correlator it minted. */
  reply: (view: TournamentView, requestId: number) => string;
}

const CREATED_REPLY = (view: TournamentView) =>
  JSON.stringify({
    type: "TournamentCreated",
    data: { code: HELPER_CODE, organizer_token: "tok", view },
  });
const JOINED_REPLY = (view: TournamentView) =>
  JSON.stringify({
    type: "TournamentJoined",
    data: { code: HELPER_CODE, player_token: "tok", view },
  });
const UPDATE_REPLY = (view: TournamentView) =>
  JSON.stringify({ type: "TournamentUpdate", data: { code: HELPER_CODE, view } });
const ACK_REPLY = (view: TournamentView, requestId: number) =>
  JSON.stringify({
    type: "TournamentActionAck",
    data: { request_id: requestId, code: HELPER_CODE, view },
  });
const REJECTED_REPLY = (message: string, requestId: number) =>
  JSON.stringify({
    type: "TournamentActionRejected",
    data: { request_id: requestId, message },
  });

/** The first frame `ws` was asked to send, parsed. */
function sentFrame(ws: MockWebSocket): {
  type: string;
  data: Record<string, unknown>;
} {
  const [payload] = ws.send.mock.calls[0] as [string];
  return JSON.parse(payload) as { type: string; data: Record<string, unknown> };
}

/**
 * The correlator the module minted for the frame it just sent. Asserting its
 * type here is what keeps every correlated assertion in this file non-vacuous:
 * a helper that stopped sending one would fail here rather than silently
 * comparing `undefined` to `undefined`.
 */
function sentCorrelator(ws: MockWebSocket): number {
  const { data } = sentFrame(ws);
  expect(typeof data.request_id).toBe("number");
  return data.request_id as number;
}

/**
 * The frame just sent, with its minted correlator stripped back off, so what
 * remains can be compared byte-for-byte against the Rust literal. The module
 * appends `request_id` last, so removing it restores the original key order.
 */
function sentFrameWithoutCorrelator(ws: MockWebSocket): string {
  const { type, data } = sentFrame(ws);
  const { request_id: _correlator, ...rest } = data;
  return JSON.stringify({ type, data: rest });
}

/** The success frame `helper` settles on, correlated when it needs to be. */
function successFrame(
  ws: MockWebSocket,
  helper: HelperCase,
  view: TournamentView,
): string {
  return helper.reply(view, helper.gated ? sentCorrelator(ws) : 0);
}

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
    gated: false,
    reply: CREATED_REPLY,
  },
  {
    name: "joinTournamentOver",
    invoke: (socket, opts) => joinTournamentOver(socket, "TOUR01", "key-a", "Alice", opts),
    frame:
      '{"type":"JoinTournament","data":{"code":"TOUR01","player_key":"key-a","display_name":"Alice"}}',
    gated: false,
    reply: JOINED_REPLY,
  },
  {
    name: "getTournamentOver",
    invoke: (socket, opts) => getTournamentOver(socket, "TOUR01", opts),
    frame: '{"type":"GetTournament","data":{"code":"TOUR01"}}',
    gated: false,
    reply: UPDATE_REPLY,
  },
  {
    name: "startTournamentRoundOver",
    invoke: (socket, opts) => startTournamentRoundOver(socket, "TOUR01", "tok", opts),
    frame: '{"type":"StartTournamentRound","data":{"code":"TOUR01","organizer_token":"tok"}}',
    gated: true,
    reply: ACK_REPLY,
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
    gated: true,
    reply: ACK_REPLY,
  },
  {
    name: "dropFromTournamentOver",
    invoke: (socket, opts) => dropFromTournamentOver(socket, "TOUR01", "tok", opts),
    frame: '{"type":"DropFromTournament","data":{"code":"TOUR01","player_token":"tok"}}',
    gated: true,
    reply: ACK_REPLY,
  },
  {
    name: "endTournamentOver",
    invoke: (socket, opts) => endTournamentOver(socket, "TOUR01", "tok", opts),
    frame: '{"type":"EndTournament","data":{"code":"TOUR01","organizer_token":"tok"}}',
    gated: true,
    reply: ACK_REPLY,
  },
];

/** The four token-gated helpers, which correlate their requests. */
const GATED = HELPERS.filter((helper) => helper.gated);
/** The three helpers with genuine point replies, unchanged by correlation. */
const UNCORRELATED = HELPERS.filter((helper) => !helper.gated);

// ---------------------------------------------------------------------------
// A. Request frames byte-match the Rust literals (matrix row 4)
// ---------------------------------------------------------------------------

describe("tournament request frames", () => {
  it.each(HELPERS)(
    "$name puts the exact protocol.rs literal on the wire",
    async ({ invoke, frame, gated }) => {
      const ws = new MockWebSocket();
      const controller = new AbortController();
      const promise = invoke(makePhaseSocket(ws), { signal: controller.signal });

      // Positive reach-guard: exactly one frame, byte-identical to the Rust
      // literal `every_client_variant_tag_is_known` sends.
      expect(ws.send).toHaveBeenCalledTimes(1);
      if (gated) {
        // A gated frame carries exactly one field the Rust literal does not —
        // the correlator, minted privately, appended last. Everything else must
        // still match byte for byte.
        expect(typeof sentCorrelator(ws)).toBe("number");
        expect(sentFrameWithoutCorrelator(ws)).toBe(frame);
      } else {
        expect(ws.send).toHaveBeenCalledWith(frame);
      }

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

    // Gated, so the minted correlator comes off before the byte comparison.
    expect(sentFrameWithoutCorrelator(ws)).toBe(
      '{"type":"ReportMatchResult","data":{"code":"TOUR01","pairing_id":7,"player_token":"tok","outcome":"Draw"}}',
    );

    controller.abort();
    await expect(promise).resolves.toMatchObject({ ok: false, reason: "aborted" });
  });

  it("mints a distinct correlator for every gated call on one socket", async () => {
    const ws = new MockWebSocket();
    const socket = makePhaseSocket(ws);
    const controller = new AbortController();
    const opts = { signal: controller.signal };

    const promises = GATED.map((helper) => helper.invoke(socket, opts));
    const correlators = ws.send.mock.calls.map((call) => {
      const [payload] = call as [string];
      return (JSON.parse(payload) as { data: { request_id?: number } }).data
        .request_id;
    });

    // Reach-guard: every gated helper really sent one, and every one is a
    // number — so the uniqueness assertion below is not comparing `undefined`s.
    expect(correlators).toHaveLength(GATED.length);
    for (const correlator of correlators) expect(typeof correlator).toBe("number");
    expect(new Set(correlators).size).toBe(GATED.length);

    controller.abort();
    await Promise.all(promises);
  });
});

// ---------------------------------------------------------------------------
// B. Five settlement paths × seven helpers, socket never shut down
//    (matrix rows 5 and 6)
// ---------------------------------------------------------------------------

describe("tournament RPC settlement paths", () => {
  it.each(HELPERS)("$name settles ok on its success frame", async (helper) => {
    const ws = new MockWebSocket();
    const promise = helper.invoke(makePhaseSocket(ws));
    ws.deliver(successFrame(ws, helper, OWN_VIEW));

    const result = await promise;
    expect(result.ok).toBe(true);
    // Paired payload assertion, so "ok" cannot be satisfied by an empty value.
    if (result.ok) {
      expect((result.value as { view: TournamentView }).view).toEqual(OWN_VIEW);
    }
    expect(ws.close).not.toHaveBeenCalled();
  });

  it.each(UNCORRELATED)("$name settles rejected on a server Error", async ({ invoke }) => {
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

  // V7 — a bare `Error` no longer settles a CORRELATED request. It provably
  // belongs to some request on this socket but not provably to ours, so
  // settling on it would be a false negative in place of the false positive
  // correlation removes. The row above is this one's reach-guard: the very same
  // frame still settles all three uncorrelated helpers.
  it.each(GATED)(
    "$name ignores a bare Error and settles on its own correlated refusal",
    async ({ invoke }) => {
      const ws = new MockWebSocket();
      const promise = invoke(makePhaseSocket(ws));
      const requestId = sentCorrelator(ws);

      ws.deliver(JSON.stringify({ type: "Error", data: { message: "Not the organizer" } }));
      expect(await settledOrPending(promise)).toBe("pending");
      expect(ws.listenerCount("message")).toBe(1);

      ws.deliver(REJECTED_REPLY("Not the organizer", requestId));
      await expect(promise).resolves.toEqual({
        ok: false,
        reason: "rejected",
        message: "Not the organizer",
      });
      expect(ws.listenerCount("message")).toBe(0);
      expect(ws.close).not.toHaveBeenCalled();
    },
  );

  it.each(GATED)(
    "$name ignores a correlated refusal minted for a different request",
    async ({ invoke }) => {
      const ws = new MockWebSocket();
      const promise = invoke(makePhaseSocket(ws));
      const requestId = sentCorrelator(ws);

      ws.deliver(REJECTED_REPLY("Not the organizer", requestId + 1));
      expect(await settledOrPending(promise)).toBe("pending");
      expect(ws.listenerCount("message")).toBe(1);

      // Paired positive: our own id does settle it.
      ws.deliver(REJECTED_REPLY("Not the organizer", requestId));
      await expect(promise).resolves.toMatchObject({ ok: false, reason: "rejected" });
    },
  );

  it("falls back to a generic message when a correlated refusal carries no text", async () => {
    const ws = new MockWebSocket();
    const promise = endTournamentOver(makePhaseSocket(ws), "TOUR01", "tok");
    ws.deliver(
      JSON.stringify({
        type: "TournamentActionRejected",
        data: { request_id: sentCorrelator(ws) },
      }),
    );

    const result = await promise;
    expect(result).toMatchObject({ ok: false, reason: "rejected" });
    if (!result.ok) expect(result.message.length).toBeGreaterThan(0);
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

  // V6 — THE MAINTAINER'S REGRESSION, client half. This group previously
  // *characterized* the bug: it asserted `{ok: true}` on a foreign actor's
  // view. Every one of those assertions is inverted here, so a regression in
  // the correlation fails immediately rather than quietly re-passing.
  describe("a foreign same-code broadcast no longer settles a gated helper (V6)", () => {
    it.each(GATED)(
      "$name settles on an ack carrying our own correlator",
      async ({ invoke }) => {
        // Positive reach-guard for the whole group: the real settlement path is
        // observed, so the tests below are not passing on "nothing ever
        // settles".
        const ws = new MockWebSocket();
        const promise = invoke(makePhaseSocket(ws));
        ws.deliver(ACK_REPLY(OWN_VIEW, sentCorrelator(ws)));

        const result = await promise;
        expect(result.ok).toBe(true);
        if (result.ok) {
          expect((result.value as { view: TournamentView }).view).toEqual(OWN_VIEW);
        }
        expect(ws.listenerCount("message")).toBe(0);
      },
    );

    it.each(GATED)(
      "$name stays pending through a foreign same-code broadcast and a foreign ack, then settles on its own refusal",
      async ({ invoke }) => {
        const ws = new MockWebSocket();
        const promise = invoke(makePhaseSocket(ws));
        const requestId = sentCorrelator(ws);
        // Premise guard: the broadcast below really is for the tournament this
        // request acts on. Without this, a code mismatch would make the
        // "stays pending" assertion pass for the wrong reason.
        expect(sentFrame(ws).data.code).toBe(HELPER_CODE);

        // The frame at the heart of the maintainer's finding: byte-identical
        // in shape to this caller's own would-be broadcast, produced by another
        // actor on the SAME tournament — `UPDATE_REPLY` carries `HELPER_CODE`,
        // the very code every helper in `GATED` was invoked with, so the old
        // tag + code filter matched it and settled this promise `{ok: true}`
        // with FOREIGN_VIEW. A different code here would make this row a
        // restatement of the adjacent different-code negative instead.
        ws.deliver(UPDATE_REPLY(FOREIGN_VIEW));
        expect(await settledOrPending(promise)).toBe("pending");
        expect(ws.listenerCount("message")).toBe(1);

        // Hostile negative: an ack IS a point reply, but this one answers a
        // different request on the same socket.
        ws.deliver(ACK_REPLY(FOREIGN_VIEW, requestId + 1));
        expect(await settledOrPending(promise)).toBe("pending");
        expect(ws.listenerCount("message")).toBe(1);

        // The real answer for OUR request — the one that used to arrive with no
        // listener left — now settles it, and leaves nothing behind.
        ws.deliver(REJECTED_REPLY("Not the organizer", requestId));
        await expect(promise).resolves.toEqual({
          ok: false,
          reason: "rejected",
          message: "Not the organizer",
        });
        expect(ws.listenerCount("message")).toBe(0);
        expect(ws.close).not.toHaveBeenCalled();
      },
    );

    it("does not settle on a foreign broadcast for a different code either", async () => {
      // Adjacent negative, retained: a different tournament's frame was never
      // the ambiguous case, and must stay non-settling.
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

    it("still settles getTournamentOver on a racing same-code broadcast", async () => {
      // The exposure the three uncorrelated helpers keep, and why it is benign
      // for this one: the question it asks is `ToSelf`-shaped, so a foreign
      // frame for the same tournament answers it correctly.
      const ws = new MockWebSocket();
      const promise = getTournamentOver(makePhaseSocket(ws), CODE);
      ws.deliver(
        JSON.stringify({ type: "TournamentUpdate", data: { code: CODE, view: FOREIGN_VIEW } }),
      );

      const result = await promise;
      expect(result.ok).toBe(true);
      if (result.ok) expect(result.value.view).toEqual(FOREIGN_VIEW);
    });
  });
});

// ---------------------------------------------------------------------------
// C2. The capability gate (V8, V9, V18, V22)
// ---------------------------------------------------------------------------

describe("gated actions against a broker that cannot answer them", () => {
  it.each(GATED)(
    "$name settles unsupported without waiting, and no later broadcast can flip it (V8)",
    async ({ invoke }) => {
      const ws = new MockWebSocket();
      const socket = makePhaseSocket(ws, { lobbyProtocolVersion: 4 });
      const promise = invoke(socket);

      // The settlement is taken synchronously after the send, so the listener
      // is already gone by the time a same-code broadcast could arrive — which
      // is exactly the assertion: it cannot flip this to `{ok: true}`.
      ws.deliver(UPDATE_REPLY(FOREIGN_VIEW));

      const result = await promise;
      expect(result).toMatchObject({ ok: false, reason: "unsupported" });
      if (!result.ok) expect(result.message.length).toBeGreaterThan(0);
      expect(ws.listenerCount("message")).toBe(0);
      expect(ws.close).not.toHaveBeenCalled();
    },
  );

  it("treats a peer that advertised no lobby version identically (V8, D8.3)", async () => {
    // The deliberate divergence from `ws-adapter.ts`, which tolerates an absent
    // version for session admission. A peer that never advertised one predates
    // the version that introduced the ack, so it cannot answer one.
    const ws = new MockWebSocket();
    const result = await startTournamentRoundOver(
      makePhaseSocket(ws, { lobbyProtocolVersion: undefined }),
      "TOUR01",
      "tok",
    );
    expect(result).toMatchObject({ ok: false, reason: "unsupported" });
  });

  it("still puts the frame on the wire at a version that cannot answer it (V9)", async () => {
    const ws = new MockWebSocket();
    await startTournamentRoundOver(
      makePhaseSocket(ws, { lobbyProtocolVersion: 4 }),
      "TOUR01",
      "tok",
    );

    // D8(b): the action IS performed server-side; only the confirmation is
    // lost. Refusing to send would break the feature outright during skew.
    expect(ws.send).toHaveBeenCalledTimes(1);
    const sent = sentFrame(ws);
    expect(sent.type).toBe("StartTournamentRound");
    expect(sent.data).toMatchObject({ code: "TOUR01", organizer_token: "tok" });
    expect(typeof sent.data.request_id).toBe("number");
  });

  // V22 — the threshold is a FLOOR frozen at the version that introduced the
  // ack, not an equality against whatever this client currently speaks. The
  // `6` row is the one that matters: a newer, fully-compatible broker must NOT
  // be refused.
  it.each([
    ["one below the floor", 4, false],
    ["exactly at the floor", 5, true],
    ["above the floor (a future broker)", 6, true],
  ] as const)(
    "a peer %s reaches the correlated path = %s (V22)",
    async (_label, lobbyProtocolVersion, correlated) => {
      // These literals are meaningful only while the floor sits where it was
      // frozen. If this fires, the floor moved — which is a decision to make,
      // not a test to refresh.
      expect(MIN_LOBBY_PROTOCOL_FOR_TOURNAMENT_ACK).toBe(5);

      const ws = new MockWebSocket();
      const promise = startTournamentRoundOver(
        makePhaseSocket(ws, { lobbyProtocolVersion }),
        "TOUR01",
        "tok",
      );
      // The frame goes out on both sides of the gate.
      expect(ws.send).toHaveBeenCalledTimes(1);

      if (!correlated) {
        await expect(promise).resolves.toMatchObject({
          ok: false,
          reason: "unsupported",
        });
        expect(ws.listenerCount("message")).toBe(0);
        return;
      }

      expect(await settledOrPending(promise)).toBe("pending");
      ws.deliver(ACK_REPLY(OWN_VIEW, sentCorrelator(ws)));
      const result = await promise;
      expect(result.ok).toBe(true);
      if (result.ok) expect(result.value.view).toEqual(OWN_VIEW);
    },
  );

  // V18 — the harness default must reach the correlated path, or every gated
  // test above would settle `"unsupported"` on send and go vacuous.
  it("reaches the correlated path on the harness default (V18)", async () => {
    const ws = new MockWebSocket();
    const promise = startTournamentRoundOver(makePhaseSocket(ws), "TOUR01", "tok");

    expect(await settledOrPending(promise)).toBe("pending");
    expect(ws.listenerCount("message")).toBe(1);

    ws.deliver(ACK_REPLY(OWN_VIEW, sentCorrelator(ws)));
    await expect(promise).resolves.toMatchObject({ ok: true });
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
  /**
   * The capability gate compared against the version this client currently
   * speaks, rather than against the frozen ack floor.
   *
   * Scoped to the comparison itself, not to the identifier: this module's own
   * prose names `LOBBY_PROTOCOL_VERSION` while explaining why the gate must NOT
   * be written against it, and that explanation has to stay legal.
   */
  const GATE_AGAINST_CURRENT_VERSION =
    /lobbyProtocolVersion\s*<\s*LOBBY_PROTOCOL_VERSION/g;

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

  // The tripwire for the D8.2 latent bug, and it has to be a STATIC one.
  //
  // The runtime V22 rows catch a gate written as an EQUALITY against the
  // current version — the `lobbyProtocolVersion: 6` fixture fails immediately
  // under that mistake. They cannot catch a gate written as
  // `< LOBBY_PROTOCOL_VERSION`: measured, that form passes all three V22 rows
  // today, because the floor and the current version are both 5, and only
  // starts refusing fully-compatible brokers at the next lobby bump. This
  // assertion closes exactly that window — it fails at the moment the mistake
  // is made rather than months later, in a release, against servers that work.
  it("never gates the ack on the version this client currently speaks", () => {
    expect(SOURCE.match(GATE_AGAINST_CURRENT_VERSION)).toBeNull();

    // Positive control — a regex that silently matches nothing cannot pass.
    expect(
      "lobbyProtocolVersion < LOBBY_PROTOCOL_VERSION;".match(GATE_AGAINST_CURRENT_VERSION),
    ).not.toBeNull();

    // Paired positive: the gate IS written against the frozen floor.
    expect(SOURCE).toMatch(
      /lobbyProtocolVersion\s*<\s*MIN_LOBBY_PROTOCOL_FOR_TOURNAMENT_ACK/,
    );
  });
});
