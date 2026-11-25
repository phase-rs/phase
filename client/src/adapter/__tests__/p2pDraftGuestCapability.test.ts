import "fake-indexeddb/auto";
import { clear, get, set } from "idb-keyval";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

vi.mock("../../services/draftPersistence", async (importOriginal) => {
  const actual = await importOriginal<typeof import("../../services/draftPersistence")>();
  return {
    ...actual,
    loadDraftGuestCapability: vi.fn(actual.loadDraftGuestCapability),
    clearDraftGuestRecovery: vi.fn(actual.clearDraftGuestRecovery),
  };
});

import { EMPTY_DRAFT_POOL_GROUPS, type DraftPlayerView } from "../draft-adapter";
import { P2PDraftGuest, type DraftGuestConnection, type DraftGuestEvent } from "../p2p-draft-guest";
import { DRAFT_PROTOCOL_VERSION, type DraftP2PMessage } from "../../network/draftProtocol";
import { FakeDraftDataConnection } from "../../network/__tests__/fakeDraftDataConnection";
import {
  clearDraftGuestRecovery,
  getDraftStore,
  loadDraftGuestCapability,
  saveActiveDraftGuest,
  saveDraftDeckSubmission,
  saveDraftGuestSession,
} from "../../services/draftPersistence";

/**
 * A real guest over the real draft peer session and real persistence on
 * fake-indexeddb: every revocation removes exactly the records its own
 * capability owns, and records older than capability names gain one once.
 */

const HOST_PEER_ID = "phase2-ABCDE";
const OTHER_HOST_PEER_ID = "phase2-FGHJK";
const GUEST_KEY = `phase-draft-guest:${HOST_PEER_ID}`;
const OUTBOX_KEY = `phase-draft-deck-submission:${HOST_PEER_ID}`;
const POINTER_KEY = "phase-active-draft-guest";

/** Use the supported raw frame so only persistence, not decompression, yields. */
function rawMessage(message: DraftP2PMessage | Record<string, unknown>): Uint8Array {
  const json = new TextEncoder().encode(JSON.stringify(message));
  const bytes = new Uint8Array(json.length + 1);
  bytes.set(json, 1);
  return bytes;
}

function view(): DraftPlayerView {
  return {
    status: "Drafting",
    kind: "Premier",
    launch_capability: "None",
    distribution: "PickAndPass",
    commanders_required: 0,
    current_pack_number: 0,
    pick_number: 1,
    pass_direction: "Left",
    current_pack: [{
      instance_id: "pack-1-card",
      name: "Island",
      set_code: "TST",
      collector_number: "1",
      rarity: "common",
      colors: [],
      cmc: 0,
      type_line: "Basic Land — Island",
    }],
    required_pick_count: 1,
    pick_selection_mode: "Direct",
    pool: [],
    draft_effects: [],
    pool_groups: EMPTY_DRAFT_POOL_GROUPS,
    seats: Array.from({ length: 4 }, (_, seatIndex) => ({
      seat_index: seatIndex,
      display_name: `Player ${seatIndex}`,
      is_bot: false,
      connected: true,
      has_submitted_deck: false,
      pick_status: "Pending",
      active_pack_count: 1,
      drafted_card_count: 0,
      face_up_draft_cards: [],
    })),
    cards_per_pack: 14,
    pick_steps_per_pack: 14,
    pack_count: 3,
    min_deck_size: 40,
    addable_cards: ["Island"],
    timer_remaining_ms: 30_000,
    standings: [],
    current_round: 0,
    next_pairing_round: 1,
    tournament_format: "Swiss",
    pod_policy: "Competitive",
    pairings: [],
    match_config: { match_type: "Bo1" },
  };
}

function welcome(draftToken: string): DraftP2PMessage {
  return {
    type: "draft_welcome",
    draftProtocolVersion: DRAFT_PROTOCOL_VERSION,
    draftToken,
    seatIndex: 2,
    draftCode: "draft-xyz",
    workspaceState: null,
    view: view(),
  };
}

const reconnectAck: DraftP2PMessage = {
  type: "draft_reconnect_ack",
  draftProtocolVersion: DRAFT_PROTOCOL_VERSION,
  seatIndex: 2,
  draftCode: "draft-xyz",
  workspaceState: null,
  view: view(),
};

/** Writes a capability's private record and public pointer through the real writers. */
async function saveCapability(draftToken: string, capabilityId: string, displayName = "Alice"): Promise<void> {
  await saveDraftGuestSession(HOST_PEER_ID, {
    draftToken,
    seatIndex: 2,
    draftCode: "draft-xyz",
    roomCode: "ABCDE",
    displayName,
    capabilityId,
  });
  saveActiveDraftGuest({ roomCode: "ABCDE", displayName, hostPeerId: HOST_PEER_ID, capabilityId });
}

