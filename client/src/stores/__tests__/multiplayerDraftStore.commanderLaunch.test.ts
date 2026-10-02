/**
 * The Commander pod's launch, join and cancel, through the REAL store actions.
 *
 * After step 3a-ii this action hosts a real N-player P2P game instead of
 * navigating to `?mode=ai`, so the whole transport has to be faked rather than
 * merely stubbed: the action does `await import("../network/connection")` and
 * calls the real `hostRoom` (which opens a PeerJS `Peer`), then parks on
 * `await roomFull`. Four modules are mocked and each one is load-bearing:
 *
 *  - `../../network/connection` — otherwise every case opens a real peer;
 *  - `../../adapter/p2p-adapter` — otherwise there is nothing to emit
 *    `roomFull` and every case hangs past the vitest timeout;
 *  - `../../game/controllers/gameLoopController` and a `getSnapshot` on the
 *    fake — `installMatchRuntime` calls `adapter.getSnapshot()`, then the REAL
 *    `useGameStore.commitEngineSnapshot`, then `createGameLoopController`, so
 *    without both the end-to-end case dies inside the runtime install rather
 *    than at its assertion.
 *
 * The fake `P2PHostAdapter`'s CONTRACT is part of this suite's discriminating
 * power. It emits `roomFull` from inside `applySeatMutation`, to whatever
 * listeners are registered at that moment, once no waiting seat is left. That
 * MIRRORS the real adapter's ORDERING without matching its mechanism: the real
 * emit runs inside `enqueuePregameOp` and is technically async, but it is the
 * op's last statement and so resolves before the caller's `await` returns
 * (`p2p-adapter.ts`, end of the `applySeatMutation` body, guarded by
 * `firstWaitingSeat() === null`). A fake shaped like the `multiplayerStore`
 * precedent (`onEvent: vi.fn()`) never emits and hangs whether or not the
 * ordering is right; a fake the test pokes by hand passes whether or not the
 * ordering is right. Only an emit driven by the mutation discriminates.
 *
 * The real adapter has a SECOND `roomFull` emit in its guest-connection
 * handler, for a joining human filling the last seat. No case here can reach
 * it, so the fake models only the mutation one.
 *
 * The PURE RE-EMIT of the `draftPodHostAdapter` forwarding case is NOT pinned
 * here — this suite replaces `DraftPodHostAdapter` with a mock, so an event
 * driven through that mock's captured listener lands straight in
 * `handleHostEvent` and skips the real forwarding case entirely. That
 * assertion lives on the real adapter, in
 * `client/src/adapter/__tests__/draftPodAdapter.test.ts`.
 *
 * The GUEST half is mocked one layer DEEPER, and deliberately so: the pod
 * adapter (`DraftPodGuestAdapter`) is REAL and only `P2PDraftGuest` beneath it
 * is faked, so a launch driven through the captured wire listener travels the
 * adapter's own forwarding case before it reaches `handleGuestEvent`. Mocking
 * the pod adapter, as the host half must, would skip exactly the arm whose
 * absence stranded the pod's guests. The game-side `P2PGuestAdapter` fake
 * carries only the surface `joinCommanderGame` calls — notably no
 * `startPregameGame` and no `terminateGame`, which are host methods.
 */

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { useMultiplayerDraftStore } from "../multiplayerDraftStore";
import { useConnectivityStore } from "../connectivityStore";
import { useGameStore } from "../gameStore";
import { processRemoteUpdate } from "../../game/dispatch";
import { P2PGuestAdapter, P2PHostAdapter } from "../../adapter/p2p-adapter";
import { P2PDraftGuest } from "../../adapter/p2p-draft-guest";
import { DraftPodGuestAdapter } from "../../adapter/draftPodGuestAdapter";
import type { DraftPlayerView, SeatPublicView } from "../../adapter/draft-adapter";
import type { CommanderSeatDecks, DraftDeckPayload } from "../../network/draftProtocol";

// ── Mocks ──────────────────────────────────────────────────────────────

let capturedHostEventHandler: ((event: unknown) => void) | null = null;

const commanderSeatDecks = vi.fn<
  (view: DraftPlayerView, localSeat: number) => Promise<CommanderSeatDecks>
>();
const sendCommanderLaunches = vi.fn();
const boosterPackPoolForGame = vi.fn(async () => null as string[] | null);
/**
 * The LOCAL-game payload, used only above the P2P seat ceiling.
 *
 * Built from `deckFor`, the same fixture every other deck in this file uses, so
 * it carries the real `DraftDeckPayload` shape (`main_deck`/`sideboard`/
 * `commander`). An ad-hoc literal here would type-check — `vi.fn` infers its
 * return rather than checking it against the adapter's signature — while
 * letting the stash assertion below pass on a shape `GameProvider` could never
 * read.
 */
const podCommanderDeckPayload = vi.fn(async (view: DraftPlayerView) => ({
  player: deckFor(0),
  opponent: deckFor(1),
  ai_decks: Array.from({ length: view.seats.length - 2 }, (_, i) => deckFor(i + 2)),
  draft_set_codes: ["CMR"],
  booster_pack_pool: await boosterPackPoolForGame(),
}));

const mockHostAdapter = {
  onEvent: vi.fn((handler: (event: unknown) => void) => {
    capturedHostEventHandler = handler;
    return vi.fn();
  }),
  initialize: vi.fn(async () => {}),
  dispose: vi.fn(async () => {}),
  commanderSeatDecks,
  boosterPackPoolForGame,
  sendCommanderLaunches,
  podCommanderDeckPayload,
  status: "lobby" as const,
  roomCode: "ABCDE",
};

/**
 * Queued ahead of `mockHostAdapter` for tests that open a SECOND pod and need
 * to tell that pod's own adapter apart from the first's — every other case in
 * this file shares the one `mockHostAdapter` singleton across every `new
 * DraftPodHostAdapter()` call, which cannot distinguish "disposed the session
 * that just opened" from "disposed the session that is leaving". See
 * `freshHostAdapter`.
 */
const mockHostAdapterQueue: Array<ReturnType<typeof freshHostAdapter>> = [];

vi.mock("../../adapter/draftPodHostAdapter", () => ({
  // `function`, not an arrow: `hostDraft` calls `new DraftPodHostAdapter()`.
  DraftPodHostAdapter: vi.fn().mockImplementation(function () {
    return mockHostAdapterQueue.shift() ?? mockHostAdapter;
  }),
}));

const transport = vi.hoisted(() => {
  const hostRoomOptions: Array<Record<string, unknown> | undefined> = [];
  /** First argument of every `hostRoom` call — the cancellation signal. */
  const hostRoomSignals: unknown[] = [];
  const hostDestroy = vi.fn();
  /**
   * Shared across fake instances so seat mutations carry an
   * `invocationCallOrder` comparable with `sendCommanderLaunches`' and
   * `startPregameGame`'s. An instance-local `vi.fn()` would record the calls
   * but leave the cross-spy ordering — the axis that decides whether a real
   * player gets kicked — unassertable.
   */
  const applySeatMutation = vi.fn();
  const startPregameGame = vi.fn(async () => ({ log_entries: [] }));
  const dispose = vi.fn();
  const terminateGame = vi.fn(async () => {});
  /** Every fake built this run, so teardown can unpark a launch left waiting. */
  const instances: Array<{ finish: () => void }> = [];
  /** `joinRoom`'s arguments, per call: the room code and the abort signal. */
  const joinRoomCalls: Array<{ code: string; signal: unknown }> = [];
  const joinDestroyPeer = vi.fn();
  const guestDispose = vi.fn();
  /** Every guest fake built this run, so a case can drive its wire events. */
  const guestInstances: Array<{ emit: (event: unknown) => void }> = [];
  /**
   * OPT-IN park for the `hostRoom` fake. It is the suite-wide module mock and
   * every other case awaits a launch through it, so parking unconditionally
   * would hang all of them; only the cancel-during-signalling case sets it.
   */
  const control = {
    parkHostRoom: false,
    /**
     * When set, `hostRoom` parks on it after logging the call — and unlike
     * `parkHostRoom` above, which can only ever REJECT (on abort), this one can
     * be RELEASED. That is what lets a case change the published view while the
     * signalling round-trip is in flight and then watch the launch continue.
     */
    hostRoomGate: null as Promise<void> | null,
    /** When set, `joinRoom` parks on it after logging the call. */
    joinRoomGate: null as Promise<void> | null,
    /** When set, the guest's `getSnapshot` parks on it. */
    guestSnapshotGate: null as Promise<void> | null,
    /** When set, the pod guest's own `leave()` (the wire ack `leave` awaits) parks on it. */
    guestLeaveGate: null as Promise<void> | null,
    /**
     * The seat the host assigns this guest, emitted during the bring-up.
     * DELIBERATELY NOT 2: `installJoinedPod`'s default `localSeat` is 2 and is
     * written to the store as `seatIndex`, so a wire seat of 2 would let
     * `commanderSeat: get().seatIndex` — the exact derivation the store forbids,
     * since human guests are seated in CONNECTION order — satisfy the seat
     * assertion. 1 also stays clear of the seat-0 fallback this step exists to
     * prevent, so all three candidate origins are distinguishable.
     */
    assignedSeat: 1,
  };
  const snapshot = {
    seq: 1,
    state: { waiting_for: { type: "Priority", data: {} } },
    legalResult: {
      actions: [],
      autoPassRecommended: false,
      endContinuousEffectOffers: [],
      manaPaymentShortcutActions: [],
      spellCosts: {},
      legalActionsByObject: {},
    },
  };
  return {
    hostRoomOptions,
    hostRoomSignals,
    hostDestroy,
    applySeatMutation,
    startPregameGame,
    dispose,
    terminateGame,
    instances,
    snapshot,
    joinRoomCalls,
    joinDestroyPeer,
    guestDispose,
    guestInstances,
    control,
  };
});

vi.mock("../../network/connection", () => ({
  hostRoom: vi.fn(async (signal: unknown, options?: Record<string, unknown>) => {
    transport.hostRoomSignals.push(signal);
    transport.hostRoomOptions.push(options);
    if (transport.control.parkHostRoom) {
      // The real `hostRoom` registers an abort listener and rejects from it.
      // Parking here is what gives a cancel a window to land IN — without it
      // the signalling round-trip is instantaneous and unobservable.
      await new Promise((_resolve, reject) => {
        (signal as AbortSignal | undefined)?.addEventListener(
          "abort",
          () => reject(new DOMException("Aborted", "AbortError")),
          { once: true },
        );
      });
    }
    // Logged and parked AFTER the signal, so a case can wait for the dial to
    // have happened and mutate store state before the round-trip returns.
    if (transport.control.hostRoomGate) await transport.control.hostRoomGate;
    return {
      roomCode: String(options?.preferredRoomCode ?? "ABCDE"),
      peerId: "peer-id",
      peer: { id: "peer-id" },
      onGuestConnected: vi.fn(() => vi.fn()),
      destroy: transport.hostDestroy,
    };
  }),
  joinRoom: vi.fn(async (code: string, signal?: unknown) => {
    transport.joinRoomCalls.push({ code, signal });
    // Logged BEFORE the park, so a case can wait for the dial to have happened
    // and press again while the first join is still in flight.
    if (transport.control.joinRoomGate) await transport.control.joinRoomGate;
    return {
      conn: { peer: "host-peer-id" },
      peer: { id: "guest-peer-id" },
      closeConn: vi.fn(),
      destroyPeer: transport.joinDestroyPeer,
    };
  }),
}));

vi.mock("../../adapter/p2p-adapter", () => ({
  P2PHostAdapter: vi.fn().mockImplementation(function (...args: unknown[]) {
    // Seats 1..N-1 start `WaitingHuman`; a `SetKind` claims one. `roomFull`
    // fires from inside the mutation that empties the set — see the header.
    const waiting = new Set(
      Array.from({ length: (args[3] as number) - 1 }, (_, i) => i + 1),
    );
    const listeners: Array<(event: unknown) => void> = [];
    const emitRoomFull = () => {
      for (const listener of [...listeners]) listener({ type: "roomFull" });
    };
    transport.instances.push({ finish: emitRoomFull });
    return {
      onEvent(listener: (event: unknown) => void) {
        listeners.push(listener);
        return () => {};
      },
      initialize: vi.fn(async () => {}),
      applySeatMutation: async (mutation: { type: string; data?: { seatIndex: number } }) => {
        transport.applySeatMutation(mutation);
        if (mutation.type === "SetKind" && mutation.data) waiting.delete(mutation.data.seatIndex);
        if (waiting.size === 0) emitRoomFull();
      },
      startPregameGame: transport.startPregameGame,
      getSnapshot: vi.fn(async () => transport.snapshot),
      dispose: transport.dispose,
      terminateGame: transport.terminateGame,
    };
  }),
  // The guest half's surface is EXACTLY what `joinCommanderGame` calls. It has
  // no `startPregameGame` and no `terminateGame` — those are host methods, and
  // a fake that accepted them would accept calls the real class rejects.
  P2PGuestAdapter: vi.fn().mockImplementation(function () {
    const listeners: Array<(event: unknown) => void> = [];
    const emit = (event: unknown) => {
      for (const listener of [...listeners]) listener(event);
    };
    transport.guestInstances.push({ emit });
    return {
      onEvent(listener: (event: unknown) => void) {
        listeners.push(listener);
        return () => {};
      },
      // EMITS WHERE PRODUCTION EMITS: when the host's reply is processed.
      //
      // The real `P2PGuestAdapter.initialize()` attaches the session and SENDS
      // (`guest_deck` or `reconnect`); it emits nothing. The only two
      // `playerIdentity` emits live in `handleHostMessage`, under `game_setup`
      // and `reconnect_ack`, each immediately followed by the settle of
      // `gameSetupPromise` — and `initializeGame()` is exactly
      // `return this.gameSetupPromise`. So the emit and the settle are one
      // step, and this fake models that step by emitting from
      // `initializeGame()` and then resolving.
      //
      // An earlier version emitted from `initialize()` instead, on the
      // reasoning that `attachSession` opens the delivery window there. The
      // window is right; the emit is not. `initialize()` contains no `await`,
      // so no inbound network message can be processed before it returns —
      // production's real constraint is "attached before the host's reply
      // arrives", not "attached before `initialize()` returns". Pinning the
      // stricter one would red a correct reordering.
      //
      // WHAT THIS PINS: a listener attached any later than the host's reply —
      // in particular the original defect, an attach moved after the bring-up,
      // which loses the identity and silently falls every guest back to the
      // HOST's seat. That reds `commanderSeat` here.
      // WHAT IT DOES NOT PIN: an attach between `initialize()` and
      // `initializeGame()`. Production tolerates that, so this stays green for
      // it — deliberately, not by omission.
      initialize: vi.fn(async () => {}),
      initializeGame: vi.fn(async () => {
        emit({ type: "playerIdentity", playerId: transport.control.assignedSeat });
        return { log_entries: [] };
      }),
      getSnapshot: vi.fn(async () => {
        if (transport.control.guestSnapshotGate) await transport.control.guestSnapshotGate;
        return transport.snapshot;
      }),
      dispose: transport.guestDispose,
    };
  }),
}));

/** The pod guest's wire, captured from the REAL `DraftPodGuestAdapter`. */
let capturedDraftGuestListener: ((event: unknown) => void) | null = null;
/**
 * The current pod guest instance's own `leave` spy, for a case that gates
 * `transport.control.guestLeaveGate` and needs to observe it having been
 * called (and not merely resolved) while parked.
 */
let capturedDraftGuestLeave: ReturnType<typeof vi.fn> | null = null;

vi.mock("../../adapter/p2p-draft-guest", () => ({
  P2PDraftGuest: vi.fn().mockImplementation(function () {
    const leave = vi.fn(async () => {
      if (transport.control.guestLeaveGate) await transport.control.guestLeaveGate;
    });
    capturedDraftGuestLeave = leave;
    return {
      onEvent: (listener: (event: unknown) => void) => {
        capturedDraftGuestListener = listener;
        return () => {};
      },
      initialize: vi.fn(async () => {}),
      dispose: vi.fn(),
      leave,
      isRecoveryRevoked: false,
    };
  }),
}));

/**
 * PARTIAL mock: everything real except `processRemoteUpdate`, which is what the
 * adapters' `stateChanged` arms forward into. Spreading the original matters —
 * `staleStateWatchdog` is in this graph and imports other members of the module,
 * and a bare factory would leave those undefined.
 */
vi.mock("../../game/dispatch", async (importOriginal) => ({
  ...(await importOriginal<typeof import("../../game/dispatch")>()),
  processRemoteUpdate: vi.fn(async () => {}),
}));

const matchLoopMock = vi.hoisted(() => ({
  controller: { start: vi.fn(), stop: vi.fn(), dispose: vi.fn() },
  create: vi.fn(),
}));

vi.mock("../../game/controllers/gameLoopController", () => ({
  createGameLoopController: (...args: unknown[]) => {
    matchLoopMock.create(...args);
    return matchLoopMock.controller;
  },
}));

// ── Fixtures ───────────────────────────────────────────────────────────

function seat(index: number, isBot: boolean, connected = true): SeatPublicView {
  return {
    seat_index: index,
    display_name: isBot ? `Bot ${index}` : `Player ${index}`,
    is_bot: isBot,
    connected,
    has_submitted_deck: true,
    pick_status: "NotDrafting",
    active_pack_count: 0,
    drafted_card_count: 0,
    face_up_draft_cards: [],
  };
}

function deckFor(index: number): DraftDeckPayload {
  return {
    main_deck: [`Commander ${index}`, `Spell ${index}`],
    sideboard: [`Side ${index}`],
    commander: [`Commander ${index}`],
  };
}

/** Seat 0 is the human host; `humanSeats` names any OTHER live human seats. */
function commanderView(
  seatCount: number,
  options: {
    draftSetCodes?: string[] | null;
    boosterPackPool?: string[] | null;
    humanSeats?: number[];
    /**
     * Human seats whose `connected` flag is FALSE — a player who has dropped.
     * `commanderSeatPlan`'s rule is `!is_bot && connected`, so a dropped human
     * is engine-piloted, which is the whole difference a stale view erases.
     */
    droppedSeats?: number[];
  } = {},
): DraftPlayerView {
  boosterPackPoolForGame.mockResolvedValue(options.boosterPackPool ?? null);
  const humans = new Set([0, ...(options.humanSeats ?? [])]);
  const dropped = new Set(options.droppedSeats ?? []);
  return {
    status: "Complete",
    kind: "CommanderDraft",
    launch_capability: "CommanderMultiplayer",
    commanders_required: 1,
    current_pack_number: 3,
    pick_number: 1,
    pass_direction: "Left",
    current_pack: null,
    required_pick_count: 0,
    pool: [],
    draft_effects: [],
    draft_set_codes: options.draftSetCodes,
    seats: Array.from({ length: seatCount }, (_, i) => seat(i, !humans.has(i), !dropped.has(i))),
    cards_per_pack: 14,
    pack_count: 3,
    min_deck_size: 60,
    addable_cards: [],
    timer_remaining_ms: null,
    standings: [],
    current_round: 0,
    next_pairing_round: 1,
    tournament_format: "Swiss",
    pod_policy: "Competitive",
    pairings: [],
    match_config: { match_type: "Bo1" },
  } as unknown as DraftPlayerView;
}

