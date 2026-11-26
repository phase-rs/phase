import "fake-indexeddb/auto";
import { clear as idbClear, del as idbDel, get as idbGet, set as idbSet } from "idb-keyval";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

/*
 * A server-hosted draft seat on the real lifecycle store. The native adapter
 * is doubled at the module boundary; every answer it gives is stated on
 * `NativeDouble` below. The P2P guest is the real `DraftPodGuestAdapter` over
 * a doubled `P2PDraftGuest` and room join, and a host is a doubled
 * `DraftPodHostAdapter`. Storage is real on fake-indexeddb; four native
 * storage functions are wrapped only to log their order and to hold or fail
 * one call.
 */

type NativeCredential = { draftCode: string; playerToken: string; seatIndex: number };
type NativeListener = (event: unknown) => void;
type Settle = { resolve: (value?: unknown) => void; reject: (error: Error) => void };
type NativeScript = (double: NativeDoubleShape, settle: Settle, args: unknown[]) => void;
interface NativeDoubleShape {
  readonly index: number;
  readonly url: string;
  seat: NativeCredential | null;
  disposeCount: number;
  readonly joinCalls: string[];
  readonly reconnectCalls: NativeCredential[];
  emit: (event: unknown) => void;
}

const native = vi.hoisted(() => {
  const state = {
    instances: [] as NativeDoubleShape[],
    /** One-shot scripts; an empty queue takes the default answer. */
    nextCreate: [] as NativeScript[],
    nextJoin: [] as NativeScript[],
    nextReconnect: [] as NativeScript[],
    /** Every answer and dispose, in order, with the double's index. */
    log: [] as string[],
    /** The view a join answers and the view a reconnect acknowledges; set by the test file. */
    lobbyView: null as unknown,
    draftingView: null as unknown,
  };

  /**
   * The constructor records the URL; `onEvent` adds a listener and returns its
   * removal; `credential` returns the seat it holds; `dispose()` counts a
   * call, rejects any held operation with "Adapter disposed before draft
   * started" and drops its listeners. Default `createDraft`: holds seat 0 of
   * NAT001, emits `waitingForPlayers` and resolves, in one synchronous step.
   * Default `joinDraft(code)`: holds seat 3 of `code` and resolves with a lobby
   * view, emitting nothing. Default `reconnectDraft(credential)`: holds that
   * credential, emits `draftViewUpdated` with a drafting view (pick 7) and
   * resolves.
   */
  class NativeDouble implements NativeDoubleShape {
    readonly index: number;
    seat: NativeCredential | null = null;
    disposeCount = 0;
    readonly joinCalls: string[] = [];
    readonly reconnectCalls: NativeCredential[] = [];
    private listeners = new Set<NativeListener>();
    private pending = new Set<Settle>();

    constructor(readonly url: string) {
      this.index = state.instances.length;
      state.instances.push(this);
    }

    get credential(): NativeCredential | null {
      return this.seat;
    }

    onEvent(listener: NativeListener): () => void {
      this.listeners.add(listener);
      return () => {
        this.listeners.delete(listener);
      };
    }

    emit(event: unknown): void {
      for (const listener of [...this.listeners]) listener(event);
    }

    private run(kind: string, script: NativeScript, args: unknown[]): Promise<unknown> {
      return new Promise((resolve, reject) => {
        const settle: Settle = {
          resolve: (value) => {
            if (!this.pending.delete(settle)) return;
            state.log.push(`${kind}:resolved:${this.index}`);
            resolve(value);
          },
          reject: (error) => {
            if (!this.pending.delete(settle)) return;
            state.log.push(`${kind}:rejected:${this.index}`);
            reject(error);
          },
        };
        this.pending.add(settle);
        script(this, settle, args);
      });
    }

    createDraft(settings: unknown): Promise<unknown> {
      return this.run("create", state.nextCreate.shift() ?? NativeDouble.answerCreate, [settings]);
    }

    joinDraft(code: string, displayName: string, password?: string): Promise<unknown> {
      this.joinCalls.push(code);
      return this.run("join", state.nextJoin.shift() ?? NativeDouble.answerJoin, [code, displayName, password]);
    }

    reconnectDraft(credential: NativeCredential): Promise<unknown> {
      this.reconnectCalls.push(credential);
      return this.run("reconnect", state.nextReconnect.shift() ?? NativeDouble.answerReconnect, [credential]);
    }

    dispose(): void {
      this.disposeCount += 1;
      state.log.push(`dispose:${this.index}`);
      const error = new Error("Adapter disposed before draft started");
      for (const settle of [...this.pending]) settle.reject(error);
      this.listeners.clear();
    }

    static answerCreate: NativeScript = (double, settle) => {
      double.seat = { draftCode: "NAT001", playerToken: "tok-host", seatIndex: 0 };
      double.emit({ type: "waitingForPlayers" });
      settle.resolve();
    };

    static answerJoin: NativeScript = (double, settle, args) => {
      double.seat = { draftCode: args[0] as string, playerToken: "tok-guest", seatIndex: 3 };
      settle.resolve(state.lobbyView);
    };

    static answerReconnect: NativeScript = (double, settle, args) => {
      double.seat = args[0] as NativeCredential;
      double.emit({ type: "draftViewUpdated", view: state.draftingView });
      settle.resolve();
    };
  }

  return { ...state, state, NativeDouble };
});

vi.mock("../../adapter/server-draft-adapter", async (importOriginal) => ({
  ...(await importOriginal<typeof import("../../adapter/server-draft-adapter")>()),
  ServerDraftAdapter: native.NativeDouble,
}));

const storage = vi.hoisted(() => ({
  log: [] as string[],
  /** One-shot gates and failures for the wrapped functions. */
  saveGate: null as Promise<void> | null,
  failSave: false,
  loadGate: null as Promise<void> | null,
  failLoad: false,
  clearGate: null as Promise<void> | null,
  /** The capability name of the last save that entered. */
  lastSavedId: null as string | null,
}));

