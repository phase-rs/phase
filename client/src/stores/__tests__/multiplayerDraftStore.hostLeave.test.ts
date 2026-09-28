import "fake-indexeddb/auto";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

/**
 * leave(false) of a host X parked on an await, while a newer session B takes
 * the store. Real store, real DraftPodHostAdapter / DraftPodGuestAdapter, real
 * draftPersistence on fake-indexeddb; P2PDraftHost, P2PDraftGuest, the
 * connection layer and the Commander game transport are faked.
 */
const probe = vi.hoisted(() => ({
  hostTerminateGate: null as Promise<void> | null,
  hostTerminateEntered: 0,
  gameTerminateGate: null as Promise<void> | null,
  gameTerminateEntered: 0,
  /** Awaited at the very top of the fake `P2PDraftHost.initialize()`, before
   *  it persists anything — models an initialization that is still parked
   *  when `leave` runs, then fails once released. */
  hostInitializeGate: null as Promise<void> | null,
  hostInitializeEntered: 0,
  /** Awaited inside the fake `P2PDraftHost.dispose()`. */
  hostDisposeGate: null as Promise<void> | null,
  hostDisposeEntered: 0,
  hosts: [] as Array<{
    persistenceId?: string;
    roomCode?: string;
    terminateDraft: ReturnType<typeof import("vitest").vi.fn>;
    dispose: ReturnType<typeof import("vitest").vi.fn>;
    emit: (e: unknown) => void;
  }>,
  guests: [] as Array<{ hostPeerId: string; dispose: ReturnType<typeof import("vitest").vi.fn> }>,
  hostRoomCodes: [] as string[],
  hostRoomCalls: 0,
  sendCommanderLaunches: 0,
}));

vi.mock("../../network/connection", async (importOriginal) => ({
  ...(await importOriginal<typeof import("../../network/connection")>()),
  hostRoom: vi.fn(async (_signal: unknown, opts?: { preferredRoomCode?: string }) => {
    probe.hostRoomCalls += 1;
    const code = opts?.preferredRoomCode ?? probe.hostRoomCodes.shift() ?? "ZZZZZ";
    return {
      roomCode: code,
      peerId: `phase2-${code}`,
      peer: { id: `phase2-${code}`, destroy: vi.fn(), on: vi.fn() },
      onGuestConnected: vi.fn(() => () => {}),
      destroy: vi.fn(),
    };
  }),
  joinRoom: vi.fn(async (code: string) => ({
    conn: { peer: `phase2-${code}` },
    peer: { id: `peer-guest-${code}`, destroy: vi.fn() },
    closeConn: vi.fn(),
    destroyPeer: vi.fn(),
  })),
}));

vi.mock("../../adapter/p2p-draft-host", async (importOriginal) => {
  const persistence = await import("../../services/draftPersistence");
  class FakeP2PDraftHost {
    persistenceId?: string;
    roomCode?: string;
    listeners: Array<(e: unknown) => void> = [];
    dispose = vi.fn(async () => {
      probe.hostDisposeEntered += 1;
      if (probe.hostDisposeGate) await probe.hostDisposeGate;
    });
    // Models p2p-draft-host.ts::terminateDraft's durable half: it clears the
    // host record under its own persistence id, then disposes.
    terminateDraft = vi.fn(async () => {
      probe.hostTerminateEntered += 1;
      if (probe.hostTerminateGate) await probe.hostTerminateGate;
      if (this.persistenceId) await persistence.clearDraftHostSession(this.persistenceId);
      await this.dispose();
    });
    sendCommanderLaunches = vi.fn(() => { probe.sendCommanderLaunches += 1; });
    constructor(...args: unknown[]) {
      this.persistenceId = args[9] as string | undefined;
      this.roomCode = args[10] as string | undefined;
      probe.hosts.push({
        persistenceId: this.persistenceId,
        roomCode: this.roomCode,
        terminateDraft: this.terminateDraft,
        dispose: this.dispose,
        emit: (e) => this.listeners.forEach((l) => l(e)),
      });
    }
    onEvent(l: (e: unknown) => void) {
      this.listeners.push(l);
      return () => { this.listeners = this.listeners.filter((x) => x !== l); };
    }
    async initialize() {
      probe.hostInitializeEntered += 1;
      if (probe.hostInitializeGate) await probe.hostInitializeGate;
      if (!this.persistenceId) return;
      await persistence.saveDraftHostSession(this.persistenceId, {
        persistenceId: this.persistenceId,
        roomCode: this.roomCode!,
        kind: "Premier",
        podSize: 8,
        hostDisplayName: "Host",
        tournamentFormat: "Swiss",
        podPolicy: "Competitive",
        seatTokens: { 0: "host-token" },
        seatNames: { 0: "Host" },
        kickedTokens: [],
        draftStarted: false,
        draftCode: "draft-12345678",
        draftSessionJson: null,
        poolInput: { type: "Set", data: { pools: [{ code: "TST" }], sequence: ["TST"] } },
      } as never);
    }
    async restoreFromPersisted() { return null; }
    getHostWorkspaceState() { return null; }
    async commanderSeatDecks() {
      const empty = { main_deck: [], sideboard: [], commander: [], planar_deck: [], scheme_deck: [] };
      return { hostDeck: empty, engineSeatDecks: [], guestDecks: [] };
    }
    async boosterPackPoolForGame() { return null; }
    async podCommanderDeckPayload() { throw new Error("unused"); }
  }
  return {
    ...(await importOriginal<typeof import("../../adapter/p2p-draft-host")>()),
    P2PDraftHost: FakeP2PDraftHost,
  };
});

