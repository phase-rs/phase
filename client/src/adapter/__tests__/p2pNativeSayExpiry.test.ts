import { afterEach, describe, expect, it, vi } from "vitest";

import { FakeDataConnection } from "../../network/__tests__/fakeDataConnection.ts";
import { WIRE_PROTOCOL_VERSION, type P2PMessage } from "../../network/protocol.ts";
import type { TransportConnection, TransportPeer } from "../../network/transport.ts";
import { NativeP2PBridge, P2PGuestAdapter, P2PHostAdapter } from "../p2p-adapter.ts";
import { EMPTY_LEGAL_ACTIONS } from "../types.ts";
import type {
  GameAction,
  GameEvent,
  GameState,
  SayPhraseExpiryTarget,
  SubmitResult,
} from "../types.ts";

/**
 * Toy Boat (UNH): "Cumulative upkeep—Say "Toy Boat" quickly." On a desktop
 * (native) P2P host the sidecar's engine owns the payment; these tests drive
 * the production `NativeP2PBridge` and `P2PHostAdapter` methods with fake
 * loopback seat sockets standing in for the sidecar.
 */

type Listener = (event: Record<string, unknown>) => void;

/** One loopback seat socket: the `WebSocketAdapter` surface the bridge uses. */
class FakeSeatClient {
  listener: Listener | null = null;
  readonly dispose = vi.fn();
  readonly sayPhraseExpiry = vi.fn<(lapse?: number) => Promise<SayPhraseExpiryTarget | null>>(
    async () => null,
  );
  readonly submitAction = vi.fn<(action: GameAction, actor: number) => Promise<SubmitResult>>(
    async () => ({ events: [], log_entries: [] }),
  );
  readonly exportPersistenceState = vi.fn(async () => {
    throw new Error("a projected read must not export trusted state");
  });

  constructor(readonly playerId: number, readonly state: GameState) {}

  onEvent(listener: Listener): () => void {
    this.listener = listener;
    return () => {};
  }

  async initializePregame() {
    return {
      gameCode: "TOYBOAT",
      playerId: this.playerId,
      playerToken: `token-${this.playerId}`,
      fullKey: { game_code: "TOYBOAT", generation: 1 },
    };
  }

  async getState(): Promise<GameState> {
    return this.state;
  }

  emit(event: Record<string, unknown>): void {
    this.listener?.(event);
  }
}

function viewState(label: string, waitingPlayer = 0): GameState {
  return {
    label,
    turn_number: 1,
    active_player: 0,
    priority_player: 0,
    phase: "Upkeep",
    players: [],
    objects: {},
    waiting_for: { type: "UnlessPayment", data: { player: waitingPlayer } },
  } as unknown as GameState;
}

type BridgeInternals = {
  attachClient: (client: unknown) => Promise<unknown>;
  reconnectClient: (playerId: number, token: string) => Promise<unknown>;
};

type HostInternals = {
  disposed: boolean;
  ownsAuthority: () => boolean;
  nativeBridge: NativeP2PBridge;
  guestSessions: Map<number, unknown>;
  disconnectedSeats: Map<number, unknown>;
  concedingSeats: Set<number>;
  gameRunState: string;
  listeners: Array<(event: { type: string } & Record<string, unknown>) => void>;
  sayExpiryGeneration: number;
  sayExpiryTimerGeneration: number;
  sayExpiryPayment: number | null;
  sayExpiryDeadline: number | null;
  sayExpiryTimer: ReturnType<typeof setTimeout> | null;
  authoritativeRevision: number;
  sayingRevisions: Set<number>;
  nativeInitialSetupPending: boolean;
  nativeWithheldSeats: Map<number, unknown>;
  nativeDeliveredViews: Map<number, unknown>;
  playerTokens: Map<number, string>;
  playerNamesForSeats: () => Record<number, string>;
  commitTerminalIfComplete: () => Promise<void>;
  enqueueDelivery: <T>(operation: () => Promise<T>) => Promise<T>;
  send: (session: unknown, msg: unknown) => Promise<boolean>;
  scheduleSayPhraseExpiry: () => void;
  expireSayPhrase: (paymentId: number, timerGen?: number) => Promise<void>;
  handleNativeRevision: (
    revision: number,
    reports: Map<number, unknown>,
    saying?: boolean | string,
  ) => Promise<void>;
  submitAction: (action: GameAction, actor: number) => Promise<SubmitResult>;
  clearSayExpiryTimer: () => void;
};