vi.mock("../../services/draftPersistence", async (importOriginal) => {
  const actual = await importOriginal<typeof import("../../services/draftPersistence")>();
  return {
    ...actual,
    saveNativeDraftCapability: async (...args: Parameters<typeof actual.saveNativeDraftCapability>) => {
      storage.log.push("privateSave:enter");
      storage.lastSavedId = args[0].capabilityId;
      const gate = storage.saveGate;
      storage.saveGate = null;
      if (gate) await gate;
      if (storage.failSave) {
        storage.failSave = false;
        throw new Error("IndexedDB write failed");
      }
      const pointer = await actual.saveNativeDraftCapability(...args);
      storage.log.push("privateSave:committed");
      return pointer;
    },
    publishActiveNativeDraft: (...args: Parameters<typeof actual.publishActiveNativeDraft>) => {
      storage.log.push(`pointerWrite:${args[0].capabilityId}`);
      return actual.publishActiveNativeDraft(...args);
    },
    clearNativeDraftCapability: async (...args: Parameters<typeof actual.clearNativeDraftCapability>) => {
      storage.log.push("clear:enter");
      const gate = storage.clearGate;
      storage.clearGate = null;
      if (gate) await gate;
      await actual.clearNativeDraftCapability(...args);
      storage.log.push("clear:done");
    },
    loadNativeDraftCapability: async (...args: Parameters<typeof actual.loadNativeDraftCapability>) => {
      storage.log.push("load:enter");
      const gate = storage.loadGate;
      storage.loadGate = null;
      if (gate) await gate;
      if (storage.failLoad) {
        storage.failLoad = false;
        return { type: "readFailure", error: new Error("IndexedDB read failed") } as const;
      }
      return actual.loadNativeDraftCapability(...args);
    },
  };
});

// The P2P guest and host doubles of `multiplayerDraftStore.sessionOpen.test.ts`.
type JoinResult = {
  conn: { peer: string };
  peer: { id: string; destroy: () => void };
  closeConn: () => void;
  destroyPeer: () => void;
};
const net = vi.hoisted(() => ({ joinRoom: vi.fn() }));
vi.mock("../../network/connection", async (importOriginal) => ({
  ...(await importOriginal<typeof import("../../network/connection")>()),
  joinRoom: net.joinRoom,
}));

type GuestInstance = {
  emit: (event: unknown) => void;
  dispose: ReturnType<typeof vi.fn>;
  leave: ReturnType<typeof vi.fn>;
};
const guests = vi.hoisted(() => ({
  instances: [] as GuestInstance[],
  nextLeave: [] as Array<() => Promise<void>>,
}));
vi.mock("../../adapter/p2p-draft-guest", () => ({
  P2PDraftGuest: vi.fn().mockImplementation(function () {
    let listener: ((event: unknown) => void) | null = null;
    const instance = {
      onEvent: (l: (event: unknown) => void) => {
        listener = l;
        return () => {
          listener = null;
        };
      },
      initialize: vi.fn(async () => {}),
      leave: vi.fn(guests.nextLeave.shift() ?? (async () => {})),
      dispose: vi.fn(),
      get isRecoveryRevoked() {
        return false;
      },
      view: null,
      seat: null,
      token: null,
    };
    guests.instances.push({ emit: (e) => listener?.(e), dispose: instance.dispose, leave: instance.leave });
    return instance;
  }),
}));

type HostInitializeConfig = { recovery?: { handover?: () => Promise<AbortSignal> } };
const hosts = vi.hoisted(() => ({
  instances: [] as Array<{ dispose: ReturnType<typeof vi.fn> }>,
  nextInitialize: [] as Array<(config: HostInitializeConfig) => Promise<void>>,
  nextDispose: [] as Array<() => Promise<void>>,
}));
vi.mock("../../adapter/draftPodHostAdapter", () => ({
  preflightDraftPodHostRecovery: async () => ({ type: "admitted" }),
  UnreadableSavedDraftError: class UnreadableSavedDraftError extends Error {},
  DraftPodHostAdapter: vi.fn().mockImplementation(function () {
    const init = hosts.nextInitialize.shift() ?? (async () => {});
    const instance = {
      onEvent: () => () => {},
      initialize: vi.fn(init),
      dispose: vi.fn(hosts.nextDispose.shift() ?? (async () => {})),
      status: "lobby",
      roomCode: "HOSTD",
    };
    hosts.instances.push({ dispose: instance.dispose });
    return instance;
  }),
}));

import { currentDraftOwnerGeneration, useMultiplayerDraftStore } from "../multiplayerDraftStore";
import { useMultiplayerStore } from "../multiplayerStore";
import { useConnectivityStore } from "../connectivityStore";
import { Draft27IncompatibilityError } from "../../adapter/draft-compatibility";
import {
  getDraftStore,
  publishActiveNativeDraft,
  saveNativeDraftCapability,
} from "../../services/draftPersistence";
import type { CreateDraftSettings } from "../../adapter/server-draft-adapter";
import type { DraftPlayerView } from "../../adapter/draft-adapter";

const POINTER_KEY = "phase-active-draft-guest";
const ENDPOINT = "wss://native.example/ws";
const nativeKey = (capabilityId: string) => `phase-native-draft-capability:${capabilityId}`;

function view(fields: Record<string, unknown>): DraftPlayerView {
  return {
    status: "Lobby",
    kind: "Premier",
    launch_capability: "None",
    distribution: "PickAndPass",
    commanders_required: 0,
    current_pack_number: 0,
    pick_number: 0,
    pass_direction: "Left",
    current_pack: null,
    required_pick_count: 1,
    pick_selection_mode: "Direct",
    pool: [],
    draft_effects: [],
    pool_groups: { creatures: [], non_creatures: [], lands: [] },
    seats: [],
    cards_per_pack: 14,
    pick_steps_per_pack: 14,
    pack_count: 3,
    min_deck_size: 40,
    addable_cards: [],
    timer_remaining_ms: null,
    standings: [],
    current_round: 0,
    next_pairing_round: 1,
    tournament_format: "Swiss",
    pod_policy: "Competitive",
    pairings: [],
    match_config: { match_type: "Bo1" },
    ...fields,
  } as unknown as DraftPlayerView;
}
native.state.lobbyView = view({ status: "Lobby" });
native.state.draftingView = view({ status: "Drafting", pick_number: 7 });

const createSettings: CreateDraftSettings = {
  displayName: "Host",
  setCodes: ["TST"],
  kind: "Premier",
  public: true,
  tournamentFormat: "Swiss",
  podPolicy: "Competitive",
  podSize: 8,
};

const hostConfig = {
  poolInput: { type: "Set", data: { pools: [{ code: "TST" }], sequence: ["TST"] } },
  kind: "Premier",
  podSize: 8,
  hostDisplayName: "Host",
  tournamentFormat: "Swiss",
  podPolicy: "Competitive",
} as unknown as Parameters<ReturnType<typeof useMultiplayerDraftStore.getState>["hostDraft"]>[0];

type DraftStore = ReturnType<typeof useMultiplayerDraftStore.getState>;

function recoveryOf(persistenceId: string): Parameters<DraftStore["resumeHostedDraft"]>[0] {
  return {
    config: { ...hostConfig, persistenceId, recovery: { session: { persistenceId } } },
    isCurrent: () => true,
  } as unknown as Parameters<DraftStore["resumeHostedDraft"]>[0];
}

