import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

/**
 * Real `multiplayerDraftStore` and real `DraftPodGuestAdapter`; the PeerJS
 * room join and the `P2PDraftGuest` protocol object are replaced. A gated
 * `joinRoom` ignores its abort signal until released, as
 * `connection.ts::joinRoom` does while it awaits `getPeerConfig()`.
 */
type JoinResult = {
  conn: { peer: string };
  peer: { id: string; destroy: () => void };
  closeConn: () => void;
  destroyPeer: () => void;
};
const net = vi.hoisted(() => ({
  plan: [] as Array<(code: string, signal?: AbortSignal) => Promise<unknown>>,
  joinRoom: vi.fn(),
}));
vi.mock("../../network/connection", async (importOriginal) => ({
  ...(await importOriginal<typeof import("../../network/connection")>()),
  joinRoom: net.joinRoom,
}));

type GuestInstance = {
  emit: (event: unknown) => void;
  dispose: ReturnType<typeof vi.fn>;
  initialize: ReturnType<typeof vi.fn>;
  leave: ReturnType<typeof vi.fn>;
};
const guests = vi.hoisted(() => ({
  instances: [] as GuestInstance[],
  nextInitialize: [] as Array<() => Promise<void>>,
  nextLeave: [] as Array<() => Promise<void>>,
}));
vi.mock("../../adapter/p2p-draft-guest", () => ({
  P2PDraftGuest: vi.fn().mockImplementation(function () {
    let listener: ((event: unknown) => void) | null = null;
    const init = guests.nextInitialize.shift() ?? (async () => {});
    const instance = {
      onEvent: (l: (event: unknown) => void) => {
        listener = l;
        return () => {
          listener = null;
        };
      },
      initialize: vi.fn(init),
      leave: vi.fn(guests.nextLeave.shift() ?? (async () => {})),
      dispose: vi.fn(),
      get isRecoveryRevoked() {
        return false;
      },
      view: null,
      seat: null,
      token: null,
    };
    guests.instances.push({
      emit: (e) => listener?.(e),
      dispose: instance.dispose,
      initialize: instance.initialize,
      leave: instance.leave,
    });
    return instance;
  }),
}));

type HostInstance = { dispose: ReturnType<typeof vi.fn>; emit: (event: unknown) => void };
/** The part of a host's initialize config a recovery's handover arrives through. */
type HostInitializeConfig = { recovery?: { handover?: () => Promise<AbortSignal> } };
const hosts = vi.hoisted(() => ({
  instances: [] as HostInstance[],
  nextInitialize: [] as Array<(config: HostInitializeConfig) => Promise<void>>,
  nextDispose: [] as Array<() => Promise<void>>,
  /** The lifecycle's recovery check answers this. */
  preflight: { type: "admitted" } as { type: "admitted" | "incompatible" },
}));
vi.mock("../../adapter/draftPodHostAdapter", () => ({
  preflightDraftPodHostRecovery: async () => hosts.preflight,
  UnreadableSavedDraftError: class UnreadableSavedDraftError extends Error {},
  DraftPodHostAdapter: vi.fn().mockImplementation(function () {
    const init = hosts.nextInitialize.shift() ?? (async () => {});
    let listener: ((event: unknown) => void) | null = null;
    const instance = {
      onEvent: (l: (event: unknown) => void) => {
        listener = l;
        return () => {
          listener = null;
        };
      },
      initialize: vi.fn(init),
      dispose: vi.fn(hosts.nextDispose.shift() ?? (async () => {})),
      status: "lobby",
      roomCode: "HOSTD",
    };
    hosts.instances.push({ dispose: instance.dispose, emit: (e) => listener?.(e) });
    return instance;
  }),
}));

const persist = vi.hoisted(() => ({
  inspectActiveDraftGuest: vi.fn((): unknown => ({ type: "absent" })),
  loadDraftGuestSession: vi.fn(),
  clearActiveDraftGuestIfCurrent: vi.fn(),
  clearActiveDraftPod: vi.fn(),
}));
vi.mock("../../services/draftPersistence", async (importOriginal) => ({
  ...(await importOriginal<typeof import("../../services/draftPersistence")>()),
  inspectActiveDraftGuest: persist.inspectActiveDraftGuest,
  loadDraftGuestSession: persist.loadDraftGuestSession,
  clearActiveDraftGuestIfCurrent: persist.clearActiveDraftGuestIfCurrent,
  clearActiveDraftPod: persist.clearActiveDraftPod,
}));

import { useMultiplayerDraftStore } from "../multiplayerDraftStore";
import { DraftPodGuestAdapter } from "../../adapter/draftPodGuestAdapter";
import { useConnectivityStore } from "../connectivityStore";
import { inspectActiveDraftPod } from "../../services/draftPersistence";

function joinResult(code: string): JoinResult {
  return {
    conn: { peer: `phase2-${code}` },
    peer: { id: `peer-${code}`, destroy: vi.fn() },
    closeConn: vi.fn(),
    destroyPeer: vi.fn(),
  };
}

/** A room join that settles only when released, and only then looks at its abort signal. */
function gateNextJoin(): { release: () => void } {
  const gate = { release: () => {} };
  net.plan.push((code, signal) => new Promise((resolve, reject) => {
    gate.release = () => {
      if (signal?.aborted) reject(new DOMException("Aborted", "AbortError"));
      else resolve(joinResult(code));
    };
  }));
  return gate;
}

const hostConfig = {
  poolInput: { type: "Set", data: { pools: [{ code: "TST" }], sequence: ["TST"] } },
  kind: "Premier",
  podSize: 8,
  hostDisplayName: "Host",
  tournamentFormat: "Swiss",
  podPolicy: "Competitive",
} as unknown as Parameters<ReturnType<typeof useMultiplayerDraftStore.getState>["hostDraft"]>[0];