/**
 * The seat plan `commanderSeatDecks` would return for `view`: every non-host
 * live human in `liveSeatDecks`, everything else engine-piloted.
 */
function seatDecksFor(view: DraftPlayerView, localSeat = 0): CommanderSeatDecks {
  const hostDeck = deckFor(localSeat);
  const live = view.seats.filter((s) => !s.is_bot && s.connected);
  return {
    hostDeck,
    liveSeatDecks: live.map((s) => ({
      seat: s.seat_index,
      deck: s.seat_index === localSeat ? hostDeck : deckFor(s.seat_index),
    })),
    engineSeatDecks: view.seats
      .filter((s) => s.is_bot || !s.connected)
      .map((s) => ({ seat: s.seat_index, deck: deckFor(s.seat_index) })),
  };
}

/**
 * Installs the store's module-private `activeHostAdapter` the way production
 * does — by driving the real `hostDraft` against the mocked host adapter —
 * then publishes a completed Commander pod over it.
 */
async function installCompletedPod(view: DraftPlayerView, localSeat = 0, signal?: AbortSignal) {
  await useMultiplayerDraftStore.getState().hostDraft({
    poolInput: { type: "Set", data: { pools: [{ code: "TST" }], sequence: ["TST"] } },
    kind: "CommanderDraft",
    podSize: view.seats.length,
    hostDisplayName: "Host",
    tournamentFormat: "Swiss",
    podPolicy: "Competitive",
    signal,
  });
  useMultiplayerDraftStore.setState({
    phase: "complete",
    role: "host",
    seatIndex: localSeat,
    roomCode: "ABCDE",
    view,
    error: null,
  });
  commanderSeatDecks.mockResolvedValue(seatDecksFor(view, localSeat));
}

/**
 * Installs the store's module-private `activeGuestAdapter` the way production
 * does — the REAL `DraftPodGuestAdapter` over a faked `P2PDraftGuest` — so an
 * event driven through `capturedDraftGuestListener` travels that adapter's own
 * forwarding case on its way to `handleGuestEvent`. Mocking the pod adapter and
 * poking its captured listener instead would skip the forwarding case entirely
 * and make the assertion vacuous.
 */
async function installJoinedPod(view: DraftPlayerView, localSeat = 2) {
  await useMultiplayerDraftStore.getState().joinDraft({
    kind: "new",
    roomCode: "ABCDE",
    displayName: `Player ${localSeat}`,
  });
  useMultiplayerDraftStore.setState({
    phase: "complete",
    role: "guest",
    seatIndex: localSeat,
    view,
    error: null,
  });
  // The pod's own room join belongs to this setup, not to the case under test:
  // the real `DraftPodGuestAdapter` dials `joinRoom` too, so leaving it in the
  // log would make every "the join dialled the host's room" assertion read the
  // POD's code instead of the launch's.
  transport.joinRoomCalls.length = 0;
}

/** The launch a host would have put on this seat's wire. */
function commanderLaunchFor(seat: number, gameId = "game-1") {
  return {
    gameId,
    roomCode: "ABCDE-commander-abcd1234",
    localDeck: deckFor(seat),
    playerCount: 4,
    draftSetCodes: ["CMM"],
  };
}

/** Parks the next guest adapter's bring-up steps on the given gates; a parked `initializeGame` is a host still filling its room. */
function parkNextGuest(gates: { initialize?: Promise<void>; initializeGame: Promise<void> }) {
  const make = vi.mocked(P2PGuestAdapter).getMockImplementation()!;
  const initialize = vi.fn();
  const initializeGame = vi.fn();
  vi.mocked(P2PGuestAdapter).mockImplementationOnce(function (this: unknown, ...args: unknown[]) {
    const adapter = (make as (...a: unknown[]) => Record<"initialize" | "initializeGame", () => Promise<unknown>>)
      .apply(this, args);
    const { initialize: attach, initializeGame: reply } = adapter;
    initialize.mockImplementation(async () => { await gates.initialize; return attach(); });
    initializeGame.mockImplementation(async () => { await gates.initializeGame; return reply(); });
    Object.assign(adapter, { initialize, initializeGame });
    return adapter as never;
  });
  return { initialize, initializeGame };
}

/** Gives each `createGameLoopController` call its own controller, so a case can tell one attempt's from another's. */
function distinctControllers() {
  const original = matchLoopMock.controller;
  const created: Array<typeof original> = [];
  matchLoopMock.create.mockImplementation(() => {
    matchLoopMock.controller = { start: vi.fn(), stop: vi.fn(), dispose: vi.fn() };
    created.push(matchLoopMock.controller);
  });
  return Object.assign(created, {
    restore: () => {
      matchLoopMock.create.mockReset();
      matchLoopMock.controller = original;
    },
  });
}

/** The seat mutations this launch applied, in call order. */
function seatMutations(): Array<Record<string, unknown>> {
  return transport.applySeatMutation.mock.calls.map((call) => call[0] as Record<string, unknown>);
}

/**
 * A `DraftPodHostAdapter` mock with its own independent `dispose` spy, queued
 * through `mockHostAdapterQueue` ahead of the shared `mockHostAdapter`.
 *
 * `dispose`'s default still takes a `preserveSession` argument (rather than
 * none) so a case overriding it can distinguish `leave`'s own call
 * (`preserveSession: false`) from `hostDraft`'s defensive teardown of a
 * still-live previous adapter (`preserveSession: true`) by that argument
 * alone.
 */
function freshHostAdapter(
  overrides: { dispose?: (options: { preserveSession: boolean }) => Promise<void> } = {},
) {
  return {
    onEvent: vi.fn((handler: (event: unknown) => void) => {
      capturedHostEventHandler = handler;
      return vi.fn();
    }),
    initialize: vi.fn(async () => {}),
    dispose: vi.fn(overrides.dispose ?? (async (_options: { preserveSession: boolean }) => {})),
    commanderSeatDecks,
    boosterPackPoolForGame,
    sendCommanderLaunches,
    podCommanderDeckPayload,
    status: "lobby" as const,
    roomCode: "ABCDE",
  };
}

const navigate = vi.fn();

// ── Tests ──────────────────────────────────────────────────────────────