const store = () => useMultiplayerDraftStore.getState();

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
function track(promise: Promise<unknown>): { state: () => "pending" | "resolved" | "rejected" } {
  let state: "pending" | "resolved" | "rejected" = "pending";
  promise.then(
    () => {
      state = "resolved";
    },
    () => {
      state = "rejected";
    },
  );
  return { state: () => state };
}

/** Lets every pending continuation run. */
async function flush(): Promise<void> {
  for (let turn = 0; turn < 5; turn++) await new Promise((resolve) => setTimeout(resolve, 0));
}

/** The next reconnect waits until the test releases (it resolves as the default does) or fails it. */
function holdNextReconnect(): { entered: () => boolean; release: () => void } {
  let pending: { double: NativeDoubleShape; settle: Settle; args: unknown[] } | null = null;
  native.nextReconnect.push((double, settle, args) => {
    pending = { double, settle, args };
  });
  return {
    entered: () => pending !== null,
    release: () => {
      if (!pending) throw new Error("the held reconnect was never entered");
      const { double, settle, args } = pending;
      native.NativeDouble.answerReconnect(double, settle, args);
    },
  };
}

/** A committed record and its published pointer, as a committed native join leaves them. */
async function storeNativePair(): Promise<{ capabilityId: string; pointerText: string }> {
  const pointer = await saveNativeDraftCapability({
    capabilityId: "cap-N",
    endpoint: ENDPOINT,
    draftCode: "NAT001",
    playerToken: "tok-guest",
    seatIndex: 3,
    participant: "guest",
  });
  publishActiveNativeDraft(pointer);
  storage.log.length = 0;
  return { capabilityId: pointer.capabilityId, pointerText: localStorage.getItem(POINTER_KEY)! };
}

/** Moves the stored pair's timestamps `ms` into the past. */
async function ageNativePair(capabilityId: string, ms: number): Promise<void> {
  const record = await storedRecord(capabilityId) as Record<string, unknown>;
  await idbSet(nativeKey(capabilityId), { ...record, timestamp: (record.timestamp as number) - ms }, getDraftStore());
  const pointer = JSON.parse(localStorage.getItem(POINTER_KEY)!) as Record<string, unknown>;
  localStorage.setItem(POINTER_KEY, JSON.stringify({ ...pointer, timestamp: (pointer.timestamp as number) - ms }));
}

function storedRecord(capabilityId: string): Promise<unknown> {
  return idbGet(nativeKey(capabilityId), getDraftStore());
}

function storedPointer(): Record<string, unknown> | null {
  const raw = localStorage.getItem(POINTER_KEY);
  return raw === null ? null : JSON.parse(raw) as Record<string, unknown>;
}

function snapshot() {
  const s = store();
  return {
    role: s.role,
    transport: s.transport,
    phase: s.phase,
    error: s.error,
    guestRecoveryFailure: s.guestRecoveryFailure,
    view: s.view,
  };
}

/** A P2P guest seated through the ordinary join. */
async function joinP2PGuest(): Promise<void> {
  await expect(store().joinDraft({ kind: "new", roomCode: "OWN", displayName: "A" }))
    .resolves.toEqual({ status: "opened" });
  expect(store()).toMatchObject({ role: "guest", transport: "p2p", phase: "lobby" });
}

/** A committed native join, published with its pointer. */
async function joinNative(signal?: AbortSignal): Promise<string> {
  await expect(store().joinNativeDraft({ endpoint: ENDPOINT, draftCode: "NAT001", displayName: "Bo", signal }))
    .resolves.toEqual({ status: "opened" });
  const capabilityId = storedPointer()?.capabilityId as string;
  expect(capabilityId).toEqual(expect.any(String));
  return capabilityId;
}

/** Every double, script, gate and store back to its starting state. */
async function resetHarness(): Promise<void> {
  vi.clearAllMocks();
  localStorage.clear();
  await idbClear(getDraftStore());
  native.state.instances.length = 0;
  native.state.nextCreate.length = 0;
  native.state.nextJoin.length = 0;
  native.state.nextReconnect.length = 0;
  native.state.log.length = 0;
  storage.log.length = 0;
  storage.saveGate = null;
  storage.failSave = false;
  storage.loadGate = null;
  storage.failLoad = false;
  storage.clearGate = null;
  storage.lastSavedId = null;
  guests.instances = [];
  guests.nextLeave = [];
  hosts.instances = [];
  hosts.nextInitialize = [];
  hosts.nextDispose = [];
  net.joinRoom.mockImplementation(async (code: string): Promise<JoinResult> => ({
    conn: { peer: `phase2-${code}` },
    peer: { id: `peer-${code}`, destroy: vi.fn() },
    closeConn: vi.fn(),
    destroyPeer: vi.fn(),
  }));
}

/** Ends whatever a test left installed. */
async function endHarness(): Promise<void> {
  useConnectivityStore.setState({ forcedOffline: false, browserOnline: true });
  await store().leave(true);
}

