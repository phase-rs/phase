import { describe, expect, it, vi } from "vitest";

import { P2PDraftHost, type DraftHostEvent } from "../p2p-draft-host";

/**
 * U13b — the plural pick channel. Every test here drives the real host seam
 * (`handleGuestMessage` / `resolveBotPicks` / `autoPickAllPending`) rather than
 * asserting a message shape, because the defect this phase closes is a runtime
 * one: a one-id pick into a Commander pod is refused by `apply_pick_inner` with
 * `WrongPickCardCount`, and `resolveBotPicks` has no try/catch, so it strands
 * the round.
 *
 * Modelled on `p2pDraftEffectPick.test.ts`, including its `REVERT-PROBE:`
 * convention: every test names the exact line whose reversion it catches.
 */

type Card = { instance_id: string };

function pack(...ids: string[]): Card[] {
  return ids.map((instance_id) => ({ instance_id }));
}

function deferred<T>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((complete) => { resolve = complete; });
  return { promise, resolve };
}

function newHost(kind: "CommanderDraft" | "Premier" = "CommanderDraft") {
  return new P2PDraftHost(
    { id: "host" } as never,
    () => () => {},
    { type: "Set", data: { pools: [{ code: "TST" }], sequence: ["TST"] } } as never,
    kind,
    4,
    "Host",
    "Swiss",
    "Competitive",
  );
}

type PrivateHost = {
  draftStarted: boolean;
  paused: boolean;
  picksThisRound: Set<number>;
  guestSessions: Map<number, { send: ReturnType<typeof vi.fn> }>;
  adapter: Record<string, ReturnType<typeof vi.fn>>;
  handleGuestMessage: (seat: number, message: unknown) => Promise<void>;
  submitHostDeck: (mainDeck: string[], commanders: string[]) => Promise<unknown>;
  resolveBotPicks: (options: { emit: boolean; persist: boolean }) => Promise<void>;
  autoPickAllPending: () => Promise<void>;
};

function asPrivate(host: P2PDraftHost): PrivateHost {
  return host as unknown as PrivateHost;
}

/** The three adapter methods `applyPick` reaches, plus a per-seat view stub. */
function stubAdapter(viewForSeat: (seat: number) => Record<string, unknown>) {
  return {
    submitPickForSeat: vi.fn(async () => ({ status: "Drafting" })),
    getViewForSeat: vi.fn(async (seat: number) => viewForSeat(seat)),
    allPicksSubmitted: vi.fn(async () => false),
    submitDeckForSeat: vi.fn(async () => ({ status: "Deckbuilding" })),
  } as unknown as Record<string, ReturnType<typeof vi.fn>>;
}

/**
 * The host's executable producer census.  Each row invokes the named concrete
 * host operation against the real `P2PDraftHost` method; it is intentionally
 * not a detached assertion about `requireDraft27HostValue`.  The producer is
 * fed a compatible legacy view once (the old first continuation must remain
 * reachable), then each raw refusal order below.  The listed continuation is
 * the first effect that must stay behind that producer's immediate return
 * admission seam.
 */
const rawHostRefusals = [
  ["FIFO flow", { flow: "FifoAsync", status: "Drafting" }],
  ["Review", { status: "Review" }],
  ["residual FIFO", { status: "Drafting", fifo: { cursor: 1 } }],
] as const;

function censusView(status = "Deckbuilding"): Record<string, unknown> {
  return {
    status,
    seats: [],
    pairings: [],
    pool: [],
    current_round: 0,
    pick_number: 0,
    current_pack: null,
    required_pick_count: 0,
  };
}

type CensusFixture = {
  host: P2PDraftHost;
  privateHost: PrivateHost;
  producer: ReturnType<typeof vi.fn>;
  admit: ReturnType<typeof vi.fn>;
  persist: ReturnType<typeof vi.fn>;
  emit: ReturnType<typeof vi.fn>;
  reconnectSession: ReturnType<typeof censusSession> | null;
  returnValue: unknown;
};

function invokeHostOperation(host: object, method: string, ...args: unknown[]): Promise<unknown> {
  const operation = Reflect.get(host, method);
  if (typeof operation !== "function") throw new Error(`Missing host census operation: ${method}`);
  return Promise.resolve(Reflect.apply(operation, host, args));
}

function censusFixture(
  producerName: string,
  producerResult: Record<string, unknown>,
  leadingResults: readonly Record<string, unknown>[] = [],
): CensusFixture {
  const host = newHost();
  const privateHost = asPrivate(host);
  const queuedResults = [...leadingResults];
  const producer = producerName === "buildLobbyView"
    ? vi.fn(() => queuedResults.shift() ?? producerResult)
    : vi.fn(async () => queuedResults.shift() ?? producerResult);
  const fallback = vi.fn(async () => censusView());
  const adapter = new Proxy<Record<string, ReturnType<typeof vi.fn>>>({
    [producerName]: producer,
  }, {
    get(target, property) {
      if (typeof property !== "string") return undefined;
      if (property in target) return target[property];
      if (property === "allPicksSubmitted") return vi.fn(async () => false);
      if (property === "exportSession") return vi.fn(async () => "{}");
      return fallback;
    },
  });
  const persist = vi.fn(async () => {});
  const emit = vi.fn();
  privateHost.adapter = adapter;
  Reflect.set(privateHost, "persistSessionStrict", persist);
  Reflect.set(privateHost, "persistSession", persist);
  Reflect.set(privateHost, "emit", emit);
  const originalAdmit = Reflect.get(privateHost, "admitHostView");
  if (typeof originalAdmit !== "function") throw new Error("Missing host-view admission seam");
  const admit = vi.fn((view: unknown) => Reflect.apply(originalAdmit, privateHost, [view]));
  Reflect.set(privateHost, "admitHostView", admit);
  if (producerName === "buildLobbyView") {
    Reflect.set(privateHost, "buildLobbyView", producer);
  }
  return {
    host,
    privateHost,
    producer,
    admit,
    persist,
    emit,
    reconnectSession: null,
    returnValue: undefined,
  };
}

function reconnectSessionFor(fixture: CensusFixture): ReturnType<typeof censusSession> {
  if (!fixture.reconnectSession) throw new Error("Missing reconnect census session");
  return fixture.reconnectSession;
}

function clearCensusReconnectGrace(privateHost: PrivateHost): void {
  const clearReconnectGrace = Reflect.get(privateHost, "clearReconnectGrace");
  if (typeof clearReconnectGrace !== "function") {
    throw new Error("Missing reconnect-grace cleanup seam");
  }
  Reflect.apply(clearReconnectGrace, privateHost, [2]);
}

type ProducerCensusRow = {
  site: string;
  producer: string;
  firstContinuation: string;
  legacyResult?: Record<string, unknown>;
  /** Values consumed before the named later site is reached. */
  leadingResults?: readonly Record<string, unknown>[];
  /** This legacy path intentionally proves its compensating continuation before surfacing the error. */
  legacyError?: string;
  setup: (fixture: CensusFixture) => void;
  invoke: (fixture: CensusFixture) => Promise<unknown>;
  legacyReach: (fixture: CensusFixture) => void | Promise<void>;
  refusalFence: (fixture: CensusFixture) => void;
};

const censusSession = () => ({
  send: vi.fn(),
  close: vi.fn(),
  onDisconnect: vi.fn((_listener: () => void) => vi.fn()),
  onMessage: vi.fn(),
});

function restoredCensusSession(extra: Record<string, unknown> = {}) {
  return {
    persistenceId: "draft-1", roomCode: "ABCDE", kind: "Premier", podSize: 4,
    hostDisplayName: "Host", tournamentFormat: "Swiss", podPolicy: "Competitive",
    seatTokens: { 0: "host" }, seatNames: { 0: "Host" }, kickedTokens: [],
    draftStarted: true, draftCode: "draft-1", draftSessionJson: "{}",
    poolInput: { type: "Set", data: { pools: [{ code: "TST" }], sequence: ["TST"] } },
    ...extra,
  };
}

/**
 * This is a source-derived, executable map: every row denotes one lexical
 * adapter return in P2PDraftHost. `leadingResults` is deliberately explicit
 * where a target is a later call in the same real operation; it prevents a
 * refusal from being caught at an earlier representative call.
 */