describe("multiplayerDraftStore Commander launch", () => {
  beforeEach(async () => {
    // PRE-WARM the two specifiers `launchCommanderGame` loads through
    // `await import()`. Nothing static in this file loads either one — the pod
    // adapter is mocked and the store imports only `type { HostResult }`, which
    // erases — so on a cold registry both dynamic imports race the mocker's
    // registration. That race was OBSERVED to matter: a second same-tick press,
    // resuming from a cold `import("../network/connection")`, reached the REAL
    // module and threw `Failed to create room: ... does not support WebRTC`
    // from real PeerJS. Warming them here keeps every press inside the mocked
    // module graph. It does NOT make the repeat-press case below discriminate a
    // late guard — see the comment there.
    await Promise.all([import("../../network/connection"), import("../../adapter/p2p-adapter")]);
    vi.clearAllMocks();
    capturedHostEventHandler = null;
    capturedDraftGuestListener = null;
    capturedDraftGuestLeave = null;
    mockHostAdapterQueue.length = 0;
    transport.hostRoomOptions.length = 0;
    transport.hostRoomSignals.length = 0;
    transport.applySeatMutation.mockClear();
    transport.instances.length = 0;
    transport.joinRoomCalls.length = 0;
    transport.guestInstances.length = 0;
    transport.control.parkHostRoom = false;
    transport.control.hostRoomGate = null;
    transport.control.joinRoomGate = null;
    transport.control.guestSnapshotGate = null;
    transport.control.guestLeaveGate = null;
    transport.control.assignedSeat = 1;
    transport.startPregameGame.mockResolvedValue({ log_entries: [] });
  });

  afterEach(async () => {
    // Unpark any launch still waiting on `roomFull`. The store's in-flight
    // handle is module-local and cleared only by its own `finally`, so a case
    // that deliberately leaves a launch parked (there are two — no guest can
    // join until step 3b) would otherwise make the NEXT case return early at
    // the in-flight guard. One macrotask drains the unparked continuation's
    // microtasks through to that `finally`.
    for (const instance of transport.instances) instance.finish();
    await new Promise((resolve) => setTimeout(resolve, 0));
    useConnectivityStore.setState({ forcedOffline: false });
    useMultiplayerDraftStore.getState().reset();
  });

  it("writes commanderLaunch from the host event arm, and moves no pod phase", async () => {
    await installCompletedPod(commanderView(4));
    expect(capturedHostEventHandler).not.toBeNull();
    const launch = {
      gameId: "game-1",
      roomCode: "ABCDE-commander-game-1",
      localDeck: deckFor(0),
      playerCount: 4,
      draftSetCodes: ["CMM"],
    };

    capturedHostEventHandler?.({ type: "commanderLaunch", launch });

    expect(useMultiplayerDraftStore.getState().commanderLaunch).toEqual(launch);
    // The launch does not move the pod: the host must stay on `CompleteView`
    // so step 4's waiting state and D7's Cancel can render.
    expect(useMultiplayerDraftStore.getState().phase).toBe("complete");
  });

  it("brings an all-bot pod up end to end and navigates to the draft-match game", async () => {
    await installCompletedPod(commanderView(4));

    await useMultiplayerDraftStore.getState().launchCommanderGame(navigate);

    // Reaching `startPregameGame` at all is what the call ordering breaks if
    // written wrong: the listener must be attached before `initialize()`,
    // because the fake — like the real adapter — emits `roomFull` from inside
    // the mutation that claims the last waiting seat.
    expect(transport.startPregameGame).toHaveBeenCalledTimes(1);
    expect(navigate).toHaveBeenCalledTimes(1);
    const url = navigate.mock.calls[0][0] as string;
    expect(url).toContain("?mode=draft-match");
    expect(url).not.toContain("mode=ai");
    expect(useMultiplayerDraftStore.getState().commanderSeat).toBe(0);
    expect(useMultiplayerDraftStore.getState().matchAdapter).not.toBeNull();
    // Unlike `startMatch`, the launch leaves the pod phase alone.
    expect(useMultiplayerDraftStore.getState().phase).toBe("complete");

    expect(sendCommanderLaunches).toHaveBeenCalledTimes(1);
    const sendOrder = sendCommanderLaunches.mock.invocationCallOrder[0];
    // THE axis: the launch is dispatched only after the LAST engine seat has
    // claimed its index. Asserting merely that the send precedes
    // `startPregameGame` does NOT discriminate this — a launch that sends
    // BEFORE the mutations satisfies that too, and that ordering is the one
    // that KICKS A REAL PLAYER: a guest invited early takes
    // `firstWaitingSeat()` = 1, and the `SetKind` landing on that index
    // invalidates its token ("Removed from the room by the host").
    const mutationOrders = transport.applySeatMutation.mock.invocationCallOrder;
    expect(mutationOrders).toHaveLength(3);
    expect(Math.max(...mutationOrders)).toBeLessThan(sendOrder);
    // And the game does not start until the invitations are out.
    expect(sendOrder).toBeLessThan(transport.startPregameGame.mock.invocationCallOrder[0]);

    // The room code is per-launch, never the pod's own code reused.
    const roomCode = sendCommanderLaunches.mock.calls[0][2] as string;
    expect(roomCode).not.toBe("ABCDE");
    expect(transport.hostRoomOptions[0]?.preferredRoomCode).toBe(roomCode);
    // `hostRoom`'s FIRST parameter is its cancellation signal. Passing
    // `undefined` there leaves the whole PeerJS signalling round-trip — the
    // longest span in the launch — uncancellable, which is precisely the window
    // the in-flight handle exists to make cancellable for step 3b.
    expect(transport.hostRoomSignals[0]).toBeInstanceOf(AbortSignal);
    // The SHARED game id: guests are told to join the very game the host
    // opened. Otherwise unpinned — a launch naming a different id than the one
    // it navigates to would satisfy every other assertion in this file, and
    // every guest would install its runtime under an id no host is serving.
    const launchedGameId = sendCommanderLaunches.mock.calls[0][1] as string;
    expect(url).toBe(`/game/${launchedGameId}?mode=draft-match`);
  });

  // A hardcoded seat count passes the first row and fails the second.
  it.each([[4], [5]])("constructs the host adapter at the pod's own seat count (%i)", async (seatCount) => {
    await installCompletedPod(commanderView(seatCount));

    await useMultiplayerDraftStore.getState().launchCommanderGame(navigate);

    expect(P2PHostAdapter).toHaveBeenCalledTimes(1);
    expect(vi.mocked(P2PHostAdapter).mock.calls[0][3]).toBe(seatCount);
  });

  /**
   * CR 903.13f(3). `commanderSeatDecks` returns a `DraftDeckPayload`, which
   * carries no set codes, so this literal is where the draft's set list ENTERS
   * the game pipeline — the passthrough edits downstream are inert without it.
   * `DeckListPayload` is not exported, so the literal is untyped and a typo'd
   * key is invisible to `tsc`: no type check can replace this assertion.
   */
  it("carries the view's draft set codes onto the host deck payload", async () => {
    const pool = ["Cube A", "Cube A", "Undealt sentinel"];
    await installCompletedPod(commanderView(4, { draftSetCodes: ["CMM", "CLB"], boosterPackPool: pool }));

    await useMultiplayerDraftStore.getState().launchCommanderGame(navigate);

    const payload = vi.mocked(P2PHostAdapter).mock.calls[0][0] as {
      player: DraftDeckPayload;
      draft_set_codes: string[] | null;
      booster_pack_pool: string[];
    };
    expect(payload.draft_set_codes).toEqual(["CMM", "CLB"]);
    expect(payload.booster_pack_pool).toEqual(pool);
    expect(payload.player).toEqual(deckFor(0));
  });

  it("spells an absent set list as null rather than an empty array", async () => {
    await installCompletedPod(commanderView(4));

    await useMultiplayerDraftStore.getState().launchCommanderGame(navigate);

    const payload = vi.mocked(P2PHostAdapter).mock.calls[0][0] as { draft_set_codes: unknown };
    expect(payload.draft_set_codes).toBeNull();
  });

  /**
   * Seat 1 is a LIVE HUMAN, so this pod cannot fill and the launch parks on
   * `await roomFull` forever — the chartered interim state until step 3b lets a
   * guest join. The action is therefore left running rather than awaited, and
   * the assertions run once the mutations have landed.
   */
  it("mutates only the engine-piloted seats, never a live human's", async () => {
    const view = commanderView(4, { humanSeats: [1] });
    await installCompletedPod(view);

    void useMultiplayerDraftStore.getState().launchCommanderGame(navigate);
    await vi.waitFor(() => expect(seatMutations()).toHaveLength(2));

    expect(seatMutations()).toEqual([
      {
        type: "SetKind",
        data: {
          seatIndex: 2,
          kind: { type: "Ai", data: { difficulty: "Medium", deck: { type: "DeckList", data: deckFor(2) } } },
        },
      },
      {
        type: "SetKind",
        data: {
          seatIndex: 3,
          kind: { type: "Ai", data: { difficulty: "Medium", deck: { type: "DeckList", data: deckFor(3) } } },
        },
      },
    ]);
    // Still parked: no seat 1 mutation, so the room never filled.
    expect(transport.startPregameGame).not.toHaveBeenCalled();
    expect(navigate).not.toHaveBeenCalled();
  });

  /**
   * The ceiling is the TRANSPORT's, not the engine's: the Commander Draft
   * format allows `max_players: 8`, while `P2PHostAdapter` throws
   * `P2P_PLAYER_COUNT` above six. Such a pod is legal and its decks are real,
   * so it gets a LOCAL game rather than a refusal — which is what it got before
   * the multiplayer launch existed, and what disabling the button took away.
   *
   * Asserted as a fork, not just an outcome: the local path must be taken AND
   * none of the P2P machinery may be entered, because the two share nothing
   * past the ceiling check.
   */
  it.each([7, 8])("launches a local game for a %i-seat pod over the peer-to-peer seat ceiling", async (seats) => {
    const pool = ["Cube A", "Cube A", "Undealt sentinel"];
    await installCompletedPod(commanderView(seats, { boosterPackPool: pool }));

    await useMultiplayerDraftStore.getState().launchCommanderGame(navigate);

    // The local game, carrying the pod's own seat count — never the literal 4
    // and never the six-seat ceiling.
    expect(navigate).toHaveBeenCalledTimes(1);
    const target = vi.mocked(navigate).mock.calls[0]?.[0] as string;
    expect(target).toContain("mode=ai");
    expect(target).toContain("format=CommanderDraft");
    const url = new URL(target, "https://phase.test");
    expect(url.searchParams.get("players")).toBe(String(seats));
    expect(url.searchParams.get("source")).toBe("multiplayer");
    expect(url.searchParams.has("draftId")).toBe(false);
    // The payload is assembled by the HOST adapter, in game-player order, and
    // stashed where the game route reads it.
    expect(podCommanderDeckPayload).toHaveBeenCalledTimes(1);
    const gameId = target.slice("/game/".length, target.indexOf("?"));
    expect(sessionStorage.getItem(`phase:draft-deck:${gameId}`)).toContain("Commander 0");
    const payload = JSON.parse(sessionStorage.getItem(`phase:draft-deck:${gameId}`)!);
    expect(payload.booster_pack_pool).toEqual(pool);
    expect(payload.ai_decks).toEqual(Array.from({ length: seats - 2 }, (_, i) => deckFor(i + 2)));
    // No banner: this is a supported outcome, not a degraded one.
    expect(useMultiplayerDraftStore.getState().error).toBeNull();
    // NONE of the peer-to-peer bring-up is entered.
    expect(P2PHostAdapter).not.toHaveBeenCalled();
    expect(commanderSeatDecks).not.toHaveBeenCalled();
    expect(sendCommanderLaunches).not.toHaveBeenCalled();
    expect(transport.hostRoomSignals).toHaveLength(0);
    // `commanderLaunch` stays null: nothing went on any wire, so there is no
    // pod session for `endCommanderSession` to end.
    expect(useMultiplayerDraftStore.getState().commanderLaunch).toBeNull();
  });

  /**
   * The control for the row above. Six seats sits ON the ceiling and must take
   * the REAL multiplayer path, so "always launch locally" cannot pass.
   */
  it("takes the peer-to-peer path for a pod exactly at the seat ceiling", async () => {
    await installCompletedPod(commanderView(6, { humanSeats: [] }));

    await useMultiplayerDraftStore.getState().launchCommanderGame(navigate);

    expect(podCommanderDeckPayload).not.toHaveBeenCalled();
    expect(commanderSeatDecks).toHaveBeenCalledTimes(1);
    expect(P2PHostAdapter).toHaveBeenCalledTimes(1);
    expect(vi.mocked(navigate).mock.calls[0]?.[0]).toContain("mode=draft-match");
  });

  it("surfaces a deck-assembly refusal as error text and does not navigate", async () => {
    const consoleErrorSpy = vi.spyOn(console, "error").mockImplementation(() => {});
    await installCompletedPod(commanderView(4));
    commanderSeatDecks.mockRejectedValue(new Error("Seat 0 has no submitted deck"));

    await useMultiplayerDraftStore.getState().launchCommanderGame(navigate);

    expect(useMultiplayerDraftStore.getState().error).toBe("Seat 0 has no submitted deck");
    expect(navigate).not.toHaveBeenCalled();
    // Thrown BEFORE the adapter exists, so the room — and only the room — is
    // torn down, through `HostResult.destroy`.
    expect(transport.hostDestroy).toHaveBeenCalledTimes(1);
    expect(transport.dispose).not.toHaveBeenCalled();
    consoleErrorSpy.mockRestore();
  });

  /**
   * The launch button carries no `disabled` prop and step 4's waiting state
   * keys on `commanderLaunch`, which is written only after the launches are
   * sent — so a double-press would open a SECOND room and a second adapter,
   * put two launches on every live seat and leak the first adapter.
   *
   * TWO windows, and they fail differently, so both are pressed here (only the
   * later press is pinned — see below).
   *
   * The SAME-TICK press is the literal double-click, and it is the one a guard
   * that claims its slot only after the constructor lets through: the second
   * call reaches the guard while `hostRoom` is still doing PeerJS signalling,
   * finds the slot empty and opens a whole second room. The LATER press covers
   * the long park on `roomFull` (this pod holds a live human at seat 1, so it
   * never fills and the launch stays parked — the chartered interim state).
   */
  it("ignores a repeat press once a launch has claimed the in-flight slot", async () => {
    await installCompletedPod(commanderView(4, { humanSeats: [1] }));

    const store = useMultiplayerDraftStore.getState();
    void store.launchCommanderGame(navigate);
    void store.launchCommanderGame(navigate); // same tick — the double-click
    await vi.waitFor(() => expect(sendCommanderLaunches).toHaveBeenCalledTimes(1));

    await store.launchCommanderGame(navigate); // and again, mid-park

    // WHAT THIS PINS, exactly: that a press arriving after the in-flight slot is
    // claimed is ignored. It reds for NO guard at all. It does NOT red for a
    // LATE guard — the handle claimed after the constructor rather than before
    // the first `await` — and the same-tick double-click on the two lines above
    // is therefore exercised but not pinned. Measured, not assumed: with the
    // late guard reinstated the second press does enter with an unclaimed slot,
    // but it is still parked inside `await Promise.all([import(...)])` when any
    // fixed flush expires, so `rooms` reads 1 either way. Pre-warming both
    // specifiers (see `beforeEach`) moves the moment it resumes but does not
    // make it deterministic, and draining the runner with repeat imports does
    // not either. The guard's PLACEMENT is pinned by review, not by this case.
    expect(transport.hostRoomOptions).toHaveLength(1);
    expect(P2PHostAdapter).toHaveBeenCalledTimes(1);
    expect(sendCommanderLaunches).toHaveBeenCalledTimes(1);
  });

  it("uses the engine's own match config rather than one the frontend invents", async () => {
    const view = commanderView(4);
    await installCompletedPod(view);

    await useMultiplayerDraftStore.getState().launchCommanderGame(navigate);

    expect(vi.mocked(P2PHostAdapter).mock.calls[0][5]).toBe(view.match_config);
  });

  it("passes no persistence binding and no native bridge", async () => {
    await installCompletedPod(commanderView(4));

    await useMultiplayerDraftStore.getState().launchCommanderGame(navigate);

    const args = vi.mocked(P2PHostAdapter).mock.calls[0];
    // 11th positional — the resume record + engine-state persistence nothing in
    // `draft-match` reads (a deliberate divergence from PLAN.md D.3).
    expect(args[10]).toBeUndefined();
    // 12th — a native bridge would drop CR 903.13f(3) on desktop only.
    expect(args[11]).toBeUndefined();
  });

  /**
   * The reported bug, at its source: this event travelled the whole guest wire
   * and then fell out of a switch with no arm for it, so the three guests sat
   * on `CompleteView` with only "Return to menu".
   *
   * Driven through the REAL `DraftPodGuestAdapter` — see `installJoinedPod`.
   */
  it("writes commanderLaunch from the guest event arm, and moves no pod phase", async () => {
    await installJoinedPod(commanderView(4, { humanSeats: [1, 2, 3] }));
    expect(capturedDraftGuestListener).not.toBeNull();
    const launch = commanderLaunchFor(2);
    // Captured, never hardcoded: the assertion is "unchanged across the call",
    // not "equal to whatever the pod happens to be in".
    const phaseBefore = useMultiplayerDraftStore.getState().phase;

    capturedDraftGuestListener?.({ type: "commanderLaunch", launch });

    expect(useMultiplayerDraftStore.getState().commanderLaunch).toEqual(launch);
    expect(useMultiplayerDraftStore.getState().phase).toBe(phaseBefore);
  });

  /**
   * The OBSERVABLE half of `commanderJoinInFlight`, which is module-local and
   * therefore unselectable — the reason the guest's Join button had no
   * in-flight feedback at all while the host's launch had a live region.
   *
   * Driven through a really-parked join rather than by seeding the field: the
   * claim is that the flag tracks the module handle's own lifetime, and seeding
   * it would assert nothing about that. `joinRoomGate` holds the join inside
   * `joinRoom`, exactly where a real guest waits.
   */
  it("publishes a pending flag for the whole guest join and clears it when the join settles", async () => {
    await installJoinedPod(commanderView(4, { humanSeats: [1, 2, 3] }));
    capturedDraftGuestListener?.({ type: "commanderLaunch", launch: commanderLaunchFor(2) });
    // Non-vacuous: the field must be driven OFF its initial value first, or a
    // fix that never sets it would satisfy the clear-assertion below.
    expect(useMultiplayerDraftStore.getState().commanderJoinPending).toBe(false);

    let openGate!: () => void;
    transport.control.joinRoomGate = new Promise<void>((resolve) => {
      openGate = resolve;
    });
    const parked = useMultiplayerDraftStore.getState().joinCommanderGame(navigate);

    // try/finally, and NOT decoration. `joinRoomGate` is a bare promise the
    // fake awaits without consulting the abort signal, so an assertion that
    // throws in here would leave this join parked FOREVER — still holding
    // `commanderJoinInFlight`, which is module-local and survives `beforeEach`.
    // Every later test in this file would then hit the re-entry guard and fail
    // for a reason that has nothing to do with it. Measured, not hypothesised:
    // without this the mutation probe for this row reds EIGHT tests, seven of
    // them collateral.
    try {
      await vi.waitFor(() =>
        expect(useMultiplayerDraftStore.getState().commanderJoinPending).toBe(true),
      );
      // Still pending while parked — the whole point is that this span is long.
      expect(navigate).not.toHaveBeenCalled();
    } finally {
      openGate();
      await parked;
    }

    expect(useMultiplayerDraftStore.getState().commanderJoinPending).toBe(false);
    expect(navigate).toHaveBeenCalledTimes(1);
  });

  it("joins the launched game on its own deck and navigates to the draft-match game", async () => {
    await installJoinedPod(commanderView(4, { humanSeats: [1, 2, 3] }));
    const launch = commanderLaunchFor(2);
    capturedDraftGuestListener?.({ type: "commanderLaunch", launch });

    await useMultiplayerDraftStore.getState().joinCommanderGame(navigate);

    // The room the HOST opened, dialled with the join's own cancellation signal.
    expect(transport.joinRoomCalls).toHaveLength(1);
    expect(transport.joinRoomCalls[0]?.code).toBe(launch.roomCode);
    expect(transport.joinRoomCalls[0]?.signal).toBeInstanceOf(AbortSignal);
    // This seat's own drafted deck, and no set list: legality is judged
    // host-side from the host's payload.
    const args = vi.mocked(P2PGuestAdapter).mock.calls[0];
    expect(args[0]).toEqual({ player: launch.localDeck });
    // The runtime is installed and the adapter is in the store BEFORE the
    // navigation — GameProvider's `draft-match` branch is passive and bails
    // when it is not.
    expect(useMultiplayerDraftStore.getState().matchAdapter).not.toBeNull();
    expect(navigate).toHaveBeenCalledTimes(1);
    // The SHARED game id: the guest installs its runtime under the very id the
    // host is serving.
    expect(navigate.mock.calls[0][0]).toBe(`/game/${launch.gameId}?mode=draft-match`);
    // The launch does not move the pod off `complete`.
    expect(useMultiplayerDraftStore.getState().phase).toBe("complete");
  });

  /**
   * Asserted in ARGUMENT POSITION, not on the instance:
   * `expect(adapter.supportsMatchConcede).toBeUndefined()` passes against a
   * mocked module regardless, because a fake instance has no such property.
   */
  it("leaves the guest's whole-match concede unbound so Concede falls through to the engine", async () => {
    await installJoinedPod(commanderView(4, { humanSeats: [1, 2, 3] }));
    capturedDraftGuestListener?.({ type: "commanderLaunch", launch: commanderLaunchFor(2) });

    await useMultiplayerDraftStore.getState().joinCommanderGame(navigate);

    // 10th positional, 0-indexed: `matchConcedeBound`. `true` — what the 1v1
    // guest arm passes — makes the guest send a `match_concede` this host
    // refuses, and the send latches, so the Concede button is inert for good.
    expect(vi.mocked(P2PGuestAdapter).mock.calls[0][9]).toBeUndefined();
  });

  /**
   * ATTACH ORDER, pinned — at the OPENING of the window, not at its end. The
   * identity that decides this client's seat arrives on an inbound host message,
   * which becomes deliverable the moment `initialize()` attaches the session's
   * message handler; `initializeGame()` only awaits the promise that handler
   * settles. The fake emits from `initialize()` for that reason, so a listener
   * attached anywhere after that call — BETWEEN the two awaits as much as after
   * both — never sees the identity and the guest silently keeps the seat-0
   * default. That reordering breaks every guest in exactly the way this step
   * exists to fix, and leaves the other join cases green: they assert on the
   * room dialled, the ctor arguments and the navigation, none of which move.
   */
  it("records the seat from the identity that lands during the bring-up", async () => {
    await installJoinedPod(commanderView(4, { humanSeats: [1, 2, 3] }));
    capturedDraftGuestListener?.({ type: "commanderLaunch", launch: commanderLaunchFor(2) });
    // Paired, so the assertion below cannot pass on the initial value.
    expect(useMultiplayerDraftStore.getState().commanderSeat).toBeNull();

    await useMultiplayerDraftStore.getState().joinCommanderGame(navigate);

    // 1, not the pod `seatIndex` of 2 and not the seat-0 fallback: the expected
    // value is what makes this assertion name the wire as the seat's source.
    expect(useMultiplayerDraftStore.getState().commanderSeat).toBe(1);
  });

  /**
   * The guest's own listener is the ONLY path by which an engine update reaches
   * the screen: `GameProvider`'s `draft-match` branch adopts the adapter without
   * subscribing, and `installMatchRuntime` commits one snapshot and subscribes
   * to nothing. Drop this arm and the guest's board renders the join-time
   * snapshot and then freezes for the rest of the game — every later spell,
   * priority pass and phase change arrives on the wire and is discarded.
   */
  it("forwards the host's state updates into the engine", async () => {
    await installJoinedPod(commanderView(4, { humanSeats: [1, 2, 3] }));
    capturedDraftGuestListener?.({ type: "commanderLaunch", launch: commanderLaunchFor(2) });
    await useMultiplayerDraftStore.getState().joinCommanderGame(navigate);
    // The bring-up commits its own snapshot through `installMatchRuntime`, which
    // does not go through `processRemoteUpdate`; clear anyway so the assertion
    // below can only be satisfied by the emit that follows it.
    vi.mocked(processRemoteUpdate).mockClear();

    const laterSnapshot = { ...transport.snapshot, seq: 2 };
    transport.guestInstances[0]?.emit({
      type: "stateChanged",
      snapshot: laterSnapshot,
      events: [],
      logEntries: [],
    });

    expect(processRemoteUpdate).toHaveBeenCalledTimes(1);
    expect(processRemoteUpdate).toHaveBeenCalledWith(laterSnapshot, [], []);
  });

  /**
   * A guest that RECONNECTS mid-game takes the `reconnect_ack` path, which
   * emits a second `playerIdentity`. A one-shot listener would keep the first.
   *
   * A single synthetic emit does NOT discriminate that — it is satisfied by the
   * very one-shot this exists to catch — so two are emitted with different ids.
   * Asserted on `commanderSeat`, because a mid-game reconnect re-runs no
   * GameProvider effect. `reconnect_ack` FIDELITY is unobservable here: the
   * module mock removes that path, so the two real emit sites are pinned by
   * code review, not by this case.
   */
  it("keeps the seat from the latest playerIdentity, not the first", async () => {
    await installJoinedPod(commanderView(4, { humanSeats: [1, 2, 3] }));
    capturedDraftGuestListener?.({ type: "commanderLaunch", launch: commanderLaunchFor(2) });
    await useMultiplayerDraftStore.getState().joinCommanderGame(navigate);
    expect(transport.guestInstances).toHaveLength(1);

    transport.guestInstances[0]?.emit({ type: "playerIdentity", playerId: 2 });
    expect(useMultiplayerDraftStore.getState().commanderSeat).toBe(2);

    transport.guestInstances[0]?.emit({ type: "playerIdentity", playerId: 3 });
    expect(useMultiplayerDraftStore.getState().commanderSeat).toBe(3);
  });

  /**
   * At N players a double-press is not merely wasteful: the second `joinRoom`
   * is answered with the NEXT waiting seat, so a later human is kicked "Lobby
   * full" and `roomFull` fires on a ghost seat.
   *
   * The press is made MID-DIAL rather than same-tick, and that is deliberate.
   * MEASURED: a same-tick second press does NOT discriminate the guard. It
   * resumes from a still-cold `import("../network/connection")`, reaches the
   * REAL module and dies inside PeerJS ("does not support WebRTC") whether the
   * guard is present or not — the same race this file's `beforeEach` documents
   * for the launch, and pre-warming does not make it deterministic. So the
   * literal double-click is exercised but not pinned; what this pins is that a
   * press arriving while a join owns the in-flight slot is refused.
   */
  it("ignores a repeat press while a join owns the in-flight slot", async () => {
    await installJoinedPod(commanderView(4, { humanSeats: [1, 2, 3] }));
    capturedDraftGuestListener?.({ type: "commanderLaunch", launch: commanderLaunchFor(2) });
    let openGate!: () => void;
    transport.control.joinRoomGate = new Promise<void>((resolve) => { openGate = resolve; });

    const store = useMultiplayerDraftStore.getState();
    const first = store.joinCommanderGame(navigate);
    await vi.waitFor(() => expect(transport.joinRoomCalls).toHaveLength(1));

    await store.joinCommanderGame(navigate);
    expect(transport.joinRoomCalls).toHaveLength(1);

    openGate();
    await first;
    expect(P2PGuestAdapter).toHaveBeenCalledTimes(1);
  });

  it("cancels a launch nobody joined, telling the guests and clearing the launch state", async () => {
    // Seat 1 is a live human, so the room never fills and the launch stays
    // parked on `roomFull` — the window Cancel exists for.
    await installCompletedPod(commanderView(4, { humanSeats: [1] }));
    void useMultiplayerDraftStore.getState().launchCommanderGame(navigate);
    await vi.waitFor(() => expect(sendCommanderLaunches).toHaveBeenCalledTimes(1));

    // PRECONDITION. `sendCommanderLaunches` is a stub here, so the host's own
    // seat-0 local emit never happens and `commanderLaunch` would never leave
    // its initial `null` — an "assert it is null after the cancel" would then
    // pass against an implementation with no store write at all. Drive the host
    // arm the way the local emit would, and pin that it took.
    capturedHostEventHandler?.({ type: "commanderLaunch", launch: commanderLaunchFor(0) });
    expect(useMultiplayerDraftStore.getState().commanderLaunch).not.toBeNull();

    await useMultiplayerDraftStore.getState().cancelCommanderLaunch();

    // `terminateGame`, never `dispose`: connected guests must be TOLD, or they
    // burn the full reconnect backoff against a Peer that is already gone.
    expect(transport.terminateGame).toHaveBeenCalledTimes(1);
    expect(transport.dispose).not.toHaveBeenCalled();
    expect(useMultiplayerDraftStore.getState().commanderLaunch).toBeNull();
    // DOCUMENTATION, not a discriminator. The reason is NOT that no host case
    // reaches the success path's non-null `set` of a seat — the end-to-end case
    // above asserts directly against that write. It is local, and it is two
    // facts: this launch is parked on `roomFull` (seat 1 is a live human, so the
    // host fake's waiting set never empties) and so never reaches that `set`;
    // and no earlier case can leak a seat in, because `afterEach` calls
    // `reset()`, which spreads an `initialState` whose `commanderSeat` is null.
    // Together those make this line unable to fail in this suite. Kept to state
    // the intended post-state; the clear it describes is deliberately defensive
    // — it drops a seat a PREVIOUS successful launch could have left behind.
    expect(useMultiplayerDraftStore.getState().commanderSeat).toBeNull();
    // A cancel is a user action, not a failure.
    expect(useMultiplayerDraftStore.getState().error).toBeNull();
  });

  it("is a silent no-op when no launch is in flight", async () => {
    await installCompletedPod(commanderView(4));

    await useMultiplayerDraftStore.getState().cancelCommanderLaunch();

    expect(transport.terminateGame).not.toHaveBeenCalled();
    expect(useMultiplayerDraftStore.getState().error).toBeNull();
  });

  /**
   * IDENTITY, not type. `toBeInstanceOf(AbortSignal)` is satisfied by a signal
   * from any controller — including one nothing ever aborts, which would make
   * cancellation inert. Cancelling during the PARKED signalling round-trip is
   * what makes the identity observable.
   */
  it("cancels through the signal the launch handed hostRoom", async () => {
    transport.control.parkHostRoom = true;
    await installCompletedPod(commanderView(4));

    void useMultiplayerDraftStore.getState().launchCommanderGame(navigate);
    await vi.waitFor(() => expect(transport.hostRoomSignals).toHaveLength(1));
    expect((transport.hostRoomSignals[0] as AbortSignal).aborted).toBe(false);

    await useMultiplayerDraftStore.getState().cancelCommanderLaunch();

    expect((transport.hostRoomSignals[0] as AbortSignal).aborted).toBe(true);
    // The launch never got past the signalling round-trip, so no adapter, no
    // navigation, and no banner — a cancel is a user action, not a failure.
    expect(P2PHostAdapter).not.toHaveBeenCalled();
    expect(navigate).not.toHaveBeenCalled();
    expect(useMultiplayerDraftStore.getState().error).toBeNull();

    // And the parked promise really REJECTED rather than being left hanging:
    // only a launch that ran through its own `finally` releases the in-flight
    // slot, so a fresh press reaching `hostRoom` at all is the proof.
    transport.control.parkHostRoom = false;
    await useMultiplayerDraftStore.getState().launchCommanderGame(navigate);
    expect(transport.hostRoomSignals).toHaveLength(2);
  });

  it("does not construct a host adapter when cancellation lands in the deferred booster source accessor", async () => {
    await installCompletedPod(commanderView(4));
    let releasePool!: (pool: string[] | null) => void;
    boosterPackPoolForGame.mockImplementationOnce(
      () => new Promise<string[] | null>((resolve) => { releasePool = resolve; }),
    );

    const launching = useMultiplayerDraftStore.getState().launchCommanderGame(navigate);
    await vi.waitFor(() => expect(boosterPackPoolForGame).toHaveBeenCalledTimes(1));

    // The accessor has already yielded, but no adapter exists yet. A final
    // abort check after its await is the ownership fence that prevents this
    // continuation from creating an unreachable host adapter.
    await useMultiplayerDraftStore.getState().cancelCommanderLaunch();
    releasePool(["Private cube source"]);
    await launching;

    expect(P2PHostAdapter).not.toHaveBeenCalled();
    expect(transport.hostDestroy).toHaveBeenCalledTimes(1);
    expect(navigate).not.toHaveBeenCalled();
    expect(useMultiplayerDraftStore.getState().error).toBeNull();
  });

  /**
   * A failure AFTER the launches are sent leaves the host with an error banner
   * AND — before this — a launch state nothing could clear, because
   * `disposeMatchAdapter`'s clear is fenced on a `matchAdapter` only the
   * success path assigns.
   */
  it("clears the stale launch state when the launch fails, and terminates rather than disposes", async () => {
    const consoleErrorSpy = vi.spyOn(console, "error").mockImplementation(() => {});
    await installCompletedPod(commanderView(4));
    // Same precondition as the cancel case: `sendCommanderLaunches` is a stub,
    // so without this the null assertion below is vacuous.
    capturedHostEventHandler?.({ type: "commanderLaunch", launch: commanderLaunchFor(0) });
    expect(useMultiplayerDraftStore.getState().commanderLaunch).not.toBeNull();
    transport.startPregameGame.mockRejectedValueOnce(new Error("engine refused the pregame"));

    await useMultiplayerDraftStore.getState().launchCommanderGame(navigate);

    expect(useMultiplayerDraftStore.getState().error).toBe("engine refused the pregame");
    expect(useMultiplayerDraftStore.getState().commanderLaunch).toBeNull();
    // Documentation, not a discriminator — same two facts as the cancel case
    // above, with a different bail: `startPregameGame` rejects, so control goes
    // to the catch, short of the success path's `set`.
    expect(useMultiplayerDraftStore.getState().commanderSeat).toBeNull();
    // In PRODUCTION guests are connected by this point, the failure being past
    // `roomFull`; this fixture's pod is all-bot, so `roomFull` came from the last
    // seat mutation and there is no guest session to flush. What the two lines
    // below pin is only that the code chose `terminateGame` over `dispose`.
    expect(transport.terminateGame).toHaveBeenCalledTimes(1);
    expect(transport.dispose).not.toHaveBeenCalled();
    expect(navigate).not.toHaveBeenCalled();
    // `clearAllMocks` does NOT drain a queued once-implementation; restore it
    // here so a bail before `startPregameGame` could never poison the next case.
    transport.startPregameGame.mockReset();
    transport.startPregameGame.mockResolvedValue({ log_entries: [] });
    consoleErrorSpy.mockRestore();
  });

  /**
   * The launch tail — `startPregameGame` and `installMatchRuntime` — is a cancel
   * window like any other, because an abort rejects a PARKED promise and never
   * interrupts an await already in flight.
   */
  it("does not navigate when a cancel lands in the launch tail", async () => {
    await installCompletedPod(commanderView(4));
    let releasePregame!: (result: { log_entries: [] }) => void;
    transport.startPregameGame.mockImplementationOnce(
      () => new Promise((resolve) => { releasePregame = resolve; }),
    );

    const launching = useMultiplayerDraftStore.getState().launchCommanderGame(navigate);
    // Parked at `startPregameGame`, i.e. AFTER `roomFull` already resolved.
    await vi.waitFor(() => expect(transport.startPregameGame).toHaveBeenCalledTimes(1));
    // SAME PRECONDITION as the two cases above, and it was missing here: with
    // `sendCommanderLaunches` stubbed the host's own seat-0 local emit never
    // happens, so `commanderLaunch` would sit at its initial `null` and the
    // "cleared" assertion below would pass against a cancel that writes nothing.
    capturedHostEventHandler?.({ type: "commanderLaunch", launch: commanderLaunchFor(0) });
    expect(useMultiplayerDraftStore.getState().commanderLaunch).not.toBeNull();

    await useMultiplayerDraftStore.getState().cancelCommanderLaunch();
    releasePregame({ log_entries: [] });
    await launching;

    expect(navigate).not.toHaveBeenCalled();
    expect(useMultiplayerDraftStore.getState().matchAdapter).toBeNull();
    expect(useMultiplayerDraftStore.getState().commanderLaunch).toBeNull();
    // A cancel is not a failure, even one that lands this late.
    expect(useMultiplayerDraftStore.getState().error).toBeNull();
    transport.startPregameGame.mockReset();
    transport.startPregameGame.mockResolvedValue({ log_entries: [] });
  });

  // ── Abandoning a bring-up that is still parked ───────────────────────
  //
  // Both handles are module-local and released ONLY by their owner's `finally`,
  // which cannot run while that owner is parked. Before `abandonCommanderBringUp`
  // nothing but `cancelCommanderLaunch` aborted them, so leaving the pod through
  // any other door left the slot claimed for the lifetime of the tab. No case in
  // this suite called `leave(` at all, which is how it shipped.

  it("releases the in-flight launch slot when the pod is left while a launch is parked", async () => {
    await installCompletedPod(commanderView(4, { humanSeats: [1] }));
    const parked = useMultiplayerDraftStore.getState().launchCommanderGame(navigate);
    // Parked on `roomFull`: seat 1 is a live human the host fake never fills.
    await vi.waitFor(() => expect(sendCommanderLaunches).toHaveBeenCalledTimes(1));

    await useMultiplayerDraftStore.getState().leave(false);
    // IMMEDIATE and mechanism-naming, ahead of the await below: leaving must
    // abort the launch's OWN signal — the identity assertion the cancel case
    // makes. Without it the `await` that follows can only ever fail as a
    // timeout, which is a true report of the defect but a mute one.
    expect((transport.hostRoomSignals[0] as AbortSignal).aborted).toBe(true);
    // AWAITED, not drained on a timer: the launch settles only once the abort
    // has unparked `roomFull`, so this both proves the unpark happened and
    // removes every guess about how many turns the continuation needs.
    await parked;

    // Torn down the way a cancel does it — `terminateGame`, never `dispose`, so
    // guests already seated are TOLD rather than left on the reconnect backoff.
    expect(transport.terminateGame).toHaveBeenCalledTimes(1);
    expect(transport.dispose).not.toHaveBeenCalled();

    // THE DEFECT ITSELF. `leave` spreads `initialState` and calls
    // `disposeMatchAdapter`, and neither can reach the module-local handle —
    // `disposeMatchAdapter`'s body is fenced on a `matchAdapter` that does not
    // exist until the launch has already succeeded. Left claimed, the guard in
    // `launchCommanderGame` silently refuses every later launch: a new pod, a
    // pressed button, and nothing happens, with no error to explain it.
    // A fresh press reaching `hostRoom` at all is the proof the slot is free —
    // the same probe the cancel-signal case above uses.
    await installCompletedPod(commanderView(4));
    await useMultiplayerDraftStore.getState().launchCommanderGame(navigate);
    expect(transport.hostRoomSignals).toHaveLength(2);
    // And that second launch is a REAL one: it filled and navigated. The
    // abandoned first launch never did.
    expect(navigate).toHaveBeenCalledTimes(1);
  });

  it("does not tear down a session opened while abandoning a parked launch is waiting to terminate the game", async () => {
    await installCompletedPod(commanderView(4, { humanSeats: [1] }));
    const parked = useMultiplayerDraftStore.getState().launchCommanderGame(navigate);
    // Parked on `roomFull`: seat 1 is a live human the host fake never fills.
    await vi.waitFor(() => expect(sendCommanderLaunches).toHaveBeenCalledTimes(1));

    let releaseTerminate!: () => void;
    transport.terminateGame.mockImplementationOnce(
      () =>
        new Promise<void>((resolve) => {
          releaseTerminate = resolve;
        }),
    );

    const leaving = useMultiplayerDraftStore.getState().leave(false);
    let snapshot: Pick<ReturnType<typeof useMultiplayerDraftStore.getState>, "role" | "phase" | "view">;
    try {
      await vi.waitFor(() => expect(transport.terminateGame).toHaveBeenCalledTimes(1));

      // A different pod, opened while `leave` is still parked on the flush above.
      await installCompletedPod(commanderView(3));
      const before = useMultiplayerDraftStore.getState();
      snapshot = { role: before.role, phase: before.phase, view: before.view };
      expect(snapshot).toMatchObject({ role: "host", phase: "complete" });
      expect(snapshot.view?.seats).toHaveLength(3);
    } finally {
      releaseTerminate();
    }
    await leaving;
    await parked;

    const after = useMultiplayerDraftStore.getState();
    expect({ role: after.role, phase: after.phase, view: after.view }).toEqual(snapshot);
  });

  /**
   * The row above proves the store's visible state survives, but that holds
   * even if `leave` disposes B's own live adapter along the way — a later
   * epoch check stops the `set` regardless. Per-instance adapters make that
   * side effect observable: B's adapter is a distinct spy from the leaving
   * session's own, so this pins that `leave` never touches it, not only that
   * B's screen looks right afterwards.
   */
  it("does not dispose a session opened while abandoning a parked launch is waiting to terminate the game", async () => {
    let releaseTerminate!: () => void;
    transport.terminateGame.mockImplementationOnce(
      () =>
        new Promise<void>((resolve) => {
          releaseTerminate = resolve;
        }),
    );
    const bAdapter = freshHostAdapter();
    mockHostAdapterQueue.push(freshHostAdapter(), bAdapter);

    await installCompletedPod(commanderView(4, { humanSeats: [1] }));
    const parked = useMultiplayerDraftStore.getState().launchCommanderGame(navigate);
    // Parked on `roomFull`: seat 1 is a live human the host fake never fills.
    await vi.waitFor(() => expect(sendCommanderLaunches).toHaveBeenCalledTimes(1));

    const leaving = useMultiplayerDraftStore.getState().leave(false);
    let snapshot: Pick<ReturnType<typeof useMultiplayerDraftStore.getState>, "role" | "phase" | "view">;
    try {
      await vi.waitFor(() => expect(transport.terminateGame).toHaveBeenCalledTimes(1));

      // B, a wholly separate pod, opens and fully settles while the leaving
      // session's own `leave` is still parked flushing the abandoned launch's
      // termination.
      await installCompletedPod(commanderView(3));
      snapshot = {
        role: useMultiplayerDraftStore.getState().role,
        phase: useMultiplayerDraftStore.getState().phase,
        view: useMultiplayerDraftStore.getState().view,
      };
      expect(snapshot).toMatchObject({ role: "host", phase: "complete" });
      expect(snapshot.view?.seats).toHaveLength(3);
    } finally {
      releaseTerminate();
    }
    await leaving;
    await parked;

    expect(bAdapter.dispose).not.toHaveBeenCalled();
    expect({
      role: useMultiplayerDraftStore.getState().role,
      phase: useMultiplayerDraftStore.getState().phase,
      view: useMultiplayerDraftStore.getState().view,
    }).toEqual(snapshot);
  });

  /**
   * The next fence down: no launch is in flight here, so `leave` parks on its
   * own pod adapter's `dispose` instead of on `abandonCommanderBringUp`. The
   * route the leaving pod was opened on is aborted during that wait, which
   * detaches it, so B has no previous adapter to wait on and opens before the
   * gate is released.
   */
  it("does not strand a session opened while the leaving session's own pod adapter is disposing", async () => {
    let releaseXDispose!: () => void;
    const xDisposeGate = new Promise<void>((resolve) => {
      releaseXDispose = resolve;
    });
    const xAdapter = freshHostAdapter({
      dispose: (options) => (options.preserveSession ? Promise.resolve() : xDisposeGate),
    });
    const bAdapter = freshHostAdapter();
    mockHostAdapterQueue.push(xAdapter, bAdapter);
    const route = new AbortController();

    await installCompletedPod(commanderView(4), 0, route.signal);
    const leaving = useMultiplayerDraftStore.getState().leave(false);
    let snapshot: Pick<ReturnType<typeof useMultiplayerDraftStore.getState>, "role" | "phase" | "view">;
    try {
      // `leave(false)` really parked on ITS OWN dispose call, not on the
      // supersede path `hostDraft` runs for B below.
      await vi.waitFor(() => expect(xAdapter.dispose).toHaveBeenCalledWith({ preserveSession: false }));

      // B, a wholly separate pod, opens and fully settles while the leaving
      // session's own `leave` is still parked disposing its own adapter.
      route.abort();
      await installCompletedPod(commanderView(3));
      snapshot = {
        role: useMultiplayerDraftStore.getState().role,
        phase: useMultiplayerDraftStore.getState().phase,
        view: useMultiplayerDraftStore.getState().view,
      };
      expect(snapshot).toMatchObject({ role: "host", phase: "complete" });
      expect(snapshot.view?.seats).toHaveLength(3);
    } finally {
      releaseXDispose();
    }
    await leaving;

    expect(xAdapter.dispose).toHaveBeenCalledOnce();
    expect(bAdapter.dispose).not.toHaveBeenCalled();
    expect({
      role: useMultiplayerDraftStore.getState().role,
      phase: useMultiplayerDraftStore.getState().phase,
      view: useMultiplayerDraftStore.getState().view,
    }).toEqual(snapshot);
  });

  /**
   * The same race as the row above, but the leaving session already has a
   * LIVE MATCH running (`launchCommanderGame` completed): a `matchAdapter`
   * and an `activeMatchController`. On `leave`'s ordinary path
   * `disposeMatchAdapter` releases both, but that function is unreachable
   * past `leave`'s own early return — the route abort's `set(initialState)`
   * clears the store's `matchAdapter` field without ever calling `.dispose()`
   * on the object it held. Both must still
   * be freed by identity, and neither dispose call may reach the newer
   * session's own (separate) pod adapter.
   */
  it("disposes the leaving session's own live match runtime when a newer pod opens while leave is parked on its pod adapter dispose", async () => {
    let releaseXDispose!: () => void;
    const xDisposeGate = new Promise<void>((resolve) => {
      releaseXDispose = resolve;
    });
    const xAdapter = freshHostAdapter({
      dispose: (options) => (options.preserveSession ? Promise.resolve() : xDisposeGate),
    });
    const bAdapter = freshHostAdapter();
    mockHostAdapterQueue.push(xAdapter, bAdapter);
    const route = new AbortController();

    await installCompletedPod(commanderView(4), 0, route.signal);
    await useMultiplayerDraftStore.getState().launchCommanderGame(navigate);
    // Reach-guards: the match runtime this row is about is really live before
    // the race starts.
    expect(useMultiplayerDraftStore.getState().matchAdapter).not.toBeNull();
    expect(transport.dispose).not.toHaveBeenCalled();
    expect(matchLoopMock.controller.dispose).not.toHaveBeenCalled();

    const leaving = useMultiplayerDraftStore.getState().leave(false);
    let snapshot: Pick<ReturnType<typeof useMultiplayerDraftStore.getState>, "role" | "phase" | "view">;
    try {
      // Parked on ITS OWN pod adapter dispose, not on the newer session's open.
      await vi.waitFor(() => expect(xAdapter.dispose).toHaveBeenCalledWith({ preserveSession: false }));

      // A wholly separate pod opens and fully settles while `leave` is still
      // parked disposing its own pod adapter. The route abort nulls the
      // store's `matchAdapter` field here, before `leave` resumes.
      route.abort();
      await installCompletedPod(commanderView(3));
      snapshot = {
        role: useMultiplayerDraftStore.getState().role,
        phase: useMultiplayerDraftStore.getState().phase,
        view: useMultiplayerDraftStore.getState().view,
      };
      expect(snapshot).toMatchObject({ role: "host", phase: "complete" });
      expect(snapshot.view?.seats).toHaveLength(3);
    } finally {
      releaseXDispose();
    }
    await leaving;

    // THE FIX: the leaving session's own match adapter and controller are
    // disposed by the identity `leave` captured before its first await, even
    // though `disposeMatchAdapter` itself is unreachable on this path.
    expect(transport.dispose).toHaveBeenCalledTimes(1);
    expect(matchLoopMock.controller.dispose).toHaveBeenCalledTimes(1);

    // Neither dispose reached the newer session: its own pod adapter is
    // untouched and its visible state survives unchanged.
    expect(xAdapter.dispose).toHaveBeenCalledOnce();
    expect(bAdapter.dispose).not.toHaveBeenCalled();
    expect({
      role: useMultiplayerDraftStore.getState().role,
      phase: useMultiplayerDraftStore.getState().phase,
      view: useMultiplayerDraftStore.getState().view,
    }).toEqual(snapshot);

    // THE NULLING LINE. B never launched its own match, so its own later
    // `reset()` still calls `disposeMatchController()` unconditionally
    // (`disposeMatchAdapter`'s opening line, gated on no `matchAdapter` of its
    // own). Without nulling `activeMatchController` above, that call would
    // still find it pointing at the already-disposed controller and dispose
    // it a second time.
    useMultiplayerDraftStore.getState().reset();
    expect(matchLoopMock.controller.dispose).toHaveBeenCalledTimes(1);
  });

  /**
   * `leave`'s FIRST fence — parked on `abandonCommanderBringUp`'s own
   * terminate flush for a SECOND, still in-flight launch — rather than its
   * second (a pod adapter dispose) or third (a guest leave ack). The live
   * match runtime under test here belongs to the FIRST launch, which already
   * completed before the second launch below is driven through the
   * `launchCommanderGame` store action directly; `leave` must still find and
   * dispose it by identity, though the terminate flush it is parked on
   * belongs to a wholly different (never-installed) adapter.
   */
  it("disposes the leaving session's own live match runtime while abandoning a second in-flight launch", async () => {
    await installCompletedPod(commanderView(4));
    await useMultiplayerDraftStore.getState().launchCommanderGame(navigate);
    expect(useMultiplayerDraftStore.getState().matchAdapter).not.toBeNull();
    expect(transport.dispose).not.toHaveBeenCalled();
    expect(matchLoopMock.controller.dispose).not.toHaveBeenCalled();

    // A second launch from the SAME pod, parked on `roomFull`: seat 1 is a
    // live human the host fake never fills.
    const secondView = commanderView(4, { humanSeats: [1] });
    useMultiplayerDraftStore.setState({ view: secondView });
    commanderSeatDecks.mockResolvedValue(seatDecksFor(secondView, 0));
    const secondLaunch = useMultiplayerDraftStore.getState().launchCommanderGame(navigate);
    await vi.waitFor(() => expect(sendCommanderLaunches).toHaveBeenCalledTimes(2));

    let releaseTerminate!: () => void;
    transport.terminateGame.mockImplementationOnce(
      () =>
        new Promise<void>((resolve) => {
          releaseTerminate = resolve;
        }),
    );

    const leaving = useMultiplayerDraftStore.getState().leave(false);
    try {
      // Parked on the SECOND launch's own terminate flush — reached before
      // `leave` ever gets to its own pod adapter's dispose.
      await vi.waitFor(() => expect(transport.terminateGame).toHaveBeenCalledTimes(1));

      // A wholly separate pod opens and fully settles while `leave` is still
      // parked here.
      await installCompletedPod(commanderView(3));
      expect(useMultiplayerDraftStore.getState()).toMatchObject({ role: "host", phase: "complete" });
      expect(useMultiplayerDraftStore.getState().view?.seats).toHaveLength(3);
    } finally {
      releaseTerminate();
    }
    await leaving;
    await secondLaunch;

    // THE FIX: the FIRST launch's own match adapter and controller — captured
    // by identity before `leave`'s first await, not the second in-flight
    // launch's, which never reached `installMatchRuntime` — are disposed
    // here, though `leave` never reaches its own pod adapter's dispose or the
    // guest-ack fence on this path.
    expect(transport.dispose).toHaveBeenCalledTimes(1);
    expect(matchLoopMock.controller.dispose).toHaveBeenCalledTimes(1);
  });

  it("an aborted launch whose tail resumes after leave's abandon installs no runtime", async () => {
    let releaseStart!: () => void;
    const startGate = new Promise<void>((resolve) => {
      releaseStart = resolve;
    });
    transport.startPregameGame.mockImplementationOnce(async () => {
      await startGate;
      return { log_entries: [] };
    });
    let releaseTerminate: () => void = () => {};
    transport.terminateGame.mockImplementationOnce(
      () =>
        new Promise<void>((resolve) => {
          releaseTerminate = resolve;
        }),
    );

    await installCompletedPod(commanderView(4));
    const parked = useMultiplayerDraftStore.getState().launchCommanderGame(navigate);
    await vi.waitFor(() => expect(transport.startPregameGame).toHaveBeenCalledTimes(1));
    // Antecedent: no match runtime exists yet.
    expect(matchLoopMock.create).not.toHaveBeenCalled();

    const leaving = useMultiplayerDraftStore.getState().leave(false);
    try {
      // `leave`'s own `abandonCommanderBringUp()` aborts this launch's signal
      // and parks on its `terminateGame()` flush.
      await vi.waitFor(() => expect(transport.terminateGame).toHaveBeenCalledTimes(1));

      // Unpark `startPregameGame`: an abort rejects a PARKED promise, it never
      // interrupts an await already in flight, so the launch's tail resumes
      // into an already-aborted signal.
      releaseStart();
      await parked;

      // Reach: the tail got as far as the installer's snapshot fetch.
      const built = vi.mocked(P2PHostAdapter).mock.results[0].value as { getSnapshot: ReturnType<typeof vi.fn> };
      expect(built.getSnapshot).toHaveBeenCalledOnce();
      expect(matchLoopMock.create).not.toHaveBeenCalled();
      expect(useMultiplayerDraftStore.getState().matchAdapter).toBeNull();
      expect(useGameStore.getState().adapter).toBeNull();
      expect(useGameStore.getState().gameId).toBeNull();

      // A wholly separate pod opens while `leave` is still parked on the
      // terminate flush above — the disposal must be scoped to the aborted
      // launch's own controller by identity, not to whatever the module
      // happens to hold when the catch runs.
      await installCompletedPod(commanderView(3));
    } finally {
      releaseStart();
      releaseTerminate();
    }
    await leaving;

    expect(useMultiplayerDraftStore.getState()).toMatchObject({ role: "host", phase: "complete" });
    expect(useMultiplayerDraftStore.getState().view?.seats).toHaveLength(3);
    expect(navigate).not.toHaveBeenCalled();
  });

  it("abandons a parked join when the pod is left, rather than landing in the game later", async () => {
    await installJoinedPod(commanderView(4, { humanSeats: [1, 2, 3] }));
    capturedDraftGuestListener?.({ type: "commanderLaunch", launch: commanderLaunchFor(2) });
    let openGate!: () => void;
    transport.control.joinRoomGate = new Promise<void>((resolve) => {
      openGate = resolve;
    });

    const parked = useMultiplayerDraftStore.getState().joinCommanderGame(navigate);
    await vi.waitFor(() => expect(transport.joinRoomCalls).toHaveLength(1));

    await useMultiplayerDraftStore.getState().leave(false);
    // Released AFTER the leave, so the join resumes into a pod that is gone —
    // which is exactly what a real `joinRoom` round-trip completing late does.
    openGate();
    // Awaited rather than drained on a timer: the join runs several more awaits
    // past the gate, and its `finally` is the last of them.
    await parked;

    // The user left the pod; they must not be dropped into a game a moment
    // later. Without the abort the join runs to completion and navigates.
    expect(navigate).not.toHaveBeenCalled();

    // And the mirror-image slot is free: `commanderJoinInFlight` is released
    // only by `joinCommanderGame`'s own `finally`, which cannot run while the
    // join is parked on the dial.
    await installJoinedPod(commanderView(4, { humanSeats: [1, 2, 3] }));
    capturedDraftGuestListener?.({ type: "commanderLaunch", launch: commanderLaunchFor(2) });
    await useMultiplayerDraftStore.getState().joinCommanderGame(navigate);
    // ONE, not two: `installJoinedPod` zeroes `joinRoomCalls` so that the pod's
    // own dial cannot be mistaken for the launch's. A still-claimed slot would
    // refuse this press at the guard and leave the log empty.
    expect(transport.joinRoomCalls).toHaveLength(1);
  });

  it("an aborted join parked in its snapshot fetch commits no runtime and releases its adapter", async () => {
    await installJoinedPod(commanderView(4, { humanSeats: [1, 2, 3] }));
    capturedDraftGuestListener?.({ type: "commanderLaunch", launch: commanderLaunchFor(2) });

    // Every game id the store passes through, so the assertions below can tell
    // "cleaned up after installing" from "never installed at all".
    const installedGameIds: Array<string | null> = [];
    const unsubscribe = useGameStore.subscribe((state) => installedGameIds.push(state.gameId));

    let openGate!: () => void;
    transport.control.guestSnapshotGate = new Promise<void>((resolve) => {
      openGate = resolve;
    });

    const parked = useMultiplayerDraftStore.getState().joinCommanderGame(navigate);
    // try/finally for the same reason the pending-flag row uses one: the gate is
    // a bare promise the fake awaits without consulting the abort signal, so an
    // assertion throwing in here would strand this join forever, still holding
    // the module-local in-flight slot, and red every later case in the file.
    try {
      await vi.waitFor(() => expect(transport.guestInstances).toHaveLength(1));
      // The pod is left while the snapshot fetch is still in flight.
      await useMultiplayerDraftStore.getState().leave(false);
    } finally {
      openGate();
      await parked;
      unsubscribe();
    }

    // Reach: the join was parked in the installer's snapshot fetch when the pod was left.
    const built = vi.mocked(P2PGuestAdapter).mock.results[0].value as { getSnapshot: ReturnType<typeof vi.fn> };
    expect(built.getSnapshot).toHaveBeenCalledOnce();
    expect(installedGameIds).not.toContain("game-1");
    expect(useGameStore.getState().adapter).toBeNull();
    expect(matchLoopMock.create).not.toHaveBeenCalled();
    expect(transport.guestDispose).toHaveBeenCalledOnce();
    // The user left the pod; they must not be dropped into the game regardless.
    expect(navigate).not.toHaveBeenCalled();
  });

  it("a launch parked in its snapshot fetch leaves a newer pod's launched runtime running when the fetch settles", async () => {
    const controllers = distinctControllers();
    let open!: () => void;
    const gate = new Promise<void>((resolve) => { open = resolve; });
    const make = vi.mocked(P2PHostAdapter).getMockImplementation()!;
    vi.mocked(P2PHostAdapter).mockImplementationOnce(function (this: unknown, ...args: unknown[]) {
      const adapter = (make as (...a: unknown[]) => Record<string, unknown>).apply(this, args);
      adapter.getSnapshot = vi.fn(async () => { await gate; return transport.snapshot; });
      return adapter as never;
    });
    await installCompletedPod(commanderView(2));
    const first = useMultiplayerDraftStore.getState().launchCommanderGame(navigate);
    try {
      await vi.waitFor(() => expect(vi.mocked(P2PHostAdapter).mock.results[0]?.value.getSnapshot).toHaveBeenCalledOnce());
      mockHostAdapterQueue.push(freshHostAdapter());
      await installCompletedPod(commanderView(2));
      await useMultiplayerDraftStore.getState().launchCommanderGame(navigate);
      const newer = vi.mocked(P2PHostAdapter).mock.results[1]?.value;
      expect(newer).toBeDefined();
      const installed = useGameStore.getState().gameId;
      expect(useGameStore.getState().adapter).toBe(newer);
      open();
      await first;
      expect(controllers).toHaveLength(1);
      expect(controllers[0].dispose).not.toHaveBeenCalled();
      expect(useGameStore.getState()).toMatchObject({ adapter: newer, gameId: installed });
      expect(useMultiplayerDraftStore.getState().matchAdapter).toBe(newer);
    } finally {
      open();
      await first;
      controllers.restore();
    }
  });

  it("a join parked in its snapshot fetch leaves a newer pod's joined runtime running when the fetch settles", async () => {
    const controllers = distinctControllers();
    await installJoinedPod(commanderView(4, { humanSeats: [2] }));
    useMultiplayerDraftStore.setState({ commanderLaunch: commanderLaunchFor(2, "game-1") });
    let open!: () => void;
    transport.control.guestSnapshotGate = new Promise<void>((resolve) => { open = resolve; });
    const first = useMultiplayerDraftStore.getState().joinCommanderGame(navigate);
    try {
      await vi.waitFor(() => expect(vi.mocked(P2PGuestAdapter).mock.results[0]?.value.getSnapshot).toHaveBeenCalledOnce());
      transport.control.guestSnapshotGate = null;
      await installJoinedPod(commanderView(4, { humanSeats: [2] }));
      useMultiplayerDraftStore.setState({ commanderLaunch: commanderLaunchFor(2, "game-2") });
      await useMultiplayerDraftStore.getState().joinCommanderGame(navigate);
      expect(navigate).toHaveBeenLastCalledWith("/game/game-2?mode=draft-match");
      const newer = vi.mocked(P2PGuestAdapter).mock.results[1].value;
      open();
      await first;
      expect(controllers).toHaveLength(1);
      expect(controllers[0].dispose).not.toHaveBeenCalled();
      expect(useGameStore.getState()).toMatchObject({ adapter: newer, gameId: "game-2" });
      expect(useMultiplayerDraftStore.getState().matchAdapter).toBe(newer);
    } finally {
      open();
      await first;
      controllers.restore();
    }
  });

  it("a new pod's Launch proceeds while the replaced pod's launch is parked in startPregameGame", async () => {
    await installCompletedPod(commanderView(2));
    let releasePregame!: (result: { log_entries: [] }) => void;
    transport.startPregameGame.mockImplementationOnce(() => new Promise((resolve) => { releasePregame = resolve; }));
    const first = useMultiplayerDraftStore.getState().launchCommanderGame(navigate);
    try {
      await vi.waitFor(() => expect(transport.startPregameGame).toHaveBeenCalledOnce());
      mockHostAdapterQueue.push(freshHostAdapter());
      await installCompletedPod(commanderView(2));
      expect((transport.hostRoomSignals[0] as AbortSignal).aborted).toBe(true);
      await useMultiplayerDraftStore.getState().launchCommanderGame(navigate);
      expect(transport.hostRoomSignals).toHaveLength(2);
      expect(navigate).toHaveBeenCalledOnce();
    } finally {
      releasePregame({ log_entries: [] });
      await first;
    }
  });

  it("a new pod's Join proceeds while the replaced pod's join is parked in initialize", async () => {
    await installJoinedPod(commanderView(4, { humanSeats: [2] }));
    useMultiplayerDraftStore.setState({ commanderLaunch: commanderLaunchFor(2, "game-1") });
    let attach!: () => void;
    const guest = parkNextGuest({ initialize: new Promise<void>((resolve) => { attach = resolve; }), initializeGame: Promise.resolve() });
    const first = useMultiplayerDraftStore.getState().joinCommanderGame(navigate);
    try {
      await vi.waitFor(() => expect(guest.initialize).toHaveBeenCalledOnce());
      const firstSignal = transport.joinRoomCalls[0].signal as AbortSignal;
      await installJoinedPod(commanderView(4, { humanSeats: [2] }));
      useMultiplayerDraftStore.setState({ commanderLaunch: commanderLaunchFor(2, "game-2") });
      expect(firstSignal.aborted).toBe(true);
      await useMultiplayerDraftStore.getState().joinCommanderGame(navigate);
      expect(transport.joinRoomCalls).toHaveLength(1);
      expect(navigate).toHaveBeenLastCalledWith("/game/game-2?mode=draft-match");
    } finally {
      attach();
      await first;
    }
  });

  /**
   * The mirror of the host tests on `leave`'s three fences above, on its
   * THIRD — parked on the pod guest's own leave-ack wait rather than on
   * `abandonCommanderBringUp` (no Commander bring-up is in flight here) or a
   * pod adapter dispose that skips the ack.
   */
  it("disposes the leaving guest's own live match runtime when a newer pod opens while leave is parked on its pod adapter's leave ack", async () => {
    await installJoinedPod(commanderView(4, { humanSeats: [1, 2, 3] }));
    capturedDraftGuestListener?.({ type: "commanderLaunch", launch: commanderLaunchFor(2) });
    await useMultiplayerDraftStore.getState().joinCommanderGame(navigate);
    // Reach-guards: the match runtime this row is about is really live before
    // the race starts.
    expect(useMultiplayerDraftStore.getState().matchAdapter).not.toBeNull();
    expect(transport.guestDispose).not.toHaveBeenCalled();
    expect(matchLoopMock.controller.dispose).not.toHaveBeenCalled();

    let releaseGuestLeave!: () => void;
    transport.control.guestLeaveGate = new Promise<void>((resolve) => {
      releaseGuestLeave = resolve;
    });

    const leaving = useMultiplayerDraftStore.getState().leave(false);
    try {
      // Parked on the pod guest's own leave-ack wait, not skipping it: this
      // call does not take the `preserveRecovery: true` branch.
      await vi.waitFor(() => expect(capturedDraftGuestLeave).toHaveBeenCalledTimes(1));

      // A wholly separate pod opens and fully settles while `leave` is still
      // parked on the ack above.
      await installCompletedPod(commanderView(3));
      expect(useMultiplayerDraftStore.getState()).toMatchObject({ role: "host", phase: "complete" });
      expect(useMultiplayerDraftStore.getState().view?.seats).toHaveLength(3);
    } finally {
      releaseGuestLeave();
    }
    await leaving;

    // THE FIX: the leaving guest's own match adapter and controller are
    // disposed by the identity `leave` captured before its first await.
    expect(transport.guestDispose).toHaveBeenCalledTimes(1);
    expect(matchLoopMock.controller.dispose).toHaveBeenCalledTimes(1);

    // THE NULLING LINE. The newer (host) session never launched its own
    // match, so its own later `reset()` still calls `disposeMatchController()`
    // unconditionally. Without nulling `activeMatchController` above, that
    // call would still find it pointing at the already-disposed controller
    // and dispose it a second time.
    useMultiplayerDraftStore.getState().reset();
    expect(matchLoopMock.controller.dispose).toHaveBeenCalledTimes(1);
  });

  it("a Commander join the host refuses in initializeGame reports the refusal and releases its slot", async () => {
    await installJoinedPod(commanderView(4, { humanSeats: [2] }));
    useMultiplayerDraftStore.setState({ commanderLaunch: commanderLaunchFor(2, "game-1") });
    let refuse!: (reason: Error) => void;
    const guest = parkNextGuest({ initializeGame: new Promise<void>((_resolve, reject) => { refuse = reject; }) });
    const joined = useMultiplayerDraftStore.getState().joinCommanderGame(navigate);
    await vi.waitFor(() => expect(guest.initializeGame).toHaveBeenCalledOnce());
    refuse(new Error("Host left the game"));
    const settled = await Promise.race([
      joined.then(() => "settled"),
      new Promise((resolve) => setTimeout(() => resolve("pending"), 50)),
    ]);
    expect(settled).toBe("settled");
    expect(useMultiplayerDraftStore.getState()).toMatchObject({ error: "Host left the game", commanderJoinPending: false });
    expect(transport.guestDispose).toHaveBeenCalledOnce();
  });

  it("a new launch abandons a join still dialing the launch it replaced", async () => {
    await installJoinedPod(commanderView(4, { humanSeats: [2] }));
    capturedDraftGuestListener!({ type: "commanderLaunch", launch: commanderLaunchFor(2, "game-1") });
    let rejectPark!: (reason: unknown) => void;
    transport.control.joinRoomGate = new Promise<void>((_resolve, reject) => { rejectPark = reject; });
    const first = useMultiplayerDraftStore.getState().joinCommanderGame(navigate);
    try {
      await vi.waitFor(() => expect(transport.joinRoomCalls).toHaveLength(1));
      const firstSignal = transport.joinRoomCalls[0].signal as AbortSignal;
      // The harness gate ignores the signal; production's `joinRoom` rejects on it.
      firstSignal.addEventListener("abort", () => rejectPark(new DOMException("Aborted", "AbortError")), { once: true });
      transport.control.joinRoomGate = null;
      capturedDraftGuestListener!({ type: "commanderLaunch", launch: commanderLaunchFor(2, "game-2") });
      expect(firstSignal.aborted).toBe(true);
      const settled = await Promise.race([
        first.then(() => "settled"),
        new Promise((resolve) => setTimeout(() => resolve("pending"), 50)),
      ]);
      expect(settled).toBe("settled");
      await useMultiplayerDraftStore.getState().joinCommanderGame(navigate);
      expect(transport.joinRoomCalls).toHaveLength(2);
      expect(navigate).toHaveBeenCalledOnce();
      expect(navigate).toHaveBeenLastCalledWith("/game/game-2?mode=draft-match");
      expect(useMultiplayerDraftStore.getState().error).toBeNull();
    } finally {
      rejectPark(new Error("unpark"));
      await first;
    }
  });

  it("a launch delivered again leaves the join bound to it running", async () => {
    await installJoinedPod(commanderView(4, { humanSeats: [2] }));
    capturedDraftGuestListener!({ type: "commanderLaunch", launch: commanderLaunchFor(2, "game-1") });
    let open!: () => void;
    transport.control.joinRoomGate = new Promise<void>((resolve) => { open = resolve; });
    const first = useMultiplayerDraftStore.getState().joinCommanderGame(navigate);
    try {
      await vi.waitFor(() => expect(transport.joinRoomCalls).toHaveLength(1));
      const again = commanderLaunchFor(2, "game-1");
      capturedDraftGuestListener!({ type: "commanderLaunch", launch: again });
      expect(useMultiplayerDraftStore.getState().commanderLaunch).toBe(again);
      expect((transport.joinRoomCalls[0].signal as AbortSignal).aborted).toBe(false);
    } finally {
      open();
      await first;
    }
    expect(navigate).toHaveBeenLastCalledWith("/game/game-1?mode=draft-match");
  });

  describe("a pod session's end supersedes its Commander bring-ups", () => {
    it("a new pod's Launch supersedes the replaced pod's launch parked on roomFull", async () => {
      await installCompletedPod(commanderView(2, { humanSeats: [1] }));
      const first = useMultiplayerDraftStore.getState().launchCommanderGame(navigate);
      await vi.waitFor(() => expect(transport.instances).toHaveLength(1));
      mockHostAdapterQueue.push(freshHostAdapter());
      await installCompletedPod(commanderView(2, { humanSeats: [1] }));
      expect(useMultiplayerDraftStore.getState()).toMatchObject({ role: "host", phase: "complete" });
      void useMultiplayerDraftStore.getState().launchCommanderGame(navigate);
      await vi.waitFor(() => expect(transport.instances).toHaveLength(2));
      expect((transport.hostRoomSignals[0] as AbortSignal).aborted).toBe(true);
      expect(transport.terminateGame).toHaveBeenCalledOnce();
      expect(transport.terminateGame.mock.contexts[0]).toBe(vi.mocked(P2PHostAdapter).mock.results[0].value);
      expect((transport.hostRoomSignals[1] as AbortSignal).aborted).toBe(false);
      await first;
    });

    it("a new pod's Join supersedes the replaced pod's join parked in joinRoom", async () => {
      await installJoinedPod(commanderView(4, { humanSeats: [2] }));
      useMultiplayerDraftStore.setState({ commanderLaunch: commanderLaunchFor(2, "game-1") });
      let rejectPark!: (reason: unknown) => void;
      transport.control.joinRoomGate = new Promise<void>((_resolve, reject) => { rejectPark = reject; });
      const first = useMultiplayerDraftStore.getState().joinCommanderGame(navigate);
      await vi.waitFor(() => expect(transport.joinRoomCalls).toHaveLength(1));
      const firstSignal = transport.joinRoomCalls[0].signal as AbortSignal;
      // The harness gate ignores the signal; production's `joinRoom` rejects on it.
      firstSignal.addEventListener("abort", () => rejectPark(new DOMException("Aborted", "AbortError")), { once: true });
      transport.control.joinRoomGate = null;
      try {
        await installJoinedPod(commanderView(4, { humanSeats: [2] }));
        useMultiplayerDraftStore.setState({ commanderLaunch: commanderLaunchFor(2, "game-2") });
        const second = useMultiplayerDraftStore.getState().joinCommanderGame(navigate);
        expect(firstSignal.aborted).toBe(true);
        await second;
        expect(transport.joinRoomCalls).toHaveLength(1);
        expect(navigate).toHaveBeenLastCalledWith("/game/game-2?mode=draft-match");
      } finally {
        rejectPark(new Error("unpark"));
        await first;
      }
    });

    it("the route owner's abort supersedes its pod's launch parked on roomFull", async () => {
      const route = new AbortController();
      await installCompletedPod(commanderView(2, { humanSeats: [1] }), 0, route.signal);
      const first = useMultiplayerDraftStore.getState().launchCommanderGame(navigate);
      await vi.waitFor(() => expect(transport.instances).toHaveLength(1));
      route.abort();
      expect(useMultiplayerDraftStore.getState().role).toBeNull();
      expect((transport.hostRoomSignals[0] as AbortSignal).aborted).toBe(true);
      expect(transport.terminateGame).toHaveBeenCalledOnce();
      transport.instances[0].finish();
      await first;
      expect(navigate).not.toHaveBeenCalled();
      expect(useMultiplayerDraftStore.getState().matchAdapter).toBeNull();
    });

    it("the route owner's abort after a launch navigates leaves the launched game running", async () => {
      const route = new AbortController();
      await installCompletedPod(commanderView(2, { humanSeats: [1] }), 0, route.signal);
      // The page's unmount, which aborts its route owner, runs after `navigate` returns at the earliest.
      navigate.mockImplementationOnce(() => queueMicrotask(() => route.abort()));
      const launched = useMultiplayerDraftStore.getState().launchCommanderGame(navigate);
      await vi.waitFor(() => expect(transport.instances).toHaveLength(1));
      transport.instances[0].finish();
      await launched;
      await new Promise((resolve) => setTimeout(resolve, 0));
      expect(navigate).toHaveBeenCalledOnce();
      expect(useMultiplayerDraftStore.getState().role).toBeNull();
      expect((transport.hostRoomSignals[0] as AbortSignal).aborted).toBe(false);
      expect(transport.terminateGame).not.toHaveBeenCalled();
    });

    it.each(["replaced", "left"] as const)(
      "a Commander join parked in initializeGame is released when its pod is %s",
      async (end) => {
        await installJoinedPod(commanderView(4, { humanSeats: [2] }));
        useMultiplayerDraftStore.setState({ commanderLaunch: commanderLaunchFor(2, "game-1") });
        let reply!: () => void;
        const guest = parkNextGuest({ initializeGame: new Promise<void>((resolve) => { reply = resolve; }) });
        const first = useMultiplayerDraftStore.getState().joinCommanderGame(navigate);
        try {
          await vi.waitFor(() => expect(guest.initializeGame).toHaveBeenCalledOnce());
          const firstSignal = transport.joinRoomCalls[0].signal as AbortSignal;
          if (end === "left") await useMultiplayerDraftStore.getState().leave();
          await installJoinedPod(commanderView(4, { humanSeats: [2] }));
          useMultiplayerDraftStore.setState({ commanderLaunch: commanderLaunchFor(2, "game-2") });
          await useMultiplayerDraftStore.getState().joinCommanderGame(navigate);
          expect(firstSignal.aborted).toBe(true);
          expect(transport.joinRoomCalls).toHaveLength(1);
          expect(navigate).toHaveBeenLastCalledWith("/game/game-2?mode=draft-match");
          expect(transport.guestDispose).toHaveBeenCalledOnce();
          expect(transport.guestDispose.mock.contexts[0]).toBe(vi.mocked(P2PGuestAdapter).mock.results[0].value);
        } finally {
          reply();
          await first;
        }
      },
    );

    it("a Commander join aborted while its adapter initializes is released though the host never replies", async () => {
      await installJoinedPod(commanderView(4, { humanSeats: [2] }));
      useMultiplayerDraftStore.setState({ commanderLaunch: commanderLaunchFor(2, "game-1") });
      let attach!: () => void;
      let reply!: () => void;
      const guest = parkNextGuest({
        initialize: new Promise<void>((resolve) => { attach = resolve; }),
        initializeGame: new Promise<void>((resolve) => { reply = resolve; }),
      });
      const first = useMultiplayerDraftStore.getState().joinCommanderGame(navigate);
      try {
        await vi.waitFor(() => expect(guest.initialize).toHaveBeenCalledOnce());
        await useMultiplayerDraftStore.getState().leave();
        attach();
        await vi.waitFor(() => expect(guest.initializeGame).toHaveBeenCalledOnce());
        const settled = await Promise.race([
          first.then(() => "settled"),
          new Promise((resolve) => setTimeout(() => resolve("pending"), 50)),
        ]);
        expect(settled).toBe("settled");
        expect(transport.guestDispose).toHaveBeenCalledOnce();
        expect(useMultiplayerDraftStore.getState().commanderJoinPending).toBe(false);
      } finally {
        reply();
        await first;
      }
    });

    it.each(["hostDraft", "joinDraft"] as const)("a Join pressed while %s waits on the replaced pod's teardown dials nothing when the call is route-aborted", async (opener) => {
      await installJoinedPod(commanderView(4, { humanSeats: [2] }));
      useMultiplayerDraftStore.setState({ commanderLaunch: commanderLaunchFor(2, "game-1") });
      useGameStore.setState({ gameId: null });
      let openTeardown!: () => void;
      const teardown = new Promise<void>((resolve) => { openTeardown = resolve; });
      const podTeardown = vi.spyOn(DraftPodGuestAdapter.prototype, "dispose").mockImplementationOnce(() => teardown);
      const route = new AbortController();
      mockHostAdapterQueue.push(freshHostAdapter());
      const replacing = opener === "hostDraft"
        ? useMultiplayerDraftStore.getState().hostDraft({
          poolInput: { type: "Set", data: { pools: [{ code: "TST" }], sequence: ["TST"] } },
          kind: "CommanderDraft", podSize: 4, hostDisplayName: "Host", tournamentFormat: "Swiss", podPolicy: "Competitive", signal: route.signal,
        })
        : useMultiplayerDraftStore.getState().joinDraft({ kind: "new", roomCode: "ABCDE", displayName: "P", signal: route.signal });
      try {
        await vi.waitFor(() => expect(podTeardown).toHaveBeenCalled());
        // Reach: the invitation is still on the store, so only the pod guard can refuse the press.
        expect(useMultiplayerDraftStore.getState().commanderLaunch?.gameId).toBe("game-1");
        const joining = useMultiplayerDraftStore.getState().joinCommanderGame(navigate);
        route.abort();
        openTeardown();
        await replacing;
        await joining;
      } finally {
        openTeardown();
        podTeardown.mockRestore();
      }
      expect(transport.joinRoomCalls).toHaveLength(0);
      expect(navigate).not.toHaveBeenCalled();
      expect(useMultiplayerDraftStore.getState().matchAdapter).toBeNull();
      expect(useGameStore.getState().gameId).not.toBe("game-1");
    });

    it("a Join pressed while a joined pod's open fails online ends with it", async () => {
      let fail!: () => void;
      const make = vi.mocked(P2PDraftGuest).getMockImplementation()!;
      vi.mocked(P2PDraftGuest).mockImplementationOnce(function (this: unknown, ...args: unknown[]) {
        return { ...(make as (...a: unknown[]) => object).apply(this, args), initialize: async () => {
          capturedDraftGuestListener!({ type: "commanderLaunch", launch: commanderLaunchFor(2, "game-1") });
          await new Promise<void>((_resolve, reject) => { fail = () => reject(new Error("host gone")); });
        } } as never;
      });
      useGameStore.setState({ gameId: null });
      const opening = useMultiplayerDraftStore.getState()
        .joinDraft({ kind: "reconnect", roomCode: "ABCDE", displayName: "Guest", hostPeerId: "host-peer-id", draftToken: "token" });
      await vi.waitFor(() => expect(useMultiplayerDraftStore.getState().commanderLaunch).not.toBeNull());
      transport.joinRoomCalls.length = 0;
      let openPark!: () => void;
      let rejectPark!: (reason: unknown) => void;
      transport.control.joinRoomGate = new Promise<void>((resolve, reject) => { openPark = resolve; rejectPark = reject; });
      const joining = useMultiplayerDraftStore.getState().joinCommanderGame(navigate);
      let signal: AbortSignal | undefined;
      try {
        await vi.waitFor(() => expect(transport.joinRoomCalls).toHaveLength(1));
        signal = transport.joinRoomCalls[0].signal as AbortSignal;
        // The harness gate ignores the signal; production's `joinRoom` rejects on it.
        signal.addEventListener("abort", () => rejectPark(new DOMException("Aborted", "AbortError")), { once: true });
        fail();
        await opening;
      } finally {
        transport.control.joinRoomGate = null;
        openPark();
        await joining;
      }
      expect(useGameStore.getState().gameId).not.toBe("game-1");
      expect(useMultiplayerDraftStore.getState().matchAdapter).toBeNull();
      expect(navigate).not.toHaveBeenCalled();
      expect(signal?.aborted).toBe(true);
    });

    it("a Launch pressed while a resumed pod's open fails online ends with it", async () => {
      let fail!: () => void;
      const pod = freshHostAdapter();
      const view = commanderView(2);
      pod.initialize.mockImplementationOnce(async () => {
        useMultiplayerDraftStore.setState({ phase: "complete", role: "host", seatIndex: 0, roomCode: "ABCDE", view, error: null });
        await new Promise<void>((_resolve, reject) => { fail = () => reject(new Error("persist failed")); });
      });
      mockHostAdapterQueue.push(pod);
      commanderSeatDecks.mockResolvedValue(seatDecksFor(view, 0));
      transport.control.parkHostRoom = true;
      const opening = useMultiplayerDraftStore.getState().hostDraft({
        poolInput: { type: "Set", data: { pools: [{ code: "TST" }], sequence: ["TST"] } },
        kind: "CommanderDraft", podSize: 2, hostDisplayName: "Host", tournamentFormat: "Swiss", podPolicy: "Competitive",
      });
      await vi.waitFor(() => expect(useMultiplayerDraftStore.getState().view).not.toBeNull());
      const launching = useMultiplayerDraftStore.getState().launchCommanderGame(navigate);
      try {
        await vi.waitFor(() => expect(transport.hostRoomSignals).toHaveLength(1));
        fail();
        await opening;
        expect((transport.hostRoomSignals[0] as AbortSignal).aborted).toBe(true);
      } finally {
        useMultiplayerDraftStore.getState().reset();
        await launching;
      }
    });

    it("a Join pressed after a joined pod's open fails online dials nothing", async () => {
      let fail!: () => void;
      const make = vi.mocked(P2PDraftGuest).getMockImplementation()!;
      vi.mocked(P2PDraftGuest).mockImplementationOnce(function (this: unknown, ...args: unknown[]) {
        return { ...(make as (...a: unknown[]) => object).apply(this, args), initialize: async () => {
          capturedDraftGuestListener!({ type: "commanderLaunch", launch: commanderLaunchFor(2, "game-1") });
          await new Promise<void>((_resolve, reject) => { fail = () => reject(new Error("host gone")); });
        } } as never;
      });
      useGameStore.setState({ gameId: null });
      const opening = useMultiplayerDraftStore.getState()
        .joinDraft({ kind: "reconnect", roomCode: "ABCDE", displayName: "Guest", hostPeerId: "host-peer-id", draftToken: "token" });
      await vi.waitFor(() => expect(useMultiplayerDraftStore.getState().commanderLaunch).not.toBeNull());
      fail();
      await opening;
      // Reach: the invitation is still on the store and nothing is in flight, so only the pod guard can refuse the press.
      expect(useMultiplayerDraftStore.getState().commanderLaunch?.gameId).toBe("game-1");
      const dialed = transport.joinRoomCalls.length;
      await Promise.race([useMultiplayerDraftStore.getState().joinCommanderGame(navigate), new Promise((resolve) => setTimeout(resolve, 50))]);
      expect(useGameStore.getState().gameId).not.toBe("game-1");
      expect(useMultiplayerDraftStore.getState().matchAdapter).toBeNull();
      expect(navigate).not.toHaveBeenCalled();
      expect(transport.joinRoomCalls).toHaveLength(dialed);
    });

    it("a Join pressed while leave waits on the guest leave ack ends with the pod", async () => {
      await installJoinedPod(commanderView(4, { humanSeats: [2] }));
      useMultiplayerDraftStore.setState({ commanderLaunch: commanderLaunchFor(2, "game-1") });
      useGameStore.setState({ gameId: null });
      let reply!: () => void;
      const guest = parkNextGuest({ initializeGame: new Promise<void>((resolve) => { reply = resolve; }) });
      let ack!: () => void;
      transport.control.guestLeaveGate = new Promise<void>((resolve) => { ack = resolve; });
      const leaving = useMultiplayerDraftStore.getState().leave(false);
      await vi.waitFor(() => expect(capturedDraftGuestLeave).toHaveBeenCalledOnce());
      const joining = useMultiplayerDraftStore.getState().joinCommanderGame(navigate);
      await vi.waitFor(() => expect(guest.initializeGame).toHaveBeenCalledOnce());
      ack();
      await leaving;
      reply();
      await joining;
      expect((transport.joinRoomCalls[0].signal as AbortSignal).aborted).toBe(true);
      expect(navigate).not.toHaveBeenCalled();
      expect(useMultiplayerDraftStore.getState().matchAdapter).toBeNull();
      expect(useGameStore.getState().gameId).not.toBe("game-1");
    });

    it("a Launch pressed while leave waits on the pod teardown ends with the pod", async () => {
      let openTeardown!: () => void;
      const teardown = new Promise<void>((resolve) => { openTeardown = resolve; });
      const pod = freshHostAdapter({ dispose: () => teardown });
      mockHostAdapterQueue.push(pod);
      await installCompletedPod(commanderView(2, { humanSeats: [1] }));
      const leaving = useMultiplayerDraftStore.getState().leave(false);
      await vi.waitFor(() => expect(pod.dispose).toHaveBeenCalled());
      transport.control.parkHostRoom = true;
      const launching = useMultiplayerDraftStore.getState().launchCommanderGame(navigate);
      await vi.waitFor(() => expect(transport.hostRoomSignals).toHaveLength(1));
      openTeardown();
      await leaving;
      expect((transport.hostRoomSignals[0] as AbortSignal).aborted).toBe(true);
      await launching;
    });

    it.each(["reset", "leave", "its failure then leave"] as const)("a launch parked in startPregameGame terminates its game once when %s ends the pod", async (end) => {
      await installCompletedPod(commanderView(4));
      let releasePregame!: (result: { log_entries: [] }) => void;
      let failPregame!: (err: Error) => void;
      transport.startPregameGame.mockImplementationOnce(() => new Promise((resolve, reject) => { releasePregame = resolve; failPregame = reject; }));
      const launching = useMultiplayerDraftStore.getState().launchCommanderGame(navigate);
      await vi.waitFor(() => expect(transport.startPregameGame).toHaveBeenCalledOnce());
      try {
        if (end === "reset") useMultiplayerDraftStore.getState().reset();
        else if (end === "leave") await useMultiplayerDraftStore.getState().leave(false);
        else {
          let finishTerminate!: () => void;
          transport.terminateGame.mockImplementationOnce(() => new Promise<void>((resolve) => { finishTerminate = resolve; }));
          failPregame(new Error("boom"));
          await vi.waitFor(() => expect(transport.terminateGame).toHaveBeenCalledOnce());
          const leaving = useMultiplayerDraftStore.getState().leave(false);
          finishTerminate();
          await leaving;
        }
        expect(transport.terminateGame).toHaveBeenCalledOnce();
      } finally {
        releasePregame({ log_entries: [] });
        await launching;
      }
    });
  });

  describe("startMatch bring-up", () => {
    const launch = {
      type: "HumanHost" as const,
      matchId: "m1",
      matchRoomCode: "MATCH-m1",
      round: 1,
      localSeat: 0,
      opponentSeat: 1,
      opponentName: "Guest",
      matchHostPeerId: "peer-0",
      deckPayload: { player: deckFor(0), opponent: deckFor(1), ai_decks: [] },
      matchConfig: { match_type: "Bo1" as const },
      binding: {
        podId: "pod-1", matchId: "m1", round: 1, sessionKey: "s", lease: "l", nonce: "n",
        revision: 1, matchAuthoritySeat: 0,
      },
    };
    const m2 = {
      ...launch, matchId: "m2", matchRoomCode: "MATCH-m2", round: 2,
      binding: { ...launch.binding, matchId: "m2", round: 2 },
    };
    const guestLaunch = (matchId: string) => ({
      type: "HumanGuest" as const, matchId, matchRoomCode: `MATCH-${matchId}`, round: 1, localSeat: 1,
      opponentSeat: 0, opponentName: "Host", matchHostPeerId: "peer-0", localDeck: deckFor(1),
      matchConfig: { match_type: "Bo1" as const }, binding: { ...launch.binding, matchId },
    });
    const settle = (p: Promise<string | null>) =>
      Promise.race([p.then((v) => `resolved:${v}`), new Promise((r) => setTimeout(() => r("pending"), 50))]);

    const podConfig = {
      poolInput: { type: "Set" as const, data: { pools: [{ code: "TST" }], sequence: ["TST"] } },
      kind: "CommanderDraft" as const, podSize: 2, hostDisplayName: "Host", tournamentFormat: "Swiss" as const, podPolicy: "Competitive" as const,
    };

    async function podInMatch() {
      await installCompletedPod(commanderView(2));
      useMultiplayerDraftStore.setState({ matchPairing: launch, phase: "matchInProgress" });
    }

    /** Parks the next host start on `park`, the awaits an abort does not interrupt. */
    function parkHostOn(park: "initialize" | "startPregameGame" | "getSnapshot", gate: Promise<void>) {
      if (park === "startPregameGame") {
        transport.startPregameGame.mockImplementationOnce(async () => { await gate; return { log_entries: [] }; });
        return;
      }
      const make = vi.mocked(P2PHostAdapter).getMockImplementation()!;
      vi.mocked(P2PHostAdapter).mockImplementationOnce(function (this: unknown, ...args: unknown[]) {
        const adapter = (make as (...a: unknown[]) => Record<string, unknown>).apply(this, args);
        if (park === "initialize") adapter.initialize = vi.fn(() => gate);
        else adapter.getSnapshot = vi.fn(async () => { await gate; return transport.snapshot; });
        return adapter as never;
      });
    }

    async function reachPark(park: "initialize" | "startPregameGame" | "getSnapshot") {
      await vi.waitFor(() => expect(transport.instances).toHaveLength(1));
      if (park === "initialize") return;
      transport.instances[0].finish();
      if (park === "startPregameGame") await vi.waitFor(() => expect(transport.startPregameGame).toHaveBeenCalled());
      else {
        const built = vi.mocked(P2PHostAdapter).mock.results[0].value as { getSnapshot: ReturnType<typeof vi.fn> };
        await vi.waitFor(() => expect(built.getSnapshot).toHaveBeenCalled());
      }
    }

    async function guestPodInMatch() {
      await installJoinedPod(commanderView(2), 1);
      useMultiplayerDraftStore.setState({ matchPairing: guestLaunch("m1"), phase: "matchInProgress" });
    }

    it("leave() while hostRoom is parked aborts it and publishes nothing", async () => {
      await podInMatch();
      let open!: () => void;
      transport.control.hostRoomGate = new Promise<void>((r) => { open = r; });
      const started = useMultiplayerDraftStore.getState().startMatch();
      await vi.waitFor(() => expect(transport.hostRoomSignals).toHaveLength(1));
      await useMultiplayerDraftStore.getState().leave();
      open();
      const settled = await settle(started);
      const s = useMultiplayerDraftStore.getState();
      expect((transport.hostRoomSignals[0] as AbortSignal | undefined)?.aborted).toBe(true);
      expect(settled).toBe("resolved:null");
      expect(transport.hostDestroy).toHaveBeenCalledOnce();
      expect(s.matchAdapter).toBeNull();
      expect(s.phase).toBe("idle");
      expect(s.error).toBeNull();
    });

    it("leave() while parked on roomFull terminates the adapter's game", async () => {
      await podInMatch();
      const started = useMultiplayerDraftStore.getState().startMatch();
      await vi.waitFor(() => expect(transport.instances).toHaveLength(1));
      await useMultiplayerDraftStore.getState().leave();
      const settled = await settle(started);
      const terminatedBeforeGuest = transport.terminateGame.mock.calls.length;
      transport.instances[0].finish();
      await new Promise((r) => setTimeout(r, 0));
      const s = useMultiplayerDraftStore.getState();
      expect(settled).toBe("resolved:null");
      expect(terminatedBeforeGuest).toBe(1);
      expect(transport.hostDestroy).not.toHaveBeenCalled();
      expect(s.matchAdapter).toBeNull();
      expect(s.phase).toBe("idle");
    });

    it("leave() while the host adapter initializes terminates its game with no unhandled rejection", async () => {
      await podInMatch();
      const unhandled: unknown[] = [];
      const prior = process.listeners("unhandledRejection");
      process.removeAllListeners("unhandledRejection");
      const onUnhandled = (reason: unknown) => unhandled.push(reason);
      process.on("unhandledRejection", onUnhandled);
      let open!: () => void;
      const gate = new Promise<void>((resolve) => { open = resolve; });
      const make = vi.mocked(P2PHostAdapter).getMockImplementation()!;
      vi.mocked(P2PHostAdapter).mockImplementationOnce(function (this: unknown, ...args: unknown[]) {
        const adapter = (make as (...a: unknown[]) => { initialize: () => Promise<void> }).apply(this, args);
        adapter.initialize = vi.fn(() => gate);
        return adapter as never;
      });
      try {
        const started = useMultiplayerDraftStore.getState().startMatch();
        await vi.waitFor(() => expect(transport.instances).toHaveLength(1));
        await useMultiplayerDraftStore.getState().leave();
        await new Promise((resolve) => setTimeout(resolve, 0));
        open();
        expect(await settle(started)).toBe("resolved:null");
        expect(transport.terminateGame).toHaveBeenCalledOnce();
        expect(unhandled).toEqual([]);
      } finally {
        process.off("unhandledRejection", onUnhandled);
        for (const listener of prior) process.on("unhandledRejection", listener as never);
      }
    });

    it("guest arm: leave() while joinRoom is parked destroys the peer", async () => {
      await podInMatch();
      useMultiplayerDraftStore.setState({
        matchPairing: {
          type: "HumanGuest", matchId: "m1", matchRoomCode: "MATCH-m1", round: 1, localSeat: 1,
          opponentSeat: 0, opponentName: "Host", matchHostPeerId: "peer-0", localDeck: deckFor(1),
          matchConfig: { match_type: "Bo1" }, binding: launch.binding,
        },
      });
      let open!: () => void;
      transport.control.joinRoomGate = new Promise<void>((r) => { open = r; });
      const started = useMultiplayerDraftStore.getState().startMatch();
      await vi.waitFor(() => expect(transport.joinRoomCalls).toHaveLength(1));
      await useMultiplayerDraftStore.getState().leave();
      open();
      const settled = await settle(started);
      expect((transport.joinRoomCalls[0].signal as AbortSignal | undefined)?.aborted).toBe(true);
      expect(settled).toBe("resolved:null");
      expect(transport.joinDestroyPeer).toHaveBeenCalledOnce();
      expect(transport.guestInstances).toHaveLength(0);
    });

    it("a failed bring-up releases what it built and keeps its error", async () => {
      await podInMatch();
      transport.startPregameGame.mockRejectedValueOnce(new Error("pregame refused"));
      const started = useMultiplayerDraftStore.getState().startMatch();
      await vi.waitFor(() => expect(transport.instances).toHaveLength(1));
      transport.instances[0].finish();
      expect(await started).toBeNull();
      expect(transport.terminateGame).toHaveBeenCalledOnce();
      expect(transport.dispose).not.toHaveBeenCalled();
      expect(useMultiplayerDraftStore.getState().error).toBe("pregame refused");
    });

    it("a second press while a start is in flight opens no second room", async () => {
      await podInMatch();
      const first = useMultiplayerDraftStore.getState().startMatch();
      await vi.waitFor(() => expect(transport.instances).toHaveLength(1));
      expect(await settle(useMultiplayerDraftStore.getState().startMatch())).toBe("resolved:null");
      expect(transport.hostRoomSignals).toHaveLength(1);
      transport.instances[0].finish();
      expect(await first).toBe("draft-match-m1");
    });

    it("a start for the next pairing supersedes one still parked on roomFull", async () => {
      await podInMatch();
      const first = useMultiplayerDraftStore.getState().startMatch();
      await vi.waitFor(() => expect(transport.instances).toHaveLength(1));
      capturedHostEventHandler!({ type: "matchStart", launch: m2 });
      const second = useMultiplayerDraftStore.getState().startMatch();
      await vi.waitFor(() => expect(transport.instances).toHaveLength(2));
      expect(await settle(first)).toBe("resolved:null");
      expect(transport.hostRoomOptions.map((o) => o?.preferredRoomCode)).toEqual(["MATCH-m1", "MATCH-m2"]);
      expect(transport.terminateGame).toHaveBeenCalledOnce();
      expect(transport.terminateGame.mock.contexts[0]).toBe(vi.mocked(P2PHostAdapter).mock.results[0].value);
      expect(await settle(useMultiplayerDraftStore.getState().startMatch())).toBe("resolved:null");
      expect(transport.hostRoomSignals).toHaveLength(2);
      transport.instances[1].finish();
      expect(await second).toBe("draft-match-m2");
    });

    it("a round's end abandons a start still parked for its pairing", async () => {
      await podInMatch();
      useGameStore.setState({ gameId: null });
      const first = useMultiplayerDraftStore.getState().startMatch();
      await vi.waitFor(() => expect(transport.instances).toHaveLength(1));
      capturedHostEventHandler!({ type: "roundAdvanced" });
      expect((transport.hostRoomSignals[0] as AbortSignal).aborted).toBe(true);
      capturedHostEventHandler!({ type: "matchStart", launch: m2 });
      expect(await settle(first)).toBe("resolved:null");
      expect(transport.terminateGame).toHaveBeenCalledOnce();
      expect(transport.terminateGame.mock.contexts[0]).toBe(vi.mocked(P2PHostAdapter).mock.results[0].value);
      transport.instances[0].finish();
      await new Promise((resolve) => setTimeout(resolve, 0));
      expect(useMultiplayerDraftStore.getState().matchAdapter).toBeNull();
      expect(useGameStore.getState().gameId).not.toBe("draft-match-m1");
      const second = useMultiplayerDraftStore.getState().startMatch();
      await vi.waitFor(() => expect(transport.instances).toHaveLength(2));
      expect(transport.hostRoomOptions.map((o) => o?.preferredRoomCode)).toEqual(["MATCH-m1", "MATCH-m2"]);
      transport.instances[1].finish();
      expect(await second).toBe("draft-match-m2");
    });

    it("guest arm: a matchStart for the next pairing abandons a start parked in initializeGame", async () => {
      await guestPodInMatch();
      useGameStore.setState({ gameId: null });
      let reply!: () => void;
      const guest = parkNextGuest({ initializeGame: new Promise<void>((resolve) => { reply = resolve; }) });
      const first = useMultiplayerDraftStore.getState().startMatch();
      try {
        await vi.waitFor(() => expect(guest.initializeGame).toHaveBeenCalledOnce());
        capturedDraftGuestListener!({ type: "matchStart", launch: guestLaunch("m2") });
        expect((transport.joinRoomCalls[0].signal as AbortSignal).aborted).toBe(true);
        expect(await settle(first)).toBe("resolved:null");
        expect(transport.guestDispose).toHaveBeenCalledOnce();
        expect(transport.guestDispose.mock.contexts[0]).toBe(vi.mocked(P2PGuestAdapter).mock.results[0].value);
      } finally {
        reply();
        await first;
      }
      expect(useMultiplayerDraftStore.getState().matchAdapter).toBeNull();
      expect(useGameStore.getState().gameId).not.toBe("draft-match-m1");
      expect(await useMultiplayerDraftStore.getState().startMatch()).toBe("draft-match-m2");
      expect(transport.joinRoomCalls.map((call) => call.code)).toEqual(["MATCH-m1", "MATCH-m2"]);
    });

    it("a matchStart repeating the pairing leaves its parked start running", async () => {
      await podInMatch();
      const first = useMultiplayerDraftStore.getState().startMatch();
      await vi.waitFor(() => expect(transport.instances).toHaveLength(1));
      const again = { ...launch };
      capturedHostEventHandler!({ type: "matchStart", launch: again });
      expect(useMultiplayerDraftStore.getState().matchPairing).toBe(again);
      expect((transport.hostRoomSignals[0] as AbortSignal).aborted).toBe(false);
      transport.instances[0].finish();
      expect(await first).toBe("draft-match-m1");
    });

    it("guest arm: a matchStart repeating the pairing leaves its parked start running", async () => {
      await guestPodInMatch();
      let reply!: () => void;
      const guest = parkNextGuest({ initializeGame: new Promise<void>((resolve) => { reply = resolve; }) });
      const first = useMultiplayerDraftStore.getState().startMatch();
      try {
        await vi.waitFor(() => expect(guest.initializeGame).toHaveBeenCalledOnce());
        const again = guestLaunch("m1");
        capturedDraftGuestListener!({ type: "matchStart", launch: again });
        expect(useMultiplayerDraftStore.getState().matchPairing).toBe(again);
        expect((transport.joinRoomCalls[0].signal as AbortSignal).aborted).toBe(false);
      } finally {
        reply();
      }
      expect(await first).toBe("draft-match-m1");
    });

    it("leave() parked on its pod's teardown ends a start parked before it and one pressed during it", async () => {
      let openTeardown!: () => void;
      const teardown = new Promise<void>((resolve) => { openTeardown = resolve; });
      const pod = freshHostAdapter({ dispose: () => teardown });
      mockHostAdapterQueue.push(pod);
      await podInMatch();
      useGameStore.setState({ gameId: null });
      const before = useMultiplayerDraftStore.getState().startMatch();
      await vi.waitFor(() => expect(transport.instances).toHaveLength(1));
      const leaving = useMultiplayerDraftStore.getState().leave();
      try {
        await vi.waitFor(() => expect(pod.dispose).toHaveBeenCalled());
        transport.instances[0].finish();
        expect(await settle(before)).toBe("resolved:null");
        const during = useMultiplayerDraftStore.getState().startMatch();
        await vi.waitFor(() => expect(transport.instances).toHaveLength(2));
        openTeardown();
        await leaving;
        expect((transport.hostRoomSignals[1] as AbortSignal).aborted).toBe(true);
        transport.instances[1].finish();
        expect(await settle(during)).toBe("resolved:null");
      } finally {
        openTeardown();
        await leaving;
      }
      expect(useMultiplayerDraftStore.getState()).toMatchObject({ matchAdapter: null, phase: "idle" });
      expect(useGameStore.getState().gameId).toBeNull();
    });

    it("a new pod whose pairing reuses the match id supersedes the replaced pod's parked start", async () => {
      await podInMatch();
      const first = useMultiplayerDraftStore.getState().startMatch();
      await vi.waitFor(() => expect(transport.instances).toHaveLength(1));
      mockHostAdapterQueue.push(freshHostAdapter());
      await installCompletedPod(commanderView(2));
      useMultiplayerDraftStore.setState({
        matchPairing: { ...launch, matchRoomCode: "POD2-m1", binding: { ...launch.binding, podId: "pod-2" } },
        phase: "matchInProgress",
      });
      const second = useMultiplayerDraftStore.getState().startMatch();
      await vi.waitFor(() => expect(transport.instances).toHaveLength(2));
      expect(await settle(first)).toBe("resolved:null");
      expect((transport.hostRoomSignals[0] as AbortSignal).aborted).toBe(true);
      expect(transport.hostRoomOptions.map((o) => o?.preferredRoomCode)).toEqual(["MATCH-m1", "POD2-m1"]);
      expect(transport.terminateGame).toHaveBeenCalledOnce();
      expect(transport.terminateGame.mock.contexts[0]).toBe(vi.mocked(P2PHostAdapter).mock.results[0].value);
      transport.instances[1].finish();
      expect(await second).toBe("draft-match-m1");
    });

    it.each(["initialize", "startPregameGame", "getSnapshot"] as const)(
      "an abandoned host terminates once while parked in %s and a new pod can start", async (park) => {
      let open!: () => void;
      await podInMatch();
      parkHostOn(park, new Promise<void>((resolve) => { open = resolve; }));
      const first = useMultiplayerDraftStore.getState().startMatch();
      try {
        await reachPark(park);
        expect(transport.terminateGame).not.toHaveBeenCalled();
        mockHostAdapterQueue.push(freshHostAdapter());
        await installCompletedPod(commanderView(2));
        useMultiplayerDraftStore.setState({
          matchPairing: { ...launch, matchRoomCode: "POD2-m1", binding: { ...launch.binding, podId: "pod-2" } },
          phase: "matchInProgress",
        });
        expect((transport.hostRoomSignals[0] as AbortSignal).aborted).toBe(true);
        expect(await settle(first)).toBe("pending");
        expect(transport.terminateGame).toHaveBeenCalledOnce();
        expect(transport.terminateGame.mock.contexts[0]).toBe(vi.mocked(P2PHostAdapter).mock.results[0].value);
        const second = useMultiplayerDraftStore.getState().startMatch();
        await vi.waitFor(() => expect(transport.instances).toHaveLength(2));
        transport.instances[1].finish();
        expect(await second).toBe("draft-match-m1");
        expect(transport.hostRoomOptions.map((o) => o?.preferredRoomCode)).toEqual(["MATCH-m1", "POD2-m1"]);
      } finally {
        open();
      }
      expect(await first).toBeNull();
      expect(transport.terminateGame).toHaveBeenCalledOnce();
    });

    it("a start parked in its snapshot fetch leaves the next pairing's runtime running when the fetch settles", async () => {
      const controllers = distinctControllers();
      let open!: () => void;
      try {
        await podInMatch();
        parkHostOn("getSnapshot", new Promise<void>((resolve) => { open = resolve; }));
        const first = useMultiplayerDraftStore.getState().startMatch();
        await reachPark("getSnapshot");
        capturedHostEventHandler!({ type: "roundAdvanced" });
        capturedHostEventHandler!({ type: "matchStart", launch: m2 });
        const second = useMultiplayerDraftStore.getState().startMatch();
        await vi.waitFor(() => expect(transport.instances).toHaveLength(2));
        transport.instances[1].finish();
        expect(await second).toBe("draft-match-m2");
        const newer = vi.mocked(P2PHostAdapter).mock.results[1].value;
        open();
        expect(await first).toBeNull();
        expect(transport.terminateGame).toHaveBeenCalledTimes(1);
        expect(controllers).toHaveLength(1);
        expect(controllers[0].dispose).not.toHaveBeenCalled();
        expect(useGameStore.getState()).toMatchObject({ adapter: newer, gameId: "draft-match-m2" });
        expect(useMultiplayerDraftStore.getState().matchAdapter).toBe(newer);
      } finally {
        open?.();
        controllers.restore();
      }
    });

    it.each([["hostDraft", "opens"], ["joinDraft", "opens"], ["hostDraft", "is route-aborted"], ["joinDraft", "is route-aborted"]] as const)(
      "a start pressed while %s waits on the replaced pod's teardown opens no room when the call %s", async (opener, end) => {
      let openTeardown!: () => void;
      const teardown = new Promise<void>((resolve) => { openTeardown = resolve; });
      const pod = freshHostAdapter({ dispose: () => teardown });
      mockHostAdapterQueue.push(pod, freshHostAdapter());
      await podInMatch();
      useGameStore.setState({ gameId: null });
      const route = new AbortController();
      const replacing = opener === "hostDraft"
        ? useMultiplayerDraftStore.getState().hostDraft({ ...podConfig, signal: route.signal })
        : useMultiplayerDraftStore.getState().joinDraft({ kind: "new", roomCode: "ABCDE", displayName: "P", signal: route.signal });
      await vi.waitFor(() => expect(pod.dispose).toHaveBeenCalled());
      // Reach: the pairing is still on the store and nothing is in flight, so only the pod guard can refuse the press.
      expect(useMultiplayerDraftStore.getState().matchPairing?.matchId).toBe("m1");
      const started = useMultiplayerDraftStore.getState().startMatch();
      expect(await settle(started)).toBe("resolved:null");
      if (end === "is route-aborted") route.abort();
      openTeardown();
      await replacing;
      for (const instance of transport.instances) instance.finish();
      expect(transport.hostRoomSignals).toHaveLength(0);
      expect(useMultiplayerDraftStore.getState().matchAdapter).toBeNull();
      expect(useGameStore.getState().gameId).not.toBe("draft-match-m1");
    });

    it.each(["hostDraft", "joinDraft"] as const)("a %s route-aborted during the replaced pod's teardown leaves no session behind", async (opener) => {
      let openTeardown!: () => void;
      const teardown = new Promise<void>((resolve) => { openTeardown = resolve; });
      const pod = freshHostAdapter({ dispose: () => teardown });
      mockHostAdapterQueue.push(pod);
      await podInMatch();
      const route = new AbortController();
      const replacing = opener === "hostDraft"
        ? useMultiplayerDraftStore.getState().hostDraft({ ...podConfig, signal: route.signal })
        : useMultiplayerDraftStore.getState().joinDraft({ kind: "new", roomCode: "ABCDE", displayName: "P", signal: route.signal });
      // Reach: the replaced pod's teardown is running, and its session is still on the store.
      await vi.waitFor(() => expect(pod.dispose).toHaveBeenCalled());
      expect(useMultiplayerDraftStore.getState()).toMatchObject({ role: "host", matchPairing: launch });
      route.abort();
      openTeardown();
      expect(await replacing).toEqual({ status: "superseded" });
      expect(useMultiplayerDraftStore.getState()).toMatchObject({ role: null, phase: "idle", matchPairing: null });
    });

    it.each([["hostDraft", "joinDraft"], ["joinDraft", "hostDraft"]] as const)(
      "a %s route-aborted during the replaced pod's teardown keeps a newer %s's session", async (aborted, newer) => {
      let openTeardown!: () => void;
      const teardown = new Promise<void>((resolve) => { openTeardown = resolve; });
      const pod = freshHostAdapter({ dispose: () => teardown });
      mockHostAdapterQueue.push(pod, freshHostAdapter());
      await podInMatch();
      const route = new AbortController();
      const openPod = (opener: "hostDraft" | "joinDraft", signal?: AbortSignal) => opener === "hostDraft"
        ? useMultiplayerDraftStore.getState().hostDraft({ ...podConfig, signal })
        : useMultiplayerDraftStore.getState().joinDraft({ kind: "new", roomCode: "ABCDE", displayName: "P", signal });
      const replacing = openPod(aborted, route.signal);
      await vi.waitFor(() => expect(pod.dispose).toHaveBeenCalled());
      route.abort();
      expect(await openPod(newer)).toMatchObject({ status: "opened" });
      openTeardown();
      expect(await replacing).toEqual({ status: "superseded" });
      expect(useMultiplayerDraftStore.getState().role).toBe(newer === "hostDraft" ? "host" : "guest");
    });

    it("a second start in a route-aborted replacement's teardown window opens no room while the replaced pod's start is parked", async () => {
      let openTeardown!: () => void;
      const teardown = new Promise<void>((resolve) => { openTeardown = resolve; });
      const pod = freshHostAdapter({ dispose: () => teardown });
      mockHostAdapterQueue.push(pod, freshHostAdapter());
      await podInMatch();
      useGameStore.setState({ gameId: null });
      let openFirst!: () => void;
      parkHostOn("initialize", new Promise<void>((resolve) => { openFirst = resolve; }));
      const first = useMultiplayerDraftStore.getState().startMatch();
      const route = new AbortController();
      try {
        await reachPark("initialize");
        const replacing = useMultiplayerDraftStore.getState().hostDraft({ ...podConfig, signal: route.signal });
        await vi.waitFor(() => expect(pod.dispose).toHaveBeenCalled());
        // Reach: the parked start was abandoned, so its slot no longer refuses a press.
        expect((transport.hostRoomSignals[0] as AbortSignal).aborted).toBe(true);
        const second = useMultiplayerDraftStore.getState().startMatch();
        expect(await settle(second)).toBe("resolved:null");
        route.abort();
        openTeardown();
        await replacing;
      } finally {
        openFirst();
      }
      expect(await first).toBeNull();
      expect(transport.hostRoomSignals).toHaveLength(1);
      expect(useMultiplayerDraftStore.getState().matchAdapter).toBeNull();
      expect(useGameStore.getState().gameId).not.toBe("draft-match-m1");
    });

    it("a start pressed before a new pod opens ends before the replaced pod's teardown settles", async () => {
      let openTeardown!: () => void;
      const teardown = new Promise<void>((resolve) => { openTeardown = resolve; });
      const pod = freshHostAdapter({ dispose: () => teardown });
      mockHostAdapterQueue.push(pod, freshHostAdapter());
      await podInMatch();
      const started = useMultiplayerDraftStore.getState().startMatch();
      await vi.waitFor(() => expect(transport.instances).toHaveLength(1));
      const replacing = installCompletedPod(commanderView(2));
      await vi.waitFor(() => expect(pod.dispose).toHaveBeenCalled());
      transport.instances[0].finish();
      expect(await settle(started)).toBe("resolved:null");
      openTeardown();
      await replacing;
    });

    it.each(["online", "offline"] as const)("a start pressed while a resumed pod's open fails %s ends with it", async (connectivity) => {
      let fail!: () => void;
      const pod = freshHostAdapter();
      pod.initialize.mockImplementationOnce(async () => {
        capturedHostEventHandler!({ type: "matchStart", launch });
        await new Promise<void>((_resolve, reject) => { fail = () => reject(new Error("persist failed")); });
      });
      mockHostAdapterQueue.push(pod);
      useGameStore.setState({ gameId: null });
      const opening = installCompletedPod(commanderView(2));
      await vi.waitFor(() => expect(useMultiplayerDraftStore.getState().matchPairing).not.toBeNull());
      const started = useMultiplayerDraftStore.getState().startMatch();
      await vi.waitFor(() => expect(transport.instances).toHaveLength(1));
      useConnectivityStore.setState({ forcedOffline: connectivity === "offline" });
      fail();
      await opening;
      for (const instance of transport.instances) instance.finish();
      const settled = await settle(started);
      expect(useGameStore.getState().gameId).not.toBe("draft-match-m1");
      expect(useMultiplayerDraftStore.getState().matchAdapter).toBeNull();
      expect(settled).toBe("resolved:null");
      expect((transport.hostRoomSignals[0] as AbortSignal).aborted).toBe(true);
    });

    it("a start pressed after a resumed pod's open fails online opens no room", async () => {
      let fail!: () => void;
      const pod = freshHostAdapter();
      pod.initialize.mockImplementationOnce(async () => {
        capturedHostEventHandler!({ type: "matchStart", launch });
        await new Promise<void>((_resolve, reject) => { fail = () => reject(new Error("persist failed")); });
      });
      mockHostAdapterQueue.push(pod);
      useGameStore.setState({ gameId: null });
      const opening = installCompletedPod(commanderView(2));
      await vi.waitFor(() => expect(useMultiplayerDraftStore.getState().matchPairing).not.toBeNull());
      fail();
      await opening;
      // Reach: the pairing is still on the store and nothing is in flight, so only the pod guard can refuse the press.
      expect(useMultiplayerDraftStore.getState().matchPairing?.matchId).toBe("m1");
      const started = useMultiplayerDraftStore.getState().startMatch();
      await new Promise((resolve) => setTimeout(resolve, 20));
      for (const instance of transport.instances) instance.finish();
      const settled = await settle(started);
      expect(useGameStore.getState().gameId).not.toBe("draft-match-m1");
      expect(useMultiplayerDraftStore.getState().matchAdapter).toBeNull();
      expect(transport.hostRoomSignals).toHaveLength(0);
      expect(settled).toBe("resolved:null");
    });

    it.each(["online", "offline"] as const)("guest arm: a start pressed while a reconnect fails %s ends with it", async (connectivity) => {
      let fail!: () => void;
      const make = vi.mocked(P2PDraftGuest).getMockImplementation()!;
      vi.mocked(P2PDraftGuest).mockImplementationOnce(function (this: unknown, ...args: unknown[]) {
        return { ...(make as (...a: unknown[]) => object).apply(this, args), initialize: async () => {
          capturedDraftGuestListener!({ type: "matchStart", launch: guestLaunch("m1") });
          await new Promise<void>((_resolve, reject) => { fail = () => reject(new Error("host gone")); });
        } } as never;
      });
      useGameStore.setState({ gameId: null });
      const opening = useMultiplayerDraftStore.getState()
        .joinDraft({ kind: "reconnect", roomCode: "ABCDE", displayName: "Guest", hostPeerId: "host-peer-id", draftToken: "token" });
      await vi.waitFor(() => expect(useMultiplayerDraftStore.getState().matchPairing).not.toBeNull());
      let reply!: () => void;
      const guest = parkNextGuest({ initializeGame: new Promise<void>((resolve) => { reply = resolve; }) });
      const started = useMultiplayerDraftStore.getState().startMatch();
      await vi.waitFor(() => expect(guest.initializeGame).toHaveBeenCalledOnce());
      useConnectivityStore.setState({ forcedOffline: connectivity === "offline" });
      fail();
      await opening;
      reply();
      const settled = await settle(started);
      expect(useGameStore.getState().gameId).not.toBe("draft-match-m1");
      expect(useMultiplayerDraftStore.getState().matchAdapter).toBeNull();
      expect(settled).toBe("resolved:null");
      expect((transport.joinRoomCalls[1].signal as AbortSignal).aborted).toBe(true);
    });

    it("guest arm: a start pressed after a reconnect fails online dials nothing", async () => {
      let fail!: () => void;
      const make = vi.mocked(P2PDraftGuest).getMockImplementation()!;
      vi.mocked(P2PDraftGuest).mockImplementationOnce(function (this: unknown, ...args: unknown[]) {
        return { ...(make as (...a: unknown[]) => object).apply(this, args), initialize: async () => {
          capturedDraftGuestListener!({ type: "matchStart", launch: guestLaunch("m1") });
          await new Promise<void>((_resolve, reject) => { fail = () => reject(new Error("host gone")); });
        } } as never;
      });
      useGameStore.setState({ gameId: null });
      const opening = useMultiplayerDraftStore.getState()
        .joinDraft({ kind: "reconnect", roomCode: "ABCDE", displayName: "Guest", hostPeerId: "host-peer-id", draftToken: "token" });
      await vi.waitFor(() => expect(useMultiplayerDraftStore.getState().matchPairing).not.toBeNull());
      fail();
      await opening;
      // Reach: the pairing is still on the store and nothing is in flight, so only the pod guard can refuse the press.
      expect(useMultiplayerDraftStore.getState().matchPairing?.matchId).toBe("m1");
      const dialed = transport.joinRoomCalls.length;
      const settled = await settle(useMultiplayerDraftStore.getState().startMatch());
      expect(useGameStore.getState().gameId).not.toBe("draft-match-m1");
      expect(useMultiplayerDraftStore.getState().matchAdapter).toBeNull();
      expect(transport.joinRoomCalls).toHaveLength(dialed);
      expect(settled).toBe("resolved:null");
    });

    it.each(["replaced", "left"] as const)(
      "guest arm: a start parked in initializeGame is released when its pod is %s",
      async (end) => {
        await podInMatch();
        const guestPairing = (matchRoomCode: string) => ({
          type: "HumanGuest" as const, matchId: "m1", matchRoomCode, round: 1, localSeat: 1,
          opponentSeat: 0, opponentName: "Host", matchHostPeerId: "peer-0", localDeck: deckFor(1),
          matchConfig: { match_type: "Bo1" as const }, binding: launch.binding,
        });
        useMultiplayerDraftStore.setState({ matchPairing: guestPairing("MATCH-m1") });
        let reply!: () => void;
        const guest = parkNextGuest({ initializeGame: new Promise<void>((resolve) => { reply = resolve; }) });
        const first = useMultiplayerDraftStore.getState().startMatch();
        try {
          await vi.waitFor(() => expect(guest.initializeGame).toHaveBeenCalledOnce());
          const firstSignal = transport.joinRoomCalls[0].signal as AbortSignal;
          if (end === "left") await useMultiplayerDraftStore.getState().leave();
          mockHostAdapterQueue.push(freshHostAdapter());
          await installCompletedPod(commanderView(2));
          useMultiplayerDraftStore.setState({ matchPairing: guestPairing("POD2-m1"), phase: "matchInProgress" });
          const second = useMultiplayerDraftStore.getState().startMatch();
          expect(await settle(first)).toBe("resolved:null");
          expect(firstSignal.aborted).toBe(true);
          expect(transport.guestDispose).toHaveBeenCalledOnce();
          expect(transport.guestDispose.mock.contexts[0]).toBe(vi.mocked(P2PGuestAdapter).mock.results[0].value);
          expect(await second).toBe("draft-match-m1");
          expect(transport.joinRoomCalls.map((call) => call.code)).toEqual(["MATCH-m1", "POD2-m1"]);
        } finally {
          reply();
          await first;
        }
      },
    );

    it("guest arm: a start the host refuses in initializeGame reports the refusal", async () => {
      await podInMatch();
      useMultiplayerDraftStore.setState({
        matchPairing: {
          type: "HumanGuest", matchId: "m1", matchRoomCode: "MATCH-m1", round: 1, localSeat: 1,
          opponentSeat: 0, opponentName: "Host", matchHostPeerId: "peer-0", localDeck: deckFor(1),
          matchConfig: { match_type: "Bo1" }, binding: launch.binding,
        },
      });
      let refuse!: (reason: Error) => void;
      const guest = parkNextGuest({ initializeGame: new Promise<void>((_resolve, reject) => { refuse = reject; }) });
      const started = useMultiplayerDraftStore.getState().startMatch();
      await vi.waitFor(() => expect(guest.initializeGame).toHaveBeenCalledOnce());
      refuse(new Error("Host left the game"));
      expect(await settle(started)).toBe("resolved:null");
      expect(useMultiplayerDraftStore.getState().error).toBe("Host left the game");
      expect(transport.guestDispose).toHaveBeenCalledOnce();
    });

    it("guest arm: leave() while the snapshot install is parked publishes nothing", async () => {
      await podInMatch();
      useMultiplayerDraftStore.setState({
        matchPairing: {
          type: "HumanGuest", matchId: "m1", matchRoomCode: "MATCH-m1", round: 1, localSeat: 1,
          opponentSeat: 0, opponentName: "Host", matchHostPeerId: "peer-0", localDeck: deckFor(1),
          matchConfig: { match_type: "Bo1" }, binding: launch.binding,
        },
      });
      let open!: () => void;
      transport.control.guestSnapshotGate = new Promise<void>((resolve) => { open = resolve; });
      const started = useMultiplayerDraftStore.getState().startMatch();
      await vi.waitFor(() => expect(transport.guestInstances).toHaveLength(1));
      await new Promise((resolve) => setTimeout(resolve, 10));
      const built = vi.mocked(P2PGuestAdapter).mock.results[0].value as { getSnapshot: ReturnType<typeof vi.fn> };
      expect(built.getSnapshot).toHaveBeenCalledOnce();
      await useMultiplayerDraftStore.getState().leave();
      const createsBefore = matchLoopMock.create.mock.calls.length;
      open();
      expect(await settle(started)).toBe("resolved:null");
      const s = useMultiplayerDraftStore.getState();
      expect(transport.guestDispose).toHaveBeenCalledOnce();
      expect(transport.joinDestroyPeer).not.toHaveBeenCalled();
      expect(matchLoopMock.create.mock.calls.length).toBe(createsBefore);
      expect(s.matchAdapter).toBeNull();
      expect(s.phase).toBe("idle");
    });

    it("host arm: leave() while startPregameGame is parked installs no game", async () => {
      await podInMatch();
      useGameStore.setState({ gameMode: null });
      let open!: () => void;
      const gate = new Promise<void>((resolve) => { open = resolve; });
      transport.startPregameGame.mockImplementationOnce(async () => { await gate; return { log_entries: [] }; });
      const started = useMultiplayerDraftStore.getState().startMatch();
      await vi.waitFor(() => expect(transport.instances).toHaveLength(1));
      transport.instances[0].finish();
      await vi.waitFor(() => expect(transport.startPregameGame).toHaveBeenCalled());
      await useMultiplayerDraftStore.getState().leave();
      open();
      expect(await settle(started)).toBe("resolved:null");
      expect(useGameStore.getState().gameMode).toBeNull();
    });

    it("reset() while hostRoom is parked aborts it", async () => {
      await podInMatch();
      let open!: () => void;
      transport.control.hostRoomGate = new Promise<void>((resolve) => { open = resolve; });
      const started = useMultiplayerDraftStore.getState().startMatch();
      await vi.waitFor(() => expect(transport.hostRoomSignals).toHaveLength(1));
      useMultiplayerDraftStore.getState().reset();
      open();
      expect((transport.hostRoomSignals[0] as AbortSignal | undefined)?.aborted).toBe(true);
      expect(await settle(started)).toBe("resolved:null");
      expect(transport.instances).toHaveLength(0);
    });
  });

  /**
   * The freshest-view contract, which `P2PDraftHost.commanderSeatDecks` states
   * in its own doc and makes the CALLER's responsibility: `handleGuestDisconnect`
   * drops a session synchronously while the engine's `connected` flag reaches
   * this store later, so a view captured before the signalling round-trip can be
   * arbitrarily stale by the time the seats are classified.
   */
  it("classifies seats from the view as it stands after the room round-trip, not before", async () => {
    await installCompletedPod(commanderView(4, { humanSeats: [1] }));
    // The assembler answers the view it is HANDED, as the real one does: this
    // suite's own `seatDecksFor` applies `commanderSeatPlan`'s exact rule
    // (`!is_bot && connected`). Left as a fixed `mockResolvedValue`, the plan
    // would be identical for both reads and the staleness would be invisible.
    commanderSeatDecks.mockImplementation(async (v, localSeat) => seatDecksFor(v, localSeat));
    let openRoom!: () => void;
    transport.control.hostRoomGate = new Promise<void>((resolve) => {
      openRoom = resolve;
    });

    void useMultiplayerDraftStore.getState().launchCommanderGame(navigate);
    await vi.waitFor(() => expect(transport.hostRoomSignals).toHaveLength(1));

    // Seat 1 drops WHILE the room is coming up — the window the callee's doc
    // names, and one that is hundreds of milliseconds to seconds wide.
    useMultiplayerDraftStore.setState({
      view: commanderView(4, { humanSeats: [1], droppedSeats: [1] }),
    });
    openRoom();

    // On the FRESH view seat 1 is engine-piloted, so the room fills and the
    // launch completes. On the stale one it is classified live: it gets no
    // engine seat, `sendToSeat` no-ops for it because there is no session, the
    // seat stays `WaitingHuman`, `roomFull` never fires — and this times out,
    // which is the host parked on "Waiting for players to join…" forever.
    await vi.waitFor(() => expect(navigate).toHaveBeenCalledTimes(1));
    expect(seatMutations().map((m) => (m.data as { seatIndex: number }).seatIndex)).toEqual([
      1, 2, 3,
    ]);
    // The classifier was handed the post-round-trip view, not the captured one.
    expect(commanderSeatDecks).toHaveBeenCalledWith(
      expect.objectContaining({
        seats: expect.arrayContaining([
          expect.objectContaining({ seat_index: 1, connected: false }),
        ]),
      }),
      0,
    );
  });
});
