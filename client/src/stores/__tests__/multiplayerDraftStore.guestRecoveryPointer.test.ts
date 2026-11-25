import "fake-indexeddb/auto";
import { clear as idbClear, get, set as idbSet } from "idb-keyval";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { FakeDraftDataConnection } from "../../network/__tests__/fakeDraftDataConnection";
import { DRAFT_PROTOCOL_VERSION } from "../../network/draftProtocol";
import type { DraftGuestCapability } from "../../services/draftPersistence";

/*
 * Who publishes a guest's public recovery pointer, on the real lifecycle
 * store, the real guest adapter and P2PDraftGuest, the real draft peer session
 * over a fake data connection, and real persistence on fake-indexeddb. Only
 * the room join is doubled; three persistence functions are wrapped so each
 * row can read the order in which the private commit, the public pointer and
 * any revocation happened.
 */
const harness = vi.hoisted(() => ({
  joinRoom: vi.fn(),
  log: [] as string[],
  /** One-shot: the next private save waits on it before writing. */
  privateSaveGate: null as Promise<void> | null,
}));

vi.mock("../../network/connection", async (importOriginal) => ({
  ...(await importOriginal<typeof import("../../network/connection")>()),
  joinRoom: harness.joinRoom,
}));

vi.mock("../../services/draftPersistence", async (importOriginal) => {
  const actual = await importOriginal<typeof import("../../services/draftPersistence")>();
  return {
    ...actual,
    saveDraftGuestSession: async (...args: Parameters<typeof actual.saveDraftGuestSession>) => {
      harness.log.push("privateSave:enter");
      const gate = harness.privateSaveGate;
      harness.privateSaveGate = null;
      if (gate) await gate;
      await actual.saveDraftGuestSession(...args);
      harness.log.push("privateSave:committed");
    },
    saveActiveDraftGuest: (...args: Parameters<typeof actual.saveActiveDraftGuest>) => {
      harness.log.push(`pointerWrite:${args[0].capabilityId}`);
      actual.saveActiveDraftGuest(...args);
    },
    clearDraftGuestRecovery: async (capability: DraftGuestCapability) => {
      harness.log.push("revoke:clear");
      await actual.clearDraftGuestRecovery(capability);
    },
  };
});

import { useMultiplayerDraftStore } from "../multiplayerDraftStore";
import { useConnectivityStore } from "../connectivityStore";
import {
  getDraftStore,
  inspectActiveDraftGuest,
  saveActiveDraftGuest,
  saveDraftGuestSession,
} from "../../services/draftPersistence";

const POINTER_KEY = "phase-active-draft-guest";

const hostPeerIdFor = (roomCode: string) => `host-peer-${roomCode}`;
const recordKeyFor = (roomCode: string) => `phase-draft-guest:${hostPeerIdFor(roomCode)}`;

function rawMessage(message: Record<string, unknown>): Uint8Array {
  const json = new TextEncoder().encode(JSON.stringify(message));
  const bytes = new Uint8Array(json.length + 1);
  bytes.set(json, 1);
  return bytes;
}

function lobbyView(): Record<string, unknown> {
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
  };
}

const welcome = (draftToken = "guest-token") => ({
  type: "draft_welcome",
  draftProtocolVersion: DRAFT_PROTOCOL_VERSION,
  draftToken,
  seatIndex: 2,
  draftCode: "draft-xyz",
  workspaceState: null,
  view: lobbyView(),
});

const reconnectAck = () => ({
  type: "draft_reconnect_ack",
  draftProtocolVersion: DRAFT_PROTOCOL_VERSION,
  seatIndex: 2,
  draftCode: "draft-xyz",
  workspaceState: null,
  view: lobbyView(),
});

function deferred() {
  let resolve!: () => void;
  const promise = new Promise<void>((res) => { resolve = res; });
  return { promise, resolve };
}

function storedRecord(roomCode = "ABCDE"): Promise<Record<string, unknown> | undefined> {
  return get(recordKeyFor(roomCode), getDraftStore());
}

function storedPointer(): Record<string, unknown> | null {
  const raw = localStorage.getItem(POINTER_KEY);
  return raw === null ? null : JSON.parse(raw) as Record<string, unknown>;
}

function pointerWrites(): string[] {
  return harness.log.filter((entry) => entry.startsWith("pointerWrite:"));
}