vi.mock("../../adapter/p2p-draft-guest", async () => {
  const persistence = await import("../../services/draftPersistence");
  return {
    P2PDraftGuest: vi.fn().mockImplementation(function (
      _peer: unknown,
      hostPeerId: string,
      _conn: unknown,
      connection: { roomCode?: string; displayName?: string },
    ) {
      const instance = {
        onEvent: () => () => {},
        initialize: vi.fn(async () => {
          persistence.saveActiveDraftGuest({
            roomCode: connection?.roomCode ?? "GGGGG",
            displayName: connection?.displayName ?? "G",
            hostPeerId,
          });
        }),
        leave: vi.fn(async () => {}),
        dispose: vi.fn(),
        get isRecoveryRevoked() { return false; },
        view: null,
        seat: null,
        token: null,
      };
      probe.guests.push({ hostPeerId, dispose: instance.dispose });
      return instance;
    }),
  };
});

vi.mock("../../adapter/p2p-adapter", () => ({
  P2PHostAdapter: vi.fn().mockImplementation(function () {
    return {
      onEvent: vi.fn(() => () => {}),
      initialize: vi.fn(async () => {}),
      applySeatMutation: vi.fn(async () => {}),
      terminateGame: vi.fn(async () => {
        probe.gameTerminateEntered += 1;
        if (probe.gameTerminateGate) await probe.gameTerminateGate;
      }),
      dispose: vi.fn(),
    };
  }),
  P2PGuestAdapter: vi.fn(),
}));

import { clear as idbClear } from "idb-keyval";
import { useMultiplayerDraftStore } from "../multiplayerDraftStore";
import { useConnectivityStore } from "../connectivityStore";
import {
  getDraftStore,
  inspectActiveDraftGuest,
  inspectActiveDraftPod,
  loadDraftHostSession,
} from "../../services/draftPersistence";

function hostConfig(persistenceId: string, signal?: AbortSignal) {
  return {
    poolInput: { type: "Set", data: { pools: [{ code: "TST" }], sequence: ["TST"] } },
    kind: "Premier",
    podSize: 8,
    hostDisplayName: "Host",
    tournamentFormat: "Swiss",
    podPolicy: "Competitive",
    persistenceId,
    signal,
  } as never;
}

function gate() {
  let release!: () => void;
  const promise = new Promise<void>((resolve) => { release = resolve; });
  return { promise, release };
}

async function flush() {
  for (let i = 0; i < 20; i++) await new Promise((r) => setTimeout(r, 0));
}

function locator(): string {
  const pod = inspectActiveDraftPod();
  return pod.type === "present" ? `${pod.meta.id}/${pod.meta.roomCode}` : pod.type;
}

function fakeHost(roomCode: string) {
  const host = probe.hosts.find((h) => h.roomCode === roomCode);
  if (!host) throw new Error(`no fake host for ${roomCode}`);
  return host;
}

