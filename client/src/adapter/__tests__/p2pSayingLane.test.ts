import { describe, expect, it, vi } from "vitest";

import { FakeDataConnection } from "../../network/__tests__/fakeDataConnection.ts";
import type { PeerSession } from "../../network/peer.ts";
import { WIRE_PROTOCOL_VERSION, type P2PMessage } from "../../network/protocol.ts";
import type { TransportConnection, TransportPeer } from "../../network/transport.ts";
import { P2PGuestAdapter, P2PHostAdapter } from "../p2p-adapter.ts";
import {
  AdapterError,
  AdapterErrorCode,
  type GameAction,
  type GameEvent,
  type GameState,
  type SayPhraseExpiryTarget,
  type SubmitResult,
} from "../types.ts";

type SayingAction = Extract<GameAction, { type: "SayPhrase" }>;
type SayingResult = {
  type: "saying_result";
  payment: number;
  seq: number;
  accepted: boolean;
  message?: string;
};
type PendingSaying = {
  resolve: (result: SubmitResult) => void;
  reject: (error: Error) => void;
  timer: ReturnType<typeof setTimeout>;
};

/**
 * The guest adapter's transport and reply slot, without a live PeerJS session.
 * These are exactly the fields `submitSaying` / `settleSaying` touch, so the
 * test exercises the real methods rather than a reimplementation of them.
 */
type GuestInternals = {
  assignedPlayerId: number | null;
  authenticatedSession: unknown;
  pendingSayings: Map<string, PendingSaying>;
  pendingResolve: ((result: SubmitResult) => void) | null;
  pendingReject: ((error: Error) => void) | null;
  pendingSubmissionTimer: ReturnType<typeof setTimeout> | null;
  send: (message: unknown) => Promise<boolean>;
  submitSaying: (action: SayingAction) => Promise<SubmitResult>;
  settleSaying: (result: SayingResult) => void;
  parkPendingSubmission: (
    resolve: (result: SubmitResult) => void,
    reject: (error: Error) => void,
  ) => void;
  settlePendingSubmission: (outcome: {
    kind: "resolve";
    result: SubmitResult;
  }) => boolean;
};

const EMPTY: SubmitResult = { events: [], log_entries: [] };

type HostInternals = {
  disposed: boolean;
  ownsAuthority: () => boolean;
  nativeBridge: {
    sayPhraseExpiry: (lapse?: number) => Promise<SayPhraseExpiryTarget | null>;
  };
  applyBrowserMutation: <T>(run: () => Promise<T>) => Promise<T>;
  publishHostSnapshot: (result: SubmitResult) => Promise<boolean>;
  broadcastStateUpdate: (transition: unknown, terminalReason?: string, saying?: boolean) => Promise<void>;
  runAiLoop: () => Promise<void>;
  persistAuthoritativeState: () => void;
  scheduleSayPhraseExpiry: () => void;
  expireSayPhrase: (paymentId: number, timerGen?: number) => Promise<void>;
  listeners?: Set<unknown>;
  sayExpiryTimerGeneration?: number;
};

type HostGuestMessageInternals = {
  handlePendingTerminalReconnectFrame: (session: unknown, msg: unknown) => boolean;
  authenticatedGuestPlayerId: (session: unknown, msg: unknown) => number | null;
  concedingSeats: Set<number>;
  eliminatedSeats: Set<number>;
  gameRunState: string;
  guestSessions: Map<number, unknown>;
  applyBrowserMutation: <T>(run: () => Promise<T>) => Promise<T>;
  nativeBridge: {
    submitAction: (action: GameAction, player: number) => Promise<{ result: SubmitResult }>;
  };
  publishHostSnapshot: (result: SubmitResult) => Promise<boolean>;
  broadcastStateUpdate: (
    transition: unknown,
    terminalReason?: string,
    saying?: boolean,
  ) => Promise<void>;
  runAiLoop: () => Promise<void>;
  persistAuthoritativeState: () => void;
  send: (session: unknown, msg: unknown) => Promise<boolean>;
  sendSayingFailure: (session: unknown, action: GameAction, error: unknown) => void;
  handleGuestMessage: (
    session: unknown,
    msg: P2PMessage,
    receivedAtMs: number,
  ) => Promise<void>;
};

