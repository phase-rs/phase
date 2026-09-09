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
import { useGameStore } from "../gameStore";
import { processRemoteUpdate } from "../../game/dispatch";
import { P2PGuestAdapter, P2PHostAdapter } from "../../adapter/p2p-adapter";
import type { DraftPlayerView, SeatPublicView } from "../../adapter/draft-adapter";
import type { CommanderSeatDecks, DraftDeckPayload } from "../../network/draftProtocol";

// ── Mocks ──────────────────────────────────────────────────────────────

let capturedHostEventHandler: ((event: unknown) => void) | null = null;

const commanderSeatDecks = vi.fn<
  (view: DraftPlayerView, localSeat: number) => Promise<CommanderSeatDecks>
>();
const sendCommanderLaunches = vi.fn();
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
const podCommanderDeckPayload = vi.fn(async () => ({
  player: deckFor(0),
  opponent: deckFor(1),
  ai_decks: [],
  draft_set_codes: ["CMR"],
}));

const mockHostAdapter = {
  onEvent: vi.fn((handler: (event: unknown) => void) => {
    capturedHostEventHandler = handler;
    return vi.fn();
  }),
  initialize: vi.fn(async () => {}),
  dispose: vi.fn(async () => {}),
  commanderSeatDecks,
  sendCommanderLaunches,
  podCommanderDeckPayload,
  status: "lobby" as const,
  roomCode: "ABCDE",
};

vi.mock("../../adapter/draftPodHostAdapter", () => ({
  // `function`, not an arrow: `hostDraft` calls `new DraftPodHostAdapter()`.
  DraftPodHostAdapter: vi.fn().mockImplementation(function () {
    return mockHostAdapter;
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
    /**
     * When set, the guest's `getSnapshot` parks on it.
     *
     * That is the ONLY window in which an abort can land after
     * `installMatchRuntime` has committed the adapter into `useGameStore` but
     * before `throwIfAborted()` observes it — `installMatchRuntime` awaits this
     * fetch, then commits synchronously, and the abort check is the next
     * statement. `joinRoomGate` parks far too early to reach it.
     */
    guestSnapshotGate: null as Promise<void> | null,
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

vi.mock("../../adapter/p2p-draft-guest", () => ({
  P2PDraftGuest: vi.fn().mockImplementation(function () {
    return {
      onEvent: (listener: (event: unknown) => void) => {
        capturedDraftGuestListener = listener;
        return () => {};
      },
      initialize: vi.fn(async () => {}),
      dispose: vi.fn(),
      leave: vi.fn(async () => {}),
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
    humanSeats?: number[];
    /**
     * Human seats whose `connected` flag is FALSE — a player who has dropped.
     * `commanderSeatPlan`'s rule is `!is_bot && connected`, so a dropped human
     * is engine-piloted, which is the whole difference a stale view erases.
     */
    droppedSeats?: number[];
  } = {},
): DraftPlayerView {
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
async function installCompletedPod(view: DraftPlayerView, localSeat = 0) {
  await useMultiplayerDraftStore.getState().hostDraft({
    poolInput: { type: "Set", data: { pools: [{ code: "TST" }], sequence: ["TST"] } },
    kind: "CommanderDraft",
    podSize: view.seats.length,
    hostDisplayName: "Host",
    tournamentFormat: "Swiss",
    podPolicy: "Competitive",
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

/** The seat mutations this launch applied, in call order. */
function seatMutations(): Array<Record<string, unknown>> {
  return transport.applySeatMutation.mock.calls.map((call) => call[0] as Record<string, unknown>);
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
    await installCompletedPod(commanderView(4, { draftSetCodes: ["CMM", "CLB"] }));

    await useMultiplayerDraftStore.getState().launchCommanderGame(navigate);

    const payload = vi.mocked(P2PHostAdapter).mock.calls[0][0] as {
      player: DraftDeckPayload;
      draft_set_codes: string[] | null;
    };
    expect(payload.draft_set_codes).toEqual(["CMM", "CLB"]);
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
  it("launches a local game for a pod over the peer-to-peer seat ceiling", async () => {
    await installCompletedPod(commanderView(7));

    await useMultiplayerDraftStore.getState().launchCommanderGame(navigate);

    // The local game, carrying the pod's own seat count — never the literal 4
    // and never the six-seat ceiling.
    expect(navigate).toHaveBeenCalledTimes(1);
    const target = vi.mocked(navigate).mock.calls[0]?.[0] as string;
    expect(target).toContain("mode=ai");
    expect(target).toContain("format=CommanderDraft");
    expect(target).toContain("players=7");
    // The payload is assembled by the HOST adapter, in game-player order, and
    // stashed where the game route reads it.
    expect(podCommanderDeckPayload).toHaveBeenCalledTimes(1);
    const gameId = target.slice("/game/".length, target.indexOf("?"));
    expect(sessionStorage.getItem(`phase:draft-deck:${gameId}`)).toContain("Commander 0");
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

  /**
   * The abort arrives LATE — after the bring-up has already committed itself to
   * `useGameStore`, which the row above cannot reach because it parks in
   * `joinRoom`, several awaits earlier.
   *
   * `installMatchRuntime` awaits a snapshot fetch and then commits
   * synchronously, so a cancel landing during that fetch leaves the runtime
   * installed and `throwIfAborted()` throwing immediately afterwards. The catch
   * disposes the adapter, but `set({ matchAdapter })` never ran — and
   * `disposeMatchAdapter`'s entire body is fenced on that field. So nothing
   * `leave`, `reset` or `endCommanderSession` can do will ever reach the
   * committed runtime: it is a DISPOSED adapter parked under a live
   * `draft-match` game id.
   */
  it("releases a runtime the aborted join had already committed", async () => {
    await installJoinedPod(commanderView(4, { humanSeats: [1, 2, 3] }));
    capturedDraftGuestListener?.({ type: "commanderLaunch", launch: commanderLaunchFor(2) });

    // Every game id the store passes through, so the assertions below can tell
    // "cleaned up after installing" from "never installed at all". Without this
    // control the final `toBeNull()` pair passes vacuously on any join that
    // aborts EARLY, and the row would go green with the fix reverted.
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

    // The control: the runtime really was committed before the abort was seen.
    expect(installedGameIds).toContain("game-1");
    // ...and nothing is left holding it. Both fields, because `adapter` alone
    // would stay null if the commit had written only `gameId`.
    expect(useGameStore.getState().adapter).toBeNull();
    expect(useGameStore.getState().gameId).toBeNull();
    // The user left the pod; they must not be dropped into the game regardless.
    expect(navigate).not.toHaveBeenCalled();
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