/** Writes a pair the way builds before capability names wrote it. */
async function seedUnnamedPair(draftToken = "guest-token"): Promise<void> {
  await idbSet(recordKeyFor("ABCDE"), {
    hostPeerId: hostPeerIdFor("ABCDE"),
    draftToken,
    seatIndex: 2,
    draftCode: "draft-xyz",
    roomCode: "ABCDE",
    displayName: "Alice",
    timestamp: Date.now(),
  }, getDraftStore());
  localStorage.setItem(POINTER_KEY, JSON.stringify({
    roomCode: "ABCDE",
    displayName: "Alice",
    hostPeerId: hostPeerIdFor("ABCDE"),
    timestamp: Date.now(),
  }));
}

/** Records `joinDraft`'s outcome in the order log once its promise settles. */
function observeJoin<T extends { status: string }>(joined: Promise<T>): Promise<T> {
  return joined.then((outcome) => {
    harness.log.push(`joinDraft:resolved:${outcome.status}`);
    return outcome;
  });
}

describe("guest recovery pointer publication", () => {
  /** Every room the store joined, newest last, by room code. */
  let rooms: Array<{ roomCode: string; conn: FakeDraftDataConnection; dials: FakeDraftDataConnection[] }> = [];
  const roomFor = (roomCode: string) => rooms.filter((room) => room.roomCode === roomCode).at(-1)!;
  const connFor = (roomCode: string) => roomFor(roomCode).conn;

  const openRoom = async (roomCode: string) => {
    const hostPeerId = hostPeerIdFor(roomCode);
    const conn = new FakeDraftDataConnection();
    (conn as unknown as { peer: string }).peer = hostPeerId;
    const room = { roomCode, conn, dials: [] as FakeDraftDataConnection[] };
    rooms.push(room);
    return {
      conn,
      peer: {
        id: `guest-peer-${roomCode}`,
        destroy: vi.fn(),
        connect: () => {
          const dial = new FakeDraftDataConnection();
          dial.open = false;
          (dial as unknown as { peer: string }).peer = hostPeerId;
          room.dials.push(dial);
          return dial;
        },
        on: vi.fn(),
        off: vi.fn(),
      },
      closeConn: vi.fn(),
      destroyPeer: vi.fn(),
    };
  };

  async function resetAll(): Promise<void> {
    await useMultiplayerDraftStore.getState().leave(true).catch(() => undefined);
  }

  beforeEach(async () => {
    vi.clearAllMocks();
    await resetAll();
    useMultiplayerDraftStore.getState().reset();
    useConnectivityStore.setState({ forcedOffline: false, browserOnline: true });
    localStorage.clear();
    await idbClear(getDraftStore());
    rooms = [];
    harness.privateSaveGate = null;
    harness.log.length = 0;
    harness.joinRoom.mockReset().mockImplementation((roomCode: string) => openRoom(roomCode));
  });

  afterEach(async () => {
    await resetAll();
    harness.privateSaveGate = null;
  });

  /** Starts a join and waits until its first contact is on the wire. The outcome stays pending. */
  async function startNewJoin(roomCode = "ABCDE", displayName = "Alice", signal?: AbortSignal) {
    const joined = observeJoin(useMultiplayerDraftStore.getState().joinDraft({ kind: "new", roomCode, displayName, signal }));
    await vi.waitFor(() => expect(rooms.some((room) => room.roomCode === roomCode)).toBe(true));
    await vi.waitFor(() => expect(connFor(roomCode).sentRaw.length).toBeGreaterThan(0));
    return { joined };
  }

  /** Starts a resume and waits until its reconnect request is on the wire. The outcome stays pending. */
  async function startResume() {
    const resumed = useMultiplayerDraftStore.getState().resumeDraft();
    await vi.waitFor(() => expect(rooms.length).toBeGreaterThan(0));
    await vi.waitFor(() => expect(connFor("ABCDE").sentRaw.length).toBeGreaterThan(0));
    return { resumed };
  }

  it("publishes the committed pointer once", async () => {
    const { joined } = await startNewJoin();
    await connFor("ABCDE").receiveRaw(rawMessage(welcome()));
    await expect(joined).resolves.toEqual({ status: "opened" });

    const record = await storedRecord();
    const capabilityId = record?.capabilityId as string;
    expect(capabilityId).toEqual(expect.any(String));
    expect(harness.log).toEqual([
      "privateSave:enter",
      "privateSave:committed",
      `pointerWrite:${capabilityId}`,
      "joinDraft:resolved:opened",
    ]);
    const pointer = inspectActiveDraftGuest();
    expect(pointer.type).toBe("present");
    expect(pointer.type === "present" && pointer.meta.capabilityId).toBe(capabilityId);
  });

  it("an owner ended during the commit leaves no pointer", async () => {
    const save = deferred();
    harness.privateSaveGate = save.promise;
    const route = new AbortController();
    const { joined } = await startNewJoin("ABCDE", "Alice", route.signal);
    const received = connFor("ABCDE").receiveRaw(rawMessage(welcome()));
    await vi.waitFor(() => expect(harness.log).toContain("privateSave:enter"));

    route.abort();
    save.resolve();
    await received;
    await expect(joined).resolves.toEqual({ status: "superseded" });

    // Reach: the private record committed while its owner was gone.
    expect(harness.log).toContain("privateSave:committed");
    await expect(storedRecord()).resolves.toMatchObject({ draftToken: "guest-token", capabilityId: expect.any(String) });
    expect(inspectActiveDraftGuest().type).toBe("absent");
    expect(useMultiplayerDraftStore.getState()).toMatchObject({ role: null, phase: "idle" });
  });

  it("a stale owner's late commit keeps the newer pointer", async () => {
    const saveA = deferred();
    harness.privateSaveGate = saveA.promise;
    const { joined: joinedA } = await startNewJoin("ABCDE", "Alice");
    const receivedA = connFor("ABCDE").receiveRaw(rawMessage(welcome("token-A")));
    await vi.waitFor(() => expect(harness.log).toContain("privateSave:enter"));

    const { joined: joinedB } = await startNewJoin("FGHJK", "Bob");
    await connFor("FGHJK").receiveRaw(rawMessage(welcome("token-B")));
    await expect(joinedB).resolves.toEqual({ status: "opened" });

    saveA.resolve();
    await receivedA;
    await expect(joinedA).resolves.toEqual({ status: "superseded" });

    // Reach: the superseded owner's private record did commit, at its own host key.
    await expect(storedRecord("ABCDE")).resolves.toMatchObject({ draftToken: "token-A", hostPeerId: hostPeerIdFor("ABCDE") });
    expect(harness.log.filter((entry) => entry === "privateSave:committed")).toHaveLength(2);
    const recordB = await storedRecord("FGHJK");
    const pointer = inspectActiveDraftGuest();
    expect(pointer.type).toBe("present");
    if (pointer.type !== "present") return;
    expect({
      displayName: pointer.meta.displayName,
      hostPeerId: pointer.meta.hostPeerId,
      capabilityId: pointer.meta.capabilityId,
    }).toEqual({ displayName: "Bob", hostPeerId: hostPeerIdFor("FGHJK"), capabilityId: recordB?.capabilityId });
  });

  it("a kick queued behind the welcome removes the published pointer", async () => {
    const { joined } = await startNewJoin();
    const conn = connFor("ABCDE");
    const admitted = conn.receiveRaw(rawMessage(welcome()));
    const kicked = conn.receiveRaw(rawMessage({ type: "draft_kicked", reason: "Removed from draft" }));
    await Promise.all([admitted, kicked]);
    await joined;

    const written = harness.log.findIndex((entry) => entry.startsWith("pointerWrite:"));
    expect(written).toBeGreaterThanOrEqual(0);
    expect(written).toBeLessThan(harness.log.indexOf("revoke:clear"));
    expect(inspectActiveDraftGuest().type).toBe("absent");
    expect(useMultiplayerDraftStore.getState().phase).toBe("kicked");
  });

  it("a background reconnect republishes the same capability", { timeout: 15_000 }, async () => {
    const { joined } = await startNewJoin();
    await connFor("ABCDE").receiveRaw(rawMessage(welcome()));
    await expect(joined).resolves.toEqual({ status: "opened" });
    const first = storedPointer();
    expect(first).toMatchObject({ capabilityId: expect.any(String) });

    connFor("ABCDE").simulateClose();
    await new Promise((done) => setTimeout(done, 1_100));
    const dial = roomFor("ABCDE").dials.at(-1)!;
    dial.simulateOpen();
    await vi.waitFor(() => expect(dial.sentRaw.length).toBeGreaterThan(0));
    await dial.receiveRaw(rawMessage(reconnectAck()));

    expect(pointerWrites()).toEqual([`pointerWrite:${first!.capabilityId}`, `pointerWrite:${first!.capabilityId}`]);
    const latest = storedPointer();
    expect(latest).toMatchObject({ capabilityId: first!.capabilityId });
    expect(latest!.timestamp as number).toBeGreaterThan(first!.timestamp as number);
  });

  it("resume names an unnamed pair and publishes it", async () => {
    await seedUnnamedPair();
    const { resumed } = await startResume();
    await connFor("ABCDE").receiveRaw(rawMessage(reconnectAck()));
    await expect(resumed).resolves.toBe("resumed");

    const capabilityId = (await storedRecord())?.capabilityId as string;
    expect(capabilityId).toEqual(expect.any(String));
    expect(capabilityId.length).toBeGreaterThan(0);
    expect(storedPointer()).toMatchObject({ capabilityId });
    expect(pointerWrites()).toEqual([`pointerWrite:${capabilityId}`]);
  });

  it("a refused attempt leaves the loaded pair as it was", async () => {
    await saveDraftGuestSession(hostPeerIdFor("ABCDE"), {
      draftToken: "guest-token",
      seatIndex: 2,
      draftCode: "draft-xyz",
      roomCode: "ABCDE",
      displayName: "Alice",
      capabilityId: "cap-S",
    });
    saveActiveDraftGuest({ roomCode: "ABCDE", displayName: "Alice", hostPeerId: hostPeerIdFor("ABCDE"), capabilityId: "cap-S" });
    const seededRecord = await storedRecord();
    const seededPointer = storedPointer();
    harness.log.length = 0;

    const { resumed } = await startResume();
    await connFor("ABCDE").receiveRaw(rawMessage({
      type: "draft_reconnect_rejected",
      kind: "ProtocolMismatch",
      reason: "Refresh both windows",
    }));
    await expect(resumed).resolves.toBe("failed");

    // Reach: only the protocol rejection's own arm reports this failure.
    expect(useMultiplayerDraftStore.getState().guestRecoveryFailure).toEqual({
      kind: "incompatible",
      message: "Refresh both windows",
    });
    await expect(storedRecord()).resolves.toEqual(seededRecord);
    expect(storedPointer()).toEqual(seededPointer);
    expect(harness.log).not.toContain("privateSave:enter");
    expect(pointerWrites()).toEqual([]);
  });

  it("a refusal before the acknowledgement removes the loaded pair", async () => {
    await seedUnnamedPair();
    const { resumed } = await startResume();
    await connFor("ABCDE").receiveRaw(rawMessage({
      type: "draft_reconnect_rejected",
      kind: "UnknownToken",
      reason: "Unknown token",
    }));
    await resumed;

    // Reach: the refusal revoked the loaded identity.
    expect(harness.log).toContain("revoke:clear");
    await expect(storedRecord()).resolves.toBeUndefined();
    expect(inspectActiveDraftGuest().type).toBe("absent");
    expect(harness.log).not.toContain("privateSave:enter");
  });

  it("after its acknowledgement commits, a kick removes the replacement", async () => {
    await seedUnnamedPair();
    const { resumed } = await startResume();
    const conn = connFor("ABCDE");
    await conn.receiveRaw(rawMessage(reconnectAck()));
    await expect(resumed).resolves.toBe("resumed");
    const capabilityId = (await storedRecord())?.capabilityId as string;
    expect(harness.log).toContain(`pointerWrite:${capabilityId}`);

    await conn.receiveRaw(rawMessage({ type: "draft_kicked", reason: "Removed from draft" }));

    await expect(storedRecord()).resolves.toBeUndefined();
    expect(inspectActiveDraftGuest().type).toBe("absent");
  });

  it("a refused publication fails the join", async () => {
    const setItem = localStorage.setItem.bind(localStorage);
    const pointerKeys: string[] = [];
    const spy = vi.spyOn(localStorage, "setItem").mockImplementation((key: string, value: string) => {
      if (key === POINTER_KEY) {
        pointerKeys.push(key);
        throw new DOMException("Quota exceeded", "QuotaExceededError");
      }
      setItem(key, value);
    });
    const errors: Array<string | null> = [];
    const unsubscribe = useMultiplayerDraftStore.subscribe((state, previous) => {
      if (state.error !== previous.error) errors.push(state.error);
    });
    try {
      const { joined } = await startNewJoin();
      await connFor("ABCDE").receiveRaw(rawMessage(welcome()));
      await expect(joined).resolves.toMatchObject({ status: "failed" });

      // Reach: the private record committed and the publication reached storage.
      expect(harness.log).toContain("privateSave:committed");
      expect(pointerKeys).toEqual([POINTER_KEY]);
      expect(errors).toContain("Could not save draft recovery details");
      expect(inspectActiveDraftGuest().type).toBe("absent");
    } finally {
      unsubscribe();
      spy.mockRestore();
    }
  });
});