function makeGuest(sendAccepted: boolean): GuestInternals {
  const adapter = Object.create(
    P2PGuestAdapter.prototype,
  ) as unknown as GuestInternals;
  adapter.assignedPlayerId = 0;
  adapter.authenticatedSession = {};
  adapter.pendingSayings = new Map();
  adapter.pendingResolve = null;
  adapter.pendingReject = null;
  adapter.pendingSubmissionTimer = null;
  adapter.send = vi.fn(async () => sendAccepted);
  return adapter;
}

function saying(seq = 1): SayingAction {
  return {
    type: "SayPhrase",
    data: { payment: 7, seq, at_ms: 1, report: "Saying" },
  };
}

function remoteState(label: string): GameState {
  return {
    label,
    turn_number: 1,
    active_player: 0,
    priority_player: 1,
    phase: "PreCombatMain",
    players: [],
    objects: {},
    waiting_for: { type: "UnlessPayment", data: { player: 1 } },
  } as unknown as GameState;
}

function createFakePeer(): {
  peer: {
    on: () => void;
    off: () => void;
    connect: () => never;
    destroy: () => void;
  };
} {
  return {
    peer: {
      on() {},
      off() {},
      connect() {
        throw new Error("not used in tests");
      },
      destroy() {},
    },
  };
}

async function authenticatedGuest(conn: FakeDataConnection): Promise<P2PGuestAdapter> {
  const { peer } = createFakePeer();
  const adapter = new P2PGuestAdapter(
    { player: { main_deck: [], sideboard: [] } },
    peer as unknown as TransportPeer,
    "host-peer",
    conn as unknown as TransportConnection,
  );
  await adapter.initialize();
  await conn.simulateData({
    type: "game_setup",
    wireProtocolVersion: WIRE_PROTOCOL_VERSION,
    assignedPlayerId: 1,
    playerToken: "seat-token",
    revision: 1,
    state: remoteState("initial"),
    events: [],
    legalActions: [],
  });
  return adapter;
}