/** Writes a capability's deck outbox through the real writer. */
async function saveOutbox(draftToken: string): Promise<void> {
  await saveDraftDeckSubmission(HOST_PEER_ID, {
    draftCode: "draft-xyz",
    roomCode: "ABCDE",
    draftToken,
    submissionId: `submission-${draftToken}`,
    mainDeck: ["Island"],
    commanders: [],
  });
}

/** Writes a pair the way builds before capability names wrote it. */
async function setUnnamedPair(draftToken: string): Promise<void> {
  await set(GUEST_KEY, {
    hostPeerId: HOST_PEER_ID,
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
    hostPeerId: HOST_PEER_ID,
    timestamp: Date.now(),
  }));
}

/** Writes a record for the same token under another host's key. */
async function setOtherHostRecord(draftToken: string): Promise<void> {
  await set(`phase-draft-guest:${OTHER_HOST_PEER_ID}`, {
    hostPeerId: OTHER_HOST_PEER_ID,
    draftToken,
    seatIndex: 1,
    draftCode: "draft-other",
    roomCode: "FGHJK",
    displayName: "Alice",
    timestamp: Date.now(),
    capabilityId: "cap-other-host",
  }, getDraftStore());
}

function storedRecord(): Promise<Record<string, unknown> | undefined> {
  return get(GUEST_KEY, getDraftStore());
}

function storedOtherHostRecord(): Promise<Record<string, unknown> | undefined> {
  return get(`phase-draft-guest:${OTHER_HOST_PEER_ID}`, getDraftStore());
}

function storedOutbox(): Promise<Record<string, unknown> | undefined> {
  return get(OUTBOX_KEY, getDraftStore());
}

function storedPointer(): Record<string, unknown> | null {
  const raw = localStorage.getItem(POINTER_KEY);
  return raw === null ? null : JSON.parse(raw) as Record<string, unknown>;
}

function ownerOf(draftToken: string) {
  return { hostPeerId: HOST_PEER_ID, draftToken, roomCode: "ABCDE", displayName: "Alice" };
}