const NOW = () => Date.now();

/** A host adapter whose own expiry and revision logic is production code. */
function makeHost(): HostInternals {
  const host = Object.create(P2PHostAdapter.prototype) as unknown as HostInternals;
  host.disposed = false;
  host.ownsAuthority = () => true;
  host.guestSessions = new Map();
  host.disconnectedSeats = new Map();
  host.concedingSeats = new Set();
  host.gameRunState = "running";
  host.listeners = [];
  host.sayExpiryGeneration = 0;
  host.sayExpiryTimerGeneration = 0;
  host.sayExpiryPayment = null;
  host.sayExpiryDeadline = null;
  host.sayExpiryTimer = null;
  host.authoritativeRevision = 0;
  host.sayingRevisions = new Set();
  host.nativeInitialSetupPending = false;
  host.nativeWithheldSeats = new Map();
  host.nativeDeliveredViews = new Map();
  host.playerTokens = new Map();
  host.playerNamesForSeats = () => ({});
  host.commitTerminalIfComplete = vi.fn(async () => {});
  host.enqueueDelivery = (operation) => operation();
  return host;
}

/** The production bridge, its revision barrier wired to the host's own handler. */
async function makeBridge(host: HostInternals, seats: FakeSeatClient[]): Promise<NativeP2PBridge> {
  const bridge = new NativeP2PBridge(
    { player: { main_deck: [], sideboard: [] } } as never,
    "Host",
    seats.length,
    undefined,
    undefined,
    { expectedServerVersion: "test" } as never,
    (revision, reports, saying) => host.handleNativeRevision(revision, reports, saying),
    async () => {},
    async () => {},
    async () => {},
    () => undefined,
  );
  const internals = bridge as unknown as BridgeInternals;
  for (const seat of seats) await internals.attachClient(seat);
  host.nativeBridge = bridge;
  return bridge;
}

function createFakePeer() {
  return { on() {}, off() {}, connect() { throw new Error("not used"); }, destroy() {} };
}

async function authenticatedGuest(conn: FakeDataConnection): Promise<P2PGuestAdapter> {
  const guest = new P2PGuestAdapter(
    { player: { main_deck: [], sideboard: [] } },
    createFakePeer() as unknown as TransportPeer,
    "host-peer",
    conn as unknown as TransportConnection,
  );
  await guest.initialize();
  await conn.simulateData({
    type: "game_setup",
    wireProtocolVersion: WIRE_PROTOCOL_VERSION,
    assignedPlayerId: 1,
    playerToken: "seat-token",
    revision: 1,
    state: viewState("initial", 1),
    events: [],
    legalActions: [],
  });
  return guest;
}

function deferred<T>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((settle) => {
    resolve = settle;
  });
  return { promise, resolve };
}

afterEach(() => {
  vi.useRealTimers();
});