async function openHost(persistenceId: string, roomCode: string, signal?: AbortSignal) {
  probe.hostRoomCodes.push(roomCode);
  await expect(useMultiplayerDraftStore.getState().hostDraft(hostConfig(persistenceId, signal)))
    .resolves.toEqual({ status: "opened" });
  // Antecedent: the pod's durable record and locator exist before it is left.
  expect(locator()).toBe(`${persistenceId}/${roomCode}`);
  expect(await loadDraftHostSession(persistenceId)).not.toBeNull();
}

function startB(kind: "host" | "guest") {
  if (kind === "host") {
    probe.hostRoomCodes.push("BBHHH");
    return useMultiplayerDraftStore.getState().hostDraft(hostConfig("pod-B"));
  }
  return useMultiplayerDraftStore.getState().joinDraft(
    { kind: "new", roomCode: "GGGGG", displayName: "G" } as never,
  );
}

/** Makes X a completed Commander pod with a launch parked on `roomFull`. */
async function parkCommanderLaunch() {
  const s = useMultiplayerDraftStore.getState();
  useMultiplayerDraftStore.setState({
    view: {
      ...(s.view ?? {}),
      status: "Complete",
      launch_capability: "CommanderMultiplayer",
      seats: [
        { seat_index: 0, display_name: "Host", is_bot: false, connected: true, has_submitted_deck: true },
        { seat_index: 1, display_name: "G1", is_bot: false, connected: true, has_submitted_deck: true },
      ],
      draft_set_codes: ["CMR"],
      match_config: { match_type: "Bo1" },
      pool: [],
    } as never,
    seatIndex: 0,
  });
  const launch = useMultiplayerDraftStore.getState().launchCommanderGame(() => {});
  await vi.waitFor(() => expect(probe.sendCommanderLaunches).toBe(1));
  // Wrapped: an async function returning the launch promise would adopt it.
  return { launch };
}

async function expectParked(leaving: Promise<unknown>) {
  let settled = false;
  void leaving.then(() => { settled = true; }, () => { settled = true; });
  await flush();
  expect(settled).toBe(false);
}

function expectBLive(kind: "host" | "guest") {
  const s = useMultiplayerDraftStore.getState();
  if (kind === "host") {
    expect({ role: s.role, phase: s.phase, roomCode: s.roomCode }).toEqual({ role: "host", phase: "lobby", roomCode: "BBHHH" });
    expect(fakeHost("BBHHH").terminateDraft).not.toHaveBeenCalled();
    expect(fakeHost("BBHHH").dispose).not.toHaveBeenCalled();
  } else {
    expect({ role: s.role, phase: s.phase }).toEqual({ role: "guest", phase: "lobby" });
    expect(probe.guests[probe.guests.length - 1]!.dispose).not.toHaveBeenCalled();
    const g = inspectActiveDraftGuest();
    expect(g.type === "present" && g.meta.hostPeerId).toBe("phase2-GGGGG");
  }
}