const P2P_DRAFT_HOST_PRODUCER_CENSUS: readonly ProducerCensusRow[] = [
  {
    site: "getHostView: pre-start buildLobbyView",
    producer: "buildLobbyView",
    firstContinuation: "public host-view return",
    setup: ({ privateHost }) => { privateHost.draftStarted = false; },
    invoke: ({ host }) => host.getHostView(),
    legacyReach: ({ producer }) => expect(producer).toHaveBeenCalledOnce(),
    refusalFence: ({ persist, emit }) => {
      expect(persist).not.toHaveBeenCalled();
      expect(emit).not.toHaveBeenCalled();
    },
  },
  {
    site: "admitNewGuest: welcome buildLobbyView",
    producer: "buildLobbyView",
    firstContinuation: "draft_welcome delivery",
    setup: ({ privateHost }) => { privateHost.draftStarted = false; },
    invoke: ({ privateHost }) => invokeHostOperation(privateHost, "admitNewGuest", censusSession(), "Guest", () => true),
    legacyReach: ({ producer, privateHost }) => {
      expect(producer).toHaveBeenCalledOnce();
      expect(privateHost.guestSessions.get(1)?.send).toHaveBeenCalledWith(expect.objectContaining({ type: "draft_welcome" }));
    },
    refusalFence: ({ persist, privateHost }) => {
      // The recovery token is durably saved before this raw view fence; only
      // the externally visible welcome must stay unreachable.
      expect(persist).toHaveBeenCalledOnce();
      expect(privateHost.guestSessions.get(1)?.send).not.toHaveBeenCalledWith(expect.objectContaining({ type: "draft_welcome" }));
    },
  },
  {
    site: "handleReconnect: pre-start acknowledgement buildLobbyView",
    producer: "buildLobbyView",
    firstContinuation: "draft_reconnect_ack delivery",
    setup: (fixture) => {
      const { privateHost } = fixture;
      privateHost.draftStarted = false;
      Reflect.set(privateHost, "seatTokens", new Map([[2, "token-2"]]));
      Reflect.set(privateHost, "seatNames", new Map([[0, "Host"], [2, "Guest"]]));
      Reflect.set(privateHost, "disconnectedSeats", new Map([[2, { deadlineAt: Date.now() + 10_000, timer: null }]]));
      fixture.reconnectSession = censusSession();
    },
    invoke: (fixture) => invokeHostOperation(fixture.privateHost, "handleReconnect", reconnectSessionFor(fixture), "token-2"),
    legacyReach: (fixture) => {
      const { producer } = fixture;
      expect(producer).toHaveBeenCalledOnce();
      expect(reconnectSessionFor(fixture).send).toHaveBeenCalledWith(expect.objectContaining({ type: "draft_reconnect_ack" }));
    },
    refusalFence: (fixture) => {
      const { persist } = fixture;
      expect(persist).toHaveBeenCalled();
      expect(reconnectSessionFor(fixture).send).not.toHaveBeenCalledWith(expect.objectContaining({ type: "draft_reconnect_ack" }));
    },
  },
  {
    site: "startDraftInner: createMultiplayerDraft",
    producer: "createMultiplayerDraft",
    firstContinuation: "draft identity, bot resolution, persistence, and start publication",
    setup: () => {},
    invoke: ({ host }) => host.startDraft(false),
    legacyReach: ({ producer, persist }) => {
      expect(producer).toHaveBeenCalledOnce();
      expect(persist).toHaveBeenCalledOnce();
    },
    refusalFence: ({ persist, emit }) => {
      expect(persist).not.toHaveBeenCalled();
      expect(emit).not.toHaveBeenCalled();
    },
  },
  {
    site: "startDraftInner: initial host getViewForSeat",
    producer: "getViewForSeat",
    firstContinuation: "bot work, guest state update, draftStarted event, and timer",
    setup: () => {},
    invoke: ({ host }) => host.startDraft(false),
    legacyReach: ({ producer, persist }) => {
      expect(producer).toHaveBeenCalled();
      expect(persist).toHaveBeenCalledOnce();
    },
    refusalFence: ({ persist, emit }) => {
      expect(persist).not.toHaveBeenCalled();
      expect(emit).not.toHaveBeenCalled();
    },
  },
  {
    site: "startDraftInner: each guest getViewForSeat",
    producer: "getViewForSeat",
    firstContinuation: "that guest's draft_state_update delivery",
    // The initial host read must be compatible; the target is the per-guest
    // read, not a refusal that exits before the loop begins.
    leadingResults: [censusView()],
    setup: ({ privateHost }) => { privateHost.guestSessions.set(2, censusSession()); },
    invoke: ({ host }) => host.startDraft(false),
    legacyReach: ({ producer, privateHost }) => {
      expect(producer).toHaveBeenCalledTimes(3);
      expect(privateHost.guestSessions.get(2)?.send).toHaveBeenCalledWith(expect.objectContaining({ type: "draft_state_update" }));
    },
    refusalFence: ({ persist, privateHost }) => {
      expect(persist).toHaveBeenCalledOnce();
      expect(privateHost.guestSessions.get(2)?.send).not.toHaveBeenCalledWith(expect.objectContaining({ type: "draft_state_update" }));
    },
  },
  {
    site: "startDraftInner: fresh host getViewForSeat",
    producer: "getViewForSeat",
    firstContinuation: "draftStarted event and timer setup",
    leadingResults: [censusView(), censusView()],
    setup: ({ privateHost }) => { privateHost.guestSessions.set(2, censusSession()); },
    invoke: ({ host }) => host.startDraft(false),
    legacyReach: ({ producer, emit }) => {
      expect(producer).toHaveBeenCalledTimes(3);
      expect(emit).toHaveBeenCalledWith(expect.objectContaining({ type: "draftStarted" }));
    },
    refusalFence: ({ persist, emit }) => {
      expect(persist).toHaveBeenCalledOnce();
      expect(emit).not.toHaveBeenCalledWith(expect.objectContaining({ type: "draftStarted" }));
    },
  },
  {
    site: "handlePick / applyPick: submitPickForSeat",
    producer: "submitPickForSeat",
    firstContinuation: "persistence, pick acknowledgement, event, bot work, and broadcast",
    setup: ({ privateHost }) => {
      privateHost.draftStarted = true;
      privateHost.paused = false;
      privateHost.guestSessions.set(2, { send: vi.fn() });
    },
    invoke: ({ privateHost }) => privateHost.handleGuestMessage(2, { type: "draft_pick", cardInstanceIds: ["card-1"] }),
    legacyReach: ({ producer, persist }) => {
      expect(producer).toHaveBeenCalledOnce();
      expect(persist).toHaveBeenCalledOnce();
    },
    refusalFence: ({ persist, privateHost }) => {
      expect(persist).not.toHaveBeenCalled();
      expect(privateHost.picksThisRound.has(2)).toBe(false);
    },
  },
  {
    site: "handlePickWithDraftEffect / applyPick: submitPickWithDraftEffectForSeat",
    producer: "submitPickWithDraftEffectForSeat",
    firstContinuation: "persistence, acknowledgement, event, bot work, and broadcast",
    setup: ({ privateHost }) => {
      privateHost.draftStarted = true;
      privateHost.paused = false;
      privateHost.guestSessions.set(2, { send: vi.fn() });
    },
    invoke: ({ privateHost }) => privateHost.handleGuestMessage(2, {
      type: "draft_pick_with_draft_effect", effectCardInstanceId: "effect-1", cardInstanceIds: ["card-1"],
    }),
    legacyReach: ({ producer, persist }) => {
      expect(producer).toHaveBeenCalledOnce();
      expect(persist).toHaveBeenCalledOnce();
    },
    refusalFence: ({ persist, privateHost }) => {
      expect(persist).not.toHaveBeenCalled();
      expect(privateHost.picksThisRound.has(2)).toBe(false);
    },
  },
  {
    site: "applyWorkspaceUpdate: getViewForSeat",
    producer: "getViewForSeat",
    firstContinuation: "workspace reconciliation and persistence",
    setup: ({ privateHost }) => { privateHost.guestSessions.set(2, { send: vi.fn() }); },
    invoke: ({ privateHost }) => privateHost.handleGuestMessage(2, {
      type: "draft_workspace_update", workspaceState: { schemaVersion: 1, placements: {}, virtualBasics: [] },
    }),
    legacyReach: ({ producer }) => expect(producer).toHaveBeenCalledOnce(),
    refusalFence: ({ persist }) => expect(persist).not.toHaveBeenCalled(),
  },
  {
    site: "applyPick: all-picked post-round host getViewForSeat",
    producer: "getViewForSeat",
    firstContinuation: "draftComplete/timer decision after round completion",
    leadingResults: [censusView()], // the preceding broadcast host read
    setup: ({ privateHost }) => {
      privateHost.draftStarted = true;
      privateHost.adapter.allPicksSubmitted = vi.fn(async () => true);
    },
    invoke: ({ privateHost }) => invokeHostOperation(privateHost, "applyPick", 0, ["card-1"], {
      acknowledge: false, emit: false, persist: false, resolveBots: false,
    }),
    legacyReach: ({ producer, emit }) => {
      expect(producer).toHaveBeenCalledTimes(3); // broadcast, post-round, direct return
      expect(emit).toHaveBeenCalledWith(expect.objectContaining({ type: "draftComplete" }));
    },
    refusalFence: ({ persist, emit }) => {
      expect(persist).not.toHaveBeenCalled();
      expect(emit).not.toHaveBeenCalledWith(expect.objectContaining({ type: "draftComplete" }));
    },
  },
  {
    site: "applyPick: final direct host getViewForSeat return",
    producer: "getViewForSeat",
    firstContinuation: "host pick return value",
    setup: ({ privateHost }) => {
      privateHost.draftStarted = true;
      privateHost.adapter.allPicksSubmitted = vi.fn(async () => false);
    },
    invoke: ({ privateHost }) => invokeHostOperation(privateHost, "applyPick", 0, ["card-1"], {
      acknowledge: false, emit: false, persist: false, resolveBots: false,
    }),
    legacyReach: ({ producer }) => expect(producer).toHaveBeenCalledOnce(),
    refusalFence: ({ persist }) => expect(persist).not.toHaveBeenCalled(),
  },
  {
    site: "applyPick: final non-host getViewForSeat return",
    producer: "getViewForSeat",
    firstContinuation: "guest pick's public host-view return",
    setup: ({ privateHost }) => {
      privateHost.draftStarted = true;
      privateHost.adapter.allPicksSubmitted = vi.fn(async () => false);
    },
    invoke: async (fixture) => {
      fixture.returnValue = await invokeHostOperation(fixture.privateHost, "applyPick", 2, ["card-1"], {
        acknowledge: false, emit: false, persist: false, resolveBots: false,
      });
      return fixture.returnValue;
    },
    legacyReach: ({ producer, returnValue }) => {
      expect(producer).toHaveBeenCalledOnce();
      expect(returnValue).toEqual(censusView());
    },
    refusalFence: ({ persist, returnValue }) => {
      expect(persist).not.toHaveBeenCalled();
      // The non-host branch must not leak an unadmitted raw result through its
      // public return. If its immediate await gate is removed, this is the
      // FIFO/Review/residual-FIFO value itself rather than `undefined`.
      expect(returnValue).toBeUndefined();
    },
  },
  {
    site: "broadcastViews: guest getViewForSeat",
    producer: "getViewForSeat",
    firstContinuation: "state delivery, local view event, auto/bot pick, and timer continuation",
    setup: ({ privateHost }) => {
      privateHost.draftStarted = true;
      privateHost.guestSessions.set(2, { send: vi.fn() });
      Reflect.set(privateHost, "activePodSize", 1);
    },
    invoke: ({ privateHost }) => invokeHostOperation(privateHost, "broadcastViews"),
    legacyReach: ({ producer, privateHost }) => {
      expect(producer).toHaveBeenCalledTimes(2);
      expect(privateHost.guestSessions.get(2)?.send).toHaveBeenCalled();
    },
    refusalFence: ({ persist, privateHost }) => {
      expect(persist).not.toHaveBeenCalled();
      expect(privateHost.guestSessions.get(2)?.send).not.toHaveBeenCalled();
    },
  },
  {
    site: "broadcastViews: host getViewForSeat",
    producer: "getViewForSeat",
    firstContinuation: "local viewUpdated event",
    leadingResults: [censusView()],
    setup: ({ privateHost }) => {
      privateHost.draftStarted = true;
      privateHost.guestSessions.set(2, censusSession());
    },
    invoke: ({ privateHost }) => invokeHostOperation(privateHost, "broadcastViews"),
    legacyReach: ({ producer, emit }) => {
      expect(producer).toHaveBeenCalledTimes(2);
      expect(emit).toHaveBeenCalledWith(expect.objectContaining({ type: "viewUpdated" }));
    },
    refusalFence: ({ persist, emit }) => {
      expect(persist).not.toHaveBeenCalled();
      expect(emit).not.toHaveBeenCalledWith(expect.objectContaining({ type: "viewUpdated" }));
    },
  },
  {
    site: "public getHostView after draft start",
    producer: "getViewForSeat",
    firstContinuation: "public host-view return",
    setup: ({ privateHost }) => { privateHost.draftStarted = true; },
    invoke: ({ host }) => host.getHostView(),
    legacyReach: ({ producer }) => expect(producer).toHaveBeenCalledOnce(),
    refusalFence: ({ persist, emit }) => {
      expect(persist).not.toHaveBeenCalled();
      expect(emit).not.toHaveBeenCalled();
    },
  },
  {
    site: "autoPickAllPending: per-seat getViewForSeat",
    producer: "getViewForSeat",
    firstContinuation: "whole-step auto pick, bot resolution, broadcast, and timer continuation",
    legacyResult: {
      ...censusView("Drafting"),
      current_pack: [{ instance_id: "auto-card" }],
      required_pick_count: 1,
    },
    setup: ({ privateHost }) => {
      privateHost.draftStarted = true;
      Reflect.set(privateHost, "activePodSize", 1);
    },
    invoke: ({ privateHost }) => invokeHostOperation(privateHost, "autoPickAllPending"),
    legacyReach: ({ producer }) => expect(producer).toHaveBeenCalled(),
    refusalFence: ({ persist, privateHost }) => {
      expect(persist).not.toHaveBeenCalled();
      expect(privateHost.picksThisRound.has(0)).toBe(false);
    },
  },
  {
    site: "resolveBotPicks: host getViewForSeat",
    producer: "getViewForSeat",
    firstContinuation: "bot-resolution scan",
    setup: ({ privateHost }) => {
      privateHost.draftStarted = true;
      Reflect.set(privateHost, "activePodSize", 2);
    },
    invoke: ({ privateHost }) => invokeHostOperation(privateHost, "resolveBotPicks", { emit: true, persist: true }),
    legacyReach: ({ producer }) => expect(producer).toHaveBeenCalledOnce(),
    refusalFence: ({ persist }) => expect(persist).not.toHaveBeenCalled(),
  },
  {
    site: "resolveBotPicks: bot-seat getViewForSeat",
    producer: "getViewForSeat",
    firstContinuation: "bot pick submission",
    leadingResults: [{ ...censusView("Drafting"), seats: [{ seat_index: 1, is_bot: true }] }],
    setup: ({ privateHost }) => { privateHost.draftStarted = true; },
    invoke: ({ privateHost }) => invokeHostOperation(privateHost, "resolveBotPicks", { emit: true, persist: true }),
    legacyReach: ({ producer }) => expect(producer).toHaveBeenCalledTimes(2),
    refusalFence: ({ persist }) => expect(persist).not.toHaveBeenCalled(),
  },
  {
    site: "reconcileEffectivePause: competitive timer-resume host getViewForSeat",
    producer: "getViewForSeat",
    firstContinuation: "pick timer start",
    legacyResult: censusView("Drafting"),
    setup: ({ privateHost }) => {
      privateHost.draftStarted = true;
      Reflect.set(privateHost, "paused", true);
      Reflect.set(privateHost, "podPolicy", "Competitive");
      Reflect.set(privateHost, "startPickTimer", vi.fn());
    },
    invoke: ({ privateHost }) => invokeHostOperation(privateHost, "reconcileEffectivePause"),
    legacyReach: async ({ producer, privateHost }) => {
      expect(producer).toHaveBeenCalledOnce();
      await vi.waitFor(() => expect(Reflect.get(privateHost, "startPickTimer")).toHaveBeenCalledOnce());
    },
    refusalFence: ({ persist, privateHost }) => {
      expect(persist).not.toHaveBeenCalled();
      expect(Reflect.get(privateHost, "startPickTimer")).not.toHaveBeenCalled();
    },
  },
  {
    site: "handleGuestMessage: new network submitDeckForSeat",
    producer: "submitDeckForSeat",
    firstContinuation: "receipt, acknowledgement, command clearing, event, broadcast, and pairing",
    setup: ({ privateHost }) => {
      privateHost.draftStarted = true;
      privateHost.guestSessions.set(2, { send: vi.fn() });
    },
    invoke: ({ privateHost }) => privateHost.handleGuestMessage(2, {
      type: "draft_submit_deck", mainDeck: ["Island"], commanders: [], submissionId: "census-deck",
    }),
    legacyReach: ({ producer, persist }) => {
      expect(producer).toHaveBeenCalled();
      expect(persist).toHaveBeenCalled();
    },
    refusalFence: ({ persist, privateHost }) => {
      expect(persist).not.toHaveBeenCalled();
      expect(privateHost.guestSessions.get(2)?.send).toHaveBeenCalledWith(expect.objectContaining({
        type: "draft_error", submissionDisposition: "Retryable",
      }));
    },
  },
  {
    site: "handleDeckSubmission: replay getViewForSeat",
    producer: "getViewForSeat",
    firstContinuation: "replayed receipt acknowledgement",
    setup: ({ privateHost }) => {
      privateHost.draftStarted = true;
      privateHost.guestSessions.set(2, censusSession());
      Reflect.set(privateHost, "deckSubmissionReceipts", new Map([["census-replay", {
        seat: 2, payloadFingerprint: "[[\"Island\",1]]|[]",
      }]]));
    },
    invoke: ({ privateHost }) => privateHost.handleGuestMessage(2, {
      type: "draft_submit_deck", mainDeck: ["Island"], commanders: [], submissionId: "census-replay",
    }),
    legacyReach: ({ producer, privateHost }) => {
      expect(producer).toHaveBeenCalled();
      expect(privateHost.guestSessions.get(2)?.send).toHaveBeenCalledWith(expect.objectContaining({ type: "draft_deck_submit_ack" }));
    },
    refusalFence: ({ persist, privateHost }) => {
      expect(persist).not.toHaveBeenCalled();
      expect(privateHost.guestSessions.get(2)?.send).toHaveBeenCalledWith(expect.objectContaining({
        type: "draft_error", submissionDisposition: "Retryable",
      }));
    },
  },
  {
    site: "publishAcceptedDeckSubmission: host getViewForSeat",
    producer: "getViewForSeat",
    firstContinuation: "deckSubmitted publication and pairing decision",
    legacyResult: {
      ...censusView(), seats: [{ seat_index: 0, has_submitted_deck: false, is_bot: false }],
    },
    setup: ({ privateHost }) => {
      privateHost.draftStarted = true;
    },
    invoke: ({ privateHost }) => privateHost.submitHostDeck(["Island"], []),
    legacyReach: ({ producer, emit }) => {
      expect(producer).toHaveBeenCalledOnce();
      expect(emit).toHaveBeenCalledWith(expect.objectContaining({ type: "deckSubmitted" }));
    },
    refusalFence: ({ persist, emit }) => {
      // The reducer's snapshot is saved before the accepted-publication read;
      // the fence specifically bars the downstream event/pairing path.
      expect(persist).toHaveBeenCalledOnce();
      expect(emit).not.toHaveBeenCalledWith(expect.objectContaining({ type: "deckSubmitted" }));
    },
  },
  {
    site: "handleDeckSubmission: final guest host getViewForSeat return",
    producer: "getViewForSeat",
    firstContinuation: "guest submission's public host-view return",
    leadingResults: [{
      ...censusView(), seats: [{ seat_index: 0, has_submitted_deck: false, is_bot: false }],
    }], // accepted-publication host read
    setup: ({ privateHost }) => {
      privateHost.draftStarted = true;
      privateHost.guestSessions.set(2, censusSession());
      privateHost.adapter.submitDeckForSeat = vi.fn(async () => ({
        ...censusView(), seats: [{ seat_index: 0, has_submitted_deck: false, is_bot: false }],
      }));
    },
    invoke: ({ privateHost }) => privateHost.handleGuestMessage(2, {
      type: "draft_submit_deck", mainDeck: ["Island"], commanders: [], submissionId: "census-final-guest-view",
    }),
    legacyReach: ({ producer }) => expect(producer).toHaveBeenCalledTimes(2),
    refusalFence: ({ persist, emit }) => {
      expect(persist).toHaveBeenCalledOnce();
      // The accepted-publication read precedes this final return read. Its
      // event remains reachable; the raw value fences only the later return.
      expect(emit).toHaveBeenCalledWith(expect.objectContaining({ type: "deckSubmitted" }));
    },
  },
  {
    site: "handleGuestLeave: live setSeatConnected(false)",
    producer: "setSeatConnected",
    firstContinuation: "durable leave/grace mutation, acknowledgement, broadcast, and pause reconciliation",
    setup: ({ privateHost }) => {
      privateHost.draftStarted = true;
      Reflect.set(privateHost, "seatTokens", new Map([[2, "token-2"]]));
      Reflect.set(privateHost, "seatNames", new Map([[0, "Host"], [2, "Guest"]]));
      privateHost.guestSessions.set(2, {
        send: vi.fn(), close: vi.fn(), onDisconnect: vi.fn(() => vi.fn()),
      } as never);
    },
    invoke: ({ privateHost }) => invokeHostOperation(privateHost, "handleGuestLeave", 2, "token-2", privateHost.guestSessions.get(2)),
    legacyReach: ({ producer, persist }) => {
      expect(producer).toHaveBeenCalled();
      expect(persist).toHaveBeenCalledOnce();
    },
    refusalFence: ({ persist, privateHost }) => {
      expect(persist).not.toHaveBeenCalled();
      expect(privateHost.guestSessions.get(2)?.send).not.toHaveBeenCalledWith(expect.objectContaining({ type: "draft_leave_ack" }));
    },
  },
  {
    site: "handleGuestLeave: failed-leave compensation setSeatConnected(true)",
    producer: "setSeatConnected",
    firstContinuation: "restored live-seat state after failed leave",
    leadingResults: [censusView()],
    legacyError: "forced snapshot failure",
    setup: ({ privateHost, persist }) => {
      privateHost.draftStarted = true;
      Reflect.set(privateHost, "seatTokens", new Map([[2, "token-2"]]));
      Reflect.set(privateHost, "seatNames", new Map([[0, "Host"], [2, "Guest"]]));
      privateHost.guestSessions.set(2, censusSession());
      persist.mockRejectedValueOnce(new Error("forced snapshot failure"));
    },
    invoke: ({ privateHost }) => invokeHostOperation(privateHost, "handleGuestLeave", 2, "token-2", privateHost.guestSessions.get(2)),
    legacyReach: ({ producer, privateHost }) => {
      expect(producer).toHaveBeenCalledTimes(2);
      expect(privateHost.guestSessions.get(2)).toBeDefined();
    },
    refusalFence: ({ privateHost }) => expect(privateHost.guestSessions.get(2)?.send).not.toHaveBeenCalledWith(expect.objectContaining({ type: "draft_leave_ack" })),
  },
  {
    site: "handleGuestLeave: session-ended recovery setSeatConnected(false)",
    producer: "setSeatConnected",
    firstContinuation: "closed-session grace recovery, durable snapshot, and effective pause",
    // The attempted leave's disconnect is compatible. The session closes
    // while its snapshot fails, so the second producer is the distinct
    // session-ended recovery rather than the live-seat compensation above.
    leadingResults: [censusView()],
    legacyError: "forced leave snapshot failure",
    setup: (fixture) => {
      const { privateHost, persist } = fixture;
      privateHost.draftStarted = true;
      Reflect.set(privateHost, "seatTokens", new Map([[2, "token-2"]]));
      Reflect.set(privateHost, "seatNames", new Map([[0, "Host"], [2, "Guest"]]));
      const session = censusSession();
      session.onDisconnect.mockImplementation((listener: () => void) => {
        listener();
        return vi.fn();
      });
      privateHost.guestSessions.set(2, session);
      persist.mockRejectedValueOnce(new Error("forced leave snapshot failure"));
      Reflect.set(privateHost, "reportDetachedMutationFailure", vi.fn());
    },
    invoke: ({ privateHost }) => invokeHostOperation(privateHost, "handleGuestLeave", 2, "token-2", privateHost.guestSessions.get(2)),
    legacyReach: ({ privateHost, producer, persist }) => {
      expect(producer).toHaveBeenCalledTimes(2);
      expect(persist).toHaveBeenCalledTimes(2);
      expect(privateHost.guestSessions.has(2)).toBe(false);
      expect((Reflect.get(privateHost, "disconnectedSeats") as Map<number, unknown>).has(2)).toBe(true);
      expect(Reflect.get(privateHost, "reportDetachedMutationFailure")).not.toHaveBeenCalled();
      clearCensusReconnectGrace(privateHost);
    },
    refusalFence: ({ privateHost, producer, persist }) => {
      expect(producer).toHaveBeenCalledTimes(2);
      expect(persist).toHaveBeenCalledTimes(2);
      expect(privateHost.guestSessions.has(2)).toBe(false);
      expect((Reflect.get(privateHost, "disconnectedSeats") as Map<number, unknown>).has(2)).toBe(true);
      // The raw result is reported by the real closed-session recovery catch;
      // removing the immediate return gate would turn it into a silent,
      // unadmitted view and skip this recovery error path.
      expect(Reflect.get(privateHost, "reportDetachedMutationFailure")).toHaveBeenCalledWith(
        "leave disconnect recovery",
        expect.anything(),
      );
      clearCensusReconnectGrace(privateHost);
    },
  },
  {
    site: "handleGuestDisconnect: setSeatConnected(false)",
    producer: "setSeatConnected",
    firstContinuation: "durable disconnect snapshot and pause publication",
    setup: ({ privateHost }) => {
      privateHost.draftStarted = true;
      privateHost.guestSessions.set(2, censusSession());
    },
    invoke: ({ privateHost }) => invokeHostOperation(privateHost, "handleGuestDisconnect", 2),
    legacyReach: ({ producer, privateHost }) => {
      expect(producer).toHaveBeenCalledOnce();
      expect(privateHost.guestSessions.has(2)).toBe(false);
    },
    refusalFence: ({ persist, privateHost }) => {
      expect(persist).not.toHaveBeenCalled();
      expect(privateHost.guestSessions.has(2)).toBe(false);
    },
  },
  {
    site: "handleReconnect: live setSeatConnected(true)",
    producer: "setSeatConnected",
    firstContinuation: "reconnect acknowledgement",
    setup: (fixture) => {
      const { privateHost } = fixture;
      privateHost.draftStarted = true;
      Reflect.set(privateHost, "seatTokens", new Map([[2, "token-2"]]));
      Reflect.set(privateHost, "seatNames", new Map([[0, "Host"], [2, "Guest"]]));
      Reflect.set(privateHost, "disconnectedSeats", new Map([[2, { deadlineAt: Date.now() + 10_000, timer: null }]]));
      fixture.reconnectSession = censusSession();
    },
    invoke: (fixture) => invokeHostOperation(fixture.privateHost, "handleReconnect", reconnectSessionFor(fixture), "token-2"),
    legacyReach: (fixture) => {
      const { producer } = fixture;
      expect(producer).toHaveBeenCalledOnce();
      expect(reconnectSessionFor(fixture).send).toHaveBeenCalledWith(expect.objectContaining({ type: "draft_reconnect_ack" }));
    },
    refusalFence: (fixture) => {
      const { persist } = fixture;
      // The rejected provisional engine update rolls back through the durable
      // reconnect record, but must never acknowledge the rejected session.
      expect(persist).toHaveBeenCalledOnce();
      expect(reconnectSessionFor(fixture).send).not.toHaveBeenCalledWith(expect.objectContaining({ type: "draft_reconnect_ack" }));
    },
  },
  {
    site: "handleReconnect: draft-started acknowledgement getViewForSeat",
    producer: "getViewForSeat",
    firstContinuation: "draft_reconnect_ack delivery",
    setup: (fixture) => {
      const { privateHost } = fixture;
      privateHost.draftStarted = true;
      Reflect.set(privateHost, "seatTokens", new Map([[2, "token-2"]]));
      Reflect.set(privateHost, "seatNames", new Map([[0, "Host"], [2, "Guest"]]));
      Reflect.set(privateHost, "disconnectedSeats", new Map([[2, { deadlineAt: Date.now() + 10_000, timer: null }]]));
      fixture.reconnectSession = censusSession();
    },
    invoke: (fixture) => invokeHostOperation(fixture.privateHost, "handleReconnect", reconnectSessionFor(fixture), "token-2"),
    legacyReach: (fixture) => {
      const { producer } = fixture;
      // The acknowledged reconnect is followed by its own guest+host view
      // publication. This first producer result remains the admitted ack.
      expect(producer).toHaveBeenCalledTimes(3);
      expect(reconnectSessionFor(fixture).send).toHaveBeenCalledWith(expect.objectContaining({ type: "draft_reconnect_ack" }));
    },
    refusalFence: (fixture) => {
      const { persist } = fixture;
      expect(persist).toHaveBeenCalled();
      expect(reconnectSessionFor(fixture).send).not.toHaveBeenCalledWith(expect.objectContaining({ type: "draft_reconnect_ack" }));
    },
  },
  {
    site: "handleReconnect: closed-while-reconnecting compensation setSeatConnected(false)",
    producer: "setSeatConnected",
    firstContinuation: "recovery of the original grace record",
    leadingResults: [censusView()],
    setup: ({ privateHost }) => {
      privateHost.draftStarted = true;
      Reflect.set(privateHost, "seatTokens", new Map([[2, "token-2"]]));
      Reflect.set(privateHost, "seatNames", new Map([[0, "Host"], [2, "Guest"]]));
      Reflect.set(privateHost, "disconnectedSeats", new Map([[2, { deadlineAt: Date.now() + 10_000, timer: null }]]));
    },
    invoke: ({ privateHost }) => invokeHostOperation(privateHost, "handleReconnect", {
      ...censusSession(), onDisconnect: vi.fn((listener: () => void) => { listener(); return vi.fn(); }),
    }, "token-2"),
    legacyReach: ({ producer }) => expect(producer).toHaveBeenCalledTimes(2),
    refusalFence: ({ persist }) => {
      // The compatible provisional reconnect is already durable; a rejected
      // compensation still persists the restored grace record twice, but it
      // never reaches a reconnect acknowledgement.
      expect(persist).toHaveBeenCalledTimes(2);
    },
  },
  {
    site: "handleReconnect: expired-grace rollback setSeatConnected(false)",
    producer: "setSeatConnected",
    firstContinuation: "durable expired-window rollback and NoReconnectWindow rejection",
    // The initial reconnect transition is compatible. The first persistence
    // fence removes the grace record so the next producer is specifically the
    // expired-grace rollback, not the live or closed-session compensation.
    leadingResults: [censusView()],
    setup: (fixture) => {
      const { privateHost, persist } = fixture;
      privateHost.draftStarted = true;
      Reflect.set(privateHost, "seatTokens", new Map([[2, "token-2"]]));
      Reflect.set(privateHost, "seatNames", new Map([[0, "Host"], [2, "Guest"]]));
      Reflect.set(privateHost, "disconnectedSeats", new Map([[2, { deadlineAt: Date.now() + 10_000, timer: null }]]));
      fixture.reconnectSession = censusSession();
      persist.mockImplementationOnce(async () => {
        (Reflect.get(privateHost, "disconnectedSeats") as Map<number, unknown>).delete(2);
      });
    },
    invoke: (fixture) => invokeHostOperation(fixture.privateHost, "handleReconnect", reconnectSessionFor(fixture), "token-2"),
    legacyReach: (fixture) => {
      const session = reconnectSessionFor(fixture);
      expect(fixture.producer).toHaveBeenCalledTimes(2);
      expect(fixture.persist).toHaveBeenCalledTimes(2);
      expect(session.send).toHaveBeenCalledWith(expect.objectContaining({
        type: "draft_reconnect_rejected", kind: "NoReconnectWindow",
      }));
      expect(session.close).toHaveBeenCalledWith("Reconnect window expired");
    },
    refusalFence: (fixture) => {
      const session = reconnectSessionFor(fixture);
      // The refused rollback goes directly to the outer recovery path. It
      // must not execute the expired-window rejection that follows the raw
      // return; the catch still closes the failed reconnect and persists its
      // own rollback record.
      expect(fixture.producer).toHaveBeenCalledTimes(3);
      expect(fixture.persist).toHaveBeenCalledTimes(2);
      expect(session.send).not.toHaveBeenCalledWith(expect.objectContaining({
        type: "draft_reconnect_rejected", kind: "NoReconnectWindow",
      }));
      expect(session.close).toHaveBeenCalledWith("Reconnect failed");
    },
  },
  {
    site: "handleReconnect: catch rollback setSeatConnected(false)",
    producer: "setSeatConnected",
    firstContinuation: "best-effort disconnected rollback before durable reconnect close",
    // Force the earlier connected transition through the real persistence
    // failure. The following adapter call is therefore the catch rollback,
    // not one of the normal reconnect branches.
    leadingResults: [censusView()],
    setup: (fixture) => {
      const { privateHost, persist } = fixture;
      privateHost.draftStarted = true;
      Reflect.set(privateHost, "seatTokens", new Map([[2, "token-2"]]));
      Reflect.set(privateHost, "seatNames", new Map([[0, "Host"], [2, "Guest"]]));
      Reflect.set(privateHost, "disconnectedSeats", new Map([[2, { deadlineAt: Date.now() + 10_000, timer: null }]]));
      fixture.reconnectSession = censusSession();
      persist.mockRejectedValueOnce(new Error("forced reconnect snapshot failure"));
    },
    invoke: (fixture) => invokeHostOperation(fixture.privateHost, "handleReconnect", reconnectSessionFor(fixture), "token-2"),
    legacyReach: (fixture) => {
      const session = reconnectSessionFor(fixture);
      expect(fixture.producer).toHaveBeenCalledTimes(2);
      expect(fixture.admit).toHaveBeenCalledTimes(2);
      expect(fixture.persist).toHaveBeenCalledTimes(2);
      expect(session.close).toHaveBeenCalledWith("Reconnect failed");
    },
    refusalFence: (fixture) => {
      const session = reconnectSessionFor(fixture);
      expect(fixture.producer).toHaveBeenCalledTimes(2);
      // This best-effort branch deliberately continues after an admission
      // failure, so its discriminating proof is that the exact catch-return
      // still entered the real admission seam before close/persistence.
      expect(fixture.admit).toHaveBeenCalledTimes(2);
      expect(fixture.persist).toHaveBeenCalledTimes(2);
      expect(session.close).toHaveBeenCalledWith("Reconnect failed");
      expect(session.send).not.toHaveBeenCalledWith(expect.objectContaining({ type: "draft_reconnect_ack" }));
    },
  },
  {
    site: "reconnect grace expiry: setSeatConnected(false)",
    producer: "setSeatConnected",
    firstContinuation: "persistence, disconnect-grace notification, and pause/seat publication",
    setup: ({ privateHost }) => {
      privateHost.draftStarted = true;
      Reflect.set(privateHost, "disconnectedSeats", new Map([[2, { deadlineAt: 0, timer: null }]]));
    },
    invoke: ({ privateHost }) => invokeHostOperation(privateHost, "expireReconnectGrace", 2),
    legacyReach: ({ producer, persist }) => {
      expect(producer).toHaveBeenCalledOnce();
      expect(persist).toHaveBeenCalledOnce();
    },
    refusalFence: ({ persist, emit }) => {
      expect(persist).not.toHaveBeenCalled();
      expect(emit).not.toHaveBeenCalledWith(expect.objectContaining({ type: "seatKicked" }));
    },
  },
  {
    site: "generatePairingsInner: generatePairings",
    producer: "generatePairings",
    firstContinuation: "persistence, launch, broadcast, pairing event, and local view event",
    setup: () => {},
    invoke: ({ host }) => host.generatePairings(),
    legacyReach: ({ producer, persist }) => {
      expect(producer).toHaveBeenCalledOnce();
      expect(persist).toHaveBeenCalled();
    },
    refusalFence: ({ persist, emit }) => {
      expect(persist).not.toHaveBeenCalled();
      expect(emit).toHaveBeenCalledWith(expect.objectContaining({ type: "error" }));
    },
  },
  {
    site: "generatePairingsInner: post-bot host getViewForSeat",
    producer: "getViewForSeat",
    firstContinuation: "pairing-launch loop",
    setup: ({ privateHost }) => {
      privateHost.adapter.generatePairings = vi.fn(async () => censusView());
    },
    invoke: ({ host }) => host.generatePairings(),
    legacyReach: ({ producer }) => expect(producer).toHaveBeenCalledTimes(3),
    refusalFence: ({ persist, emit }) => {
      expect(persist).toHaveBeenCalledOnce();
      expect(emit).toHaveBeenCalledWith(expect.objectContaining({ type: "error" }));
    },
  },
  {
    site: "generatePairingsInner: latest host getViewForSeat",
    producer: "getViewForSeat",
    firstContinuation: "durable latest snapshot and pairingsGenerated publication",
    leadingResults: [censusView()],
    setup: ({ privateHost }) => {
      privateHost.adapter.generatePairings = vi.fn(async () => censusView());
    },
    invoke: ({ host }) => host.generatePairings(),
    legacyReach: ({ producer, persist, emit }) => {
      expect(producer).toHaveBeenCalledTimes(3);
      expect(persist).toHaveBeenCalledTimes(2);
      expect(emit).toHaveBeenCalledWith(expect.objectContaining({ type: "pairingsGenerated" }));
    },
    refusalFence: ({ persist, emit }) => {
      expect(persist).toHaveBeenCalledOnce();
      expect(emit).toHaveBeenCalledWith(expect.objectContaining({ type: "error" }));
    },
  },
  {
    site: "reportMatchResult",
    producer: "reportMatchResult",
    firstContinuation: "persistence, result receipt/event, broadcast, local view, and cleanup",
    setup: () => {},
    invoke: ({ host }) => host.reportMatchResult("match-1", 0),
    legacyReach: ({ producer, persist }) => {
      expect(producer).toHaveBeenCalledOnce();
      expect(persist).toHaveBeenCalledOnce();
    },
    refusalFence: ({ persist }) => expect(persist).not.toHaveBeenCalled(),
  },
  {
    site: "match-settlement authorization read: getViewForSeat",
    producer: "getViewForSeat",
    firstContinuation: "settlement acceptance, durable intent, result receipt, and acknowledgement",
    legacyResult: {
      ...censusView(),
      current_round: 0,
      pairings: [{ match_id: "match-1", round: 0, seat_a: 2, seat_b: 3, status: "Pending" }],
    },
    setup: ({ privateHost }) => {
      Reflect.set(privateHost, "matchBindings", new Map([["match-1", {
        podId: "draft-1", matchId: "match-1", round: 0, sessionKey: "key", lease: "lease",
        nonce: "nonce", revision: 0, matchAuthoritySeat: 2,
      }]]));
      Reflect.set(privateHost, "podPolicy", "Casual");
      privateHost.guestSessions.set(2, { send: vi.fn() });
    },
    invoke: ({ privateHost }) => invokeHostOperation(privateHost, "acceptMatchSettlement", 2, {
      receiptId: "receipt-1", winnerSeat: 2,
      binding: {
        podId: "draft-1", matchId: "match-1", round: 0, sessionKey: "key", lease: "lease",
        nonce: "nonce", revision: 0, matchAuthoritySeat: 2,
      },
    }),
    legacyReach: ({ producer, persist }) => {
      expect(producer).toHaveBeenCalled();
      expect(persist).toHaveBeenCalled();
    },
    refusalFence: ({ persist }) => expect(persist).not.toHaveBeenCalled(),
  },
  {
    site: "between-games authorization read: getViewForSeat",
    producer: "getViewForSeat",
    firstContinuation: "durable between-games prompt and prompt delivery",
    legacyResult: {
      ...censusView(),
      current_round: 0,
      pairings: [{ match_id: "match-1", round: 0, seat_a: 2, seat_b: 3, status: "Pending" }],
    },
    setup: ({ privateHost }) => {
      Reflect.set(privateHost, "matchBindings", new Map([["match-1", {
        podId: "draft-1", matchId: "match-1", round: 0, sessionKey: "key", lease: "lease",
        nonce: "nonce", revision: 0, matchAuthoritySeat: 2,
      }]]));
      Reflect.set(privateHost, "podPolicy", "Casual");
      privateHost.guestSessions.set(2, { send: vi.fn() });
    },
    invoke: ({ privateHost }) => invokeHostOperation(privateHost, "handleGuestBetweenGames", 2, {
      matchId: "match-1", gameNumber: 1, score: { p0_wins: 0, p1_wins: 0 }, loserSeat: 3,
    }),
    legacyReach: ({ producer, persist }) => {
      expect(producer).toHaveBeenCalledOnce();
      expect(persist).toHaveBeenCalledOnce();
    },
    refusalFence: ({ persist }) => expect(persist).not.toHaveBeenCalled(),
  },
  {
    site: "ignored advanceRound return",
    producer: "advanceRound",
    firstContinuation: "persistence, roundAdvanced event, and pairing generation",
    setup: () => {},
    invoke: ({ host }) => host.advanceRound(),
    legacyReach: ({ producer, persist }) => {
      expect(producer).toHaveBeenCalledOnce();
      expect(persist).toHaveBeenCalled();
    },
    refusalFence: ({ persist, emit }) => {
      expect(persist).not.toHaveBeenCalled();
      expect(emit).not.toHaveBeenCalledWith(expect.objectContaining({ type: "roundAdvanced" }));
    },
  },
  {
    site: "ignored replaceSeatWithBot return",
    producer: "replaceSeatWithBot",
    firstContinuation: "grace/token/name mutation, persistence, broadcast, and pause reconciliation",
    setup: () => {},
    invoke: ({ host }) => host.replaceSeatWithBot(2),
    legacyReach: ({ producer, persist }) => {
      expect(producer).toHaveBeenCalledOnce();
      expect(persist).toHaveBeenCalledOnce();
    },
    refusalFence: ({ persist }) => expect(persist).not.toHaveBeenCalled(),
  },
  {
    site: "restoreFromPersisted: importSession",
    producer: "importSession",
    firstContinuation: "restored-seat effects, persistence, workspace/status/view publication, pairing launch, and install",
    setup: () => {},
    invoke: ({ host }) => host.restoreFromPersisted(restoredCensusSession() as never),
    legacyReach: ({ producer }) => expect(producer).toHaveBeenCalledOnce(),
    refusalFence: ({ persist, emit }) => {
      expect(persist).not.toHaveBeenCalled();
      expect(emit).not.toHaveBeenCalled();
    },
  },
  {
    site: "restoreFromPersisted: restored-expired-seat setSeatConnected(false)",
    producer: "setSeatConnected",
    firstContinuation: "restored-seat persistence, workspace/status/view publication, pairing launch, and install",
    setup: ({ privateHost }) => {
      privateHost.adapter.importSession = vi.fn(async () => censusView());
    },
    invoke: ({ host }) => host.restoreFromPersisted(restoredCensusSession({ expiredDisconnectedSeats: [2] }) as never),
    legacyReach: ({ producer }) => expect(producer).toHaveBeenCalledOnce(),
    refusalFence: ({ persist, emit }) => {
      expect(persist).not.toHaveBeenCalled();
      expect(emit).not.toHaveBeenCalled();
    },
  },
  {
    site: "restoreFromPersisted: Pairing return host getViewForSeat",
    producer: "getViewForSeat",
    firstContinuation: "recovered pairing read return",
    leadingResults: [censusView(), censusView(), censusView()],
    setup: ({ privateHost }) => {
      privateHost.adapter.importSession = vi.fn(async () => ({ ...censusView("Pairing"), pairings: [] }));
    },
    invoke: ({ host }) => host.restoreFromPersisted(restoredCensusSession() as never),
    legacyReach: ({ producer }) => expect(producer).toHaveBeenCalled(),
    refusalFence: ({ persist, emit }) => {
      // Recovery's grace snapshot plus pairing generation and its view
      // publication precede the final recovered return read; the raw value is
      // injected only at that return.
      expect(persist).toHaveBeenCalledTimes(3);
      expect(emit).toHaveBeenCalledWith(expect.objectContaining({ type: "viewUpdated" }));
    },
  },
  {
    site: "handleSharedStackDecision: submitSharedStackDecisionForSeat",
    producer: "submitSharedStackDecisionForSeat",
    firstContinuation: "persistence, decision acknowledgement, bot work, and broadcast",
    setup: ({ privateHost }) => {
      privateHost.draftStarted = true;
      privateHost.paused = false;
      privateHost.guestSessions.set(2, { send: vi.fn() });
    },
    invoke: ({ privateHost }) => privateHost.handleGuestMessage(2, { type: "draft_pile_decision", pile: 0, decision: "Take" }),
    legacyReach: async ({ producer, persist, privateHost }) => {
      expect(producer).toHaveBeenCalledOnce();
      expect(persist).toHaveBeenCalledOnce();
      // A legacy view passes through as the same value the engine returned.
      expect(privateHost.guestSessions.get(2)?.send).toHaveBeenCalledWith({
        type: "draft_pick_ack",
        view: await producer.mock.results[0].value,
      });
    },
    refusalFence: ({ persist, privateHost }) => {
      expect(persist).not.toHaveBeenCalled();
      expect(privateHost.guestSessions.get(2)?.send).not.toHaveBeenCalledWith(expect.objectContaining({ type: "draft_pick_ack" }));
      expect(privateHost.guestSessions.get(2)?.send).toHaveBeenCalledWith(expect.objectContaining({ type: "draft_error" }));
    },
  },
  {
    site: "handleSharedStackDecision: post-bot host getViewForSeat",
    producer: "getViewForSeat",
    firstContinuation: "draftComplete event or pick timer",
    // The bot sweep's read and the broadcast's guest and host reads.
    leadingResults: [censusView(), censusView(), censusView()],
    setup: ({ privateHost }) => {
      privateHost.draftStarted = true;
      privateHost.paused = false;
      privateHost.guestSessions.set(2, { send: vi.fn() });
    },
    invoke: ({ privateHost }) => privateHost.handleGuestMessage(2, { type: "draft_pile_decision", pile: 0, decision: "Take" }),
    legacyReach: ({ producer, emit }) => {
      expect(producer).toHaveBeenCalledTimes(4);
      expect(emit).toHaveBeenCalledWith(expect.objectContaining({ type: "draftComplete" }));
    },
    refusalFence: ({ producer, emit, privateHost }) => {
      expect(producer).toHaveBeenCalledTimes(4);
      expect(emit).not.toHaveBeenCalledWith(expect.objectContaining({ type: "draftComplete" }));
      expect(privateHost.guestSessions.get(2)?.send).toHaveBeenCalledWith(expect.objectContaining({ type: "draft_error" }));
    },
  },
  {
    site: "autoDecideSharedStackTurn: getViewForSeat",
    producer: "getViewForSeat",
    firstContinuation: "the engine's forced decision",
    legacyResult: { ...censusView("Drafting"), shared_stack: { active_seat: 1, active_pile: 0 } },
    setup: ({ privateHost }) => {
      privateHost.adapter.sharedStackForcedDecision = vi.fn(async () => null);
    },
    invoke: ({ privateHost }) => invokeHostOperation(privateHost, "autoDecideSharedStackTurn"),
    legacyReach: ({ privateHost }) => expect(privateHost.adapter.sharedStackForcedDecision).toHaveBeenCalledWith(1),
    refusalFence: ({ admit, privateHost }) => {
      expect(admit).toHaveBeenCalledOnce();
      expect(privateHost.adapter.sharedStackForcedDecision).not.toHaveBeenCalled();
    },
  },
  {
    site: "requestResume: getViewForSeat",
    producer: "getViewForSeat",
    firstContinuation: "the stuck bot's re-drive and pause reconciliation",
    legacyResult: { ...censusView("Drafting"), shared_stack: { active_seat: 1, active_pile: 0 } },
    setup: ({ privateHost }) => {
      Reflect.set(privateHost, "manualPause", true);
      Reflect.set(privateHost, "reconcileEffectivePause", vi.fn());
    },
    invoke: async ({ host, privateHost }) => {
      host.requestResume();
      await Reflect.get(privateHost, "mutationQueue");
    },
    legacyReach: ({ producer, privateHost }) => {
      expect(producer).toHaveBeenCalledOnce();
      expect(Reflect.get(privateHost, "reconcileEffectivePause")).toHaveBeenCalledOnce();
    },
    refusalFence: ({ privateHost }) => {
      expect(Reflect.get(privateHost, "reconcileEffectivePause")).not.toHaveBeenCalled();
    },
  },
  {
    site: "suggestLandsForSeatInner: getViewForSeat",
    producer: "getViewForSeat",
    firstContinuation: "workspace reconciliation and the engine's land suggestion",
    setup: ({ privateHost }) => {
      Reflect.get(privateHost, "perSeatWorkspaceSnapshots").set(2, { schemaVersion: 1, placements: {}, virtualBasics: [] });
      privateHost.adapter.suggestLandsForSeat = vi.fn(async () => ({}));
    },
    invoke: ({ host }) => host.suggestLandsForSeat(2),
    legacyReach: ({ privateHost }) => expect(privateHost.adapter.suggestLandsForSeat).toHaveBeenCalledOnce(),
    refusalFence: ({ admit, privateHost }) => {
      expect(admit).toHaveBeenCalledOnce();
      expect(privateHost.adapter.suggestLandsForSeat).not.toHaveBeenCalled();
    },
  },
] as const;