describe("the native draft owner", () => {
  beforeEach(resetHarness);
  afterEach(endHarness);

  it("a native create commits its capability, then its owner publishes the pointer", async () => {
    const outcome = await store().createNativeDraft({ endpoint: " wss://NATIVE.example/ws ", settings: createSettings });
    storage.log.push(`create:${outcome.status}`);

    // Reach: one adapter, constructed on the canonical endpoint.
    expect(native.instances).toHaveLength(1);
    expect(native.instances[0]!.url).toBe(ENDPOINT);
    const capabilityId = storage.lastSavedId!;
    expect(storage.log).toEqual([
      "privateSave:enter", "privateSave:committed", `pointerWrite:${capabilityId}`, "create:opened",
    ]);
    expect(store()).toMatchObject({ role: "host", transport: "native", phase: "lobby", draftCode: "NAT001", seatIndex: 0 });
    expect(storedPointer()).toMatchObject({ transport: "native", capabilityId, participant: "host" });

    // Hostile: an owner ended while its private write is held.
    await store().leave(true);
    storage.log.length = 0;
    localStorage.clear();
    const write = held();
    storage.saveGate = write.promise;
    const creating = store().createNativeDraft({ endpoint: ENDPOINT, settings: createSettings });
    await vi.waitFor(() => expect(storage.log).toContain("privateSave:enter"));
    const endedId = storage.lastSavedId!;
    await store().leave(true);
    write.resolve();
    await expect(creating).resolves.toEqual({ status: "superseded" });
    expect(storage.log.filter((entry) => entry.startsWith("pointerWrite:"))).toEqual([]);
    expect(storedPointer()).toBeNull();
    expect(storage.log).toContain("privateSave:committed");
    await expect(storedRecord(endedId)).resolves.toBeUndefined();
  });

  it("a native join installs its admitted view", async () => {
    const capabilityId = await joinNative();

    // Reach: the join was asked once, with the code.
    expect(native.instances).toHaveLength(1);
    expect(native.instances[0]!.joinCalls).toEqual(["NAT001"]);
    expect(store().view?.status).toBe("Lobby");
    expect(store()).toMatchObject({ role: "guest", transport: "native", phase: "lobby", draftCode: "NAT001", seatIndex: 3 });
    expect(storedPointer()).toMatchObject({ transport: "native", capabilityId, participant: "guest" });

    // Hostile: the join's view is refused, as the real adapter refuses one its join's `Joined` result carries.
    storage.log.length = 0;
    const failure = {
      kind: "incompatible" as const,
      code: "fifo_flow" as const,
      message: "This draft uses FIFO delivery, which this connection cannot accept.",
    };
    native.nextJoin.push((double, settle) => {
      double.emit({ type: "incompatible", failure });
      settle.reject(new Draft27IncompatibilityError(failure));
    });
    const refused = await store().joinNativeDraft({ endpoint: ENDPOINT, draftCode: "FIFO01", displayName: "Bo" });
    expect(refused).toMatchObject({ status: "failed", code: "fifo_flow" });
    expect(store().guestRecoveryFailure?.kind).toBe("incompatible");
    expect(store()).toMatchObject({ role: null, transport: null, phase: "idle" });
    expect(storage.log).not.toContain("privateSave:enter");
  });

  it("a P2P resume leaves a native pointer as it is", async () => {
    const { pointerText } = await storeNativePair();
    // Reach: the native pointer is stored before the call.
    expect(localStorage.getItem(POINTER_KEY)).toBe(pointerText);

    await expect(store().resumeDraft()).resolves.toBe("absent");
    expect(localStorage.getItem(POINTER_KEY)).toBe(pointerText);
  });

  it("a native resume keeps the held guest until the server answers", async () => {
    await joinP2PGuest();
    const { capabilityId } = await storeNativePair();
    await ageNativePair(capabilityId, 60_000);
    const recordBefore = await storedRecord(capabilityId) as { timestamp: number };
    const pointerBefore = storedPointer()!;
    const reconnect = holdNextReconnect();

    const resuming = store().resumeNativeDraft();
    await vi.waitFor(() => expect(reconnect.entered()).toBe(true));
    // Reach: the reconnect carries the stored credential.
    expect(native.instances[0]!.reconnectCalls).toEqual([{ draftCode: "NAT001", playerToken: "tok-guest", seatIndex: 3 }]);
    expect(store()).toMatchObject({ role: "guest", transport: "p2p" });
    expect(guests.instances[0]!.dispose).not.toHaveBeenCalled();

    reconnect.release();
    await expect(resuming).resolves.toEqual({ status: "resumed" });
    expect(store()).toMatchObject({ role: "guest", transport: "native" });
    expect(guests.instances[0]!.dispose).toHaveBeenCalled();
    const pointerAfter = storedPointer()!;
    expect(pointerAfter.capabilityId).toBe(capabilityId);
    expect(pointerAfter.timestamp as number).toBeGreaterThan(pointerBefore.timestamp as number);
    const recordAfter = await storedRecord(capabilityId) as { timestamp: number };
    expect(recordAfter.timestamp).toBeGreaterThan(recordBefore.timestamp);
  });

  it("a native resume shows the view its acknowledgement carried", async () => {
    await storeNativePair();
    // Reach: the resume committed.
    await expect(store().resumeNativeDraft()).resolves.toEqual({ status: "resumed" });
    expect(store().view?.pick_number).toBe(7);
  });

  it.each(["leave(false)", "leave(true)", "the route's end", "a leave(false) overtaken"] as const)(
    "leave(false) clears the native capability; leave(true) and the route's end keep it: %s",
    async (ending) => {
      const route = new AbortController();
      const capabilityId = await joinNative(route.signal);
      // Reach: the record and its pointer are there before the ending.
      await expect(storedRecord(capabilityId)).resolves.toMatchObject({ capabilityId });
      expect(storedPointer()).toMatchObject({ capabilityId });
      const adapter = native.instances[0]!;

      switch (ending) {
        case "leave(false)":
          await store().leave(false);
          break;
        case "leave(true)":
          await store().leave(true);
          break;
        case "the route's end":
          route.abort();
          await flush();
          break;
        case "a leave(false) overtaken": {
          const clear = held();
          storage.clearGate = clear.promise;
          const leaving = store().leave(false);
          const leave = track(leaving);
          try {
            await vi.waitFor(() => expect(storage.log).toContain("clear:enter"));
            await expect(store().joinDraft({ kind: "new", roomCode: "NEWER", displayName: "B" }))
              .resolves.toEqual({ status: "opened" });
            await flush();
            expect(leave.state()).toBe("resolved");
          } finally {
            clear.resolve();
          }
          await leaving;
          await flush();
          expect(adapter.disposeCount).toBe(1);
          expect(store()).toMatchObject({ role: "guest", transport: "p2p" });
          return;
        }
      }

      expect(adapter.disposeCount).toBe(1);
      expect(store().transport).toBeNull();
      if (ending === "leave(false)") {
        await expect(storedRecord(capabilityId)).resolves.toBeUndefined();
        expect(storedPointer()).toBeNull();
      } else {
        await expect(storedRecord(capabilityId)).resolves.toMatchObject({ capabilityId });
        expect(storedPointer()).toMatchObject({ capabilityId });
      }
    },
  );

  it("a resumed native owner ends with its route", async () => {
    const { capabilityId, pointerText } = await storeNativePair();
    const route = new AbortController();
    await expect(store().resumeNativeDraft({ signal: route.signal })).resolves.toEqual({ status: "resumed" });
    // Reach: the resumed owner is installed.
    expect(store().transport).toBe("native");
    const adapter = native.instances[0]!;

    route.abort();
    await flush();
    expect(adapter.disposeCount).toBe(1);
    expect(store()).toMatchObject({ role: null, transport: null, phase: "idle" });
    await expect(storedRecord(capabilityId)).resolves.toMatchObject({ capabilityId });
    expect(storedPointer()?.capabilityId).toBe(capabilityId);
    expect(localStorage.getItem(POINTER_KEY)).not.toBeNull();
    expect(pointerText).not.toBeNull();
  });

  it("no draft state remains on the multiplayer store", () => {
    const state = useMultiplayerStore.getState() as unknown as Record<string, unknown>;
    // Reach: this is the multiplayer store's state.
    expect(state).toHaveProperty("serverInfo");
    for (const key of ["draftAdapter", "draftView", "draftPhase", "joinServerDraft", "createServerDraft"]) {
      expect(state).not.toHaveProperty(key);
    }
  });

  it("refuses a native endpoint the authority rejects", async () => {
    await joinP2PGuest();
    const before = snapshot();

    await expect(store().joinNativeDraft({ endpoint: "wss://u:p@h/", draftCode: "NAT001", displayName: "Bo" }))
      .resolves.toEqual({ status: "failed", error: null });
    expect(native.instances).toHaveLength(0);
    expect(guests.instances[0]!.dispose).not.toHaveBeenCalled();
    expect(snapshot()).toStrictEqual(before);

    // Reach: the same call on an accepted endpoint constructs one.
    await expect(store().joinNativeDraft({ endpoint: "wss://h/", draftCode: "NAT001", displayName: "Bo" }))
      .resolves.toEqual({ status: "opened" });
    expect(native.instances).toHaveLength(1);
  });

  it.each(["incompatible", "disconnected", "error"] as const)(
    "an installed native owner shows a refused view and a lost connection: %s",
    async (kind) => {
      await joinNative();
      // Reach: the native owner is installed and live.
      expect(store()).toMatchObject({ transport: "native", phase: "lobby", error: null });
      const adapter = native.instances[0]!;

      switch (kind) {
        case "incompatible":
          adapter.emit({
            type: "incompatible",
            failure: { kind: "incompatible", code: "review_status", message: "This draft is in Review." },
          });
          expect(store().phase).toBe("error");
          expect(store().guestRecoveryFailure).toMatchObject({ kind: "incompatible", code: "review_status" });
          break;
        case "disconnected":
          adapter.emit({ type: "disconnected" });
          expect(store().phase).toBe("error");
          break;
        case "error":
          adapter.emit({ type: "error", message: "Cannot send message: WebSocket is not open." });
          expect(store().error).toBe("Cannot send message: WebSocket is not open.");
          break;
      }
    },
  );

  it("a native resume with nothing to resume leaves the store as it was", async () => {
    const before = snapshot();

    await expect(store().resumeNativeDraft()).resolves.toEqual({ status: "absent" });

    localStorage.setItem(POINTER_KEY, JSON.stringify({ transport: "native", capabilityId: "cap-M" }));
    // Reach: the malformed pointer is there before its call.
    expect(localStorage.getItem(POINTER_KEY)).not.toBeNull();
    await expect(store().resumeNativeDraft()).resolves.toEqual({ status: "invalid" });
    expect(localStorage.getItem(POINTER_KEY)).toBeNull();

    const { capabilityId } = await storeNativePair();
    await idbDel(nativeKey(capabilityId), getDraftStore());
    // Reach: the orphan pointer is there before its call.
    expect(storedPointer()).toMatchObject({ capabilityId });
    await expect(store().resumeNativeDraft()).resolves.toEqual({ status: "invalid" });
    expect(localStorage.getItem(POINTER_KEY)).toBeNull();

    expect(snapshot()).toStrictEqual(before);
    expect(native.instances).toHaveLength(0);

    // A held, live capability is not rebuilt.
    await joinNative();
    expect(native.instances).toHaveLength(1);
    await expect(store().resumeNativeDraft()).resolves.toEqual({ status: "resumed" });
    expect(native.instances).toHaveLength(1);

    // Sibling: a P2P resume beside a held native joiner builds no guest.
    await expect(store().resumeDraft()).resolves.toBe("resumed");
    expect(guests.instances).toHaveLength(0);
  });

  it("a native resume reconnects a held seat whose connection was lost", async () => {
    const capabilityId = await joinNative();
    const first = native.instances[0]!;
    first.emit({ type: "disconnected" });
    // Reach: the held seat is not live, and the pointer names it.
    expect(store().phase).toBe("error");
    expect(storedPointer()?.capabilityId).toBe(capabilityId);

    await expect(store().resumeNativeDraft()).resolves.toEqual({ status: "resumed" });
    expect(native.instances).toHaveLength(2);
    const second = native.instances[1]!;
    expect(second.url).toBe(ENDPOINT);
    expect(second.reconnectCalls).toEqual([{ draftCode: "NAT001", playerToken: "tok-guest", seatIndex: 3 }]);
    expect(first.disposeCount).toBe(1);
    expect(store()).toMatchObject({ role: "guest", transport: "native" });
    expect(store().view?.pick_number).toBe(7);
  });

  it("a refused native resume leaves the held guest and its state", async () => {
    await joinP2PGuest();
    const { capabilityId, pointerText } = await storeNativePair();
    const recordBefore = await storedRecord(capabilityId);
    const before = snapshot();
    const failure = {
      kind: "incompatible" as const,
      code: "review_status" as const,
      message: "This draft is in Review, which this connection cannot accept.",
    };
    native.nextReconnect.push((double, settle) => {
      double.emit({ type: "incompatible", failure });
      settle.reject(new Draft27IncompatibilityError(failure));
    });

    const outcome = await store().resumeNativeDraft();
    // Reach: the reconnect carried the stored credential.
    expect(native.instances[0]!.reconnectCalls).toEqual([{ draftCode: "NAT001", playerToken: "tok-guest", seatIndex: 3 }]);
    expect(outcome).toMatchObject({ status: "failed", code: "review_status" });
    expect(snapshot()).toStrictEqual(before);
    expect(guests.instances[0]!.dispose).not.toHaveBeenCalled();
    expect(native.instances[0]!.disposeCount).toBe(1);
    expect(localStorage.getItem(POINTER_KEY)).toBe(pointerText);
    await expect(storedRecord(capabilityId)).resolves.toEqual(recordBefore);
  });

  it("a native resume whose reconnect fails leaves the held guest and its state", async () => {
    await joinP2PGuest();
    const { capabilityId, pointerText } = await storeNativePair();
    const recordBefore = await storedRecord(capabilityId);
    const before = snapshot();
    native.nextReconnect.push((_double, settle) => {
      settle.reject(new Error("Reconnect was not confirmed in time"));
    });

    const outcome = await store().resumeNativeDraft();
    // Reach: the reconnect carried the stored credential.
    expect(native.instances[0]!.reconnectCalls).toEqual([{ draftCode: "NAT001", playerToken: "tok-guest", seatIndex: 3 }]);
    expect(outcome).toEqual({ status: "failed", error: "Reconnect was not confirmed in time" });
    expect(snapshot()).toStrictEqual(before);
    expect(guests.instances[0]!.dispose).not.toHaveBeenCalled();
    expect(native.instances[0]!.disposeCount).toBe(1);
    expect(localStorage.getItem(POINTER_KEY)).toBe(pointerText);
    await expect(storedRecord(capabilityId)).resolves.toEqual(recordBefore);
  });

  it.each(["offline at entry", "offline while reconnecting", "the refresh write failing", "the record read failing"] as const)(
    "a native resume that cannot commit writes nothing: %s",
    async (leg) => {
      await joinP2PGuest();
      const { capabilityId, pointerText } = await storeNativePair();
      const recordBefore = await storedRecord(capabilityId);
      const before = snapshot();
      let outcome: unknown;
      switch (leg) {
        case "offline at entry":
          useConnectivityStore.setState({ forcedOffline: true });
          outcome = await store().resumeNativeDraft();
          expect(outcome).toEqual({ status: "offline" });
          break;
        case "offline while reconnecting": {
          const reconnect = holdNextReconnect();
          const resuming = store().resumeNativeDraft();
          await vi.waitFor(() => expect(reconnect.entered()).toBe(true));
          useConnectivityStore.setState({ forcedOffline: true });
          reconnect.release();
          outcome = await resuming;
          expect(outcome).toEqual({ status: "offline" });
          break;
        }
        case "the refresh write failing":
          storage.failSave = true;
          outcome = await store().resumeNativeDraft();
          expect(outcome).toEqual({ status: "failed", error: "Could not save draft recovery details" });
          break;
        case "the record read failing":
          storage.failLoad = true;
          outcome = await store().resumeNativeDraft();
          // Reach: the read leg reached the record read.
          expect(storage.log).toContain("load:enter");
          expect(outcome).toEqual({ status: "failed", error: null });
          break;
      }
      if (leg === "offline at entry" || leg === "the record read failing") {
        expect(native.instances).toHaveLength(0);
      } else {
        expect(native.instances).toHaveLength(1);
        expect(native.instances[0]!.disposeCount).toBe(1);
      }
      expect(snapshot()).toStrictEqual(before);
      expect(guests.instances[0]!.dispose).not.toHaveBeenCalled();
      expect(localStorage.getItem(POINTER_KEY)).toBe(pointerText);
      await expect(storedRecord(capabilityId)).resolves.toEqual(recordBefore);
    },
  );

  it("a native create that fails on its own leaves no native owner", async () => {
    native.nextCreate.push((_double, settle) => {
      settle.reject(new Error("Draft rejected by server"));
    });
    await expect(store().createNativeDraft({ endpoint: ENDPOINT, settings: createSettings }))
      .resolves.toEqual({ status: "failed", error: "Draft rejected by server" });
    // Reach: one adapter was constructed.
    expect(native.instances).toHaveLength(1);
    expect(store()).toMatchObject({ role: null, transport: null, error: "Draft rejected by server" });
    expect(native.instances[0]!.disposeCount).toBe(1);
    expect(storedPointer()).toBeNull();
    expect(storage.log).not.toContain("privateSave:enter");

    // Sibling: its private write failing is its own failure too.
    storage.failSave = true;
    await expect(store().createNativeDraft({ endpoint: ENDPOINT, settings: createSettings }))
      .resolves.toEqual({ status: "failed", error: "Could not save draft recovery details" });
    expect(native.instances).toHaveLength(2);
    expect(store()).toMatchObject({ role: null, transport: null });
    expect(native.instances[1]!.disposeCount).toBe(1);
    expect(storedPointer()).toBeNull();
    await expect(storedRecord(storage.lastSavedId!)).resolves.toBeUndefined();
  });

  describe("a waiting leave", () => {
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

    it.each(["join", "resume"] as const)("a native %s overtakes a guest leave waiting for its host", async (start) => {
      const answer = held();
      guests.nextLeave.push(() => answer.promise);
      await joinP2PGuest();
      if (start === "resume") await storeNativePair();
      const leaving = store().leave(false);
      const leave = track(leaving);
      // Reach: the guest leave reached its host's acknowledgement.
      await vi.waitFor(() => expect(guests.instances[0]!.leave).toHaveBeenCalledTimes(1));

      if (start === "join") {
        await expect(store().joinNativeDraft({ endpoint: ENDPOINT, draftCode: "NAT001", displayName: "Bo" }))
          .resolves.toEqual({ status: "opened" });
      } else {
        await expect(store().resumeNativeDraft()).resolves.toEqual({ status: "resumed" });
      }
      // Reach: one native adapter was constructed.
      expect(native.instances).toHaveLength(1);
      await flush();
      expect(leave.state()).toBe("resolved");

      answer.reject(new Error("Draft host did not acknowledge leave"));
      await flush();
      expect(leave.state()).toBe("resolved");
      expect(unhandled).toEqual([]);
      expect(store().transport).toBe("native");
    });
  });

  it.each([
    ["create", "recovery"],
    ["join", "recovery"],
    ["create", "leave"],
    ["join", "leave"],
    ["join", "signal"],
  ] as const)("a native %s waiting for its predecessor's teardown is superseded by a %s", async (start, by) => {
    const teardownA = held();
    const teardown = track(teardownA.promise);
    hosts.nextDispose.push(() => teardownA.promise);
    await expect(store().hostDraft({ ...hostConfig, persistenceId: "A-POD" })).resolves.toEqual({ status: "opened" });
    const route = new AbortController();
    const starting = start === "create"
      ? store().createNativeDraft({ endpoint: ENDPOINT, settings: createSettings, signal: route.signal })
      : store().joinNativeDraft({ endpoint: ENDPOINT, draftCode: "NAT001", displayName: "Bo", signal: route.signal });
    // Reach: the start retired A and waits for its teardown.
    await vi.waitFor(() => expect(hosts.instances[0]!.dispose).toHaveBeenCalledTimes(1));

    let recovering: Promise<unknown> | null = null;
    const restoreC = held();
    try {
      switch (by) {
        case "recovery": {
          let initializing = false;
          hosts.nextInitialize.push(async (config) => {
            initializing = true;
            await restoreC.promise;
            await config.recovery!.handover!();
          });
          recovering = store().resumeHostedDraft(recoveryOf("C-POD"));
          // Reach: C's provisional host began initializing before A's release.
          await vi.waitFor(() => expect(hosts.instances).toHaveLength(2));
          await vi.waitFor(() => expect(initializing).toBe(true));
          break;
        }
        case "leave":
          await store().leave();
          break;
        case "signal":
          route.abort();
          await flush();
          // Reach: A's teardown is still pending when the signal aborts.
          expect(teardown.state()).toBe("pending");
          break;
      }

      teardownA.resolve();
      await expect(starting).resolves.toEqual({ status: "superseded" });
      expect(native.instances).toHaveLength(0);
      expect(store()).toMatchObject({ role: null, transport: null });
    } finally {
      teardownA.resolve();
      restoreC.resolve();
    }
    if (recovering) {
      await expect(recovering).resolves.toBe("resumed");
      expect(store()).toMatchObject({ role: "host", transport: "p2p", hostPersistenceId: "C-POD" });
    }
  });

  it.each(["leave", "native join", "signal"] as const)("a superseded native resume ends its adapter at once: %s", async (by) => {
    const { capabilityId, pointerText } = await storeNativePair();
    const recordBefore = await storedRecord(capabilityId);
    const before = snapshot();
    const reconnect = holdNextReconnect();
    const route = new AbortController();
    const resuming = store().resumeNativeDraft({ signal: route.signal });
    await vi.waitFor(() => expect(reconnect.entered()).toBe(true));
    // Reach: the resume constructed its adapter.
    expect(native.instances).toHaveLength(1);
    const adapter = native.instances[0]!;

    switch (by) {
      case "leave":
        await store().leave();
        break;
      case "native join":
        await store().joinNativeDraft({ endpoint: ENDPOINT, draftCode: "NAT002", displayName: "Bo" });
        break;
      case "signal":
        route.abort();
        break;
    }
    expect(adapter.disposeCount).toBe(1);

    reconnect.release();
    await expect(resuming).resolves.toEqual({ status: "superseded" });
    expect(adapter.disposeCount).toBe(1);
    if (by === "signal") {
      expect(snapshot()).toStrictEqual(before);
      expect(localStorage.getItem(POINTER_KEY)).toBe(pointerText);
      await expect(storedRecord(capabilityId)).resolves.toEqual(recordBefore);
    }
  });

  it.each([
    "entry",
    "its record read",
    "its record read, by leave",
    "its reconnect's resolution",
    "its refresh write",
  ] as const)("a native resume that ends at %s builds and installs nothing", async (end) => {
    const route = new AbortController();
    if (end === "entry") {
      localStorage.setItem(POINTER_KEY, JSON.stringify({ transport: "native", capabilityId: "cap-M" }));
      const pointerText = localStorage.getItem(POINTER_KEY);
      // Reach: the malformed pointer is there before the call.
      expect(pointerText).not.toBeNull();
      route.abort();
      await expect(store().resumeNativeDraft({ signal: route.signal })).resolves.toEqual({ status: "superseded" });
      expect(localStorage.getItem(POINTER_KEY)).toBe(pointerText);
      expect(native.instances).toHaveLength(0);
      return;
    }

    const { capabilityId, pointerText } = await storeNativePair();
    switch (end) {
      case "its record read":
      case "its record read, by leave": {
        const read = held();
        storage.loadGate = read.promise;
        const resuming = store().resumeNativeDraft({ signal: route.signal });
        // Reach: the record read was entered.
        await vi.waitFor(() => expect(storage.log).toContain("load:enter"));
        if (end === "its record read") route.abort();
        else await store().leave();
        read.resolve();
        await expect(resuming).resolves.toEqual({ status: "superseded" });
        expect(native.instances).toHaveLength(0);
        return;
      }
      case "its reconnect's resolution": {
        await ageNativePair(capabilityId, 60_000);
        const recordBefore = await storedRecord(capabilityId) as { timestamp: number };
        const agedPointer = localStorage.getItem(POINTER_KEY);
        const reconnect = holdNextReconnect();
        const resuming = store().resumeNativeDraft({ signal: route.signal });
        await vi.waitFor(() => expect(reconnect.entered()).toBe(true));
        // The held reconnect resolves, and in the same synchronous turn, before
        // the resume's continuation runs, the signal aborts.
        reconnect.release();
        route.abort();
        await expect(resuming).resolves.toEqual({ status: "superseded" });
        // Reach: one reconnect, resolved before the adapter's one dispose.
        expect(native.instances[0]!.reconnectCalls).toHaveLength(1);
        expect(native.log).toEqual(["reconnect:resolved:0", "dispose:0"]);
        expect(storage.log).not.toContain("privateSave:enter");
        await expect(storedRecord(capabilityId)).resolves.toMatchObject({ timestamp: recordBefore.timestamp });
        expect(localStorage.getItem(POINTER_KEY)).toBe(agedPointer);
        expect(store().transport).toBeNull();
        return;
      }
      case "its refresh write": {
        const write = held();
        storage.saveGate = write.promise;
        const resuming = store().resumeNativeDraft({ signal: route.signal });
        await vi.waitFor(() => expect(storage.log).toContain("privateSave:enter"));
        // Reach: the refresh write was entered after the reconnect.
        expect(native.log).toEqual(["reconnect:resolved:0"]);
        route.abort();
        write.resolve();
        await expect(resuming).resolves.toEqual({ status: "superseded" });
        expect(store()).toMatchObject({ role: null, transport: null });
        expect(native.instances[0]!.disposeCount).toBe(1);
        expect(localStorage.getItem(POINTER_KEY)).toBe(pointerText);
        return;
      }
    }
  });
});