type DraftStore = ReturnType<typeof useMultiplayerDraftStore.getState>;

/** A promise the test settles. */
function held<T = void>(): { promise: Promise<T>; resolve: (value: T) => void; reject: (error: Error) => void } {
  let resolve!: (value: T) => void;
  let reject!: (error: Error) => void;
  const promise = new Promise<T>((res, rej) => {
    resolve = res;
    reject = rej;
  });
  return { promise, resolve, reject };
}

/** Records how `promise` settles, so a test can read "still pending" without awaiting it. */
function track(promise: Promise<unknown>): { state: () => "pending" | "resolved" | "rejected"; error: () => unknown } {
  let state: "pending" | "resolved" | "rejected" = "pending";
  let error: unknown;
  promise.then(
    () => {
      state = "resolved";
    },
    (reason: unknown) => {
      state = "rejected";
      error = reason;
    },
  );
  return { state: () => state, error: () => error };
}

/** Lets every pending continuation run. */
async function flush(): Promise<void> {
  for (let turn = 0; turn < 5; turn++) await new Promise((resolve) => setTimeout(resolve, 0));
}

/** A saved host's recovery; the lifecycle's check answers `hosts.preflight`. */
function recoveryOf(persistenceId: string, isCurrent: () => boolean = () => true): Parameters<DraftStore["resumeHostedDraft"]>[0] {
  return {
    config: { ...hostConfig, persistenceId, recovery: { session: { persistenceId } } },
    isCurrent,
  } as unknown as Parameters<DraftStore["resumeHostedDraft"]>[0];
}

const savedSeat = { roomCode: "OLD", displayName: "A", hostPeerId: "phase2-OLD", timestamp: 1 };
const LEAVE_UNACKNOWLEDGED = "Draft host did not acknowledge leave";