describe("native P2P spoken-payment expiry", () => {
  it("arms from the sidecar's bounded read while the host view is withheld, latest read winning", async () => {
    const host = makeHost();
    const hostSeat = new FakeSeatClient(0, viewState("before the first saying"));
    const bridge = await makeBridge(host, [hostSeat]);
    // The host seat's projection of the first saying was withheld: its cached
    // view predates the payment's deadline.
    hostSeat.emit({ type: "stateDelivery", withheld: { revision: 5, reason: "over bound" }, saying: true });
    // The withheld revision crosses the barrier, which itself re-reads the payment.
    await vi.waitFor(() => expect(host.authoritativeRevision).toBe(5));
    await vi.waitFor(() => expect(hostSeat.sayPhraseExpiry).toHaveBeenCalledTimes(1));
    await Promise.resolve();

    const older = deferred<SayPhraseExpiryTarget | null>();
    const newer = deferred<SayPhraseExpiryTarget | null>();
    hostSeat.sayPhraseExpiry.mockImplementationOnce(() => older.promise);
    hostSeat.sayPhraseExpiry.mockImplementationOnce(() => newer.promise);
    host.scheduleSayPhraseExpiry();
    host.scheduleSayPhraseExpiry();

    // Exactly one read is in-flight: overlapping schedules coalesce to a pending refresh.
    expect(hostSeat.sayPhraseExpiry).toHaveBeenCalledTimes(2);

    // Resolving the in-flight read drains the pending refresh, initiating the newer read.
    older.resolve(null);
    await vi.waitFor(() => expect(hostSeat.sayPhraseExpiry).toHaveBeenCalledTimes(3));

    const deadline = NOW() + 60_000;
    newer.resolve({ payer: 0, submitter: 0, payment: 7, deadline_ms: deadline });
    await vi.waitFor(() => expect(host.sayExpiryPayment).toBe(7));
    expect(host.sayExpiryDeadline).toBe(deadline);
    expect(host.sayExpiryTimer).not.toBeNull();
    // Viewer reads stay projected: no trusted export substitutes for them.
    expect((await bridge.getState()) as unknown as { label: string }).toMatchObject({
      label: "before the first saying",
    });
    expect(hostSeat.exportPersistenceState).not.toHaveBeenCalled();
    host.clearSayExpiryTimer();
  });

  it("coalesces overlapping expiry reads and recovers when a request receives no reply while the payer is absent", async () => {
    vi.useFakeTimers();
    try {
      const host = makeHost();
      const hostSeat = new FakeSeatClient(0, viewState("host"));
      const payerSeat = new FakeSeatClient(1, viewState("payer", 1));
      const bridge = await makeBridge(host, [hostSeat, payerSeat]);

      // The payer's channel dropped: the bridge suspended its loopback socket.
      bridge.suspendGuest(1);
      host.disconnectedSeats.set(1, { disconnectedAt: NOW(), timer: null });
      expect(bridge.isGuestAttached(1)).toBe(false);

      // The first read receives no reply / times out with an error.
      hostSeat.sayPhraseExpiry.mockRejectedValueOnce(new Error("request timed out with no reply"));

      // Trigger scheduled refresh and waking timer read concurrently while absent:
      host.scheduleSayPhraseExpiry();
      const timerRead = host.expireSayPhrase(7, 0);

      // Only one in-flight read occurred (waking timer read shares the in-flight scheduled read):
      expect(hostSeat.sayPhraseExpiry).toHaveBeenCalledTimes(1);

      await timerRead;
      // Flush microtasks for rejection:
      await Promise.resolve();
      await Promise.resolve();
      expect(host.sayExpiryPayment).toBeNull();

      // After SAY_EXPIRY_RETRY_MS (250ms), it automatically retries with a fresh read:
      const deadline = NOW() + 1000;
      hostSeat.sayPhraseExpiry.mockResolvedValueOnce({
        payer: 1,
        submitter: 1,
        payment: 7,
        deadline_ms: deadline,
      });

      await vi.advanceTimersByTimeAsync(250);
      await Promise.resolve();
      await Promise.resolve();
      expect(hostSeat.sayPhraseExpiry).toHaveBeenCalledTimes(2);
      expect(host.sayExpiryPayment).toBe(7);
      expect(host.sayExpiryDeadline).toBe(deadline);
      expect(bridge.isGuestAttached(1)).toBe(false);

      // Advance timer past deadline to exercise eventual lapse:
      hostSeat.sayPhraseExpiry.mockImplementation(async (lapse?: number) => {
        if (lapse === 7) return null; // lapsed
        return { payer: 1, submitter: 1, payment: 7, deadline_ms: deadline };
      });
      await vi.advanceTimersByTimeAsync(1001);
      await Promise.resolve();
      await Promise.resolve();
      // The expiry timer fired and requested the lapse on hostSeat:
      expect(hostSeat.sayPhraseExpiry).toHaveBeenCalledWith(7);
      expect(host.sayExpiryPayment).toBeNull();
      host.clearSayExpiryTimer();
    } finally {
      vi.useRealTimers();
    }
  });

  it("services a newer publication's read when the in-flight read fails", async () => {
    vi.useFakeTimers();
    try {
      const host = makeHost();
      const hostSeat = new FakeSeatClient(0, viewState("host"));
      await makeBridge(host, [hostSeat]);
      // No timer is armed: only the drain can arm one.
      const failing = deferred<SayPhraseExpiryTarget | null>();
      const deadline = NOW() + 60_000;
      hostSeat.sayPhraseExpiry.mockImplementationOnce(
        () => failing.promise.then(() => Promise.reject(new Error("socket dropped the reply"))),
      );
      hostSeat.sayPhraseExpiry.mockResolvedValueOnce({
        payer: 0,
        submitter: 0,
        payment: 7,
        deadline_ms: deadline,
      });

      host.scheduleSayPhraseExpiry();
      // A newer publication lands while that read is in flight: it advances the
      // generation and leaves its own read pending behind the first.
      host.scheduleSayPhraseExpiry();
      expect(hostSeat.sayPhraseExpiry).toHaveBeenCalledTimes(1);

      failing.resolve(null);
      // The stale failure does not end the drain: the newer read is issued at
      // once, without waiting for the retry pause, and arms the payment.
      await vi.waitFor(() => expect(hostSeat.sayPhraseExpiry).toHaveBeenCalledTimes(2));
      await vi.waitFor(() => expect(host.sayExpiryPayment).toBe(7));
      expect(host.sayExpiryDeadline).toBe(deadline);
      expect(host.sayExpiryTimer).not.toBeNull();
      host.clearSayExpiryTimer();
    } finally {
      vi.useRealTimers();
    }
  });

  it("keeps a browser host's expiry timer alive when its refresh read fails", async () => {
    vi.useFakeTimers();
    try {
      const host = makeHost() as HostInternals & { wasm: { getState: () => Promise<GameState> } };
      (host as unknown as { nativeBridge: undefined }).nativeBridge = undefined;
      const deadline = NOW() + 60_000;
      const live = {
        ...viewState("live payment"),
        say_payment: { id: 7, owed: 2, said: 1, clock_ms: 0, deadline_ms: deadline },
      } as unknown as GameState;
      const getState = vi
        .fn<() => Promise<GameState>>()
        .mockRejectedValueOnce(new Error("engine worker busy"))
        .mockResolvedValue(live);
      host.wasm = { getState };
      host.sayExpiryTimerGeneration = 1;
      host.sayExpiryPayment = 7;
      host.sayExpiryDeadline = NOW() - 1;

      // The timer fired, but its refresh read failed.
      await host.expireSayPhrase(7, 1);
      expect(getState).toHaveBeenCalledTimes(1);

      // It stays the payment's owner: after the pause it reads again and
      // re-times the payment instead of leaving it untimed.
      await vi.advanceTimersByTimeAsync(250);
      await vi.waitFor(() => expect(getState).toHaveBeenCalledTimes(2));
      await vi.waitFor(() => expect(host.sayExpiryDeadline).toBe(deadline));
      expect(host.sayExpiryPayment).toBe(7);
      expect(host.sayExpiryTimer).not.toBeNull();
      host.clearSayExpiryTimer();
    } finally {
      vi.useRealTimers();
    }
  });

  it("drains through the engine-authorized seat under turn control and revalidates when authority changes", async () => {
    const host = makeHost();
    const hostSeat = new FakeSeatClient(0, viewState("host"));
    const controllerSeat = new FakeSeatClient(1, viewState("controller", 1));
    const payerSeat = new FakeSeatClient(2, viewState("payer", 2));
    await makeBridge(host, [hostSeat, controllerSeat, payerSeat]);

    // Scenario 1: Timely queued controller frame.
    // Payer is seat 2, but under turn control (CR 723.5 / Mindslaver), controller/submitter is seat 1.
    let controllerFrameApplied = false;
    const controllerSession = {
      drain: vi.fn(async () => {
        // Simulates timely saying frame queued on controller's WebRTC channel arriving and being applied:
        controllerFrameApplied = true;
      }),
    };
    const payerSession = {
      drain: vi.fn(async () => {}),
    };
    host.guestSessions.set(1, controllerSession);
    host.guestSessions.set(2, payerSession);

    // Initial read: payer is 2, submitter is 1 (distinct seats).
    // When controllerSession drains, controllerFrameApplied becomes true and the engine advances deadline.
    hostSeat.sayPhraseExpiry.mockImplementation(async (lapse?: number) => {
      if (lapse !== undefined) return null;
      if (controllerFrameApplied) {
        // A timely saying from controller moved the deadline:
        return { payer: 2, submitter: 1, payment: 7, deadline_ms: NOW() + 5000 };
      }
      return { payer: 2, submitter: 1, payment: 7, deadline_ms: NOW() - 1 };
    });

    host.sayExpiryTimerGeneration = 1;
    host.sayExpiryPayment = 7;
    await host.expireSayPhrase(7, 1);

    // Drained the authorized controller seat (1), NOT the semantic payer (2):
    expect(controllerSession.drain).toHaveBeenCalled();
    expect(payerSession.drain).not.toHaveBeenCalled();
    // Because the timely frame arrived during drain, lapse was NOT called!
    expect(hostSeat.sayPhraseExpiry).not.toHaveBeenCalledWith(7);
    // And expiry was re-armed with the new deadline:
    expect(host.sayExpiryDeadline).toBeGreaterThan(NOW());

    // Scenario 2: Controller stops responding after the first saying.
    // Expiry fires past the deadline. Controller sends nothing more.
    controllerFrameApplied = false;
    hostSeat.sayPhraseExpiry.mockImplementation(async (lapse?: number) => {
      if (lapse === 7) return null; // successfully lapsed under controller authority
      return { payer: 2, submitter: 1, payment: 7, deadline_ms: NOW() - 1 };
    });
    host.sayExpiryTimerGeneration = 2;
    host.sayExpiryPayment = 7;
    await host.expireSayPhrase(7, 2);

    // Drain was called again on controller seat:
    expect(controllerSession.drain).toHaveBeenCalledTimes(2);
    // Lapse was submitted and settled:
    expect(hostSeat.sayPhraseExpiry).toHaveBeenCalledWith(7);
    expect(host.sayExpiryPayment).toBeNull();
  });

  it("lapses through the host-only authority and leaves a suspended payer absent", async () => {
    const host = makeHost();
    const hostSeat = new FakeSeatClient(0, viewState("host"));
    const payerSeat = new FakeSeatClient(1, viewState("payer", 1));
    const bridge = await makeBridge(host, [hostSeat, payerSeat]);
    const reconnect = vi.fn(async () => {
      throw new Error("the payer must not be reattached");
    });
    (bridge as unknown as BridgeInternals).reconnectClient = reconnect;
    // The payer's channel dropped: the bridge suspended its loopback socket.
    bridge.suspendGuest(1);
    host.disconnectedSeats.set(1, { disconnectedAt: NOW(), timer: null });
    expect(bridge.isGuestAttached(1)).toBe(false);

    const order: string[] = [];
    const drain = vi.fn(async () => {
      order.push("drain");
      expect(bridge.isGuestAttached(1)).toBe(false);
    });
    host.guestSessions.set(1, { drain });
    hostSeat.sayPhraseExpiry.mockImplementation(async (lapse?: number) => {
      order.push(lapse === undefined ? "read" : `lapse ${lapse}`);
      expect(bridge.isGuestAttached(1)).toBe(false);
      return lapse === undefined ? { payer: 1, submitter: 1, payment: 7, deadline_ms: NOW() - 1 } : null;
    });
    host.sayExpiryTimerGeneration = 3;
    host.sayExpiryPayment = 7;

    await host.expireSayPhrase(7, 3);

    expect(order).toEqual(["read", "drain", "read", "lapse 7"]);
    expect(reconnect).not.toHaveBeenCalled();
    expect(payerSeat.submitAction).not.toHaveBeenCalled();
    expect(hostSeat.submitAction).not.toHaveBeenCalled();
    expect(bridge.isGuestAttached(1)).toBe(false);
    // The lapse settled the payment: nothing is timed any more.
    expect(host.sayExpiryPayment).toBeNull();
    expect(host.sayExpiryTimer).toBeNull();
  });

  it("re-reports a lapse the sidecar refused after a pause instead of spinning", async () => {
    vi.useFakeTimers();
    const host = makeHost();
    const hostSeat = new FakeSeatClient(0, viewState("host"));
    const payerSeat = new FakeSeatClient(1, viewState("payer", 1));
    const bridge = await makeBridge(host, [hostSeat, payerSeat]);
    // The payer's channel dropped: the bridge suspended its loopback socket.
    bridge.suspendGuest(1);
    host.disconnectedSeats.set(1, { disconnectedAt: NOW(), timer: null });
    expect(bridge.isGuestAttached(1)).toBe(false);

    const drain = vi.fn(async () => {});
    host.guestSessions.set(1, { drain });

    // A pending takeback vote refuses the lapse; the payment stays live.
    // The socket is open and payer is absent (payer === 1).
    hostSeat.sayPhraseExpiry.mockImplementation(async () => ({
      payer: 1,
      submitter: 1,
      payment: 7,
      deadline_ms: NOW() - 1,
    }));
    host.sayExpiryTimerGeneration = 1;
    host.sayExpiryPayment = 7;
    const schedule = vi.fn();
    host.scheduleSayPhraseExpiry = schedule;

    await host.expireSayPhrase(7, 1);
    expect(hostSeat.sayPhraseExpiry.mock.calls).toEqual([[undefined], [undefined], [7]]);
    expect(drain).toHaveBeenCalled();
    expect(bridge.isGuestAttached(1)).toBe(false);
    expect(schedule).not.toHaveBeenCalled();

    await vi.advanceTimersByTimeAsync(250);
    expect(schedule).toHaveBeenCalledTimes(1);
    expect(bridge.isGuestAttached(1)).toBe(false);
  });

  it("relays a withheld saying revision to a guest without settling its unrelated submission, keeping the expiry armed", async () => {
    const conn = new FakeDataConnection();
    const guest = await authenticatedGuest(conn);
    const ordinary = guest.submitAction({ type: "PassPriority" } as GameAction, 1);
    let ordinarySettled = false;
    void ordinary.then(
      () => { ordinarySettled = true; },
      () => { ordinarySettled = true; },
    );

    const host = makeHost();
    const sent: unknown[] = [];
    host.send = vi.fn(async (_session, msg) => {
      sent.push(msg);
      await conn.simulateData(msg as P2PMessage);
      return true;
    });
    host.guestSessions.set(1, {});
    const hostSeat = new FakeSeatClient(0, viewState("host"));
    const guestSeat = new FakeSeatClient(1, viewState("guest", 1));
    const deadline = NOW() + 60_000;
    hostSeat.sayPhraseExpiry.mockImplementation(async () => ({ payer: 0, submitter: 0, payment: 7, deadline_ms: deadline }));
    await makeBridge(host, [hostSeat, guestSeat]);

    // The sidecar withholds revision 3 from both seats; only the submitting
    // host seat's correlated reply knows it was a saying.
    hostSeat.emit({ type: "stateDelivery", withheld: { revision: 3, reason: "over bound" }, saying: true });
    guestSeat.emit({ type: "stateDelivery", withheld: { revision: 3, reason: "over bound" } });

    await vi.waitFor(() =>
      expect(sent).toContainEqual(expect.objectContaining({ type: "state_withheld", revision: 3, saying: true })),
    );
    await vi.waitFor(() => expect(host.sayExpiryPayment).toBe(7));
    expect(host.sayExpiryDeadline).toBe(deadline);
    expect(ordinarySettled).toBe(false);

    // Now test delivered revision (state_update with saying: true): ordinary command still does not settle
    hostSeat.emit({
      type: "stateChanged",
      serverRevision: 4,
      snapshot: { state: viewState("host"), legalResult: EMPTY_LEGAL_ACTIONS },
      events: [],
      logEntries: [],
      saying: true,
    });
    guestSeat.emit({
      type: "stateChanged",
      serverRevision: 4,
      snapshot: { state: viewState("guest", 1), legalResult: EMPTY_LEGAL_ACTIONS },
      events: [],
      logEntries: [],
    });

    await vi.waitFor(() =>
      expect(sent).toContainEqual(expect.objectContaining({ type: "state_update", revision: 4, saying: true })),
    );
    expect(ordinarySettled).toBe(false);

    // The ordinary command settles on its own reply
    await conn.simulateData({
      type: "state_update",
      revision: 5,
      state: viewState("guest", 1),
      events: [],
      logEntries: [],
      ...EMPTY_LEGAL_ACTIONS,
    } as unknown as P2PMessage);

    await vi.waitFor(() => expect(ordinarySettled).toBe(true));

    host.clearSayExpiryTimer();
    guest.dispose();
  });

  it("presents a native host saying once while fanning it out to the guest", async () => {
    const conn = new FakeDataConnection();
    const guest = await authenticatedGuest(conn);
    const host = makeHost();
    const sent: unknown[] = [];
    host.send = vi.fn(async (_session, msg) => {
      sent.push(msg);
      await conn.simulateData(msg as P2PMessage);
      return true;
    });
    host.guestSessions.set(1, {});
    host.scheduleSayPhraseExpiry = vi.fn();
    const hostSeat = new FakeSeatClient(0, viewState("host"));
    const guestSeat = new FakeSeatClient(1, viewState("guest", 1));
    await makeBridge(host, [hostSeat, guestSeat]);

    const said = { type: "SayPhraseObserved", data: { marker: "host-said" } } as unknown as GameEvent;
    const log = { marker: "host-said-log" };
    // The sidecar's correlated reply resolves the saying and, as for every
    // native seat, also emits the seat's view for the revision barrier.
    hostSeat.submitAction.mockImplementation(async () => {
      hostSeat.emit({
        type: "stateChanged",
        serverRevision: 4,
        snapshot: { state: viewState("after saying"), legalResult: EMPTY_LEGAL_ACTIONS },
        events: [said],
        logEntries: [log],
        saying: true,
      });
      guestSeat.emit({
        type: "stateChanged",
        serverRevision: 4,
        snapshot: { state: viewState("after saying", 1), legalResult: EMPTY_LEGAL_ACTIONS },
        events: [said],
        logEntries: [log],
      });
      return { events: [said], log_entries: [log] } as unknown as SubmitResult;
    });
    const presented: Array<{ events?: unknown[]; logEntries?: unknown[] }> = [];
    host.listeners.push((event) => {
      if (event.type === "stateChanged") presented.push(event as never);
    });

    const action: GameAction = {
      type: "SayPhrase",
      data: { payment: 7, seq: 1, at_ms: NOW(), report: "Saying" },
    };
    const result = await host.submitAction(action, 0);
    await vi.waitFor(() => expect(presented).toHaveLength(1));
    await vi.waitFor(() =>
      expect(sent).toContainEqual(expect.objectContaining({ type: "state_update", revision: 4, saying: true })),
    );

    const hostEvents = [...result.events, ...presented.flatMap((update) => update.events ?? [])];
    const hostLogs = [...(result.log_entries ?? []), ...presented.flatMap((update) => update.logEntries ?? [])];
    expect(hostEvents).toEqual([said]);
    expect(hostLogs).toEqual([log]);
    guest.dispose();
  });
});