describe("P2P saying reply lane", () => {
  it("rejects a saying whose bytes never left instead of counting it", async () => {
    const guest = makeGuest(false);

    await expect(guest.submitSaying(saying())).rejects.toMatchObject({
      code: AdapterErrorCode.ACTION_NOT_SENT,
    });
    expect(guest.pendingSayings.size).toBe(0);
  });

  it("settles a saying without consuming a parked ordinary command", async () => {
    const guest = makeGuest(true);
    const said = guest.submitSaying(saying());
    expect(guest.pendingSayings.size).toBe(1);

    // Stay Quiet parks an ordinary submission behind the saying.
    const ordinaryResolve = vi.fn();
    const ordinaryReject = vi.fn();
    guest.parkPendingSubmission(ordinaryResolve, ordinaryReject);

    guest.settleSaying({
      type: "saying_result",
      payment: 7,
      seq: 1,
      accepted: true,
    });

    await expect(said).resolves.toEqual(EMPTY);
    expect(ordinaryResolve).not.toHaveBeenCalled();
    expect(ordinaryReject).not.toHaveBeenCalled();

    guest.settlePendingSubmission({ kind: "resolve", result: EMPTY });
    expect(ordinaryResolve).toHaveBeenCalledWith(EMPTY);
  });

  it("does not settle a parked Stay Quiet submission when a saying's state_update or state_withheld arrives, and only settles it on its own result", async () => {
    const conn = new FakeDataConnection();
    const guest = await authenticatedGuest(conn);

    const stayQuietAction: GameAction = {
      type: "ChooseUnlessCostBranch",
      data: { choice: { type: "Decline" } },
    };
    const ordinary = guest.submitAction(stayQuietAction, 1);
    let ordinarySettled = false;
    void ordinary.then(
      () => {
        ordinarySettled = true;
      },
      () => {
        ordinarySettled = true;
      },
    );

    // A saying's state_update broadcast arrives:
    await conn.simulateData({
      type: "state_update",
      saying: true,
      revision: 2,
      state: remoteState("saying-state"),
      events: [{ marker: "saying-applied" } as unknown as GameEvent],
      logEntries: [],
    });
    expect(ordinarySettled).toBe(false);

    // A saying's state_withheld broadcast arrives:
    await conn.simulateData({
      type: "state_withheld",
      saying: true,
      revision: 3,
    });
    expect(ordinarySettled).toBe(false);

    // A saying_result arrives:
    await conn.simulateData({
      type: "saying_result",
      payment: 7,
      seq: 1,
      accepted: true,
    });
    expect(ordinarySettled).toBe(false);

    // Now its own state_update arrives (without saying):
    await conn.simulateData({
      type: "state_update",
      revision: 4,
      state: remoteState("quiet-state"),
      events: [{ marker: "quiet-applied" } as unknown as GameEvent],
      logEntries: [],
    });

    await expect(ordinary).resolves.toEqual({
      events: [{ marker: "quiet-applied" }],
      log_entries: [],
    });
    expect(ordinarySettled).toBe(true);
    guest.dispose();
  });

  it("forwards a native rejection through host to guest as a failed saying_result", async () => {
    const sentMessages: unknown[] = [];
    const session = {} as unknown as PeerSession;
    const rejection = new AdapterError(
      AdapterErrorCode.ACTION_REJECTED,
      "Payment already expired",
      false,
    );
    const submitAction = vi.fn(async () => {
      throw rejection;
    });

    const host = Object.create(
      P2PHostAdapter.prototype,
    ) as unknown as HostGuestMessageInternals;
    host.handlePendingTerminalReconnectFrame = () => false;
    host.authenticatedGuestPlayerId = () => 1;
    host.concedingSeats = new Set();
    host.eliminatedSeats = new Set();
    host.gameRunState = "running";
    host.guestSessions = new Map([[1, session]]);
    host.applyBrowserMutation = (run) => run();
    host.nativeBridge = { submitAction };
    host.send = vi.fn((_session: unknown, msg: unknown) => {
      sentMessages.push(msg);
      return Promise.resolve(true);
    });

    await host.handleGuestMessage(
      session,
      {
        type: "action",
        senderPlayerId: 1,
        action: saying(1),
      },
      Date.now(),
    );

    expect(sentMessages).toEqual([
      {
        type: "saying_result",
        payment: 7,
        seq: 1,
        accepted: false,
        message: "Payment already expired",
      },
    ]);
  });

  it("restamps a guest's saying with the host's receipt clock", async () => {
    const sentMessages: unknown[] = [];
    const session = {} as unknown as PeerSession;
    const submitted: Array<{ action: GameAction; player: number }> = [];
    const submitAction = vi.fn(async (action: GameAction, player: number) => {
      submitted.push({ action, player });
      return { result: EMPTY };
    });

    const host = Object.create(
      P2PHostAdapter.prototype,
    ) as unknown as HostGuestMessageInternals;
    host.handlePendingTerminalReconnectFrame = () => false;
    host.authenticatedGuestPlayerId = () => 1;
    host.concedingSeats = new Set();
    host.eliminatedSeats = new Set();
    host.gameRunState = "running";
    host.guestSessions = new Map([[1, session]]);
    host.applyBrowserMutation = (run) => run();
    host.nativeBridge = { submitAction };
    host.send = vi.fn((_session: unknown, msg: unknown) => {
      sentMessages.push(msg);
      return Promise.resolve(true);
    });
    host.publishHostSnapshot = vi.fn(async () => true);
    host.broadcastStateUpdate = vi.fn(async () => {});
    host.runAiLoop = vi.fn(async () => {});
    host.persistAuthoritativeState = vi.fn();

    // The guest claims a stale instant and a sequence of its choosing. The
    // payment identity and sequence are the guest's observation; the instant
    // is the host's evidence and must be replaced at receipt.
    await host.handleGuestMessage(
      session,
      {
        type: "action",
        senderPlayerId: 1,
        action: saying(3),
      },
      1_730_000_000_123,
    );

    expect(submitted).toEqual([
      {
        action: {
          type: "SayPhrase",
          data: {
            payment: 7,
            seq: 3,
            at_ms: 1_730_000_000_123,
            report: "Saying",
          },
        },
        player: 1,
      },
    ]);
    expect(sentMessages).toEqual([
      {
        type: "saying_result",
        payment: 7,
        seq: 3,
        accepted: true,
      },
    ]);
  });

  it("propagates definite no-send for untimed P2P payments and preserves retry", async () => {
    const currentConn = new FakeDataConnection();
    const { peer } = createFakePeer();
    const guest = new P2PGuestAdapter(
      { player: { main_deck: [], sideboard: [] } },
      peer as unknown as TransportPeer,
      "host-peer",
      currentConn as unknown as TransportConnection,
    );
    await guest.initialize();
    await currentConn.simulateData({
      type: "game_setup",
      wireProtocolVersion: WIRE_PROTOCOL_VERSION,
      assignedPlayerId: 1,
      playerToken: "seat-token",
      revision: 1,
      state: remoteState("initial"),
      events: [],
      legalActions: [],
    });

    const guestInternals = guest as unknown as {
      parkPendingSubmission: (
        resolve: (result: SubmitResult) => void,
        reject: (error: Error) => void,
      ) => void;
      attachSession: (conn: TransportConnection) => void;
      attemptReconnect: () => Promise<void>;
      handleHostDisconnect: (session: unknown) => void;
      session: unknown;
    };
    guestInternals.attemptReconnect = vi.fn().mockResolvedValue(undefined);

    const untimedPayment: GameAction = {
      type: "ChooseUnlessCostBranch",
      data: { choice: { type: "Pay", data: { index: 0 } } },
    };

    // 1. Failed transport send rejects with ACTION_NOT_SENT:
    vi.spyOn(currentConn, "send").mockImplementation(() => {
      throw new Error("transport send failed");
    });

    await expect(guest.submitAction(untimedPayment, 1)).rejects.toMatchObject({
      code: AdapterErrorCode.ACTION_NOT_SENT,
    });
    vi.restoreAllMocks();

    // 2. Disconnect while parked and unsent also rejects with ACTION_NOT_SENT:
    guestInternals.attemptReconnect = vi.fn().mockResolvedValue(undefined);
    const unsentPromise = new Promise<SubmitResult>((resolve, reject) => {
      guestInternals.parkPendingSubmission(resolve, reject);
    });
    guestInternals.handleHostDisconnect(guestInternals.session);
    await expect(unsentPromise).rejects.toMatchObject({
      code: AdapterErrorCode.ACTION_NOT_SENT,
    });

    // 2b. A frame still queued in the session's FIFO when it is lost may yet
    // reach the host, so it is in doubt rather than retryable as unsent:
    const reattach = async () => {
      const conn = new FakeDataConnection();
      guestInternals.attachSession(conn as unknown as TransportConnection);
      await conn.simulateData({
        type: "reconnect_ack",
        wireProtocolVersion: WIRE_PROTOCOL_VERSION,
        assignedPlayerId: 1,
        revision: 1,
        state: remoteState("reattached"),
        events: [],
        playerNames: { 0: "Host", 1: "Guest" },
        legalActions: [],
      });
      return conn;
    };
    await reattach();
    const queued = guest.submitAction(untimedPayment, 1);
    guestInternals.handleHostDisconnect(guestInternals.session);
    await expect(queued).rejects.toMatchObject({ code: "P2P_ERROR" });

    // 2c. A frame the transport accepted is in doubt too:
    const sentConn = await reattach();
    const sentAction = guest.submitAction(untimedPayment, 1);
    await vi.waitFor(async () =>
      expect((await sentConn.getSentMessages()).some(
        (m) => (m as { type?: string }).type === "action",
      )).toBe(true));
    guestInternals.handleHostDisconnect(guestInternals.session);
    await expect(sentAction).rejects.toMatchObject({ code: "P2P_ERROR" });

    // 2d. A queued saying is in doubt for the same reason:
    await reattach();
    const queuedSaying = guest.submitAction(saying(1), 1);
    guestInternals.handleHostDisconnect(guestInternals.session);
    await expect(queuedSaying).rejects.toMatchObject({ code: "P2P_ERROR" });

    // 2e. A queued frame whose session closes before it is encoded never left:
    const closingConn = await reattach();
    const neverLeft = guest.submitAction(untimedPayment, 1);
    closingConn.open = false;
    guestInternals.handleHostDisconnect(guestInternals.session);
    await expect(neverLeft).rejects.toMatchObject({
      code: AdapterErrorCode.ACTION_NOT_SENT,
    });

    // 3. Reattach session with an open connection and acknowledge reconnect.
    const reconnectedConn = new FakeDataConnection();
    guestInternals.attachSession(reconnectedConn as unknown as TransportConnection);

    await reconnectedConn.simulateData({
      type: "reconnect_ack",
      wireProtocolVersion: WIRE_PROTOCOL_VERSION,
      assignedPlayerId: 1,
      revision: 2,
      state: remoteState("reconnected"),
      events: [],
      playerNames: { 0: "Host", 1: "Guest" },
      legalActions: [],
    });

    // Retry untimed payment through the restored session:
    const retried = guest.submitAction(untimedPayment, 1);

    // Host receives the action and broadcasts state update:
    await reconnectedConn.simulateData({
      type: "state_update",
      revision: 3,
      state: remoteState("paid"),
      events: [{ marker: "paid" } as unknown as GameEvent],
      logEntries: [],
    });

    await expect(retried).resolves.toEqual({
      events: [{ marker: "paid" }],
      log_entries: [],
    });

    guest.dispose();
  });

  it("schedules expiry and marks saying revisions on the host submitAction path without settling an unrelated pending guest submission", async () => {
    const conn = new FakeDataConnection();
    const guest = await authenticatedGuest(conn);

    const host = Object.create(P2PHostAdapter.prototype) as unknown as HostInternals & {
      authoritativeRevision: number;
      sayingRevisions: Set<number>;
      guestSessions: Map<number, unknown>;
      disconnectedSeats: Map<number, unknown>;
      concedingSeats: Set<number>;
      gameRunState: string;
      submitAction: (action: GameAction, actor: number) => Promise<SubmitResult>;
      broadcastStateUpdate: (transition: unknown, terminalReason?: string, saying?: boolean) => Promise<void>;
      send: (session: unknown, msg: unknown) => Promise<boolean>;
      projectTransitionForViewer: (pid: number) => Promise<{ snapshot: { state: GameState }; events: GameEvent[]; fresh: boolean }>;
      scheduleSayPhraseExpiry: () => void;
      runAiLoop: () => Promise<void>;
      persistAuthoritativeState: () => void;
      wasm: { submitAction: (action: GameAction, actor: number) => Promise<{ result: SubmitResult; events: GameEvent[]; generation: number }> };
    };
    host.disposed = false;
    host.ownsAuthority = () => true;
    host.concedingSeats = new Set();
    host.disconnectedSeats = new Map();
    host.listeners = new Set();
    host.gameRunState = "running";
    host.authoritativeRevision = 1;
    host.sayingRevisions = new Set();
    host.guestSessions = new Map([[1, guest]]);
    host.scheduleSayPhraseExpiry = vi.fn();
    host.runAiLoop = vi.fn(async () => {});
    host.persistAuthoritativeState = vi.fn();
    host.applyBrowserMutation = (run) => run();
    host.wasm = {
      submitAction: vi.fn(async () => ({
        result: EMPTY,
        events: [],
        generation: 1,
      })),
    };

    const sentWireMessages: unknown[] = [];
    host.send = vi.fn(async (_session, msg) => {
      sentWireMessages.push(msg);
      await conn.simulateData(msg as P2PMessage);
      return true;
    });
    host.projectTransitionForViewer = vi.fn(async () => ({
      snapshot: { state: remoteState("saying-state") },
      events: [{ marker: "host-said" } as unknown as GameEvent],
      fresh: true,
    }));
    host.broadcastStateUpdate = P2PHostAdapter.prototype[
      "broadcastStateUpdate"
    ] as unknown as HostInternals["broadcastStateUpdate"];
    (host as unknown as { broadcastStateUpdateInner: unknown }).broadcastStateUpdateInner =
      P2PHostAdapter.prototype["broadcastStateUpdateInner"];
    (host as unknown as { enqueueDelivery: (fn: () => Promise<void>) => Promise<void> }).enqueueDelivery =
      (fn) => fn();

    // Guest has an unrelated submission pending:
    const guestAction: GameAction = {
      type: "ChooseUnlessCostBranch",
      data: { choice: { type: "Decline" } },
    };
    const ordinary = guest.submitAction(guestAction, 1);
    let ordinarySettled = false;
    void ordinary.then(
      () => {
        ordinarySettled = true;
      },
      () => {
        ordinarySettled = true;
      },
    );

    // Host submits host saying directly via host-seat submitAction:
    const hostResult = await host.submitAction(saying(1), 0);
    expect(hostResult).toEqual(EMPTY);

    // 1. scheduleSayPhraseExpiry was called on the host path (authority timer scheduled even with no UI panel/fallback):
    expect(host.scheduleSayPhraseExpiry).toHaveBeenCalledTimes(1);

    // 2. Host marked revision as saying:
    expect(host.sayingRevisions.has(2)).toBe(true);

    // 3. Guest received wire state_update with saying: true:
    expect(sentWireMessages).toContainEqual(
      expect.objectContaining({
        type: "state_update",
        revision: 2,
        saying: true,
      }),
    );

    // 4. Guest's unrelated submission is NOT settled:
    expect(ordinarySettled).toBe(false);

    guest.dispose();
  });

  it("prevents an older expiry callback from clearing a newer payment's timer or state", async () => {
    vi.useFakeTimers();
    try {
      const host = Object.create(P2PHostAdapter.prototype) as unknown as HostInternals & {
        sayExpiryGeneration: number;
        sayExpiryTimerGeneration: number;
        sayExpiryPayment: number | null;
        sayExpiryDeadline: number | null;
        sayExpiryTimer: ReturnType<typeof setTimeout> | null;
        clearSayExpiryTimer: () => void;
        disarmSayPhraseExpiry: (ownerGen?: number) => void;
        expireSayPhrase: (id: number, timerGen: number) => Promise<void>;
        refreshSayPhraseExpiry: (gen: number) => Promise<void>;
      };
      host.disposed = false;
      host.ownsAuthority = () => true;
      host.sayExpiryGeneration = 1;
      host.sayExpiryTimerGeneration = 1;
      host.sayExpiryPayment = 7;
      host.sayExpiryDeadline = Date.now() - 1;
      host.sayExpiryTimer = setTimeout(() => {}, 1000);
      host.clearSayExpiryTimer = P2PHostAdapter.prototype["clearSayExpiryTimer"];
      host.disarmSayPhraseExpiry = P2PHostAdapter.prototype["disarmSayPhraseExpiry"];
      host.expireSayPhrase = P2PHostAdapter.prototype["expireSayPhrase"];
      host.applyBrowserMutation = (run) => run();

      let resolveRead: (value: SayPhraseExpiryTarget | null) => void;
      const readPromise = new Promise<SayPhraseExpiryTarget | null>((resolve) => {
        resolveRead = resolve;
      });
      const lapse = vi.fn(() => readPromise);
      host.nativeBridge = { sayPhraseExpiry: lapse };

      // Call expireSayPhrase with generation 1 (Payment 7):
      const expiryPromise = host.expireSayPhrase(7, 1);

      // While paused reading state, a rekeyed payment (Payment 8) arms a newer timer with generation 2:
      host.sayExpiryTimerGeneration = 2;
      host.sayExpiryPayment = 8;
      host.sayExpiryDeadline = Date.now() + 1000;
      const newerTimer = setTimeout(() => {}, 1000);
      host.sayExpiryTimer = newerTimer;

      // Now resolve the old authority read with the newer payment live:
      resolveRead!({ payer: 0, payment: 8, deadline_ms: Date.now() + 1000 });

      await expiryPromise;
      // The superseded owner never asked the authority to lapse anything.
      expect(lapse.mock.calls).toEqual([[]]);

      // The older callback saw payment.id (8) !== paymentId (7).
      // Because timerGen (1) !== sayExpiryTimerGeneration (2), it MUST NOT disarm or clear the new timer:
      expect(host.sayExpiryPayment).toBe(8);
      expect(host.sayExpiryTimer).toBe(newerTimer);
      expect(host.sayExpiryTimerGeneration).toBe(2);
      clearTimeout(newerTimer);
    } finally {
      vi.useRealTimers();
    }
  });

  it("keeps detached expiry AI revisions (both delivered and withheld) out of the guest reply lane with a parked guest action", async () => {
    const conn = new FakeDataConnection();
    const guest = await authenticatedGuest(conn);

    const sentWireMessages: unknown[] = [];
    const session = {} as unknown as PeerSession;

    const host = Object.create(P2PHostAdapter.prototype) as unknown as HostInternals & {
      authoritativeRevision: number;
      sayingRevisions: Set<number>;
      guestSessions: Map<number, unknown>;
      disconnectedSeats: Map<number, unknown>;
      concedingSeats: Set<number>;
      playerNamesForSeats: () => Record<number, string>;
      playerTokens: Map<number, string>;
      nativeInitialSetupPending: boolean;
      nativeWithheldSeats: Map<number, unknown>;
      recordNativeSeatOutcome: () => void;
      scheduleSayPhraseExpiry: () => void;
      handleNativeRevision: (
        revision: number,
        reports: Map<number, unknown>,
        sayingOrReason?: boolean | string,
        terminalReason?: string,
      ) => Promise<void>;
      send: (session: unknown, msg: unknown) => Promise<boolean>;
      emit: () => void;
    };
    host.disposed = false;
    host.ownsAuthority = () => true;
    host.authoritativeRevision = 1;
    host.sayingRevisions = new Set();
    host.guestSessions = new Map([[1, session]]);
    host.disconnectedSeats = new Map();
    host.concedingSeats = new Set();
    host.nativeInitialSetupPending = false;
    host.nativeWithheldSeats = new Map();
    host.playerNamesForSeats = () => ({ 0: "Host", 1: "Guest" });
    host.playerTokens = new Map([[1, "guest-token"]]);
    host.recordNativeSeatOutcome = vi.fn();
    host.scheduleSayPhraseExpiry = vi.fn();
    host.emit = vi.fn();
    host.handleNativeRevision = P2PHostAdapter.prototype["handleNativeRevision"] as unknown as typeof host.handleNativeRevision;

    // Guest has a parked ordinary submission that arrived after ingress drain:
    const guestAction: GameAction = {
      type: "ChooseUnlessCostBranch",
      data: { choice: { type: "Decline" } },
    };
    const ordinary = guest.submitAction(guestAction, 1);
    let ordinarySettled = false;
    void ordinary.then(
      () => {
        ordinarySettled = true;
      },
      () => {
        ordinarySettled = true;
      },
    );

    // Wire up host.send to deliver to guest via fake connection
    host.send = vi.fn(async (_session, msg) => {
      sentWireMessages.push(msg);
      await conn.simulateData(msg as P2PMessage);
      return true;
    });

    const hostReport = {
      kind: "update",
      update: {
        snapshot: { state: remoteState("host-state"), legalResult: { actions: [] } },
        events: [],
        logEntries: [],
      },
    };

    // 1. Detached expiry first publication (lapse) is delivered to guest:
    const lapseReports = new Map<number, unknown>([
      [0, hostReport],
      [
        1,
        {
          kind: "update",
          update: {
            snapshot: { state: remoteState("lapse-applied"), legalResult: { actions: [] } },
            events: [{ marker: "lapse-events" } as unknown as GameEvent],
            logEntries: [],
          },
        },
      ],
    ]);
    await host.handleNativeRevision(2, lapseReports, true);

    // 2. Detached expiry follow-up AI publication (e.g. Blood Artist trigger target) is withheld for guest:
    const aiReports = new Map<number, unknown>([
      [0, hostReport],
      [
        1,
        {
          kind: "withheld",
          notice: { revision: 3 },
        },
      ],
    ]);
    await host.handleNativeRevision(3, aiReports, true);

    // Verify both frames were broadcast to guest with saying: true:
    // 1. Delivered state_update with saying: true
    // 2. Withheld state_withheld with saying: true
    expect(sentWireMessages).toContainEqual(
      expect.objectContaining({
        type: "state_update",
        revision: 2,
        saying: true,
      }),
    );
    expect(sentWireMessages).toContainEqual(
      expect.objectContaining({
        type: "state_withheld",
        revision: 3,
        saying: true,
      }),
    );

    // Crucially: guest's ordinary parked submission was NOT settled by either frame!
    expect(ordinarySettled).toBe(false);

    // When guest's own ordinary completion arrives without saying, it settles:
    const ordinaryReports = new Map<number, unknown>([
      [0, hostReport],
      [
        1,
        {
          kind: "update",
          update: {
            snapshot: { state: remoteState("ordinary-completed"), legalResult: { actions: [] } },
            events: [{ marker: "ordinary-done" } as unknown as GameEvent],
            logEntries: [],
          },
        },
      ],
    ]);
    await host.handleNativeRevision(4, ordinaryReports, false);

    expect(ordinarySettled).toBe(true);
    await expect(ordinary).resolves.toEqual({
      events: [{ marker: "ordinary-done" }],
      log_entries: [],
    });

    guest.dispose();
  });

});