describe("session-open outcome of a superseded attempt", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    net.plan = [];
    guests.instances = [];
    guests.nextInitialize = [];
    guests.nextLeave = [];
    hosts.instances = [];
    hosts.nextInitialize = [];
    hosts.nextDispose = [];
    hosts.preflight = { type: "admitted" };
    persist.inspectActiveDraftGuest.mockReturnValue({ type: "absent" });
    net.joinRoom.mockImplementation((code: string, signal?: AbortSignal) => {
      const next = net.plan.shift();
      return next ? next(code, signal) : Promise.resolve(joinResult(code));
    });
  });

  afterEach(async () => {
    useConnectivityStore.setState({ forcedOffline: false, browserOnline: true });
    await useMultiplayerDraftStore.getState().leave(true);
  });

  it("reports its own failure as failed", async () => {
    net.plan.push(async () => {
      throw new Error("Failed to connect: Could not connect to peer OWN");
    });
    const outcome = await useMultiplayerDraftStore.getState().joinDraft({ kind: "new", roomCode: "OWN", displayName: "A" });
    expect(outcome).toEqual({ status: "failed", error: "Failed to connect: Could not connect to peer OWN" });
    expect(useMultiplayerDraftStore.getState()).toMatchObject({ role: "guest", phase: "error", error: "Failed to connect: Could not connect to peer OWN" });
  });

  it.each([
    ["a newer join that failed", "failed"],
    ["a newer join that was kicked", "kicked"],
    ["a newer join that is live", "live"],
  ] as const)("reports superseded, keeping the newer session's role, phase and error, when it settles after %s", async (_label, newerShape) => {
    const stale = gateNextJoin();
    const staleJoin = useMultiplayerDraftStore.getState().joinDraft({ kind: "new", roomCode: "STALE", displayName: "A" });
    try {
      await vi.waitFor(() => expect(net.joinRoom).toHaveBeenCalledTimes(1));

      if (newerShape === "failed") {
        net.plan.push(async () => {
          throw new Error("Failed to connect: Could not connect to peer NEWER");
        });
      }
      await useMultiplayerDraftStore.getState().joinDraft({ kind: "new", roomCode: "NEWER", displayName: "B" });
      if (newerShape === "kicked") guests.instances[0]!.emit({ type: "kicked", reason: "Kicked by host" });
      const before = useMultiplayerDraftStore.getState();
      const snapshot = { role: before.role, phase: before.phase, error: before.error };
      expect(snapshot.phase).toBe(newerShape === "failed" ? "error" : newerShape === "kicked" ? "kicked" : "lobby");

      stale.release();
      const outcome = await staleJoin;
      expect(outcome).toEqual({ status: "superseded" });
      const after = useMultiplayerDraftStore.getState();
      expect({ role: after.role, phase: after.phase, error: after.error }).toEqual(snapshot);
      for (const g of guests.instances) expect(g.dispose).not.toHaveBeenCalled();
    } finally {
      stale.release();
    }
  });

  it("reports superseded when its join rejects on the newer attempt's abort", async () => {
    net.plan.push((_code, signal) => new Promise((_resolve, reject) => {
      signal?.addEventListener("abort", () => reject(new DOMException("Aborted", "AbortError")), { once: true });
    }));
    const staleJoin = useMultiplayerDraftStore.getState().joinDraft({ kind: "new", roomCode: "STALE", displayName: "A" });
    await vi.waitFor(() => expect(net.joinRoom).toHaveBeenCalledTimes(1));
    await useMultiplayerDraftStore.getState().joinDraft({ kind: "new", roomCode: "NEWER", displayName: "B" });
    expect(await staleJoin).toEqual({ status: "superseded" });
  });

  it("reports superseded when the session was left while it was pending", async () => {
    const stale = gateNextJoin();
    const staleJoin = useMultiplayerDraftStore.getState().joinDraft({ kind: "new", roomCode: "STALE", displayName: "A" });
    let outcome: Awaited<typeof staleJoin>;
    try {
      await vi.waitFor(() => expect(net.joinRoom).toHaveBeenCalledTimes(1));
      await useMultiplayerDraftStore.getState().leave();
      stale.release();
      outcome = await staleJoin;
    } finally {
      stale.release();
    }
    expect(outcome).toEqual({ status: "superseded" });
  });

  it.each(["resolves", "rejects"] as const)(
    "host: reports superseded when a newer join takes the session before its initialize %s",
    async (settlement) => {
      let settleHost!: () => void;
      hosts.nextInitialize.push(() => new Promise<void>((resolve, reject) => {
        settleHost = settlement === "resolves" ? resolve : () => reject(new Error("host signaling failed"));
      }));
      const hosting = useMultiplayerDraftStore.getState().hostDraft(hostConfig);
      await vi.waitFor(() => expect(hosts.instances).toHaveLength(1));
      let snapshot: Pick<ReturnType<typeof useMultiplayerDraftStore.getState>, "role" | "phase" | "error">;
      // try/finally: an assertion throwing before `settleHost()` would
      // otherwise leave the host's `initialize()` parked forever.
      try {
        net.plan.push(async () => {
          throw new Error("Failed to connect: Could not connect to peer NEWER");
        });
        await useMultiplayerDraftStore.getState().joinDraft({ kind: "new", roomCode: "NEWER", displayName: "B" });
        const before = useMultiplayerDraftStore.getState();
        snapshot = { role: before.role, phase: before.phase, error: before.error };
      } finally {
        settleHost();
      }
      const outcome = await hosting;
      expect(outcome).toEqual({ status: "superseded" });
      const after = useMultiplayerDraftStore.getState();
      expect({ role: after.role, phase: after.phase, error: after.error }).toEqual(snapshot);
    },
  );

  it("reports superseded when a newer attempt starts while it waits for the previous session's teardown", async () => {
    await useMultiplayerDraftStore.getState().joinDraft({ kind: "new", roomCode: "HELD", displayName: "A" });
    const replacing = useMultiplayerDraftStore.getState().joinDraft({ kind: "new", roomCode: "MIDDLE", displayName: "A" });
    const newest = useMultiplayerDraftStore.getState().joinDraft({ kind: "new", roomCode: "NEWEST", displayName: "A" });
    const outcome = await replacing;
    await newest;
    expect(outcome).toEqual({ status: "superseded" });
  });

  it("host: reports superseded when a newer attempt starts while it waits for the previous session's teardown", async () => {
    await useMultiplayerDraftStore.getState().hostDraft(hostConfig);
    const replacing = useMultiplayerDraftStore.getState().hostDraft(hostConfig);
    const newest = useMultiplayerDraftStore.getState().joinDraft({ kind: "new", roomCode: "NEWEST", displayName: "A" });
    const outcome = await replacing;
    await newest;
    expect(outcome).toEqual({ status: "superseded" });
  });

  it("host: reports its own failure as failed, with the error it recorded", async () => {
    hosts.nextInitialize.push(async () => {
      hosts.instances[0]!.emit({ type: "error", message: "host signaling failed" });
      throw new Error("host signaling failed");
    });
    const outcome = await useMultiplayerDraftStore.getState().hostDraft(hostConfig);
    expect(outcome).toEqual({ status: "failed", error: "host signaling failed" });
  });

  it("host: reports superseded when the session was left while it was pending", async () => {
    let failHost!: (e: Error) => void;
    hosts.nextInitialize.push(() => new Promise<void>((_resolve, reject) => {
      failHost = reject;
    }));
    const hosting = useMultiplayerDraftStore.getState().hostDraft(hostConfig);
    await vi.waitFor(() => expect(hosts.instances).toHaveLength(1));
    // try/finally: an assertion throwing before `failHost()` would otherwise
    // leave the host's `initialize()` parked forever.
    try {
      await useMultiplayerDraftStore.getState().leave(true);
    } finally {
      failHost(new Error("host signaling failed"));
    }
    const outcome = await hosting;
    expect(outcome).toEqual({ status: "superseded" });
  });

  it("in caller-report mode, reports its own failure with its error and keeps none of its state", async () => {
    net.plan.push(async () => {
      throw new Error("Failed to connect: Could not connect to peer OWN");
    });
    const outcome = await useMultiplayerDraftStore
      .getState()
      .joinDraft({ kind: "new", roomCode: "OWN", displayName: "A" }, { failureReport: "caller" });
    expect(outcome).toEqual({ status: "failed", error: "Failed to connect: Could not connect to peer OWN" });
    expect(useMultiplayerDraftStore.getState()).toMatchObject({ role: null, phase: "idle", error: null });
    expect(persist.clearActiveDraftPod).not.toHaveBeenCalled();
  });

  it("in caller-report mode, writes nothing to the store when it is refused offline", async () => {
    useConnectivityStore.setState({ forcedOffline: true });
    const outcome = await useMultiplayerDraftStore
      .getState()
      .joinDraft({ kind: "new", roomCode: "OWN", displayName: "A" }, { failureReport: "caller" });
    expect(outcome).toEqual({ status: "failed", error: "offline.startUnavailable" });
    expect(useMultiplayerDraftStore.getState()).toMatchObject({ role: null, phase: "idle", error: null });
    expect(net.joinRoom).not.toHaveBeenCalled();
  });

  it("in caller-report mode, keeps none of its state when it goes offline during the previous session's teardown", async () => {
    let releaseTeardown!: () => void;
    hosts.nextDispose.push(() => new Promise<void>((resolve) => {
      releaseTeardown = resolve;
    }));
    await expect(useMultiplayerDraftStore.getState().hostDraft(hostConfig)).resolves.toEqual({ status: "opened" });
    const joining = useMultiplayerDraftStore
      .getState()
      .joinDraft({ kind: "new", roomCode: "OWN", displayName: "A" }, { failureReport: "caller" });
    try {
      await vi.waitFor(() => expect(hosts.instances[0]!.dispose).toHaveBeenCalledTimes(1));
      useConnectivityStore.setState({ forcedOffline: true });
      releaseTeardown();
      await expect(joining).resolves.toEqual({ status: "failed", error: "offline.startUnavailable" });
      expect(useMultiplayerDraftStore.getState()).toMatchObject({ role: null, phase: "idle", error: null });
      expect(net.joinRoom).not.toHaveBeenCalled();
    } finally {
      releaseTeardown();
    }
  });

  it("in caller-report mode, leaves a session that started while the failed attempt was disposing its adapter", async () => {
    const realDispose = DraftPodGuestAdapter.prototype.dispose;
    let resumeDispose!: () => void;
    const paused = new Promise<void>((resolve) => {
      resumeDispose = resolve;
    });
    const disposeSpy = vi
      .spyOn(DraftPodGuestAdapter.prototype, "dispose")
      .mockImplementationOnce(async function (this: DraftPodGuestAdapter, options) {
        await paused;
        return realDispose.call(this, options);
      });
    net.plan.push(async () => {
      throw new Error("Failed to connect: Could not connect to peer OWN");
    });
    const failing = useMultiplayerDraftStore
      .getState()
      .joinDraft({ kind: "new", roomCode: "OWN", displayName: "A" }, { failureReport: "caller" });
    try {
      await vi.waitFor(() => expect(disposeSpy).toHaveBeenCalledTimes(1));

      await expect(
        useMultiplayerDraftStore.getState().joinDraft({ kind: "new", roomCode: "NEWER", displayName: "B" }),
      ).resolves.toEqual({ status: "opened" });
      const before = useMultiplayerDraftStore.getState();
      const snapshot = { role: before.role, phase: before.phase, error: before.error };
      expect(snapshot).toEqual({ role: "guest", phase: "lobby", error: null });

      resumeDispose();
      await expect(failing).resolves.toEqual({ status: "superseded" });
      const after = useMultiplayerDraftStore.getState();
      expect({ role: after.role, phase: after.phase, error: after.error }).toEqual(snapshot);
      expect(guests.instances).toHaveLength(1);
      expect(guests.instances[0]!.dispose).not.toHaveBeenCalled();
    } finally {
      resumeDispose();
      disposeSpy.mockRestore();
    }
  });

  it("does not write the offline error over a leave that landed while the failed join was disposing its adapter", async () => {
    const realDispose = DraftPodGuestAdapter.prototype.dispose;
    let resumeDispose!: () => void;
    const paused = new Promise<void>((resolve) => {
      resumeDispose = resolve;
    });
    const disposeSpy = vi
      .spyOn(DraftPodGuestAdapter.prototype, "dispose")
      .mockImplementationOnce(async function (this: DraftPodGuestAdapter, options) {
        await paused;
        return realDispose.call(this, options);
      });
    net.plan.push(async () => {
      throw new Error("Failed to connect: Could not connect to peer OWN");
    });
    const failing = useMultiplayerDraftStore.getState().joinDraft({ kind: "new", roomCode: "OWN", displayName: "A" });
    try {
      await vi.waitFor(() => expect(disposeSpy).toHaveBeenCalledTimes(1));
      await useMultiplayerDraftStore.getState().leave(true);
      useConnectivityStore.setState({ forcedOffline: true });

      resumeDispose();
      await expect(failing).resolves.toEqual({ status: "superseded" });
      expect(useMultiplayerDraftStore.getState()).toMatchObject({ role: null, phase: "idle", error: null });
    } finally {
      resumeDispose();
      disposeSpy.mockRestore();
    }
  });

  it("host: does not write the offline error over a leave that landed while the failed host was disposing its adapter", async () => {
    let resumeDispose!: () => void;
    hosts.nextDispose.push(() => new Promise<void>((resolve) => {
      resumeDispose = resolve;
    }));
    hosts.nextInitialize.push(async () => {
      throw new Error("host signaling failed");
    });
    const hosting = useMultiplayerDraftStore.getState().hostDraft(hostConfig);
    try {
      await vi.waitFor(() => expect(hosts.instances[0]?.dispose).toHaveBeenCalledTimes(1));
      await useMultiplayerDraftStore.getState().leave(true);
      useConnectivityStore.setState({ forcedOffline: true });

      resumeDispose();
      await expect(hosting).resolves.toEqual({ status: "superseded" });
      expect(useMultiplayerDraftStore.getState()).toMatchObject({ role: null, phase: "idle", error: null });
    } finally {
      resumeDispose();
    }
  });

  it("does not tear down a session opened while a leave is still awaiting the host's leave acknowledgement", async () => {
    let releaseLeave!: () => void;
    guests.nextLeave.push(() => new Promise<void>((resolve) => {
      releaseLeave = resolve;
    }));
    await expect(
      useMultiplayerDraftStore.getState().joinDraft({ kind: "new", roomCode: "OWN", displayName: "A" }),
    ).resolves.toEqual({ status: "opened" });

    const leaving = useMultiplayerDraftStore.getState().leave(false);
    try {
      await vi.waitFor(() => expect(guests.instances[0]!.leave).toHaveBeenCalledTimes(1));

      await expect(
        useMultiplayerDraftStore.getState().joinDraft({ kind: "new", roomCode: "NEWER", displayName: "B" }),
      ).resolves.toEqual({ status: "opened" });
      const before = useMultiplayerDraftStore.getState();
      const snapshot = { role: before.role, phase: before.phase, error: before.error };
      expect(snapshot).toEqual({ role: "guest", phase: "lobby", error: null });

      releaseLeave();
      await leaving;

      const after = useMultiplayerDraftStore.getState();
      expect({ role: after.role, phase: after.phase, error: after.error }).toEqual(snapshot);
      expect(guests.instances[1]!.dispose).not.toHaveBeenCalled();

      // A fence placed only right before the final `set` would still let the
      // stale `leave` call `beginDraftLifecycle()`, bumping the generation the
      // newer session's own event listener checks against and silently
      // dropping this event.
      guests.instances[1]!.emit({ type: "kicked", reason: "test kick" });
      expect(useMultiplayerDraftStore.getState().phase).toBe("kicked");
    } finally {
      releaseLeave();
    }
  });

  it("resumeDraft reports failed when its own reconnect fails", async () => {
    const locator = { roomCode: "OLD", displayName: "A", hostPeerId: "phase2-OLD", timestamp: 1 };
    persist.inspectActiveDraftGuest.mockReturnValue({ type: "present", meta: locator, capture: locator });
    persist.loadDraftGuestSession.mockResolvedValue({ draftToken: "tok" });
    guests.nextInitialize.push(async () => {
      throw new Error("handshake refused");
    });
    const outcome = await useMultiplayerDraftStore.getState().resumeDraft();
    expect(outcome).toBe("failed");
    expect(guests.instances).toHaveLength(1);
  });

  it("resumeDraft reports superseded when its reconnect is superseded", async () => {
    const locator = { roomCode: "OLD", displayName: "A", hostPeerId: "phase2-OLD", timestamp: 1 };
    persist.inspectActiveDraftGuest.mockReturnValue({ type: "present", meta: locator, capture: locator });
    persist.loadDraftGuestSession.mockResolvedValue({ draftToken: "tok" });
    const stale = gateNextJoin();
    const resuming = useMultiplayerDraftStore.getState().resumeDraft();
    let outcome: Awaited<typeof resuming>;
    let snapshot: Pick<ReturnType<typeof useMultiplayerDraftStore.getState>, "role" | "phase" | "error">;
    try {
      await vi.waitFor(() => expect(net.joinRoom).toHaveBeenCalledTimes(1));
      await useMultiplayerDraftStore.getState().joinDraft({ kind: "new", roomCode: "NEWER", displayName: "B" });
      useConnectivityStore.setState({ forcedOffline: true });
      const before = useMultiplayerDraftStore.getState();
      snapshot = { role: before.role, phase: before.phase, error: before.error };
      stale.release();
      outcome = await resuming;
    } finally {
      stale.release();
    }
    const after = useMultiplayerDraftStore.getState();
    expect(outcome).toBe("superseded");
    expect({ role: after.role, phase: after.phase, error: after.error }).toEqual(snapshot);
  });

  it("a leave beside an installed host ends a guest resume still reading its seat", async () => {
    await expect(useMultiplayerDraftStore.getState().hostDraft(hostConfig)).resolves.toEqual({ status: "opened" });
    persist.inspectActiveDraftGuest.mockReturnValue({ type: "present", meta: savedSeat, capture: savedSeat });
    const read = held<{ draftToken: string }>();
    persist.loadDraftGuestSession.mockImplementationOnce(() => read.promise);
    const resuming = useMultiplayerDraftStore.getState().resumeDraft();
    // Reach: the resume is reading its seat.
    await vi.waitFor(() => expect(persist.loadDraftGuestSession).toHaveBeenCalledTimes(1));

    await useMultiplayerDraftStore.getState().leave(false);
    read.resolve({ draftToken: "tok" });

    await expect(resuming).resolves.toBe("superseded");
    expect(net.joinRoom).not.toHaveBeenCalled();
    expect(guests.instances).toHaveLength(0);
    expect(useMultiplayerDraftStore.getState()).toMatchObject({ role: null, phase: "idle" });
  });

  describe("a leave whose owner ends or is overtaken while it waits", () => {
    let unhandled: unknown[];
    const onUnhandled = (reason: unknown) => {
      unhandled.push(reason);
    };
    beforeEach(() => {
      unhandled = [];
      process.on("unhandledRejection", onUnhandled);
    });
    afterEach(() => {
      process.off("unhandledRejection", onUnhandled);
    });

    /** Guest A joined, and its leave parked on the host's acknowledgement, which the test rejects later. */
    async function parkGuestLeave(signal?: AbortSignal) {
      const answer = held();
      guests.nextLeave.push(() => answer.promise);
      await expect(useMultiplayerDraftStore.getState().joinDraft({
        kind: "new", roomCode: "OWN", displayName: "A", signal,
      })).resolves.toEqual({ status: "opened" });
      const leaving = useMultiplayerDraftStore.getState().leave(false);
      const leave = track(leaving);
      // Reach: the leave reached the host's acknowledgement.
      await vi.waitFor(() => expect(guests.instances[0]!.leave).toHaveBeenCalledTimes(1));
      return { answer, leave };
    }

    it("a guest leave waiting for its host's answer settles when a recovered host takes over", async () => {
      const { answer, leave } = await parkGuestLeave();
      let handovers = 0;
      hosts.nextInitialize.push(async (config) => {
        handovers++;
        await config.recovery!.handover!();
      });

      await expect(useMultiplayerDraftStore.getState().resumeHostedDraft(recoveryOf("REC-POD"))).resolves.toBe("resumed");
      await flush();
      // Reach: the recovery's handover ran once, on the one host built.
      expect(handovers).toBe(1);
      expect(hosts.instances).toHaveLength(1);
      expect(leave.state()).toBe("resolved");

      answer.reject(new Error(LEAVE_UNACKNOWLEDGED));
      await flush();
      expect(leave.state()).toBe("resolved");
      expect(unhandled).toEqual([]);
      expect(useMultiplayerDraftStore.getState()).toMatchObject({ role: "host", hostPersistenceId: "REC-POD" });
      hosts.instances[0]!.emit({ type: "statusChanged", status: "lobby" });
      expect(useMultiplayerDraftStore.getState()).toMatchObject({ role: "host", phase: "lobby" });
    });

    it("a guest leave waiting for its host's answer settles when a newer join takes over", async () => {
      const { answer, leave } = await parkGuestLeave();

      await expect(useMultiplayerDraftStore.getState().joinDraft({
        kind: "new", roomCode: "NEWER", displayName: "B",
      })).resolves.toEqual({ status: "opened" });
      await flush();
      expect(leave.state()).toBe("resolved");

      answer.reject(new Error(LEAVE_UNACKNOWLEDGED));
      await flush();
      expect(leave.state()).toBe("resolved");
      expect(unhandled).toEqual([]);
      guests.instances[1]!.emit({ type: "kicked", reason: "test kick" });
      expect(useMultiplayerDraftStore.getState()).toMatchObject({ role: "guest", phase: "kicked" });
    });

    it("a leave answered while its owner is still installed rejects as it did", async () => {
      const { answer, leave } = await parkGuestLeave();

      answer.reject(new Error(LEAVE_UNACKNOWLEDGED));
      await flush();
      expect(leave.state()).toBe("rejected");
      expect(String(leave.error())).toContain(LEAVE_UNACKNOWLEDGED);
    });

    it("a guest leave waiting for its host's answer keeps waiting when its route ends, and rejects as it did", async () => {
      const route = new AbortController();
      const { answer, leave } = await parkGuestLeave(route.signal);

      route.abort();
      await flush();
      // Reach: the route's end detached the guest and ended its transport.
      expect(useMultiplayerDraftStore.getState()).toMatchObject({ role: null, phase: "idle" });
      expect(guests.instances[0]!.dispose).toHaveBeenCalledTimes(1);
      expect(leave.state()).toBe("pending");

      answer.reject(new Error(LEAVE_UNACKNOWLEDGED));
      await flush();
      expect(leave.state()).toBe("rejected");
      expect(String(leave.error())).toContain(LEAVE_UNACKNOWLEDGED);
    });
  });

  it("a host leave waiting for its teardown keeps waiting when the recovered host's restore fails", async () => {
    const teardown = held();
    hosts.nextDispose.push(() => teardown.promise);
    const restore = held();
    hosts.nextInitialize.push(async (config) => {
      await config.recovery!.handover!();
      await restore.promise;
      throw new Error("Invalid draft snapshot: restore refused");
    });
    const recovering = useMultiplayerDraftStore.getState().resumeHostedDraft(recoveryOf("R-POD"));
    const recovery = track(recovering);
    // Reach: the handover committed the recovered host.
    await vi.waitFor(() => expect(useMultiplayerDraftStore.getState()).toMatchObject({ role: "host", hostPersistenceId: "R-POD" }));

    const leave = track(useMultiplayerDraftStore.getState().leave(false));
    restore.resolve();
    // Reach: the failed restore ended the committed host, not an uncommitted one.
    await vi.waitFor(() => expect(useMultiplayerDraftStore.getState()).toMatchObject({ role: null, phase: "idle" }));
    await flush();
    expect(hosts.instances).toHaveLength(1);
    expect(hosts.instances[0]!.dispose).toHaveBeenCalledTimes(1);
    expect(leave.state()).toBe("pending");

    teardown.resolve();
    await flush();
    expect(leave.state()).toBe("resolved");
    expect(recovery.state()).toBe("resolved");
    await expect(recovering).resolves.toBe("initializationFailure");
  });

  it.each(["hostDraft", "joinDraft"] as const)(
    "%s: a start superseded by a recovery while it waits for its predecessor's teardown publishes nothing and builds nothing",
    async (start) => {
      const teardownA = held();
      hosts.nextDispose.push(() => teardownA.promise);
      await expect(useMultiplayerDraftStore.getState().hostDraft({ ...hostConfig, persistenceId: "A-POD" }))
        .resolves.toEqual({ status: "opened" });
      const starting = start === "hostDraft"
        ? useMultiplayerDraftStore.getState().hostDraft(hostConfig)
        : useMultiplayerDraftStore.getState().joinDraft({ kind: "new", roomCode: "BJOIN", displayName: "B" });
      // Reach: B retired A and waits for its teardown.
      await vi.waitFor(() => expect(hosts.instances[0]!.dispose).toHaveBeenCalledTimes(1));

      const restoreC = held();
      let handovers = 0;
      let initializing = false;
      hosts.nextInitialize.push(async (config) => {
        initializing = true;
        await restoreC.promise;
        handovers++;
        await config.recovery!.handover!();
      });
      const recovering = useMultiplayerDraftStore.getState().resumeHostedDraft(recoveryOf("C-POD"));
      // Reach: C built its provisional host right after its sweep, before A's release.
      await vi.waitFor(() => expect(hosts.instances).toHaveLength(2));
      await vi.waitFor(() => expect(initializing).toBe(true));

      // C stays held from here on, so every publication recorded belongs to
      // the superseded start.
      const published: Pick<ReturnType<typeof useMultiplayerDraftStore.getState>, "role" | "phase">[] = [];
      const unsubscribe = useMultiplayerDraftStore.subscribe(({ role, phase }) => {
        published.push({ role, phase });
      });
      try {
        teardownA.resolve();
        await expect(starting).resolves.toEqual({ status: "superseded" });
      } finally {
        unsubscribe();
      }
      // Reach: the superseded start's own reset was recorded.
      expect(published).toContainEqual({ role: null, phase: "idle" });
      expect(published.filter(({ phase }) => phase === "connecting")).toEqual([]);
      expect(handovers).toBe(0);
      expect(hosts.instances).toHaveLength(2);
      expect(guests.instances).toHaveLength(0);
      expect(useMultiplayerDraftStore.getState()).toMatchObject({ role: null, phase: "idle" });

      restoreC.resolve();
      await expect(recovering).resolves.toBe("resumed");
      expect(handovers).toBe(1);
      expect(useMultiplayerDraftStore.getState()).toMatchObject({ role: "host", hostPersistenceId: "C-POD" });
    },
  );

  it.each([
    ["a guest resume that finds no seat leaves a waiting host start to open", "no seat"],
    ["a guest resume whose saved seat is invalid leaves a waiting host start to open", "invalid seat"],
    ["a guest resume whose saved seat has expired leaves a waiting host start to open", "expired seat"],
    ["a guest resume whose seat read fails leaves a waiting host start to open", "seat read fails"],
    ["a host recovery no longer current at entry leaves a waiting host start to open", "recovery not current"],
    ["a host recovery whose saved session is refused leaves a waiting host start to open", "recovery refused"],
  ] as const)("%s", async (_title, leg) => {
    const teardownA = held();
    hosts.nextDispose.push(() => teardownA.promise);
    await expect(useMultiplayerDraftStore.getState().hostDraft(hostConfig)).resolves.toEqual({ status: "opened" });
    const starting = useMultiplayerDraftStore.getState().hostDraft(hostConfig);
    // Reach: B retired A and waits for its teardown.
    await vi.waitFor(() => expect(hosts.instances[0]!.dispose).toHaveBeenCalledTimes(1));

    let outcome: string;
    switch (leg) {
      case "no seat":
        persist.inspectActiveDraftGuest.mockReturnValue({ type: "absent" });
        outcome = await useMultiplayerDraftStore.getState().resumeDraft();
        expect(outcome).toBe("absent");
        break;
      case "invalid seat":
        persist.inspectActiveDraftGuest.mockReturnValue({ type: "invalid", capture: null });
        outcome = await useMultiplayerDraftStore.getState().resumeDraft();
        expect(outcome).toBe("invalid");
        break;
      case "expired seat":
        persist.inspectActiveDraftGuest.mockReturnValue({ type: "present", meta: savedSeat, capture: savedSeat });
        persist.loadDraftGuestSession.mockResolvedValueOnce(null);
        outcome = await useMultiplayerDraftStore.getState().resumeDraft();
        expect(outcome).toBe("invalid");
        break;
      case "seat read fails":
        persist.inspectActiveDraftGuest.mockReturnValue({ type: "present", meta: savedSeat, capture: savedSeat });
        persist.loadDraftGuestSession.mockRejectedValueOnce(new Error("storage unavailable"));
        outcome = await useMultiplayerDraftStore.getState().resumeDraft();
        expect(outcome).toBe("failed");
        break;
      case "recovery not current":
        outcome = await useMultiplayerDraftStore.getState().resumeHostedDraft(recoveryOf("R-POD", () => false));
        expect(outcome).toBe("superseded");
        break;
      case "recovery refused":
        hosts.preflight = { type: "incompatible" };
        outcome = await useMultiplayerDraftStore.getState().resumeHostedDraft(recoveryOf("R-POD"));
        expect(outcome).toBe("incompatible");
        break;
    }
    // Reach: each resume leg read the saved pointer, and the expired-seat and
    // read-fails legs read the seat itself.
    if (leg !== "recovery not current" && leg !== "recovery refused") {
      expect(persist.inspectActiveDraftGuest).toHaveBeenCalled();
    }
    expect(persist.loadDraftGuestSession).toHaveBeenCalledTimes(
      leg === "expired seat" || leg === "seat read fails" ? 1 : 0,
    );

    teardownA.resolve();
    await expect(starting).resolves.toEqual({ status: "opened" });
    expect(hosts.instances).toHaveLength(2);
    expect(useMultiplayerDraftStore.getState().role).toBe("host");
  });

  it("a host recovery that finds its host already installed leaves a guest resume reading its seat to resume", async () => {
    await expect(useMultiplayerDraftStore.getState().hostDraft({ ...hostConfig, persistenceId: "P-POD" }))
      .resolves.toEqual({ status: "opened" });
    persist.inspectActiveDraftGuest.mockReturnValue({ type: "present", meta: savedSeat, capture: savedSeat });
    const read = held<{ draftToken: string }>();
    persist.loadDraftGuestSession.mockImplementationOnce(() => read.promise);
    const resuming = useMultiplayerDraftStore.getState().resumeDraft();
    // Reach: the resume is reading its seat.
    await vi.waitFor(() => expect(persist.loadDraftGuestSession).toHaveBeenCalledTimes(1));

    await expect(useMultiplayerDraftStore.getState().resumeHostedDraft(recoveryOf("P-POD"))).resolves.toBe("resumed");
    // Reach: the recovery built no host, so its outcome came from the already-installed return.
    expect(hosts.instances).toHaveLength(1);

    read.resolve({ draftToken: "tok" });
    await expect(resuming).resolves.toBe("resumed");
    expect(guests.instances).toHaveLength(1);
    expect(useMultiplayerDraftStore.getState()).toMatchObject({ role: "guest", phase: "lobby" });
  });

  it.each(["hostDraft", "joinDraft", "hostDraft while offline", "joinDraft while offline"] as const)(
    "an already-cancelled %s changes nothing",
    async (call) => {
      await expect(useMultiplayerDraftStore.getState().joinDraft({ kind: "new", roomCode: "OWN", displayName: "A" }))
        .resolves.toEqual({ status: "opened" });
      // Reach: a guest is seated before the cancelled call.
      expect(useMultiplayerDraftStore.getState()).toMatchObject({ role: "guest", transport: "p2p", phase: "lobby" });
      if (call.endsWith("while offline")) useConnectivityStore.setState({ forcedOffline: true });
      const published = () => {
        const state = useMultiplayerDraftStore.getState();
        return { role: state.role, transport: state.transport, phase: state.phase, error: state.error };
      };
      const before = published();
      const cancelled = new AbortController();
      cancelled.abort();

      const outcome = call.startsWith("hostDraft")
        ? await useMultiplayerDraftStore.getState()
          .hostDraft({ ...hostConfig, persistenceId: "K-POD", signal: cancelled.signal })
        : await useMultiplayerDraftStore.getState()
          .joinDraft({ kind: "new", roomCode: "NEXT", displayName: "B", signal: cancelled.signal });
      expect(outcome).toEqual({ status: "superseded" });
      expect(guests.instances[0]!.dispose).not.toHaveBeenCalled();
      expect(hosts.instances).toHaveLength(0);
      expect(guests.instances).toHaveLength(1);
      expect(published()).toEqual(before);
    },
  );

  it("a reset during a host start's initialization ends it before it saves its pod", async () => {
    localStorage.clear();
    const initialize = held();
    let initializing = false;
    hosts.nextInitialize.push(async () => {
      initializing = true;
      await initialize.promise;
    });
    const hosting = useMultiplayerDraftStore.getState().hostDraft({ ...hostConfig, persistenceId: "R-POD" });
    try {
      // Reach: the host's initialize was entered while it is the published host.
      await vi.waitFor(() => expect(initializing).toBe(true));
      expect(useMultiplayerDraftStore.getState()).toMatchObject({ role: "host", phase: "connecting" });

      useMultiplayerDraftStore.getState().reset();
    } finally {
      initialize.resolve();
    }
    await expect(hosting).resolves.toEqual({ status: "superseded" });
    expect(inspectActiveDraftPod()).toEqual({ type: "absent" });
    expect(hosts.instances[0]!.dispose).toHaveBeenCalledTimes(1);
    expect(useMultiplayerDraftStore.getState()).toMatchObject({ role: null, phase: "idle" });
  });

  it("a reset during a guest join's room join ends it", async () => {
    const room = gateNextJoin();
    const joining = useMultiplayerDraftStore.getState().joinDraft({ kind: "new", roomCode: "RESET", displayName: "A" });
    try {
      // Reach: the room join was entered while it is the published guest.
      await vi.waitFor(() => expect(net.joinRoom).toHaveBeenCalledTimes(1));
      expect(useMultiplayerDraftStore.getState()).toMatchObject({ role: "guest", phase: "connecting" });

      useMultiplayerDraftStore.getState().reset();
    } finally {
      room.release();
    }
    await expect(joining).resolves.toEqual({ status: "superseded" });
    expect(guests.instances).toHaveLength(1);
    expect(guests.instances[0]!.dispose).toHaveBeenCalled();
    expect(useMultiplayerDraftStore.getState()).toMatchObject({ role: null, phase: "idle" });
  });

  it.each(["hostDraft", "joinDraft"] as const)(
    "a reset while a %s waits for its predecessor's teardown ends it before it installs anything",
    async (start) => {
      const teardownA = held();
      const teardown = track(teardownA.promise);
      hosts.nextDispose.push(() => teardownA.promise);
      await expect(useMultiplayerDraftStore.getState().hostDraft({ ...hostConfig, persistenceId: "A-POD" }))
        .resolves.toEqual({ status: "opened" });
      localStorage.clear();
      const starting = start === "hostDraft"
        ? useMultiplayerDraftStore.getState().hostDraft({ ...hostConfig, persistenceId: "B-POD" })
        : useMultiplayerDraftStore.getState().joinDraft({ kind: "new", roomCode: "BJOIN", displayName: "B" });
      const opening = track(starting);
      try {
        // Reach: B retired A and waits for its teardown.
        await vi.waitFor(() => expect(hosts.instances[0]!.dispose).toHaveBeenCalledTimes(1));

        useMultiplayerDraftStore.getState().reset();
        await flush();
        // Reach: the reset landed while B still waits for A's teardown.
        expect(teardown.state()).toBe("pending");
        expect(opening.state()).toBe("pending");
      } finally {
        teardownA.resolve();
      }

      await expect(starting).resolves.toEqual({ status: "superseded" });
      expect(hosts.instances).toHaveLength(1);
      expect(guests.instances).toHaveLength(0);
      expect(net.joinRoom).not.toHaveBeenCalled();
      expect(inspectActiveDraftPod()).toEqual({ type: "absent" });
      expect(useMultiplayerDraftStore.getState()).toMatchObject({ role: null, transport: null, phase: "idle" });
    },
  );

  it.each(["succeeds", "fails"] as const)(
    "a reset while a recovered host restores its saved session ends the recovery when that restore %s",
    async (restoreOutcome) => {
      const restore = held();
      let committed = false;
      hosts.nextInitialize.push(async (config) => {
        await config.recovery!.handover!();
        committed = true;
        await restore.promise;
        if (restoreOutcome === "fails") throw new Error("Invalid draft snapshot: restore refused");
      });
      const recovering = useMultiplayerDraftStore.getState().resumeHostedDraft(recoveryOf("R-POD"));
      const recovery = track(recovering);
      let published: ReturnType<typeof useMultiplayerDraftStore.getState>;
      try {
        // Reach: the handover installed the recovered host, which is still restoring.
        await vi.waitFor(() => expect(committed).toBe(true));
        expect(useMultiplayerDraftStore.getState()).toMatchObject({ role: "host", hostPersistenceId: "R-POD" });

        useMultiplayerDraftStore.getState().reset();
        await flush();
        expect(recovery.state()).toBe("pending");
        published = useMultiplayerDraftStore.getState();
        // The reset's own generation, which the initial state's would not match.
        expect(published.interactionGeneration).toBeGreaterThan(0);
      } finally {
        restore.resolve();
      }

      await expect(recovering).resolves.toBe("superseded");
      expect(hosts.instances).toHaveLength(1);
      if (restoreOutcome === "fails") expect(hosts.instances[0]!.dispose).toHaveBeenCalledTimes(1);
      // Nothing is written over the state the reset published.
      expect(useMultiplayerDraftStore.getState()).toBe(published);
      expect(useMultiplayerDraftStore.getState()).toMatchObject({ role: null, phase: "idle" });
    },
  );
});