describe("a native seat's private record once no pointer leads to it", () => {
  beforeEach(resetHarness);
  afterEach(endHarness);

  /** Another seat's live record, named by no pointer, that no clear here may touch. */
  async function storeBystanderRecord(): Promise<unknown> {
    await saveNativeDraftCapability({
      capabilityId: "cap-other",
      endpoint: ENDPOINT,
      draftCode: "NAT002",
      playerToken: "tok-other",
      seatIndex: 1,
      participant: "guest",
    });
    return storedRecord("cap-other");
  }

  it.each(["an expired pointer", "a mismatched record", "an expired record"] as const)(
    "a resume that clears an unusable pointer removes the record it named: %s",
    async (leg) => {
      const { capabilityId, pointerText } = await storeNativePair();
      const bystander = await storeBystanderRecord();
      switch (leg) {
        case "an expired pointer": {
          const pointer = JSON.parse(pointerText) as Record<string, unknown>;
          localStorage.setItem(POINTER_KEY, JSON.stringify({ ...pointer, timestamp: Date.now() - 5 * 60 * 60 * 1000 }));
          break;
        }
        case "a mismatched record": {
          const record = await storedRecord(capabilityId) as Record<string, unknown>;
          await idbSet(nativeKey(capabilityId), { ...record, draftCode: "NAT999" }, getDraftStore());
          break;
        }
        case "an expired record": {
          const record = await storedRecord(capabilityId) as Record<string, unknown>;
          await idbSet(nativeKey(capabilityId), { ...record, timestamp: Date.now() - 5 * 60 * 60 * 1000 }, getDraftStore());
          break;
        }
      }
      // Reach: the pointer still names the record, which is stored, before the resume.
      expect(storedPointer()).toMatchObject({ transport: "native", capabilityId });
      await expect(storedRecord(capabilityId)).resolves.toMatchObject({ capabilityId, playerToken: "tok-guest" });

      await expect(store().resumeNativeDraft()).resolves.toEqual({ status: "invalid" });
      // Reach: the expired pointer never reaches the record read; the other legs do.
      expect(storage.log.includes("load:enter")).toBe(leg !== "an expired pointer");
      expect(storedPointer()).toBeNull();
      await expect(storedRecord(capabilityId)).resolves.toBeUndefined();
      await expect(storedRecord("cap-other")).resolves.toEqual(bystander);
      expect(native.instances).toHaveLength(0);
    },
  );

  it("a native successor's pointer removes the record its predecessor's pointer named", async () => {
    const predecessor = await joinNative();
    // Reach: the predecessor's record and pointer are stored before the successor starts.
    await expect(storedRecord(predecessor)).resolves.toMatchObject({ capabilityId: predecessor, playerToken: "tok-guest" });
    expect(storedPointer()).toMatchObject({ capabilityId: predecessor });
    const bystander = await storeBystanderRecord();

    await expect(store().createNativeDraft({ endpoint: ENDPOINT, settings: createSettings }))
      .resolves.toEqual({ status: "opened" });
    const successor = storedPointer()?.capabilityId as string;
    expect(successor).toEqual(expect.any(String));
    expect(successor).not.toBe(predecessor);
    expect(native.instances[0]!.disposeCount).toBe(1);
    await expect(storedRecord(predecessor)).resolves.toBeUndefined();
    // Sibling: the live record the current pointer names stays, as does a record no clear here names.
    await expect(storedRecord(successor)).resolves.toMatchObject({ capabilityId: successor, playerToken: "tok-host" });
    await expect(storedRecord("cap-other")).resolves.toEqual(bystander);
  });

  it("a P2P guest's committed pointer removes the native record the pointer it replaced named", async () => {
    const predecessor = await joinNative();
    await joinP2PGuest();
    // Reach: the native owner is retired, and its pointer and record stay until the guest publishes.
    expect(native.instances[0]!.disposeCount).toBe(1);
    expect(storedPointer()).toMatchObject({ transport: "native", capabilityId: predecessor });
    await expect(storedRecord(predecessor)).resolves.toMatchObject({ capabilityId: predecessor });

    guests.instances[0]!.emit({
      type: "recoveryCommitted",
      pointer: { roomCode: "ABCDE", displayName: "A", hostPeerId: "phase2-OWN", capabilityId: "cap-guest" },
    });
    await flush();
    expect(storedPointer()).toMatchObject({ hostPeerId: "phase2-OWN", capabilityId: "cap-guest" });
    expect(storedPointer()).not.toHaveProperty("transport");
    await expect(storedRecord(predecessor)).resolves.toBeUndefined();
  });

  it("a resume that republishes its own capability keeps that record", async () => {
    const capabilityId = await joinNative();
    native.instances[0]!.emit({ type: "disconnected" });
    const before = await storedRecord(capabilityId) as { timestamp: number };
    // Reach: the pointer this resume replaces names the record it keeps.
    expect(storedPointer()?.capabilityId).toBe(capabilityId);

    await expect(store().resumeNativeDraft()).resolves.toEqual({ status: "resumed" });
    expect(native.instances).toHaveLength(2);
    expect(native.instances[0]!.disposeCount).toBe(1);
    expect(storedPointer()?.capabilityId).toBe(capabilityId);
    const after = await storedRecord(capabilityId) as { capabilityId: string; timestamp: number };
    expect(after.capabilityId).toBe(capabilityId);
    expect(after.timestamp).toBeGreaterThanOrEqual(before.timestamp);
  });
});