describe("P2P plural pick channel", () => {
  describe("P2PDraftHost immediate-return producer census", () => {
    it.each(P2P_DRAFT_HOST_PRODUCER_CENSUS)("keeps the compatible legacy continuation reachable for $site", async (row) => {
      const fixture = censusFixture(row.producer, row.legacyResult ?? censusView(), row.leadingResults);
      row.setup(fixture);

      if (row.legacyError) {
        await expect(row.invoke(fixture)).rejects.toThrow(row.legacyError);
      } else {
        await row.invoke(fixture);
      }

      await row.legacyReach(fixture);
    });

    it.each(rawHostRefusals)("fences every censused producer before its named continuation on %s", async (_label, refusal) => {
      for (const row of P2P_DRAFT_HOST_PRODUCER_CENSUS) {
        const fixture = censusFixture(row.producer, refusal, row.leadingResults);
        row.setup(fixture);

        try {
          await row.invoke(fixture);
        } catch {
          // Some public operations surface the refusal; others deliberately
          // report it locally.  In both cases the producer must have reached
          // the common raw boundary and the row's named first continuation
          // must still be absent.
        }

        expect(fixture.producer).toHaveBeenCalled();
        row.refusalFence(fixture);
      }
    });

    it("refuses a FIFO view on a forced shared-stack turn before asking the engine for the move", async () => {
      const stack = { active_seat: 1, active_pile: 0 };
      const fixture = censusFixture("getViewForSeat", { flow: "FifoAsync", status: "Drafting", shared_stack: stack });
      fixture.privateHost.adapter.sharedStackForcedDecision = vi.fn(async () => null);
      const reported = vi.spyOn(console, "error").mockImplementation(() => {});

      await invokeHostOperation(fixture.privateHost, "autoDecideSharedStackTurn");

      expect(reported).toHaveBeenCalledWith(expect.any(String), expect.objectContaining({ code: "fifo_flow" }));
      expect(fixture.privateHost.adapter.sharedStackForcedDecision).not.toHaveBeenCalled();
      reported.mockRestore();
    });

    it("refuses residual FIFO state on a land suggestion before asking the engine for lands", async () => {
      const fixture = censusFixture("getViewForSeat", { ...censusView("Deckbuilding"), fifo: { cursor: 1 } });
      Reflect.get(fixture.privateHost, "perSeatWorkspaceSnapshots").set(2, { schemaVersion: 1, placements: {}, virtualBasics: [] });
      fixture.privateHost.adapter.suggestLandsForSeat = vi.fn(async () => ({}));

      await expect(fixture.host.suggestLandsForSeat(2)).rejects.toMatchObject({ code: "residual_fifo" });

      expect(fixture.producer).toHaveBeenCalledOnce();
      expect(fixture.privateHost.adapter.suggestLandsForSeat).not.toHaveBeenCalled();
    });
  });

  it.each([
    ["FIFO flow", { flow: "FifoAsync", status: "Drafting" }],
    ["Review", { status: "Review" }],
    ["residual FIFO", { status: "Drafting", fifo: { cursor: 1 } }],
  ])("rejects a %s reducer return before pick persistence or acknowledgement", async (_label, refusedView) => {
    const host = newHost();
    const privateHost = asPrivate(host);
    privateHost.draftStarted = true;
    privateHost.paused = false;
    const session = { send: vi.fn() };
    privateHost.guestSessions.set(2, session);
    privateHost.adapter = {
      ...stubAdapter(() => ({ status: "Lobby", seats: [] })),
      submitPickForSeat: vi.fn(async () => refusedView),
    };

    await expect(privateHost.handleGuestMessage(2, {
      type: "draft_pick",
      cardInstanceIds: ["card-1"],
    })).rejects.toMatchObject({ name: "Draft27IncompatibilityError" });

    expect(privateHost.picksThisRound.has(2)).toBe(false);
    expect(session.send).toHaveBeenCalledWith(expect.objectContaining({ type: "draft_error" }));
  });

  /**
   * V3. The wire carries a whole CR 903.13b pick step and the host must hand
   * every id to `submitPickForSeat`, in order, unmangled.
   *
   * REVERT-PROBE: change `p2p-draft-host.ts`'s `applyPick` default
   * `submitPickForSeat(seat, cardInstanceIds)` to `(seat, [cardInstanceIds[0]])`
   * and this reds.
   */
  it("routes a guest's whole pick step to the adapter, in order", async () => {
    const host = newHost();
    const privateHost = asPrivate(host);
    privateHost.draftStarted = true;
    privateHost.paused = false;
    // Seat 2 has no `seatNames` entry (the constructor sets only seat 0) and no
    // `guestSessions` entry, so `isBotSeat(2)` is true and `applyPick`'s
    // `options.resolveBots && !this.isBotSeat(seat)` is already false — that is
    // the guard that actually fires and keeps this test on the pick seam. The
    // stub's "Lobby" is a real second belt (`resolveBotPicks` returns unless the
    // host view says "Drafting") but it is never reached here.
    privateHost.adapter = stubAdapter(() => ({ status: "Lobby", seats: [] }));

    await privateHost.handleGuestMessage(2, {
      type: "draft_pick",
      cardInstanceIds: ["card-1", "card-2"],
    });

    expect(privateHost.adapter.submitPickForSeat).toHaveBeenCalledWith(2, [
      "card-1",
      "card-2",
    ]);
    // Paired positive reach-guard: the call RESOLVED and the seat entered
    // `picksThisRound`. A thrown `assertPickAllowed` would otherwise satisfy a
    // bare not-called negative vacuously.
    expect(privateHost.picksThisRound.has(2)).toBe(true);
  });

  /**
   * The plan's one judgement call (§R5). On the draft-effect path `applyPick`'s
   * second positional argument — and therefore the `pickReceived` payload —
   * names the cards the seat DRAFTED, never the effect card that paid for them.
   * Nothing else in the tree pins that: `pickReceived` has no other test reader,
   * and both candidates are `string[]`, so `tsc` catches nothing if the argument
   * moves. This is the only revert-probe for the change.
   *
   * REVERT-PROBE: change `handlePickWithDraftEffect`'s second `applyPick`
   * argument from `cardInstanceIds` back to `[effectCardInstanceId]` and this
   * reds — the emitted payload becomes `["cogwork-1"]`.
   */
  it("reports the drafted cards, not the effect card, on the draft-effect path", async () => {
    const host = newHost();
    const privateHost = asPrivate(host);
    privateHost.draftStarted = true;
    privateHost.paused = false;
    privateHost.adapter = {
      ...stubAdapter(() => ({ status: "Lobby", seats: [] })),
      submitPickWithDraftEffectForSeat: vi.fn(async () => ({ status: "Drafting" })),
    };
    const events: DraftHostEvent[] = [];
    host.onEvent((event) => events.push(event));

    await privateHost.handleGuestMessage(2, {
      type: "draft_pick_with_draft_effect",
      effectCardInstanceId: "cogwork-1",
      cardInstanceIds: ["card-1", "card-2"],
    });

    // Reach-guard: the run really took the draft-effect path, and the effect
    // card really did reach the engine — through the `submitPick` override,
    // which is the one place it belongs. Without this the payload assertion
    // below could pass on a run that never reached the effect branch at all.
    expect(
      privateHost.adapter.submitPickWithDraftEffectForSeat,
    ).toHaveBeenCalledWith(2, "cogwork-1", ["card-1", "card-2"]);
    expect(privateHost.adapter.submitPickForSeat).not.toHaveBeenCalled();

    expect(events.filter((e) => e.type === "pickReceived")).toEqual([
      {
        type: "pickReceived",
        seatIndex: 2,
        cardInstanceIds: ["card-1", "card-2"],
      },
    ]);
  });

  /**
   * V4. RED AT BASE: before this phase `resolveBotPicks` submitted exactly one
   * random card, which a Commander pod refuses.
   *
   * REVERT-PROBE: change `resolveBotPicks`'s
   * `randomDistinctCards(pack, view.required_pick_count)` back to
   * `[pack[randomIndex].instance_id]` and the length assertion reds.
   */
  it("a bot seat drafts a whole step, not one card", async () => {
    const host = newHost();
    const privateHost = asPrivate(host);
    privateHost.draftStarted = true;
    privateHost.paused = false;
    privateHost.adapter = stubAdapter((seat) =>
      seat === 0
        ? { status: "Drafting", seats: [{ seat_index: 1, is_bot: true }] }
        : {
            status: "Drafting",
            current_pack: pack("c1", "c2", "c3"),
            required_pick_count: 2,
          },
    );

    await privateHost.resolveBotPicks({ emit: false, persist: false });

    expect(privateHost.adapter.submitPickForSeat).toHaveBeenCalledTimes(1);
    const [seat, ids] = privateHost.adapter.submitPickForSeat.mock.calls[0] as [
      number,
      string[],
    ];
    expect(seat).toBe(1);
    expect(ids).toHaveLength(2);
    // Distinct: `apply_pick_inner` refuses a repeated id with
    // `DuplicatePickCardId`.
    expect(new Set(ids).size).toBe(2);
    expect(ids.every((id) => ["c1", "c2", "c3"].includes(id))).toBe(true);
  });

  /**
   * The count is the VIEW's, never the kind's. A `kind === "CommanderDraft" ? 2
   * : 1` implementation, or a literal 2, passes the test above and fails this
   * one — which is the whole reason the engine publishes the number.
   */
  it("reads the count from the view, not from the kind", async () => {
    const host = newHost("CommanderDraft");
    const privateHost = asPrivate(host);
    privateHost.draftStarted = true;
    privateHost.paused = false;
    privateHost.adapter = stubAdapter((seat) =>
      seat === 0
        ? { status: "Drafting", seats: [{ seat_index: 1, is_bot: true }] }
        : {
            status: "Drafting",
            current_pack: pack("c1", "c2", "c3"),
            // CR 903.13b's odd-pack final step, on a Commander session.
            required_pick_count: 1,
          },
    );

    await privateHost.resolveBotPicks({ emit: false, persist: false });

    const [, ids] = privateHost.adapter.submitPickForSeat.mock.calls[0] as [
      number,
      string[],
    ];
    expect(ids).toHaveLength(1);
  });

  it("a one-card pack yields a one-card step, with no out-of-range slice", async () => {
    const host = newHost();
    const privateHost = asPrivate(host);
    privateHost.draftStarted = true;
    privateHost.paused = false;
    privateHost.adapter = stubAdapter((seat) =>
      seat === 0
        ? { status: "Drafting", seats: [{ seat_index: 1, is_bot: true }] }
        : {
            status: "Drafting",
            current_pack: pack("only-card"),
            required_pick_count: 1,
          },
    );

    await privateHost.resolveBotPicks({ emit: false, persist: false });

    expect(privateHost.adapter.submitPickForSeat).toHaveBeenCalledWith(1, [
      "only-card",
    ]);
  });

  /**
   * V5. The pick-timer sweep owes whole steps too. Its `try`/`catch` logs and
   * swallows, so the assertion is on `submitPickForSeat`'s call payload — never
   * on a rejected promise.
   *
   * REVERT-PROBE: change `autoPickAllPending`'s
   * `randomDistinctCards(view.current_pack, view.required_pick_count)` back to
   * a single `view.current_pack[randomIndex].instance_id` and this reds.
   */
  it("the timer sweep drafts whole steps", async () => {
    const host = newHost();
    const privateHost = asPrivate(host);
    privateHost.draftStarted = true;
    privateHost.paused = false;
    privateHost.adapter = stubAdapter((seat) =>
      seat === 1
        ? {
            status: "Drafting",
            current_pack: pack("c1", "c2", "c3"),
            required_pick_count: 2,
          }
        : { status: "Drafting", seats: [], current_pack: [], required_pick_count: 0 },
    );

    await privateHost.autoPickAllPending();

    expect(privateHost.adapter.submitPickForSeat).toHaveBeenCalledTimes(1);
    const [seat, ids] = privateHost.adapter.submitPickForSeat.mock.calls[0] as [
      number,
      string[],
    ];
    expect(seat).toBe(1);
    expect(ids).toHaveLength(2);
    expect(new Set(ids).size).toBe(2);
  });

  it("the timer sweep skips a seat already in picksThisRound", async () => {
    const host = newHost();
    const privateHost = asPrivate(host);
    privateHost.draftStarted = true;
    privateHost.paused = false;
    privateHost.adapter = stubAdapter((seat) =>
      seat === 1 || seat === 2
        ? {
            status: "Drafting",
            current_pack: pack("c1", "c2", "c3"),
            required_pick_count: 2,
          }
        : { status: "Drafting", seats: [], current_pack: [], required_pick_count: 0 },
    );
    privateHost.picksThisRound.add(1);

    await privateHost.autoPickAllPending();

    const seats = privateHost.adapter.submitPickForSeat.mock.calls.map(
      (call) => (call as [number, string[]])[0],
    );
    expect(seats).not.toContain(1);
    // Paired positive: seat 2, unseeded and otherwise identical, IS picked for
    // in the same sweep — so the negative above cannot pass by way of a sweep
    // that did nothing at all.
    expect(seats).toContain(2);
  });

  /**
   * V11. A pick submitted while the draft is paused is refused, and nothing
   * reaches the engine.
   *
   * REVERT-PROBE: remove `handleGuestMessage`'s `canGuestPick(seat)` guard on
   * the `draft_pick` case and this reds.
   */
  it("refuses a pick while paused and submits nothing", async () => {
    const host = newHost();
    const privateHost = asPrivate(host);
    privateHost.draftStarted = true;
    privateHost.paused = true;
    privateHost.adapter = stubAdapter(() => ({ status: "Lobby", seats: [] }));
    const send = vi.fn();
    privateHost.guestSessions.set(2, { send });

    await privateHost.handleGuestMessage(2, {
      type: "draft_pick",
      cardInstanceIds: ["card-1", "card-2"],
    });

    expect(send).toHaveBeenCalledWith({
      type: "draft_error",
      reason: "Draft is paused",
    });
    expect(privateHost.adapter.submitPickForSeat).not.toHaveBeenCalled();

    // Paired positive: the identical fixture with `paused = false` DOES submit,
    // so the negative above is not a fixture that could never pick at all.
    privateHost.paused = false;
    await privateHost.handleGuestMessage(2, {
      type: "draft_pick",
      cardInstanceIds: ["card-1", "card-2"],
    });
    expect(privateHost.adapter.submitPickForSeat).toHaveBeenCalledWith(2, [
      "card-1",
      "card-2",
    ]);
  });

  it.each([
    ["FIFO flow", { flow: "FifoAsync", status: "Deckbuilding" }],
    ["Review", { status: "Review" }],
    ["residual FIFO", { status: "Deckbuilding", fifo: { cursor: 1 } }],
  ])("keeps a refused network deck delivery retryable only for its exact session (%s)", async (_label, refusedView) => {
    const host = newHost();
    const privateHost = asPrivate(host);
    privateHost.draftStarted = true;
    const delivery = { send: vi.fn() };
    privateHost.guestSessions.set(2, delivery);
    privateHost.adapter = {
      ...stubAdapter(() => ({ status: "Deckbuilding", seats: [] })),
      submitDeckForSeat: vi.fn(async () => refusedView),
    };

    await expect(privateHost.handleGuestMessage(2, {
      type: "draft_submit_deck",
      mainDeck: ["Island"],
      commanders: [],
      submissionId: "submission-1",
    })).rejects.toMatchObject({ name: "Draft27IncompatibilityError" });

    expect(delivery.send).toHaveBeenCalledTimes(1);
    expect(delivery.send).toHaveBeenCalledWith(expect.objectContaining({
      type: "draft_error",
      submissionId: "submission-1",
      submissionDisposition: "Retryable",
    }));
    expect(privateHost.adapter.submitDeckForSeat).toHaveBeenCalledOnce();
  });

  it("does not settle a replacement connection for an older refused network deck delivery", async () => {
    const host = newHost();
    const privateHost = asPrivate(host);
    privateHost.draftStarted = true;
    const originatingPeer = { send: vi.fn() };
    const replacementPeer = { send: vi.fn() };
    const refused = deferred<Record<string, unknown>>();
    privateHost.guestSessions.set(2, originatingPeer);
    privateHost.adapter = {
      ...stubAdapter(() => ({ status: "Deckbuilding", seats: [] })),
      submitDeckForSeat: vi.fn(() => refused.promise),
    };

    const delivery = privateHost.handleGuestMessage(2, {
      type: "draft_submit_deck",
      mainDeck: ["Island"],
      commanders: [],
      submissionId: "submission-1",
    });
    // The delivery owns the old connection while the reducer promise is live.
    // Replacing its seat must not move the error to either connection.
    expect(privateHost.adapter.submitDeckForSeat).toHaveBeenCalledOnce();
    privateHost.guestSessions.set(2, replacementPeer);
    refused.resolve({ status: "Review" });

    await expect(delivery).rejects.toMatchObject({ name: "Draft27IncompatibilityError" });
    expect(originatingPeer.send).not.toHaveBeenCalled();
    expect(replacementPeer.send).not.toHaveBeenCalled();
  });

  it.each([
    ["Retryable", () => ({ status: "Review" })],
    ["Rejected", () => { throw new Error("IDB unavailable"); }],
  ])("surfaces a direct host deck %s without creating network delivery metadata", async (_disposition, refuse) => {
    const host = newHost();
    const privateHost = asPrivate(host);
    privateHost.draftStarted = true;
    const unrelatedPeer = { send: vi.fn() };
    // A hostile fixture makes this explicit: even a peer object at seat zero
    // is not an inbound delivery session for the direct host API.
    privateHost.guestSessions.set(0, unrelatedPeer);
    privateHost.adapter = {
      ...stubAdapter(() => ({ status: "Deckbuilding", seats: [] })),
      submitDeckForSeat: vi.fn(async () => refuse()),
    };

    await expect(privateHost.submitHostDeck(["Island"], [])).rejects.toBeDefined();
    expect(unrelatedPeer.send).not.toHaveBeenCalled();
  });
});