describe("P2P draft guest capability", () => {
  const guests: P2PDraftGuest[] = [];

  function createGuest(connection: DraftGuestConnection) {
    const conn = new FakeDraftDataConnection();
    // The fake implements only the DataConnection subset used by the session.
    const guest = new P2PDraftGuest({ destroy: vi.fn() } as never, HOST_PEER_ID, conn as never, connection);
    guests.push(guest);
    const events: DraftGuestEvent[] = [];
    guest.onEvent((event) => events.push(event));
    return { guest, conn, events };
  }

  function reconnectGuest(draftToken: string) {
    return createGuest({ kind: "reconnect", roomCode: "ABCDE", displayName: "Alice", draftToken });
  }

  beforeEach(async () => {
    vi.clearAllMocks();
    localStorage.clear();
    await clear(getDraftStore());
  });

  afterEach(() => {
    for (const guest of guests.splice(0)) guest.dispose();
    vi.restoreAllMocks();
  });

  it.each(["own", "replaced"] as const)("a kicked guest clears exactly its own records (%s)", async (variant) => {
    await saveOutbox("token-A");
    const { guest, conn, events } = createGuest({ kind: "new", roomCode: "ABCDE", displayName: "Alice" });
    const initialized = guest.initialize();
    await conn.receiveRaw(rawMessage(welcome("token-A")));
    await initialized;
    const admitted = await storedRecord();
    expect(admitted).toMatchObject({ draftToken: "token-A", capabilityId: expect.any(String) });
    // The guest writes no pointer; it hands the committed one to its owner.
    expect(storedPointer()).toBeNull();
    const committed = events.find(
      (event): event is Extract<DraftGuestEvent, { type: "recoveryCommitted" }> => event.type === "recoveryCommitted",
    );
    expect(committed).toEqual({
      type: "recoveryCommitted",
      pointer: { roomCode: "ABCDE", displayName: "Alice", hostPeerId: HOST_PEER_ID, capabilityId: admitted!.capabilityId },
    });
    // Owner double: the lifecycle store publishes exactly this pointer.
    saveActiveDraftGuest(committed!.pointer);
    expect(storedPointer()).toMatchObject({ capabilityId: admitted!.capabilityId });
    // The welcome replays the stored submission; let it reach the wire first.
    await vi.waitFor(() => expect(conn.sentRaw).toHaveLength(2));

    if (variant === "replaced") {
      await saveCapability("token-B", "cap-B");
      await saveOutbox("token-B");
    }
    await conn.receiveRaw(rawMessage({ type: "draft_kicked", reason: "Removed from draft" }));

    expect(events).toContainEqual({ type: "kicked", reason: "Removed from draft" });
    expect(guest.isRecoveryRevoked).toBe(true);
    expect(clearDraftGuestRecovery).toHaveBeenCalledExactlyOnceWith({
      ...ownerOf("token-A"),
      capabilityId: admitted!.capabilityId,
    });
    expect(loadDraftGuestCapability).not.toHaveBeenCalled();
    if (variant === "own") {
      await expect(storedRecord()).resolves.toBeUndefined();
      expect(storedPointer()).toBeNull();
      await expect(storedOutbox()).resolves.toBeUndefined();
    } else {
      await expect(storedRecord()).resolves.toMatchObject({ draftToken: "token-B", capabilityId: "cap-B" });
      expect(storedPointer()).toMatchObject({ capabilityId: "cap-B" });
      await expect(storedOutbox()).resolves.toMatchObject({ draftToken: "token-B" });
    }
  });

  it("an unnamed capability gains one name at its first admission and keeps it", async () => {
    await setUnnamedPair("token-L");
    const seededPointer = storedPointer();

    const first = reconnectGuest("token-L");
    const firstInitialized = first.guest.initialize(undefined, 1);
    await first.conn.receiveRaw(rawMessage(reconnectAck));
    await firstInitialized;

    const named = await storedRecord();
    const capabilityId = named?.capabilityId;
    expect(capabilityId).toEqual(expect.any(String));
    expect((capabilityId as string).length).toBeGreaterThan(0);
    // The guest leaves the public pointer to its owner.
    expect(storedPointer()).toEqual(seededPointer);
    const committedPointer = { roomCode: "ABCDE", displayName: "Alice", hostPeerId: HOST_PEER_ID, capabilityId };
    expect(first.events).toContainEqual({ type: "recoveryCommitted", pointer: committedPointer });
    expect(loadDraftGuestCapability).toHaveBeenCalledOnce();
    first.guest.dispose();

    const second = reconnectGuest("token-L");
    const secondInitialized = second.guest.initialize(undefined, 1);
    await second.conn.receiveRaw(rawMessage(reconnectAck));
    await secondInitialized;

    await expect(storedRecord()).resolves.toMatchObject({ draftToken: "token-L", capabilityId });
    expect(second.events).toContainEqual({ type: "recoveryCommitted", pointer: committedPointer });
    expect(loadDraftGuestCapability).toHaveBeenCalledTimes(2);
  });

  it.each(["own", "pointer replaced"] as const)(
    "a refused unnamed capability removes its own pair only (%s)",
    async (variant) => {
      await setUnnamedPair("token-L");
      if (variant === "pointer replaced") {
        saveActiveDraftGuest({ roomCode: "ABCDE", displayName: "Alice", hostPeerId: HOST_PEER_ID, capabilityId: "cap-B" });
      }
      const { guest, conn, events } = reconnectGuest("token-L");
      const initialized = expect(guest.initialize(undefined, 1)).rejects.toThrow("Unknown token");

      await conn.receiveRaw(rawMessage({ type: "draft_reconnect_rejected", kind: "UnknownToken", reason: "Unknown token" }));
      await initialized;

      expect(events).toContainEqual({
        type: "reconnectFailed",
        failure: { kind: "invalid", message: "Unknown token" },
      });
      expect(loadDraftGuestCapability).toHaveBeenCalledOnce();
      await expect(storedRecord()).resolves.toBeUndefined();
      if (variant === "own") {
        expect(storedPointer()).toBeNull();
      } else {
        expect(storedPointer()).toMatchObject({ capabilityId: "cap-B" });
      }
    },
  );

  it.each(["own", "replaced"] as const)(
    "a leave sent before the reconnect acknowledgement revokes the capability it bound (%s)",
    async (variant) => {
      await saveCapability("token-L", "cap-S");
      await saveOutbox("token-L");
      await setOtherHostRecord("token-L");
      const { guest, conn } = reconnectGuest("token-L");
      const initialized = guest.initialize(undefined, 1);
      const leaveResult = guest.leave().then(
        () => ({ status: "fulfilled" as const }),
        (error: unknown) => ({ status: "rejected" as const, error }),
      );
      // The reconnect request and the leave are both on the wire before any reply.
      await vi.waitFor(() => expect(conn.sentRaw).toHaveLength(2));

      await conn.receiveRaw(rawMessage(reconnectAck));
      await initialized;
      await expect(storedRecord()).resolves.toMatchObject({ draftToken: "token-L", capabilityId: "cap-S" });
      expect(storedPointer()).toMatchObject({ capabilityId: "cap-S" });
      expect(loadDraftGuestCapability).toHaveBeenCalledOnce();

      if (variant === "replaced") {
        await saveCapability("token-B", "cap-B");
        await saveOutbox("token-B");
      }
      await conn.receiveRaw(rawMessage({
        type: "draft_leave_ack", draftProtocolVersion: DRAFT_PROTOCOL_VERSION, draftToken: "token-L",
      }));

      await expect(leaveResult).resolves.toEqual({ status: "fulfilled" });
      expect(guest.isRecoveryRevoked).toBe(true);
      expect(loadDraftGuestCapability).toHaveBeenCalledOnce();
      expect(clearDraftGuestRecovery).toHaveBeenCalledExactlyOnceWith({
        hostPeerId: HOST_PEER_ID,
        draftToken: "token-L",
        roomCode: "ABCDE",
        displayName: "Alice",
        capabilityId: "cap-S",
      });
      if (variant === "own") {
        await expect(storedRecord()).resolves.toBeUndefined();
        expect(storedPointer()).toBeNull();
        await expect(storedOutbox()).resolves.toBeUndefined();
      } else {
        await expect(storedRecord()).resolves.toMatchObject({ draftToken: "token-B", capabilityId: "cap-B" });
        expect(storedPointer()).toMatchObject({ capabilityId: "cap-B" });
        await expect(storedOutbox()).resolves.toMatchObject({ draftToken: "token-B" });
      }
      await expect(storedOtherHostRecord()).resolves.toMatchObject({ draftToken: "token-L" });
    },
  );

  it.each(["own", "replaced"] as const)(
    "a host that leaves before the acknowledgement revokes the capability its token owns (%s)",
    async (variant) => {
      if (variant === "own") {
        await saveCapability("token-L", "cap-S");
        await saveOutbox("token-L");
      } else {
        await saveCapability("token-B", "cap-B");
        await saveOutbox("token-B");
      }
      await setOtherHostRecord("token-L");
      const { guest, conn, events } = reconnectGuest("token-L");
      const initialized = expect(guest.initialize(undefined, 1))
        .rejects.toThrow("Draft host disconnected before acknowledging");

      await conn.receiveRaw(rawMessage({ type: "draft_host_left", reason: "Host left" }));
      conn.simulateClose();
      await initialized;

      expect(events).toContainEqual({ type: "hostLeft", reason: "Host left" });
      expect(guest.isRecoveryRevoked).toBe(true);
      expect(loadDraftGuestCapability).toHaveBeenCalledExactlyOnceWith(ownerOf("token-L"));
      if (variant === "own") {
        expect(clearDraftGuestRecovery).toHaveBeenCalledExactlyOnceWith({ ...ownerOf("token-L"), capabilityId: "cap-S" });
        await expect(storedRecord()).resolves.toBeUndefined();
        expect(storedPointer()).toBeNull();
        await expect(storedOutbox()).resolves.toBeUndefined();
      } else {
        expect(clearDraftGuestRecovery).not.toHaveBeenCalled();
        await expect(storedRecord()).resolves.toMatchObject({ draftToken: "token-B", capabilityId: "cap-B" });
        expect(storedPointer()).toMatchObject({ capabilityId: "cap-B" });
        await expect(storedOutbox()).resolves.toMatchObject({ draftToken: "token-B" });
      }
      await expect(storedOtherHostRecord()).resolves.toMatchObject({ draftToken: "token-L" });
    },
  );

  it("a new guest refused before its welcome keeps the seated capability's records", async () => {
    await saveCapability("token-A", "cap-A");
    await saveOutbox("token-A");
    const { guest, conn, events } = createGuest({ kind: "new", roomCode: "ABCDE", displayName: "Bob" });
    const initialized = expect(guest.initialize()).rejects.toThrow("Draft host disconnected before acknowledging");

    await conn.receiveRaw(rawMessage({ type: "draft_kicked", reason: "Draft already in progress" }));
    conn.simulateClose();
    await initialized;

    expect(events).toContainEqual({ type: "kicked", reason: "Draft already in progress" });
    expect(guest.isRecoveryRevoked).toBe(true);
    expect(loadDraftGuestCapability).not.toHaveBeenCalled();
    expect(clearDraftGuestRecovery).not.toHaveBeenCalled();
    await expect(storedRecord()).resolves.toMatchObject({ draftToken: "token-A", capabilityId: "cap-A" });
    expect(storedPointer()).toMatchObject({ capabilityId: "cap-A" });
    await expect(storedOutbox()).resolves.toMatchObject({ draftToken: "token-A" });
  });
});