describe("the draft owner generation", () => {
  beforeEach(resetHarness);
  afterEach(endHarness);

  it.each(["host start", "join", "native create", "native join"] as const)(
    "a %s moves it once as it retires the seated guest, and once more as it installs",
    async (start) => {
      await joinP2PGuest();
      const before = currentDraftOwnerGeneration();

      const starting = start === "host start"
        ? store().hostDraft({ ...hostConfig, persistenceId: "B-POD" })
        : start === "join"
          ? store().joinDraft({ kind: "new", roomCode: "NEXT", displayName: "A" })
          : start === "native create"
            ? store().createNativeDraft({ endpoint: ENDPOINT, settings: createSettings })
            : store().joinNativeDraft({ endpoint: ENDPOINT, draftCode: "NAT001", displayName: "Bo" });
      // The retirement moves it before the start's first await.
      expect(currentDraftOwnerGeneration()).toBe(before + 1);

      await expect(starting).resolves.toEqual({ status: "opened" });
      expect(currentDraftOwnerGeneration()).toBe(before + 2);
    },
  );

  it("a native recovery moves it only at its handoff, which retires and installs", async () => {
    await joinP2PGuest();
    await storeNativePair();
    const reconnect = holdNextReconnect();
    const before = currentDraftOwnerGeneration();

    const resuming = store().resumeNativeDraft();
    await vi.waitFor(() => expect(reconnect.entered()).toBe(true));
    // Reach: the provisional recovery is reconnecting, with the guest still installed.
    expect(store()).toMatchObject({ role: "guest", transport: "p2p" });
    expect(currentDraftOwnerGeneration()).toBe(before);

    reconnect.release();
    await expect(resuming).resolves.toEqual({ status: "resumed" });
    expect(currentDraftOwnerGeneration()).toBe(before + 2);
  });

  it("a leave moves it as it begins, and a reset moves it again", async () => {
    await joinP2PGuest();
    const before = currentDraftOwnerGeneration();

    const leaving = store().leave(false);
    expect(currentDraftOwnerGeneration()).toBe(before + 1);
    await leaving;
    // Reach: the leave ended the guest.
    expect(store().role).toBeNull();
    expect(currentDraftOwnerGeneration()).toBe(before + 1);

    store().reset();
    expect(currentDraftOwnerGeneration()).toBe(before + 2);
  });

  it("an owner's own start failure, a view and its route's end leave it as it was", async () => {
    native.nextJoin.push((_double, settle) => {
      settle.reject(new Error("Draft is full"));
    });
    const failing = store().joinNativeDraft({ endpoint: ENDPOINT, draftCode: "NAT001", displayName: "Bo" });
    // Reach: the failing join's double was constructed.
    expect(native.instances).toHaveLength(1);
    const afterStart = currentDraftOwnerGeneration();
    await expect(failing).resolves.toEqual({ status: "failed", error: "Draft is full" });
    expect(store().error).toBe("Draft is full");
    expect(currentDraftOwnerGeneration()).toBe(afterStart);

    const route = new AbortController();
    await joinNative(route.signal);
    const seated = currentDraftOwnerGeneration();
    native.instances[1]!.emit({ type: "draftViewUpdated", view: native.state.draftingView });
    // Reach: the view was applied.
    expect(store().view).toBe(native.state.draftingView);
    expect(currentDraftOwnerGeneration()).toBe(seated);

    route.abort();
    // Reach: the route's end ended the owner.
    expect(store().role).toBeNull();
    expect(currentDraftOwnerGeneration()).toBe(seated);
  });
});