describe("leave(false) of a host finishes ending it when a newer session takes the store", () => {
  const gates: Array<() => void> = [];
  beforeEach(async () => {
    vi.clearAllMocks();
    probe.hosts = [];
    probe.guests = [];
    probe.hostRoomCodes = [];
    probe.hostRoomCalls = 0;
    probe.hostTerminateGate = null;
    probe.gameTerminateGate = null;
    probe.hostTerminateEntered = 0;
    probe.gameTerminateEntered = 0;
    probe.hostInitializeGate = null;
    probe.hostInitializeEntered = 0;
    probe.hostDisposeGate = null;
    probe.hostDisposeEntered = 0;
    probe.sendCommanderLaunches = 0;
    useConnectivityStore.setState({ forcedOffline: false, browserOnline: true });
    localStorage.clear();
    await idbClear(getDraftStore());
  });
  afterEach(async () => {
    for (const release of gates.splice(0)) release();
    probe.hostTerminateGate = null;
    probe.gameTerminateGate = null;
    probe.hostInitializeGate = null;
    probe.hostDisposeGate = null;
    await useMultiplayerDraftStore.getState().leave(true);
    await flush();
  });

  function hold(which: "game" | "host") {
    const g = gate();
    gates.push(g.release);
    if (which === "game") probe.gameTerminateGate = g.promise;
    else probe.hostTerminateGate = g.promise;
    return g;
  }

  for (const bKind of ["host", "guest"] as const) {
    it(`ends the left host's pod when a ${bKind} session opens while leave waits on the Commander termination`, async () => {
      await openHost("pod-X", "XXAAA");
      const { launch } = await parkCommanderLaunch();
      const g = hold("game");
      const leaving = useMultiplayerDraftStore.getState().leave(false);
      await vi.waitFor(() => expect(probe.gameTerminateEntered).toBe(1));
      await expectParked(leaving);

      await expect(startB(bKind)).resolves.toEqual({ status: "opened" });
      g.release();
      await leaving;
      await launch;
      await flush();

      expect(fakeHost("XXAAA").terminateDraft).toHaveBeenCalledOnce();
      expect(await loadDraftHostSession("pod-X")).toBeNull();
      expect(locator()).toBe(bKind === "host" ? "pod-B/BBHHH" : "absent");
      if (bKind === "host") expect(await loadDraftHostSession("pod-B")).not.toBeNull();
      expectBLive(bKind);
    });

    it(`ends the left host's pod once when a ${bKind} session opens while the host is being terminated`, async () => {
      await openHost("pod-X", "XXAAA");
      const g = hold("host");
      const leaving = useMultiplayerDraftStore.getState().leave(false);
      await vi.waitFor(() => expect(probe.hostTerminateEntered).toBe(1));
      await expectParked(leaving);

      const b = startB(bKind);
      g.release();
      await expect(b).resolves.toEqual({ status: "opened" });
      await leaving;
      await flush();

      expect(fakeHost("XXAAA").terminateDraft).toHaveBeenCalledOnce();
      expect(fakeHost("XXAAA").dispose).toHaveBeenCalledOnce();
      expect(await loadDraftHostSession("pod-X")).toBeNull();
      expect(locator()).toBe(bKind === "host" ? "pod-B/BBHHH" : "absent");
      expectBLive(bKind);
    });
  }

  it("leaves the locator of a host opened after the left host's route was aborted", async () => {
    const route = new AbortController();
    await openHost("pod-X", "XXAAA", route.signal);
    const g = hold("host");
    const leaving = useMultiplayerDraftStore.getState().leave(false);
    await vi.waitFor(() => expect(probe.hostTerminateEntered).toBe(1));
    await expectParked(leaving);

    route.abort();
    // B has no previous adapter to wait on, so it opens while X is still being terminated.
    await expect(startB("host")).resolves.toEqual({ status: "opened" });
    expect(locator()).toBe("pod-B/BBHHH");
    g.release();
    await leaving;
    await flush();

    expect(fakeHost("XXAAA").terminateDraft).toHaveBeenCalledOnce();
    expect(await loadDraftHostSession("pod-X")).toBeNull();
    expect(locator()).toBe("pod-B/BBHHH");
    expect(await loadDraftHostSession("pod-B")).not.toBeNull();
    expectBLive("host");
  });

  it("clears the left host's locator after the host re-saved it during the wait", async () => {
    await openHost("pod-X", "XXAAA");
    const { launch } = await parkCommanderLaunch();
    const g = hold("game");
    const leaving = useMultiplayerDraftStore.getState().leave(false);
    await vi.waitFor(() => expect(probe.gameTerminateEntered).toBe(1));
    const before = inspectActiveDraftPod();
    await new Promise((r) => setTimeout(r, 5));
    fakeHost("XXAAA").emit({ type: "draftComplete" });
    const after = inspectActiveDraftPod();
    // Antecedent: the locator is still X's, and it moved.
    expect(after.type === "present" && after.meta.id).toBe("pod-X");
    expect(before.type === "present" && after.type === "present" && after.meta.updatedAt > before.meta.updatedAt).toBe(true);

    await expect(startB("guest")).resolves.toEqual({ status: "opened" });
    g.release();
    await leaving;
    await launch;
    await flush();

    expect(locator()).toBe("absent");
    expect(await loadDraftHostSession("pod-X")).toBeNull();
    expectBLive("guest");
  });

  for (const park of ["Commander termination", "host termination"] as const) {
    it(`lets a host reusing the left pod's persistence id open only after the left pod's records are gone (leave waiting on the ${park})`, async () => {
      const route = new AbortController();
      await openHost("pod-S", "SSXXX", route.signal);
      let launch: Promise<void> = Promise.resolve();
      let game: { release: () => void } | null = null;
      if (park === "Commander termination") {
        ({ launch } = await parkCommanderLaunch());
        game = hold("game");
      }
      const hostTerm = hold("host");
      const leaving = useMultiplayerDraftStore.getState().leave(false);
      if (park === "Commander termination") {
        await vi.waitFor(() => expect(probe.gameTerminateEntered).toBe(1));
      }
      await expectParked(leaving);

      // A route abort detaches X while leave waits; X's termination is held.
      route.abort();
      await vi.waitFor(() => expect(probe.hostTerminateEntered).toBe(1));
      probe.hostRoomCodes.push("SSBBB");
      const hostRoomCallsBeforeB = probe.hostRoomCalls;
      const b = useMultiplayerDraftStore.getState().hostDraft(hostConfig("pod-S"));
      await flush();
      // B has not reached its transport while X's termination runs.
      expect(probe.hostRoomCalls).toBe(hostRoomCallsBeforeB);

      hostTerm.release();
      await expect(b).resolves.toEqual({ status: "opened" });
      game?.release();
      await leaving;
      await launch;
      await flush();

      expect(fakeHost("SSXXX").terminateDraft).toHaveBeenCalledOnce();
      expect(locator()).toBe("pod-S/SSBBB");
      expect((await loadDraftHostSession("pod-S"))?.roomCode).toBe("SSBBB");
      const s = useMultiplayerDraftStore.getState();
      expect({ role: s.role, phase: s.phase, roomCode: s.roomCode }).toEqual({ role: "host", phase: "lobby", roomCode: "SSBBB" });
      expect(fakeHost("SSBBB").terminateDraft).not.toHaveBeenCalled();
      expect(fakeHost("SSBBB").dispose).not.toHaveBeenCalled();
    });
  }

  it("lets a host reusing the left pod's persistence id open directly, with no route abort, only once its own termination completes (leave parked on the Commander termination)", async () => {
    await openHost("pod-D", "DDAAA");
    const { launch } = await parkCommanderLaunch();
    const g = hold("game");
    const leaving = useMultiplayerDraftStore.getState().leave(false);
    await vi.waitFor(() => expect(probe.gameTerminateEntered).toBe(1));
    await expectParked(leaving);

    // No route abort: X is still `activeHostAdapter` when B's own `hostDraft`
    // call detaches it directly (`hostDraft`'s own entry, not `abortOwner`).
    const hostTerm = hold("host");
    probe.hostRoomCodes.push("DDBBB");
    const hostRoomCallsBeforeB = probe.hostRoomCalls;
    const b = useMultiplayerDraftStore.getState().hostDraft(hostConfig("pod-D"));
    await vi.waitFor(() => expect(probe.hostTerminateEntered).toBe(1));
    await flush();
    // B has not reached its transport while X's termination runs.
    expect(probe.hostRoomCalls).toBe(hostRoomCallsBeforeB);

    hostTerm.release();
    await expect(b).resolves.toEqual({ status: "opened" });
    g.release();
    await leaving;
    await launch;
    await flush();

    expect(fakeHost("DDAAA").terminateDraft).toHaveBeenCalledOnce();
    expect(locator()).toBe("pod-D/DDBBB");
    expect((await loadDraftHostSession("pod-D"))?.roomCode).toBe("DDBBB");
    const s = useMultiplayerDraftStore.getState();
    expect({ role: s.role, phase: s.phase, roomCode: s.roomCode }).toEqual({ role: "host", phase: "lobby", roomCode: "DDBBB" });
    expect(fakeHost("DDBBB").terminateDraft).not.toHaveBeenCalled();
    expect(fakeHost("DDBBB").dispose).not.toHaveBeenCalled();
  });

  it("doesn't let a host reusing the left pod's persistence id save a locator that outlives it, when leave marked the host before its OWN initialization failed", async () => {
    // X never reaches "opened": its own `P2PDraftHost.initialize()` is parked,
    // then rejects, so this is `hostDraft`'s own initialization-failure
    // `finally` -- not `leave`'s direct dispose and not a route abort -- that
    // first nulls `activeHostAdapter`/`activeHostPersistenceId` for X.
    probe.hostRoomCodes.push("XXAAA");
    const initGate = gate();
    probe.hostInitializeGate = initGate.promise.then(() => {
      throw new Error("host init failed");
    });
    const disposeGate = gate();
    probe.hostDisposeGate = disposeGate.promise;
    // Safety net: if an assertion below throws, release both so a
    // never-resolving teardown does not leak into `retainedDraftSessionTeardowns`
    // and hang every later test that reuses "pod-X".
    gates.push(initGate.release, disposeGate.release);
    const xHostDraft = useMultiplayerDraftStore.getState().hostDraft(hostConfig("pod-X"));
    await vi.waitFor(() => expect(probe.hostInitializeEntered).toBe(1));

    // leave(false) marks X in `leftHostAdapters` and reaches the (still gated)
    // dispose before X's own initialize has settled.
    const leaving = useMultiplayerDraftStore.getState().leave(false);
    await vi.waitFor(() => expect(probe.hostDisposeEntered).toBe(1));
    await expectParked(leaving);
    await expectParked(xHostDraft);

    // Release X's initialize so it fails; its own `finally` runs the
    // own-failure branch (nulls the adapter refs) while the dispose stays
    // gated, so `xHostDraft` stays parked on the SAME memoized teardown.
    initGate.release();
    probe.hostInitializeGate = null;
    await flush();
    await expectParked(leaving);
    await expectParked(xHostDraft);

    // B calls `hostDraft` reusing the SAME persistence id while X's teardown
    // is still gated, and is given the same 20-tick margin `expectParked`
    // uses to run as far as it can on its own: unfixed, `claimDraftSessionOwner`
    // does not block it, so it reaches "opened" and saves its locator BEFORE
    // X's still-gated teardown clears it out from under B.
    probe.hostRoomCodes.push("XXBBB");
    const hostRoomCallsBeforeB = probe.hostRoomCalls;
    const b = useMultiplayerDraftStore.getState().hostDraft(hostConfig("pod-X"));
    await flush();
    expect(probe.hostRoomCalls).toBe(hostRoomCallsBeforeB);
    disposeGate.release();
    await expect(b).resolves.toEqual({ status: "opened" });
    await expect(xHostDraft).resolves.toEqual({ status: "superseded" });
    await leaving;
    await flush();

    expect(locator()).toBe("pod-X/XXBBB");
    expect((await loadDraftHostSession("pod-X"))?.roomCode).toBe("XXBBB");
    const s = useMultiplayerDraftStore.getState();
    expect({ role: s.role, phase: s.phase, roomCode: s.roomCode }).toEqual({ role: "host", phase: "lobby", roomCode: "XXBBB" });
    expect(fakeHost("XXAAA").dispose).toHaveBeenCalledOnce();
    expect(fakeHost("XXBBB").dispose).not.toHaveBeenCalled();
  });

  it("keeps the saved hosted pod when a guest session it gave way to is left", async () => {
    await openHost("pod-X", "XXAAA");
    await expect(startB("guest")).resolves.toEqual({ status: "opened" });
    // Antecedent: joining preserved X's recovery.
    expect(fakeHost("XXAAA").terminateDraft).not.toHaveBeenCalled();
    expect(locator()).toBe("pod-X/XXAAA");

    await useMultiplayerDraftStore.getState().leave(false);

    expect(locator()).toBe("pod-X/XXAAA");
    expect(await loadDraftHostSession("pod-X")).not.toBeNull();
  });

  it("disposes a left host once when leave(false) is called twice on it", async () => {
    await openHost("pod-X", "XXAAA");
    const first = useMultiplayerDraftStore.getState().leave(false);
    const second = useMultiplayerDraftStore.getState().leave(false);
    await Promise.all([first, second]);

    expect(fakeHost("XXAAA").terminateDraft).toHaveBeenCalledOnce();
    expect(await loadDraftHostSession("pod-X")).toBeNull();
    expect(locator()).toBe("absent");
  });

  it("control: leave(false) with no newer session ends the host's pod", async () => {
    await openHost("pod-X", "XXAAA");
    await useMultiplayerDraftStore.getState().leave(false);
    expect(fakeHost("XXAAA").terminateDraft).toHaveBeenCalledOnce();
    expect(await loadDraftHostSession("pod-X")).toBeNull();
    expect(locator()).toBe("absent");
    const s = useMultiplayerDraftStore.getState();
    expect({ role: s.role, phase: s.phase }).toEqual({ role: null, phase: "idle" });
  });

  it("control: leave(true) keeps the host's pod recoverable", async () => {
    await openHost("pod-X", "XXAAA");
    await useMultiplayerDraftStore.getState().leave(true);
    expect(fakeHost("XXAAA").terminateDraft).not.toHaveBeenCalled();
    expect(await loadDraftHostSession("pod-X")).not.toBeNull();
    expect(locator()).toBe("pod-X/XXAAA");
  });
});
