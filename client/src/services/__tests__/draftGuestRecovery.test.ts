import "fake-indexeddb/auto";
import { clear, del, get, set, setMany } from "idb-keyval";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { parseWebSocketUrl } from "../../config/multiplayerServer";
import { isUserOwnedStorageKey } from "../../constants/storage";
import { watchPointerStepReads } from "../../test/helpers/pointerStepReads";
import {
  installFifoWebLocks,
  installRefusingWebLocks,
  readIdbGenerationForTests,
  uninstallWebLocks,
} from "../../test/helpers/webLocks";
import { GENERATION_IO_TIMEOUT_MS, VIEW_TIMEOUT_MS } from "../crossTabTransaction";
import { withSavedDeckLibrary } from "../savedDeckTransaction";
import {
  ActiveDraftPointerUnavailableError,
  canonicalNativeDraftEndpoint,
  clearActiveDraftGuest,
  clearActiveDraftGuestIfCurrent,
  clearActiveNativeDraftIfCurrent,
  clearDraftDeckSubmission,
  clearDraftGuestRecovery,
  clearNativeDraftCapability,
  clearUnusableNativeDraft,
  getDraftStore,
  inspectActiveDraftGuest,
  inspectActiveNativeDraft,
  loadActiveDraftGuest,
  loadDraftDeckSubmission,
  loadDraftGuestCapability,
  loadDraftGuestSession,
  loadNativeDraftCapability,
  publishActiveNativeDraft,
  republishActiveDraftGuest,
  republishActiveNativeDraft,
  saveActiveDraftGuest,
  saveDraftDeckSubmission,
  saveDraftGuestSession,
  saveNativeDraftCapability,
  withActiveDraftPointer,
} from "../draftPersistence";
import type {
  ActiveDraftPointerTxn,
  ActiveNativeDraftPointer,
  DraftGuestCapability,
  DraftGuestCapabilityOwner,
  NativeDraftCapabilityRecord,
  NativeDraftPointerCapture,
} from "../draftPersistence";

/**
 * A guest's durable recovery records on real idb-keyval over fake-indexeddb:
 * every clear acts only on the exact capability, and loads never mutate.
 */

const HOST1 = "phase2-HOST1";
const HOST2 = "phase2-HOST2";
const POINTER_KEY = "phase-active-draft-guest";
const FIVE_HOURS_MS = 5 * 60 * 60 * 1000;

/** A capability whose records carry a name. */
type NamedCapability = DraftGuestCapability & { capabilityId: string };

const capA: NamedCapability = {
  hostPeerId: HOST1,
  draftToken: "token-A",
  roomCode: "ABCDE",
  displayName: "Al",
  capabilityId: "cap-A",
};
const ownerA: DraftGuestCapabilityOwner = {
  hostPeerId: HOST1,
  draftToken: "token-A",
  roomCode: "ABCDE",
  displayName: "Al",
};

function guestKey(hostPeerId: string): string {
  return `phase-draft-guest:${hostPeerId}`;
}

function outboxKey(hostPeerId: string): string {
  return `phase-draft-deck-submission:${hostPeerId}`;
}

/** Writes both halves of a named capability through the real writers. */
async function saveCapability(capability: NamedCapability): Promise<void> {
  await saveDraftGuestSession(capability.hostPeerId, {
    draftToken: capability.draftToken,
    seatIndex: 1,
    draftCode: "draft-1",
    roomCode: capability.roomCode,
    displayName: capability.displayName,
    capabilityId: capability.capabilityId,
  });
  await saveActiveDraftGuest({
    roomCode: capability.roomCode,
    displayName: capability.displayName,
    hostPeerId: capability.hostPeerId,
    capabilityId: capability.capabilityId,
  });
}

/** Writes a private record raw, as an earlier build or a corrupt store would. */
async function setRawRecord(hostPeerId: string, fields: Record<string, unknown> = {}): Promise<void> {
  await set(guestKey(hostPeerId), {
    hostPeerId,
    draftToken: "token-A",
    seatIndex: 1,
    draftCode: "draft-1",
    roomCode: "ABCDE",
    displayName: "Al",
    timestamp: Date.now(),
    ...fields,
  }, getDraftStore());
}

/** Writes the public pointer raw. */
function setRawPointer(fields: Record<string, unknown> = {}): void {
  localStorage.setItem(POINTER_KEY, JSON.stringify({
    roomCode: "ABCDE",
    displayName: "Al",
    hostPeerId: HOST1,
    timestamp: Date.now(),
    ...fields,
  }));
}

function storedRecord(hostPeerId: string): Promise<unknown> {
  return get(guestKey(hostPeerId), getDraftStore());
}

function storedPointer(): Record<string, unknown> | null {
  const raw = localStorage.getItem(POINTER_KEY);
  return raw === null ? null : JSON.parse(raw) as Record<string, unknown>;
}

function storedOutbox(hostPeerId: string): Promise<unknown> {
  return get(outboxKey(hostPeerId), getDraftStore());
}

async function saveOutbox(draftToken: string, submissionId: string, roomCode = "ABCDE"): Promise<void> {
  await saveDraftDeckSubmission(HOST1, {
    draftCode: "draft-1",
    roomCode,
    draftToken,
    submissionId,
    mainDeck: ["Island"],
    commanders: [],
  });
}

describe("guest recovery records", () => {
  beforeEach(async () => {
    localStorage.clear();
    await clear(getDraftStore());
  });

  afterEach(() => {
    vi.restoreAllMocks();
  });

  describe("terminal clears", () => {
    // The public pointer carries a name and never the token, so a pointer
    // naming `cap-A` is `cap-A`'s whichever token's record sits behind it.
    it.each<[string, NamedCapability, "kept" | "removed"]>([
      ["record of another token", { ...capA, draftToken: "token-B", capabilityId: "cap-B" }, "kept"],
      ["record of the same token under another name", { ...capA, capabilityId: "cap-X" }, "kept"],
      ["record with the same name on another token", { ...capA, draftToken: "token-B" }, "removed"],
      ["record of another display name", { ...capA, displayName: "Alicia" }, "kept"],
    ])("a terminal clear keeps a %s", async (_label, stored, pointerOutcome) => {
      await saveCapability(stored);
      await setRawRecord(HOST2, { capabilityId: "cap-A" });

      await clearDraftGuestRecovery(capA);

      await expect(loadDraftGuestSession(HOST1)).resolves.toMatchObject({
        draftToken: stored.draftToken,
        displayName: stored.displayName,
        capabilityId: stored.capabilityId,
      });
      if (pointerOutcome === "kept") {
        expect(storedPointer()).toMatchObject({ capabilityId: stored.capabilityId, displayName: stored.displayName });
      } else {
        expect(storedPointer()).toBeNull();
      }
      await expect(storedRecord(HOST2)).resolves.toMatchObject({ draftToken: "token-A", capabilityId: "cap-A" });

      await clearDraftGuestRecovery(stored);

      await expect(storedRecord(HOST1)).resolves.toBeUndefined();
      expect(storedPointer()).toBeNull();
      await expect(storedRecord(HOST2)).resolves.toMatchObject({ draftToken: "token-A", capabilityId: "cap-A" });
    });

    it("a terminal clear removes its own capability's pair", async () => {
      await saveCapability(capA);
      expect(inspectActiveDraftGuest().type).toBe("present");
      await expect(loadDraftGuestSession(HOST1)).resolves.toMatchObject({ draftToken: "token-A" });

      await clearDraftGuestRecovery({ ...capA, roomCode: " abcde ", displayName: " Al " });

      await expect(storedRecord(HOST1)).resolves.toBeUndefined();
      await expect(loadDraftGuestSession(HOST1)).resolves.toBeNull();
      expect(storedPointer()).toBeNull();
      expect(loadActiveDraftGuest()).toBeNull();
    });

    interface PointerCase {
      seed: () => Promise<void>;
      capability: DraftGuestCapability;
      privateRecordAfter: "absent" | "kept";
      pointerAfter: "absent" | "kept";
      /** The capability whose clear removes what the first clear kept. */
      thenClearedBy?: DraftGuestCapability;
    }

    it.each<[string, PointerCase]>([
      ["a named pointer of another capability", {
        seed: async () => {
          await setRawRecord(HOST1, { capabilityId: "cap-A" });
          setRawPointer({ capabilityId: "cap-B" });
        },
        capability: capA,
        privateRecordAfter: "absent",
        pointerAfter: "kept",
      }],
      ["an unnamed pointer and the capability's own unnamed record", {
        seed: async () => {
          await setRawRecord(HOST1);
          setRawPointer();
        },
        capability: { ...capA, capabilityId: null },
        privateRecordAfter: "absent",
        pointerAfter: "absent",
      }],
      ["an unnamed pointer and another token's unnamed record", {
        seed: async () => {
          await setRawRecord(HOST1, { draftToken: "token-B" });
          setRawPointer();
        },
        capability: { ...capA, capabilityId: null },
        privateRecordAfter: "kept",
        pointerAfter: "kept",
        thenClearedBy: { ...capA, draftToken: "token-B", capabilityId: null },
      }],
      ["an unnamed pointer and the capability's own named record", {
        seed: async () => {
          await setRawRecord(HOST1, { capabilityId: "cap-A" });
          setRawPointer();
        },
        capability: capA,
        privateRecordAfter: "absent",
        pointerAfter: "kept",
      }],
      ["a named pointer with no private record", {
        seed: async () => {
          setRawPointer({ capabilityId: "cap-A" });
        },
        capability: capA,
        privateRecordAfter: "absent",
        pointerAfter: "absent",
      }],
    ])("removes a pointer only when it names the same capability: %s", async (_label, testCase) => {
      await testCase.seed();
      const pointerBefore = localStorage.getItem(POINTER_KEY);

      await clearDraftGuestRecovery(testCase.capability);

      if (testCase.privateRecordAfter === "absent") {
        await expect(storedRecord(HOST1)).resolves.toBeUndefined();
      } else {
        await expect(storedRecord(HOST1)).resolves.toMatchObject({ hostPeerId: HOST1 });
      }
      if (testCase.pointerAfter === "absent") {
        expect(localStorage.getItem(POINTER_KEY)).toBeNull();
      } else {
        expect(localStorage.getItem(POINTER_KEY)).toBe(pointerBefore);
      }

      if (testCase.thenClearedBy) {
        await clearDraftGuestRecovery(testCase.thenClearedBy);
        await expect(storedRecord(HOST1)).resolves.toBeUndefined();
        expect(localStorage.getItem(POINTER_KEY)).toBeNull();
      }
    });

    interface UntouchedCase {
      seed: () => Promise<void>;
      snapshot: () => Promise<unknown>;
      seedTwin: () => Promise<void>;
      twinRemoved: () => Promise<boolean>;
    }

    const pointerCase = (raw: string): UntouchedCase => ({
      seed: async () => {
        localStorage.setItem(POINTER_KEY, raw);
      },
      snapshot: async () => localStorage.getItem(POINTER_KEY),
      seedTwin: async () => {
        saveActiveDraftGuest({ roomCode: "ABCDE", displayName: "Al", hostPeerId: HOST1, capabilityId: "cap-A" });
      },
      twinRemoved: async () => localStorage.getItem(POINTER_KEY) === null,
    });
    const recordCase = (fields: Record<string, unknown>): UntouchedCase => ({
      seed: () => setRawRecord(HOST1, fields),
      snapshot: () => storedRecord(HOST1),
      seedTwin: () => setRawRecord(HOST1, { capabilityId: "cap-A" }),
      twinRemoved: async () => (await storedRecord(HOST1)) === undefined,
    });

    it.each<[string, UntouchedCase]>([
      ["a malformed pointer", pointerCase("{not-json")],
      ["a pointer with a non-string capability name", pointerCase(JSON.stringify({
        roomCode: "ABCDE", displayName: "Al", hostPeerId: HOST1, timestamp: Date.now(), capabilityId: 42,
      }))],
      ["a private record with an empty capability name", recordCase({ capabilityId: "" })],
      ["an expired own private record", recordCase({ capabilityId: "cap-A", timestamp: Date.now() - FIVE_HOURS_MS })],
      ["an expired own named pointer", pointerCase(JSON.stringify({
        roomCode: "ABCDE", displayName: "Al", hostPeerId: HOST1,
        timestamp: Date.now() - FIVE_HOURS_MS, capabilityId: "cap-A",
      }))],
    ])("a terminal clear leaves %s as it is", async (_label, testCase) => {
      await testCase.seed();
      const before = await testCase.snapshot();
      expect(before).not.toBeNull();
      expect(before).not.toBeUndefined();

      await clearDraftGuestRecovery(capA);

      expect(await testCase.snapshot()).toEqual(before);

      await testCase.seedTwin();
      await clearDraftGuestRecovery(capA);
      expect(await testCase.twinRemoved()).toBe(true);
    });

    it("a terminal outbox clear keeps another capability's outbox", async () => {
      await saveOutbox("token-B", "sub-1");
      await clearDraftDeckSubmission(ownerA);
      await expect(storedOutbox(HOST1)).resolves.toMatchObject({ draftToken: "token-B", submissionId: "sub-1" });

      const malformed = {
        hostPeerId: HOST1,
        draftCode: "draft-1",
        roomCode: "ABCDE",
        draftToken: "token-A",
        submissionId: "sub-1",
        mainDeck: ["Island"],
        timestamp: Date.now(),
      };
      await set(outboxKey(HOST1), malformed, getDraftStore());
      await clearDraftDeckSubmission(ownerA);
      await expect(storedOutbox(HOST1)).resolves.toEqual(malformed);

      await saveOutbox("token-A", "sub-1");
      await clearDraftDeckSubmission(ownerA);
      await expect(storedOutbox(HOST1)).resolves.toBeUndefined();
    });
  });

  describe("per-submission clears", () => {
    it("a per-submission clear removes only this capability's submission", async () => {
      await saveOutbox("token-B", "sub-1");
      await clearDraftDeckSubmission(ownerA, "sub-1");
      await expect(storedOutbox(HOST1)).resolves.toMatchObject({ draftToken: "token-B", submissionId: "sub-1" });

      await saveOutbox("token-A", "sub-1");
      await clearDraftDeckSubmission({ ...ownerA, roomCode: "FGHJK" }, "sub-1");
      await expect(storedOutbox(HOST1)).resolves.toMatchObject({ draftToken: "token-A", submissionId: "sub-1" });

      await clearDraftDeckSubmission(ownerA, "sub-1");
      await expect(storedOutbox(HOST1)).resolves.toBeUndefined();

      // The replacement is written while the clear is in flight: the clear
      // reads and deletes in one transaction, so the replacement survives.
      await saveOutbox("token-A", "sub-1");
      const deleteSpy = vi.spyOn(IDBObjectStore.prototype, "delete");
      const clearing = clearDraftDeckSubmission(ownerA, "sub-1");
      const replacing = saveOutbox("token-B", "sub-2");
      await Promise.all([clearing, replacing]);

      expect(deleteSpy).toHaveBeenCalledExactlyOnceWith(outboxKey(HOST1));
      await expect(storedOutbox(HOST1)).resolves.toMatchObject({ draftToken: "token-B", submissionId: "sub-2" });
    });

    it("canonicalizes a deck submission room code for replay and removal", async () => {
      await saveDraftDeckSubmission(HOST1, {
        draftCode: "draft-xyz",
        roomCode: " abcde ",
        draftToken: "token-abc",
        submissionId: "submission-1",
        mainDeck: ["Island"],
        commanders: ["Kenrith, the Returned King"],
      });

      await expect(loadDraftDeckSubmission(HOST1, {
        roomCode: "abcde",
        draftToken: "token-abc",
      })).resolves.toMatchObject({ roomCode: "ABCDE" });
      await clearDraftDeckSubmission({ hostPeerId: HOST1, roomCode: " abcde ", draftToken: "token-abc" }, "submission-1");
      await expect(loadDraftDeckSubmission(HOST1)).resolves.toBeNull();
    });
  });

  describe("loads", () => {
    it("loads leave expired and malformed records in place", async () => {
      await setRawRecord(HOST1, { capabilityId: "cap-A" });
      await expect(loadDraftGuestSession(HOST1)).resolves.toMatchObject({ draftToken: "token-A" });
      await expect(loadDraftGuestCapability(ownerA)).resolves.toEqual(capA);

      await setRawRecord(HOST1);
      await expect(loadDraftGuestCapability(ownerA)).resolves.toEqual({ ...capA, capabilityId: null });

      await setRawRecord(HOST1, { capabilityId: "cap-A", timestamp: Date.now() - FIVE_HOURS_MS });
      const expired = await storedRecord(HOST1);
      await expect(loadDraftGuestSession(HOST1)).resolves.toBeNull();
      await expect(loadDraftGuestCapability(ownerA)).resolves.toBeNull();
      await expect(storedRecord(HOST1)).resolves.toEqual(expired);

      for (const fields of [{ capabilityId: "" }, { seatIndex: -1 }, { draftToken: "token-B" }]) {
        await setRawRecord(HOST1, fields);
        const before = await storedRecord(HOST1);
        await expect(loadDraftGuestCapability(ownerA)).resolves.toBeNull();
        await expect(storedRecord(HOST1)).resolves.toEqual(before);
      }

      const removeItem = vi.spyOn(localStorage, "removeItem");
      localStorage.setItem(POINTER_KEY, "{not-json");
      expect(inspectActiveDraftGuest()).toEqual({ type: "invalid", capture: null });
      expect(loadActiveDraftGuest()).toBeNull();
      expect(localStorage.getItem(POINTER_KEY)).toBe("{not-json");

      setRawPointer({ capabilityId: "cap-A", timestamp: Date.now() - FIVE_HOURS_MS });
      const expiredPointer = localStorage.getItem(POINTER_KEY);
      const inspected = inspectActiveDraftGuest();
      expect(inspected.type).toBe("invalid");
      expect(inspected.type === "invalid" ? inspected.capture : null).toMatchObject({ capabilityId: "cap-A" });
      expect(loadActiveDraftGuest()).toBeNull();
      expect(localStorage.getItem(POINTER_KEY)).toBe(expiredPointer);
      expect(removeItem).not.toHaveBeenCalledWith(POINTER_KEY);
    });

    it.each([
      ["malformed", "{not-json"],
      ["expired", JSON.stringify({
        roomCode: "ABCDE",
        displayName: "Alice",
        hostPeerId: HOST1,
        timestamp: Date.now() - FIVE_HOURS_MS,
      })],
    ])("keeps a %s guest locator intact during inspection, then load leaves it too", (_label, raw) => {
      const removeItem = vi.spyOn(localStorage, "removeItem");
      localStorage.setItem(POINTER_KEY, raw);

      const inspected = inspectActiveDraftGuest();

      expect(inspected.type).toBe("invalid");
      expect(localStorage.getItem(POINTER_KEY)).toBe(raw);

      expect(loadActiveDraftGuest()).toBeNull();
      expect(localStorage.getItem(POINTER_KEY)).toBe(raw);
      expect(removeItem).not.toHaveBeenCalledWith(POINTER_KEY);
    });

    it("binds a pointer to the private record it names", async () => {
      await saveCapability(capA);

      await expect(loadDraftGuestSession(HOST1, {
        roomCode: "ABCDE", displayName: "Al", capabilityId: "cap-A",
      })).resolves.toMatchObject({ draftToken: "token-A" });
      await expect(loadDraftGuestSession(HOST1, {
        roomCode: "ABCDE", displayName: "Al", capabilityId: "cap-X",
      })).resolves.toBeNull();
      await expect(loadDraftGuestSession(HOST1, {
        roomCode: "ABCDE", displayName: "Al",
      })).resolves.toMatchObject({ draftToken: "token-A" });
    });

    it("refuses a token record stored under a different host peer key", async () => {
      await set(guestKey("phase2-EXPECTED"), {
        hostPeerId: "phase2-OTHER",
        draftToken: "cross-key-token",
        seatIndex: 1,
        draftCode: "draft-xyz",
        roomCode: "ABCDE",
        displayName: "Alice",
        timestamp: Date.now(),
      }, getDraftStore());

      await expect(loadDraftGuestSession("phase2-EXPECTED", {
        roomCode: "ABCDE",
        displayName: "Alice",
      })).resolves.toBeNull();
    });

    it("returns null for non-existent session", async () => {
      await expect(loadDraftGuestSession("nonexistent")).resolves.toBeNull();
    });
  });

  describe("writers", () => {
    it("writes the capability name to both halves", async () => {
      await expect(saveDraftGuestSession(HOST2, {
        draftToken: "token-A",
        seatIndex: 1,
        draftCode: "draft-1",
        roomCode: "ABCDE",
        displayName: "Al",
        capabilityId: "",
      })).rejects.toThrow("Draft recovery capability is required");
      await expect(storedRecord(HOST2)).resolves.toBeUndefined();
      saveActiveDraftGuest({ roomCode: "ABCDE", displayName: "Al", hostPeerId: HOST1, capabilityId: "" });
      expect(localStorage.getItem(POINTER_KEY)).toBeNull();

      await saveCapability(capA);

      await expect(storedRecord(HOST1)).resolves.toMatchObject({ draftToken: "token-A", capabilityId: "cap-A" });
      expect(storedPointer()).toMatchObject({ hostPeerId: HOST1, capabilityId: "cap-A" });
      expect(localStorage.getItem(POINTER_KEY)).not.toContain("token-A");
    });

    it("saves and loads a guest session", async () => {
      await saveDraftGuestSession(HOST1, {
        draftToken: "token-abc",
        seatIndex: 3,
        draftCode: "draft-xyz",
        roomCode: "ABCDE",
        displayName: "Alice",
        capabilityId: "cap-abc",
      });

      const loaded = await loadDraftGuestSession(HOST1);
      expect(loaded).not.toBeNull();
      expect(loaded!.draftToken).toBe("token-abc");
      expect(loaded!.seatIndex).toBe(3);
      expect(loaded!.draftCode).toBe("draft-xyz");
      expect(loaded!.hostPeerId).toBe(HOST1);
      expect(loaded!.roomCode).toBe("ABCDE");
      expect(loaded!.displayName).toBe("Alice");
      expect(loaded!.capabilityId).toBe("cap-abc");
    });
  });

  describe("records written before capability names", () => {
    it("reads records written before capability names", async () => {
      await setRawRecord(HOST1);
      setRawPointer();

      const active = inspectActiveDraftGuest();
      expect(active.type).toBe("present");
      if (active.type !== "present") throw new Error("expected guest locator");
      expect(active.capture.capabilityId).toBeNull();
      await expect(loadDraftGuestSession(HOST1, active.meta)).resolves.toMatchObject({ draftToken: "token-A" });
    });

    it("uses a non-secret room-code locator to bind the IndexedDB capability", async () => {
      await saveDraftGuestSession(HOST1, {
        draftToken: "token-abc",
        seatIndex: 3,
        draftCode: "draft-xyz",
        roomCode: "ABCDE",
        displayName: "Alice",
        capabilityId: "cap-abc",
      });
      saveActiveDraftGuest({ roomCode: "ABCDE", displayName: "Alice", hostPeerId: HOST1, capabilityId: "cap-abc" });

      expect(loadActiveDraftGuest()).toMatchObject({
        roomCode: "ABCDE",
        displayName: "Alice",
        hostPeerId: HOST1,
      });
      expect(localStorage.getItem(POINTER_KEY)).not.toContain("token-abc");
      await expect(loadDraftGuestSession(HOST1, {
        roomCode: "ABCDE",
        displayName: "Alice",
      })).resolves.toMatchObject({ draftToken: "token-abc" });
      await expect(loadDraftGuestSession(HOST1, {
        roomCode: "ABCDE",
        displayName: "Mallory",
      })).resolves.toBeNull();
    });

    it("does not clear a same-timestamp locator whose display identity changed", () => {
      const timestamp = Date.now();
      localStorage.setItem(POINTER_KEY, JSON.stringify({
        roomCode: "ABCDE",
        displayName: "Alice",
        hostPeerId: HOST1,
        timestamp,
      }));
      const active = inspectActiveDraftGuest();
      if (active.type !== "present") throw new Error("expected guest locator");

      localStorage.setItem(POINTER_KEY, JSON.stringify({
        roomCode: "ABCDE",
        displayName: "Alicia",
        hostPeerId: HOST1,
        timestamp,
      }));
      clearActiveDraftGuestIfCurrent(active.capture);

      expect(inspectActiveDraftGuest()).toMatchObject({
        type: "present",
        meta: { displayName: "Alicia", timestamp },
      });
    });
  });

  describe("pointer captures", () => {
    it("keeps a same-timestamp pointer that names another capability", () => {
      const timestamp = Date.now();
      setRawPointer({ timestamp, capabilityId: "cap-A" });
      const captured = inspectActiveDraftGuest();
      if (captured.type !== "present") throw new Error("expected guest locator");

      setRawPointer({ timestamp, capabilityId: "cap-B" });
      clearActiveDraftGuestIfCurrent(captured.capture);

      expect(inspectActiveDraftGuest()).toMatchObject({ type: "present", meta: { capabilityId: "cap-B", timestamp } });

      const current = inspectActiveDraftGuest();
      if (current.type !== "present") throw new Error("expected guest locator");
      clearActiveDraftGuestIfCurrent(current.capture);
      expect(localStorage.getItem(POINTER_KEY)).toBeNull();
    });

    it("inspects guest locators without mutation and only clears the exact captured locator", () => {
      saveActiveDraftGuest({ roomCode: "ABCDE", displayName: "Alice", hostPeerId: HOST1, capabilityId: "cap-1" });
      const active = inspectActiveDraftGuest();
      expect(active.type).toBe("present");
      expect(localStorage.getItem(POINTER_KEY)).not.toBeNull();
      if (active.type !== "present") throw new Error("expected guest locator");

      saveActiveDraftGuest({ roomCode: "FGHJK", displayName: "Bob", hostPeerId: HOST2, capabilityId: "cap-2" });
      clearActiveDraftGuestIfCurrent(active.capture);

      expect(inspectActiveDraftGuest()).toMatchObject({
        type: "present",
        meta: { roomCode: "FGHJK", hostPeerId: HOST2 },
      });
    });
  });

  describe("native draft capabilities", () => {
    const ENDPOINT = "wss://h.example/ws";
    const capC: Omit<NativeDraftCapabilityRecord, "timestamp"> = {
      capabilityId: "cap-C",
      endpoint: ENDPOINT,
      draftCode: "NAT001",
      playerToken: "tok-C",
      seatIndex: 2,
      participant: "guest",
    };

    function nativeKey(capabilityId: string): string {
      return `phase-native-draft-capability:${capabilityId}`;
    }

    function storedNative(capabilityId: string): Promise<unknown> {
      return get(nativeKey(capabilityId), getDraftStore());
    }

    /** A native pointer naming `capabilityId`, as the installed owner publishes it. */
    function nativePointer(fields: Partial<ActiveNativeDraftPointer> = {}): ActiveNativeDraftPointer {
      return {
        transport: "native",
        capabilityId: "cap-C",
        endpoint: ENDPOINT,
        draftCode: "NAT001",
        seatIndex: 2,
        participant: "guest",
        timestamp: Date.now(),
        ...fields,
      };
    }

    it("canonicalizes a native endpoint once", () => {
      const accepted: [string, string][] = [
        ["wss://h.example/ws", "wss://h.example/ws"],
        ["ws://h:9374", "ws://h:9374/"],
        [" wss://h.example/ws ", "wss://h.example/ws"],
        ["wss://H.EXAMPLE:443/ws", "wss://h.example/ws"],
      ];
      for (const [input, canonical] of accepted) {
        const output = canonicalNativeDraftEndpoint(input);
        // Reach: the output is the socket authority's own serialization.
        expect(output).toBe(parseWebSocketUrl(input.trim())!.href);
        expect(output).toBe(canonical);
        expect(canonicalNativeDraftEndpoint(output!)).toBe(output);
      }
      for (const refused of ["wss://u:p@h/", "wss://h/?t=1", "wss://h/?", "wss://h/#", "https://h/", " "]) {
        expect(canonicalNativeDraftEndpoint(refused)).toBeNull();
      }
    });

    it("commits the native secret half and returns a pointer without it", async () => {
      const pointer = await saveNativeDraftCapability(capC);

      // Reach: the record read back is the input, stamped.
      await expect(storedNative("cap-C")).resolves.toEqual({ ...capC, timestamp: pointer.timestamp });
      expect(Object.keys(pointer).sort()).toEqual([
        "capabilityId", "draftCode", "endpoint", "participant", "seatIndex", "timestamp", "transport",
      ]);
      expect(pointer).toMatchObject({ transport: "native", capabilityId: "cap-C", endpoint: ENDPOINT });
      expect(localStorage.getItem(POINTER_KEY)).toBeNull();

      publishActiveNativeDraft(pointer);
      const published = localStorage.getItem(POINTER_KEY);
      expect(published).not.toBeNull();
      expect(published).not.toContain("tok-C");
      expect(JSON.parse(published!)).toEqual(pointer);

      // Hostile: refused input throws before anything is written.
      await expect(saveNativeDraftCapability({ ...capC, capabilityId: "cap-D", endpoint: "ws://h:9374" }))
        .rejects.toThrow();
      await expect(saveNativeDraftCapability({ ...capC, capabilityId: "" })).rejects.toThrow();
      await expect(storedNative("cap-D")).resolves.toBeUndefined();
      await expect(storedNative("")).resolves.toBeUndefined();
    });

    it("reads untagged pointers as P2P and leaves a tagged one to its own reader", () => {
      setRawPointer({ capabilityId: "cap-A" });
      expect(inspectActiveDraftGuest().type).toBe("present");
      expect(inspectActiveNativeDraft()).toEqual({ type: "absent" });

      publishActiveNativeDraft(nativePointer());
      expect(inspectActiveDraftGuest()).toEqual({ type: "absent" });
      expect(inspectActiveNativeDraft()).toMatchObject({ type: "present", pointer: { capabilityId: "cap-C" } });

      setRawPointer({ transport: "other" });
      expect(inspectActiveDraftGuest()).toEqual({ type: "absent" });
      expect(inspectActiveNativeDraft()).toEqual({ type: "absent" });

      localStorage.setItem(POINTER_KEY, JSON.stringify({ transport: "native", capabilityId: "cap-C" }));
      expect(inspectActiveDraftGuest()).toEqual({ type: "absent" });
      expect(inspectActiveNativeDraft()).toMatchObject({ type: "invalid" });
    });

    it("native loads never mutate storage", async () => {
      const legs: [Record<string, unknown>, string][] = [
        [{ ...capC, timestamp: Date.now() - FIVE_HOURS_MS }, "invalid"],
        [{ ...capC, playerToken: "", timestamp: Date.now() }, "invalid"],
        [{ ...capC, draftCode: "NAT999", timestamp: Date.now() }, "mismatch"],
      ];
      for (const [record, outcome] of legs) {
        await set(nativeKey("cap-C"), record, getDraftStore());
        const before = await storedNative("cap-C");

        await expect(loadNativeDraftCapability(nativePointer())).resolves.toEqual({ type: outcome });

        await expect(storedNative("cap-C")).resolves.toEqual(before);
      }
    });

    it("clears only this native capability", async () => {
      const hold = { capabilityId: "cap-C", endpoint: ENDPOINT, draftCode: "NAT001", playerToken: "tok-C" };

      // Its own pair.
      publishActiveNativeDraft(await saveNativeDraftCapability(capC));
      await expect(storedNative("cap-C")).resolves.toMatchObject({ playerToken: "tok-C" });
      await clearNativeDraftCapability(hold);
      await expect(storedNative("cap-C")).resolves.toBeUndefined();
      expect(localStorage.getItem(POINTER_KEY)).toBeNull();

      // Its record, with a pointer naming another capability.
      await saveNativeDraftCapability(capC);
      publishActiveNativeDraft(nativePointer({ capabilityId: "cap-D" }));
      const pointerD = localStorage.getItem(POINTER_KEY);
      await clearNativeDraftCapability(hold);
      await expect(storedNative("cap-C")).resolves.toBeUndefined();
      expect(localStorage.getItem(POINTER_KEY)).toBe(pointerD);

      // Its name under another token, with its pointer.
      publishActiveNativeDraft(await saveNativeDraftCapability({ ...capC, playerToken: "tok-X" }));
      await clearNativeDraftCapability(hold);
      await expect(storedNative("cap-C")).resolves.toMatchObject({ playerToken: "tok-X" });
      expect(localStorage.getItem(POINTER_KEY)).toBeNull();
    });

    it("a native republication writes only over the pointer it captured", async () => {
      /** Stores cap-C's record and pointer, and returns the pointer's capture. */
      const storeAndCapture = async () => {
        const pointer = await saveNativeDraftCapability(capC);
        publishActiveNativeDraft(pointer);
        const active = inspectActiveNativeDraft();
        if (active.type !== "present") throw new Error("expected a native pointer");
        return { pointer, capture: active.capture };
      };

      // Over its own capture: written in the call's own step, record kept.
      {
        const { pointer, capture } = await storeAndCapture();
        const refreshed = nativePointer({ timestamp: pointer.timestamp + 1_000 });
        const result = await republishActiveNativeDraft(capture, refreshed);
        expect(result.status).toBe("published");
        expect(JSON.parse(localStorage.getItem(POINTER_KEY)!)).toEqual(refreshed);
        await result.settled;
        await expect(storedNative("cap-C")).resolves.toMatchObject({ capabilityId: "cap-C" });
      }

      // Over another writer's pointer: that pointer and its record stay, and
      // this capability's record goes once no pointer names it.
      {
        localStorage.clear();
        await clear(getDraftStore());
        const { pointer, capture } = await storeAndCapture();
        await saveNativeDraftCapability({ ...capC, capabilityId: "cap-D", playerToken: "tok-D" });
        const recordD = await storedNative("cap-D");
        const otherText = JSON.stringify(nativePointer({ capabilityId: "cap-D" }));
        localStorage.setItem(POINTER_KEY, otherText);
        const result = await republishActiveNativeDraft(capture, nativePointer({ timestamp: pointer.timestamp + 1_000 }));
        expect(result.status).toBe("superseded");
        expect(localStorage.getItem(POINTER_KEY)).toBe(otherText);
        await result.settled;
        await expect(storedNative("cap-C")).resolves.toBeUndefined();
        await expect(storedNative("cap-D")).resolves.toEqual(recordD);
        expect(localStorage.getItem(POINTER_KEY)).toBe(otherText);
      }

      // Over the same capability's newer pointer: that pointer stays, and so does the record it names.
      {
        localStorage.clear();
        await clear(getDraftStore());
        const { pointer, capture } = await storeAndCapture();
        const newerText = JSON.stringify(nativePointer({ timestamp: pointer.timestamp + 500 }));
        localStorage.setItem(POINTER_KEY, newerText);
        const result = await republishActiveNativeDraft(capture, nativePointer({ timestamp: pointer.timestamp + 1_000 }));
        expect(result.status).toBe("superseded");
        await result.settled;
        expect(localStorage.getItem(POINTER_KEY)).toBe(newerText);
        await expect(storedNative("cap-C")).resolves.toMatchObject({ capabilityId: "cap-C" });
      }

      // Hostile: a pointer naming another capability than the capture, or one
      // that does not validate, throws and writes nothing.
      {
        localStorage.clear();
        await clear(getDraftStore());
        const { capture } = await storeAndCapture();
        await expect(republishActiveNativeDraft(capture, nativePointer({ capabilityId: "cap-D" }))).rejects.toThrow();
        await expect(republishActiveNativeDraft(capture, nativePointer({ endpoint: "ws://h:9374" }))).rejects.toThrow();
        expect(localStorage.getItem(POINTER_KEY)).toBe(capture.raw);
      }
    });

    it("a guest republication writes only over the pointer it stands on", async () => {
      const publication = { roomCode: "ABCDE", displayName: "Al", hostPeerId: HOST1, capabilityId: "cap-A" };
      const timestamp = Date.now() - 1_000;
      const standsOn = { roomCode: "ABCDE", displayName: "Al", hostPeerId: HOST1, timestamp, capabilityId: "cap-A" };

      // Over an equal pointer: written, with a new timestamp, and its capture returned.
      setRawPointer({ timestamp, capabilityId: "cap-A" });
      const written = await republishActiveDraftGuest(standsOn, publication);
      expect(written).toMatchObject({ roomCode: "ABCDE", hostPeerId: HOST1, capabilityId: "cap-A" });
      expect(written!.timestamp).toBeGreaterThan(timestamp);
      const active = inspectActiveDraftGuest();
      expect(active.type === "present" && active.capture).toEqual(written);

      // Over a pointer differing only in its capability: refused, unchanged.
      setRawPointer({ timestamp, capabilityId: "cap-B" });
      const otherGuest = localStorage.getItem(POINTER_KEY);
      await expect(republishActiveDraftGuest(standsOn, publication)).resolves.toBeNull();
      expect(localStorage.getItem(POINTER_KEY)).toBe(otherGuest);

      // Over a native pointer: refused, and the pointer and its record stay.
      publishActiveNativeDraft(await saveNativeDraftCapability(capC));
      const nativeText = localStorage.getItem(POINTER_KEY);
      const nativeRecord = await storedNative("cap-C");
      await expect(republishActiveDraftGuest(standsOn, publication)).resolves.toBeNull();
      expect(localStorage.getItem(POINTER_KEY)).toBe(nativeText);
      await expect(storedNative("cap-C")).resolves.toEqual(nativeRecord);

      // Over no stored pointer: refused, nothing written.
      localStorage.removeItem(POINTER_KEY);
      await expect(republishActiveDraftGuest(standsOn, publication)).resolves.toBeNull();
      expect(localStorage.getItem(POINTER_KEY)).toBeNull();
    });

    describe("the cross-tab recovery-pointer transaction", () => {
      const LOCK = "phase-active-draft-pointer";
      const GENERATION_KEY = "phase-active-draft-pointer-generation";
      const RECORD_KEY = "phase-active-draft-pointer-record";
      const capB: Omit<NativeDraftCapabilityRecord, "timestamp"> = {
        ...capC,
        capabilityId: "cap-B",
        draftCode: "NAT002",
        playerToken: "tok-B",
        seatIndex: 1,
      };

      type PersistenceModule = typeof import("../draftPersistence");

      /**
       * Another same-origin tab: its own module graph of the persistence
       * authority, so its own runner and capability, sharing only what tabs
       * share (localStorage, IndexedDB and the lock manager).
       */
      async function otherTab(): Promise<PersistenceModule> {
        vi.resetModules();
        const tab = await vi.importActual<PersistenceModule>("../draftPersistence");
        // Reach: a distinct graph, not this one.
        expect(tab.publishActiveNativeDraft).not.toBe(publishActiveNativeDraft);
        expect(tab.getDraftStore).not.toBe(getDraftStore);
        return tab;
      }

      /** Runs `onRead` at the `nth` read of the pointer key from now, which still returns what it read. */
      function atPointerRead(nth: number, onRead: () => void): { fired: () => number } {
        const getItem = localStorage.getItem.bind(localStorage);
        let reads = 0;
        let fired = 0;
        vi.spyOn(localStorage, "getItem").mockImplementation((key: string) => {
          const seen = getItem(key);
          if (key === POINTER_KEY && fired === 0 && ++reads === nth) {
            fired += 1;
            onRead();
          }
          return seen;
        });
        return { fired: () => fired };
      }

      /** Counts removals of the pointer key from now, in `events` order. */
      function recordPointerRemovals(events: string[]): () => number {
        const removeItem = localStorage.removeItem.bind(localStorage);
        let removals = 0;
        vi.spyOn(localStorage, "removeItem").mockImplementation((key: string) => {
          if (key === POINTER_KEY) {
            removals += 1;
            events.push("remove");
          }
          removeItem(key);
        });
        return () => removals;
      }

      /** Records how `promise` settles, so a test can read "still pending" without awaiting it. */
      function track(promise: Promise<unknown>): () => "pending" | "resolved" | "rejected" {
        let state: "pending" | "resolved" | "rejected" = "pending";
        promise.then(
          () => {
            state = "resolved";
          },
          () => {
            state = "rejected";
          },
        );
        return () => state;
      }

      /**
       * Refuses a step's result write: a put of the record key in a transaction that has not
       * written the generation key (a write-ahead writes both in one transaction). "throw" throws
       * from the put; "abort" issues the put and then aborts its transaction, as a write that fails
       * while committing does. Refuses the first `times` such puts, or every one until restored.
       */
      function refuseResultWrites(
        times: number | "every",
        shape: "throw" | "abort",
      ): { attempts: () => number; refused: () => number; aborted: () => number; restore: () => void } {
        const put = IDBObjectStore.prototype.put;
        const writesAhead = new WeakSet<IDBTransaction>();
        let attempts = 0;
        let refused = 0;
        let aborted = 0;
        const spy = vi.spyOn(IDBObjectStore.prototype, "put").mockImplementation(function (
          this: IDBObjectStore,
          value: unknown,
          key?: IDBValidKey,
        ) {
          if (key === GENERATION_KEY) writesAhead.add(this.transaction);
          if (key !== RECORD_KEY || writesAhead.has(this.transaction)) return put.call(this, value, key);
          attempts += 1;
          if (times !== "every" && refused >= times) return put.call(this, value, key);
          refused += 1;
          if (shape === "throw") throw new DOMException("refused", "UnknownError");
          const { transaction } = this;
          transaction.addEventListener("abort", () => {
            aborted += 1;
          });
          const request = put.call(this, value, key);
          transaction.abort();
          return request;
        });
        let restored = false;
        return {
          attempts: () => attempts,
          refused: () => refused,
          aborted: () => aborted,
          restore: () => {
            if (restored) return;
            restored = true;
            spy.mockRestore();
          },
        };
      }

      /** Counts pointer steps' entries into their view wait: registrations of a `storage` listener, from now. */
      function enteredViewWait(): () => number {
        const addEventListener = window.addEventListener.bind(window);
        let waits = 0;
        vi.spyOn(window, "addEventListener").mockImplementation((...args: Parameters<typeof window.addEventListener>) => {
          if (args[0] === "storage") waits += 1;
          addEventListener(...args);
        });
        return () => waits;
      }

      /**
       * Lets real tasks run, IndexedDB's among them, until `reached()` holds or a bound passes,
       * without moving faked time: a fake-timer advance made before a step's IndexedDB read has
       * settled would fire that read's own bound first.
       */
      async function untilReached(reached: () => boolean): Promise<void> {
        for (let round = 0; round < 1_000 && !reached(); round += 1) {
          await new Promise<void>((resolve) => setImmediate(resolve));
        }
      }

      /**
       * Runs `commit`, another context's step, and then holds `keys` at the values this tab's view
       * showed before it, as a view that context's writes have not reached. `deliver()` lets them
       * arrive: each key takes the other context's value unless this tab has written it since.
       */
      async function withholdFromView(keys: readonly string[], commit: () => Promise<void>): Promise<() => void> {
        const before = keys.map((key) => [key, localStorage.getItem(key)] as const);
        await commit();
        const withheld = keys.map((key) => [key, localStorage.getItem(key)] as const);
        const write = (key: string, value: string | null) => {
          if (value === null) localStorage.removeItem(key);
          else localStorage.setItem(key, value);
        };
        for (const [key, value] of before) write(key, value);
        const written = new Set<string>();
        const setItem = localStorage.setItem.bind(localStorage);
        const removeItem = localStorage.removeItem.bind(localStorage);
        const setSpy = vi.spyOn(localStorage, "setItem").mockImplementation((key: string, value: string) => {
          written.add(key);
          setItem(key, value);
        });
        const removeSpy = vi.spyOn(localStorage, "removeItem").mockImplementation((key: string) => {
          written.add(key);
          removeItem(key);
        });
        return () => {
          setSpy.mockRestore();
          removeSpy.mockRestore();
          for (const [key, value] of withheld) {
            if (!written.has(key)) write(key, value);
          }
        };
      }

      /** cap-C's record and published pointer, and the pointer's capture. */
      async function storeNativeA(): Promise<{ pointer: ActiveNativeDraftPointer; capture: NativeDraftPointerCapture }> {
        const pointer = await saveNativeDraftCapability(capC);
        await publishActiveNativeDraft(pointer);
        const active = inspectActiveNativeDraft();
        if (active.type !== "present") throw new Error("expected a native pointer");
        return { pointer, capture: active.capture };
      }

      /** The capture of the guest pointer `saveCapability(capA)` stored. */
      async function storeGuestA() {
        await saveCapability(capA);
        const active = inspectActiveDraftGuest();
        if (active.type !== "present") throw new Error("expected a guest pointer");
        return active.capture;
      }

      const guestPublicationA = { roomCode: "ABCDE", displayName: "Al", hostPeerId: HOST1, capabilityId: "cap-A" };

      /**
       * One clear and the read it compares on: the last read of the pointer
       * key before its removal, counted among the reads its step's body makes.
       */
      type ClearLeg = { comparisonRead: number; stage: () => Promise<() => Promise<unknown>> };
      const clearLegs: Record<string, ClearLeg> = {
        clearActiveNativeDraftIfCurrent: {
          comparisonRead: 1,
          stage: async () => {
            const { capture } = await storeNativeA();
            return () => clearActiveNativeDraftIfCurrent(capture);
          },
        },
        "clearUnusableNativeDraft (an expired native pointer)": {
          comparisonRead: 1,
          stage: async () => {
            await saveNativeDraftCapability(capC);
            localStorage.setItem(POINTER_KEY, JSON.stringify(nativePointer({ timestamp: Date.now() - FIVE_HOURS_MS })));
            const active = inspectActiveNativeDraft();
            if (active.type !== "invalid") throw new Error("expected an expired native pointer");
            return () => clearUnusableNativeDraft(active.capture);
          },
        },
        clearNativeDraftCapability: {
          comparisonRead: 2,
          stage: async () => {
            await storeNativeA();
            return () => clearNativeDraftCapability({
              capabilityId: "cap-C", endpoint: ENDPOINT, draftCode: "NAT001", playerToken: "tok-C",
            });
          },
        },
        clearActiveDraftGuestIfCurrent: {
          comparisonRead: 1,
          stage: async () => {
            const capture = await storeGuestA();
            return () => clearActiveDraftGuestIfCurrent(capture);
          },
        },
        clearDraftGuestRecovery: {
          comparisonRead: 2,
          stage: async () => {
            await storeGuestA();
            return () => clearDraftGuestRecovery(capA);
          },
        },
      };

      it.each([...Object.keys(clearLegs), "clearActiveDraftGuest (an unparseable text)"])(
        "an exact clear leaves a pointer another context publishes during it: %s",
        async (leg) => {
          installFifoWebLocks();
          try {
            const tab = await otherTab();
            const pointerB = await tab.saveNativeDraftCapability(capB);
            const recordB = await storedNative("cap-B");
            const events: string[] = [];
            let publishedB: Promise<void> | null = null;
            const publishB = () => {
              events.push("published");
              publishedB = tab.publishActiveNativeDraft(pointerB);
            };

            if (leg in clearLegs) {
              const { comparisonRead, stage } = clearLegs[leg]!;
              const clear = await stage();
              const trigger = watchPointerStepReads({ nth: comparisonRead, onRead: publishB });
              const removals = recordPointerRemovals(events);
              await clear();
              await publishedB;
              // Reach: the clear compared at that read, then reached its removal.
              expect(trigger.fired()).toBe(1);
              expect(removals()).toBe(1);
              expect(events).toEqual(["published", "remove"]);
            } else {
              localStorage.setItem(POINTER_KEY, "{not json");
              const trigger = atPointerRead(1, publishB);
              // The resume's inspection, then another tab's publication before its clear.
              expect(inspectActiveDraftGuest()).toEqual({ type: "invalid", capture: null });
              await publishedB;
              const removals = recordPointerRemovals(events);
              await clearActiveDraftGuest();
              expect(trigger.fired()).toBe(1);
              expect(removals()).toBe(0);
            }

            expect(storedPointer()).toMatchObject({ transport: "native", capabilityId: "cap-B" });
            await expect(storedNative("cap-B")).resolves.toEqual(recordB);

            if (!(leg in clearLegs)) {
              // Reach: with no other writer, the same call removes an unparseable text.
              localStorage.setItem(POINTER_KEY, "{not json");
              await clearActiveDraftGuest();
              expect(localStorage.getItem(POINTER_KEY)).toBeNull();
            }
          } finally {
            uninstallWebLocks();
          }
        },
      );

      /**
       * Every exact clear, and the unparseable-text clear, whose last read of
       * the pointer before its removal is its second: the first is its
       * inspection of the text as unreadable.
       */
      const guestContentionClearLegs: Record<string, ClearLeg> = {
        ...clearLegs,
        "clearActiveDraftGuest (an unparseable text)": {
          comparisonRead: 2,
          stage: async () => {
            localStorage.setItem(POINTER_KEY, "{not json");
            return () => clearActiveDraftGuest();
          },
        },
      };

      it.each(Object.keys(guestContentionClearLegs))(
        "an exact clear leaves a guest pointer another context's guest seat publishes during it: %s",
        async (leg) => {
          installFifoWebLocks();
          try {
            const tab = await otherTab();
            await tab.saveDraftGuestSession(HOST2, {
              draftToken: "token-G",
              seatIndex: 3,
              draftCode: "draft-G",
              roomCode: "FGHJK",
              displayName: "Bob",
              capabilityId: "cap-G",
            });
            const recordB = await storedRecord(HOST2);
            const events: string[] = [];
            let publishedB: Promise<void> | null = null;
            const publishB = () => {
              events.push("published");
              publishedB = tab.saveActiveDraftGuest({
                roomCode: "FGHJK",
                displayName: "Bob",
                hostPeerId: HOST2,
                capabilityId: "cap-G",
              });
            };

            const { comparisonRead, stage } = guestContentionClearLegs[leg]!;
            const clear = await stage();
            const trigger = watchPointerStepReads({ nth: comparisonRead, onRead: publishB });
            const removals = recordPointerRemovals(events);
            await clear();
            await publishedB;
            // Reach: the clear compared at that read, then reached its removal.
            expect(trigger.fired()).toBe(1);
            expect(removals()).toBe(1);
            expect(events).toEqual(["published", "remove"]);

            expect(storedPointer()).toMatchObject({ roomCode: "FGHJK", hostPeerId: HOST2, capabilityId: "cap-G" });
            expect(storedPointer()).not.toHaveProperty("transport");
            await expect(storedRecord(HOST2)).resolves.toEqual(recordB);
            await expect(tab.loadDraftGuestSession(HOST2)).resolves.toMatchObject({ draftToken: "token-G" });
          } finally {
            uninstallWebLocks();
          }
        },
      );

      /**
       * One pointer step over this tab's stale view (A's), and what it leaves
       * once that view shows B's native pointer, or at once without lag.
       */
      type LagLeg = {
        stage: () => Promise<() => Promise<unknown>>;
        afterLag: (result: unknown) => Promise<void>;
        inStep: (result: unknown) => Promise<void>;
      };
      const removedLeg = (stage: LagLeg["stage"], result?: unknown): LagLeg => ({
        stage,
        afterLag: async (outcome) => {
          if (result !== undefined) expect(outcome).toBe(false);
        },
        inStep: async (outcome) => {
          if (result !== undefined) expect(outcome).toBe(result);
          expect(localStorage.getItem(POINTER_KEY)).toBeNull();
        },
      });
      const lagLegs: Record<string, LagLeg> = {
        republishActiveNativeDraft: (() => {
          let refreshed: ActiveNativeDraftPointer;
          return {
            stage: async () => {
              const { pointer, capture } = await storeNativeA();
              refreshed = nativePointer({ timestamp: pointer.timestamp + 1_000 });
              return () => republishActiveNativeDraft(capture, refreshed);
            },
            afterLag: async (outcome) => {
              expect(outcome).toMatchObject({ status: "superseded" });
            },
            inStep: async (outcome) => {
              expect(outcome).toMatchObject({ status: "published" });
              expect(JSON.parse(localStorage.getItem(POINTER_KEY)!)).toEqual(refreshed);
            },
          };
        })(),
        clearActiveNativeDraftIfCurrent: removedLeg(async () => {
          const { capture } = await storeNativeA();
          return () => clearActiveNativeDraftIfCurrent(capture);
        }, true),
        clearUnusableNativeDraft: removedLeg(async () => {
          await saveNativeDraftCapability(capC);
          localStorage.setItem(POINTER_KEY, JSON.stringify(nativePointer({ timestamp: Date.now() - FIVE_HOURS_MS })));
          const active = inspectActiveNativeDraft();
          if (active.type !== "invalid") throw new Error("expected an expired native pointer");
          return () => clearUnusableNativeDraft(active.capture);
        }),
        clearNativeDraftCapability: removedLeg(async () => {
          await storeNativeA();
          return () => clearNativeDraftCapability({
            capabilityId: "cap-C", endpoint: ENDPOINT, draftCode: "NAT001", playerToken: "tok-C",
          });
        }),
        clearActiveDraftGuestIfCurrent: removedLeg(async () => {
          const capture = await storeGuestA();
          return () => clearActiveDraftGuestIfCurrent(capture);
        }),
        clearDraftGuestRecovery: removedLeg(async () => {
          await storeGuestA();
          return () => clearDraftGuestRecovery(capA);
        }),
        clearActiveDraftGuest: removedLeg(async () => {
          localStorage.setItem(POINTER_KEY, "{not json");
          return () => clearActiveDraftGuest();
        }),
        "a capability's republishGuest": {
          stage: async () => {
            const standsOn = await storeGuestA();
            return () => withActiveDraftPointer((pointer) => pointer.republishGuest(standsOn, guestPublicationA));
          },
          afterLag: async (outcome) => {
            expect(outcome).toEqual({ status: "committed", value: null });
          },
          inStep: async (outcome) => {
            expect(outcome).toMatchObject({ status: "committed", value: { capabilityId: "cap-A", hostPeerId: HOST1 } });
            expect(storedPointer()).toMatchObject({ capabilityId: "cap-A", hostPeerId: HOST1 });
          },
        },
        "a capability's publishGuest": {
          stage: async () => {
            await storeGuestA();
            return () => withActiveDraftPointer((pointer) => pointer.publishGuest(guestPublicationA));
          },
          afterLag: async (outcome) => {
            const committed = outcome as { status: string; value: Promise<void> };
            expect(committed.status).toBe("committed");
            // It wrote over the pointer its view showed, B's, whose record then has no route back.
            await committed.value;
            expect(storedPointer()).toMatchObject({ capabilityId: "cap-A", hostPeerId: HOST1 });
            expect(storedPointer()).not.toHaveProperty("transport");
            await expect(storedNative("cap-B")).resolves.toBeUndefined();
          },
          inStep: async (outcome) => {
            const committed = outcome as { status: string; value: Promise<void> };
            expect(committed.status).toBe("committed");
            await committed.value;
            expect(storedPointer()).toMatchObject({ capabilityId: "cap-A", hostPeerId: HOST1 });
          },
        },
      };

      it.each(Object.keys(lagLegs).flatMap((leg) => [[leg, "lagging"], [leg, "in step"]] as const))(
        "a pointer step waits for a lagging view and then acts on what it shows: %s, %s",
        async (leg, view) => {
          installFifoWebLocks();
          try {
            const { stage, afterLag, inStep } = lagLegs[leg]!;
            const step = await stage();
            const staged = localStorage.getItem(POINTER_KEY);
            const recordB = { ...capB, timestamp: Date.now() };
            await set(nativeKey("cap-B"), recordB, getDraftStore());
            const textB = JSON.stringify(nativePointer({ capabilityId: "cap-B", draftCode: "NAT002", seatIndex: 1 }));
            const events: string[] = [];
            const removals = recordPointerRemovals(events);

            if (view === "in step") {
              // Control: with this tab's view current, the step acts at once on what it shows.
              await inStep(await step());
              return;
            }

            // Another tab committed the next generation, and this tab's storage does not show it yet.
            const committed = Number(localStorage.getItem(GENERATION_KEY) ?? 0) + 1;
            await set(GENERATION_KEY, committed, getDraftStore());
            await expect(get(GENERATION_KEY, getDraftStore())).resolves.toBe(committed);
            const running = Promise.resolve(step());
            const state = track(running);
            await new Promise((resolve) => setTimeout(resolve, 150));
            expect(state()).toBe("pending");
            expect((await navigator.locks.query()).held).toEqual([{ name: LOCK, mode: "exclusive" }]);
            expect(removals()).toBe(0);
            expect(localStorage.getItem(POINTER_KEY)).toBe(staged);

            // The other tab's pointer and generation arrive.
            localStorage.setItem(POINTER_KEY, textB);
            const body = watchPointerStepReads();
            localStorage.setItem(GENERATION_KEY, String(committed));
            window.dispatchEvent(new StorageEvent("storage", { key: GENERATION_KEY }));
            const outcome = await running;
            // Reach: the step's body read the pointer once its view showed B's.
            expect(body.reads()).toBeGreaterThan(0);
            body.restore();

            await afterLag(outcome);
            if (leg !== "a capability's publishGuest") {
              expect(localStorage.getItem(POINTER_KEY)).toBe(textB);
              expect(removals()).toBe(0);
              await expect(storedNative("cap-B")).resolves.toEqual(recordB);
            }
          } finally {
            uninstallWebLocks();
          }
        },
      );

      it.each(Object.keys(lagLegs).filter((leg) => leg !== "a capability's publishGuest"))(
        "a step and a later one keep another context's publication this tab's view has not shown: %s",
        async (leg) => {
          installFifoWebLocks();
          try {
            const { stage, afterLag } = lagLegs[leg]!;
            const step = await stage();
            const staged = localStorage.getItem(POINTER_KEY);
            const marker = localStorage.getItem(GENERATION_KEY);
            const tab = await otherTab();
            const pointerB = await tab.saveNativeDraftCapability(capB);
            const recordB = await storedNative("cap-B");
            const deliver = await withholdFromView([POINTER_KEY, GENERATION_KEY], async () => {
              await tab.publishActiveNativeDraft(pointerB);
              // Reach: the other context committed its step past this tab's generation.
              expect(await get(GENERATION_KEY, getDraftStore())).toBeGreaterThan(Number(marker ?? 0));
              expect(storedPointer()).toMatchObject({ transport: "native", capabilityId: "cap-B" });
            });
            // Reach: this tab's view shows neither that step's pointer nor its generation.
            expect(localStorage.getItem(POINTER_KEY)).toBe(staged);
            expect(localStorage.getItem(GENERATION_KEY)).toBe(marker);

            vi.useFakeTimers({ toFake: ["setTimeout", "clearTimeout", "setInterval", "clearInterval"] });
            const waits = enteredViewWait();
            const first = Promise.resolve(step());
            first.catch(() => {}); // its outcome is asserted last
            const state = track(first);
            await untilReached(() => waits() > 0);
            expect(waits()).toBe(1);
            await vi.advanceTimersByTimeAsync(VIEW_TIMEOUT_MS - 1);
            // Reach: the step is still waiting on its view, under the lock.
            expect(state()).toBe("pending");
            expect((await navigator.locks.query()).held).toEqual([{ name: LOCK, mode: "exclusive" }]);
            await vi.advanceTimersByTimeAsync(1);
            await Promise.allSettled([first]);

            const body = watchPointerStepReads();
            const later = Promise.resolve(step());
            await Promise.allSettled([later]);
            // Reach: the later step's body read the pointer, and the step did not wait on its view.
            expect(body.reads()).toBeGreaterThan(0);
            expect(waits()).toBe(1);
            body.restore();
            vi.useRealTimers();
            deliver();

            expect(storedPointer()).toMatchObject({ transport: "native", capabilityId: "cap-B" });
            await expect(storedNative("cap-B")).resolves.toEqual(recordB);
            // The step that outlasted its wait acted on that pointer.
            await afterLag(await first);
          } finally {
            vi.useRealTimers();
            uninstallWebLocks();
          }
        },
      );

      it.each(["it rejects", "it does not settle within its bound"] as const)(
        "a step whose write-ahead fails takes no generation as seen, so a later step waits for another context's step of that generation: %s",
        async (leg) => {
          installFifoWebLocks();
          let failing: { mockRestore: () => void } | null = null;
          let abandon = () => {};
          try {
            const { pointer, capture } = await storeNativeA();
            const generation = (await get(GENERATION_KEY, getDraftStore())) as number;
            // Reach: this tab's view is caught up, so its step reaches its write-ahead at once.
            expect(localStorage.getItem(GENERATION_KEY)).toBe(String(generation));
            const tab = await otherTab();
            const pointerB = await tab.saveNativeDraftCapability(capB);
            const recordB = await storedNative("cap-B");
            const republish = () =>
              republishActiveNativeDraft(capture, nativePointer({ timestamp: pointer.timestamp + 1_000 }));

            let attempts = 0;
            const put = IDBObjectStore.prototype.put;
            failing = vi.spyOn(IDBObjectStore.prototype, "put").mockImplementation(function (
              this: IDBObjectStore,
              value: unknown,
              key?: IDBValidKey,
            ) {
              if (key !== GENERATION_KEY) return put.call(this, value, key);
              attempts += 1;
              if (leg === "it rejects") throw new DOMException("full", "QuotaExceededError");
              // Holds the write-ahead's transaction open past its bound, then aborts it unlanded.
              const request = put.call(this, value, key);
              let open = true;
              const keepAlive = (objects: IDBObjectStore) => {
                if (open) objects.get("keep-alive").onsuccess = () => keepAlive(objects);
              };
              keepAlive(this);
              abandon = () => {
                if (!open) return;
                open = false;
                request.transaction?.abort();
              };
              return request;
            });
            vi.useFakeTimers({ toFake: ["setTimeout", "clearTimeout", "setInterval", "clearInterval"] });
            const failed = Promise.resolve(republish());
            failed.catch(() => {}); // the assertions below attach after the advance
            const failedState = track(failed);
            await untilReached(() => attempts > 0 || failedState() !== "pending");
            if (leg === "it does not settle within its bound") {
              expect(failedState()).toBe("pending");
              await vi.advanceTimersByTimeAsync(GENERATION_IO_TIMEOUT_MS);
            }
            await expect(failed).rejects.toMatchObject({ reason: "generation-unpublished" });
            abandon();
            failing.mockRestore();
            failing = null;
            // Reach: the write-ahead was attempted once and never landed.
            expect(attempts).toBe(1);
            await expect(get(GENERATION_KEY, getDraftStore())).resolves.toBe(generation);
            const markerAfterFailure = localStorage.getItem(GENERATION_KEY);

            // Another context, whose view has not received this tab's marker, commits its step.
            const deliver = await withholdFromView([POINTER_KEY, GENERATION_KEY], async () => {
              localStorage.setItem(GENERATION_KEY, String(generation));
              await tab.publishActiveNativeDraft(pointerB);
              // Reach: it committed the generation after the one this tab's failed step started from.
              await expect(get(GENERATION_KEY, getDraftStore())).resolves.toBe(generation + 1);
              expect(storedPointer()).toMatchObject({ transport: "native", capabilityId: "cap-B" });
            });
            // Reach: this tab's view shows neither that step's pointer nor its generation.
            expect(localStorage.getItem(POINTER_KEY)).toBe(capture.raw);
            expect(localStorage.getItem(GENERATION_KEY)).toBe(markerAfterFailure);

            const waits = enteredViewWait();
            const later = Promise.resolve(republish());
            later.catch(() => {}); // its outcome is asserted last
            const laterState = track(later);
            await untilReached(() => waits() > 0 || laterState() !== "pending");
            await vi.advanceTimersByTimeAsync(VIEW_TIMEOUT_MS);
            await Promise.allSettled([later]);
            vi.useRealTimers();
            deliver();

            expect(storedPointer()).toMatchObject({ transport: "native", capabilityId: "cap-B" });
            await expect(storedNative("cap-B")).resolves.toEqual(recordB);
            // The failed step left this tab's marker where it was, and the later step waited on its view.
            expect(markerAfterFailure).toBe(String(generation));
            expect(waits()).toBe(1);
            // It restored the other context's pointer and acted on it.
            await expect(later).resolves.toMatchObject({ status: "superseded" });
          } finally {
            abandon();
            failing?.mockRestore();
            vi.useRealTimers();
            uninstallWebLocks();
          }
        },
      );

      it("a committed generation with no record skips the step and takes no generation as seen", async () => {
        installFifoWebLocks();
        try {
          const { pointer, capture } = await storeNativeA();
          await set(GENERATION_KEY, 3, getDraftStore());
          await del(RECORD_KEY, getDraftStore());
          localStorage.setItem(GENERATION_KEY, "2");
          // Reach: the committed generation is ahead of this tab's view, and nothing recorded it.
          await expect(get(GENERATION_KEY, getDraftStore())).resolves.toBe(3);
          await expect(get(RECORD_KEY, getDraftStore())).resolves.toBeUndefined();
          vi.useFakeTimers({ toFake: ["setTimeout", "clearTimeout", "setInterval", "clearInterval"] });
          const waits = enteredViewWait();

          const first = Promise.resolve(clearActiveNativeDraftIfCurrent(capture));
          await untilReached(() => waits() > 0);
          expect(waits()).toBe(1);
          await vi.advanceTimersByTimeAsync(VIEW_TIMEOUT_MS);
          await expect(first).resolves.toBe(false);
          expect(localStorage.getItem(GENERATION_KEY)).toBe("2");
          expect(localStorage.getItem(POINTER_KEY)).toBe(capture.raw);

          const second = Promise.resolve(
            republishActiveNativeDraft(capture, nativePointer({ timestamp: pointer.timestamp + 1_000 })),
          );
          second.catch(() => {}); // the assertions below attach after the advance
          const secondState = track(second);
          await untilReached(() => waits() > 1 || secondState() !== "pending");
          expect(waits()).toBe(2);
          await vi.advanceTimersByTimeAsync(VIEW_TIMEOUT_MS);
          await expect(second).rejects.toBeInstanceOf(ActiveDraftPointerUnavailableError);
          await expect(second).rejects.toMatchObject({ reason: "view-stale" });
          expect(localStorage.getItem(GENERATION_KEY)).toBe("2");
          expect(localStorage.getItem(POINTER_KEY)).toBe(capture.raw);

          // Positive pair: once the committed generation has a record, a step proceeds on it.
          await set(RECORD_KEY, { generation: 3, value: capture.raw }, getDraftStore());
          const third = Promise.resolve(clearActiveNativeDraftIfCurrent(capture));
          await untilReached(() => waits() > 2);
          expect(waits()).toBe(3);
          await vi.advanceTimersByTimeAsync(VIEW_TIMEOUT_MS);
          await expect(third).resolves.toBe(true);
          expect(localStorage.getItem(POINTER_KEY)).toBeNull();
        } finally {
          vi.useRealTimers();
          uninstallWebLocks();
        }
      });

      it.each(["the last step left a pointer", "the last step left none"] as const)(
        "after a localStorage wipe that leaves IndexedDB, a publication restores the recorded pointer and writes over it: %s",
        async (leg) => {
          installFifoWebLocks();
          try {
            const { capture } = await storeNativeA();
            if (leg === "the last step left none") {
              await expect(Promise.resolve(clearActiveNativeDraftIfCurrent(capture))).resolves.toBe(true);
              // A record a wrongly restored pointer would lead the publication to remove.
              await saveNativeDraftCapability(capC);
            }
            const recorded = await get(RECORD_KEY, getDraftStore());
            localStorage.clear();
            // Reach: the wipe left the committed generation ahead of this tab's view.
            await expect(get(GENERATION_KEY, getDraftStore())).resolves.toBeGreaterThan(0);
            expect(localStorage.getItem(GENERATION_KEY)).toBeNull();

            vi.useFakeTimers({ toFake: ["setTimeout", "clearTimeout", "setInterval", "clearInterval"] });
            const waits = enteredViewWait();
            const pointerD = nativePointer({ capabilityId: "cap-D" });
            const published = Promise.resolve(publishActiveNativeDraft(pointerD));
            published.catch(() => {}); // the assertion below attaches after the advance
            await untilReached(() => waits() > 0);
            expect(waits()).toBe(1);
            await vi.advanceTimersByTimeAsync(VIEW_TIMEOUT_MS);
            await expect(published).resolves.toBeUndefined();
            vi.useRealTimers();
            // Reach: before the step, the last step had recorded the pointer it left.
            expect(recorded).toMatchObject({ value: leg === "the last step left a pointer" ? capture.raw : null });

            expect(storedPointer()).toEqual(pointerD);
            if (leg === "the last step left a pointer") {
              // It wrote over the restored pointer, so it removed the record that pointer named.
              await expect(storedNative("cap-C")).resolves.toBeUndefined();
            } else {
              // Nothing was restored, so it replaced no pointer and removed no record.
              await expect(storedNative("cap-C")).resolves.toMatchObject({ capabilityId: "cap-C" });
            }
            await expect(get(RECORD_KEY, getDraftStore())).resolves.toEqual({
              generation: await get(GENERATION_KEY, getDraftStore()),
              value: localStorage.getItem(POINTER_KEY),
            });
          } finally {
            vi.useRealTimers();
            uninstallWebLocks();
          }
        },
      );

      it.each(["its write landed", "its write did not land"] as const)(
        "a step after another context's unrecorded step restores the pointer recorded before it: %s",
        async (leg) => {
          installFifoWebLocks();
          try {
            const { capture } = await storeNativeA();
            const generation = (await get(GENERATION_KEY, getDraftStore())) as number;
            // Another context wrote the next generation ahead and never recorded its result.
            await setMany(
              [
                [GENERATION_KEY, generation + 1],
                [RECORD_KEY, { generation, value: capture.raw }],
              ],
              getDraftStore(),
            );
            if (leg === "its write landed") {
              const textB = JSON.stringify(nativePointer({ capabilityId: "cap-B", draftCode: "NAT002", seatIndex: 1 }));
              localStorage.setItem(POINTER_KEY, textB);
              // Reach: this tab's view shows that step's unrecorded pointer.
              expect(storedPointer()).toMatchObject({ capabilityId: "cap-B" });
            }
            expect(localStorage.getItem(GENERATION_KEY)).toBe(String(generation));

            vi.useFakeTimers({ toFake: ["setTimeout", "clearTimeout", "setInterval", "clearInterval"] });
            const waits = enteredViewWait();
            const cleared = Promise.resolve(clearActiveNativeDraftIfCurrent(capture));
            await untilReached(() => waits() > 0);
            expect(waits()).toBe(1);
            await vi.advanceTimersByTimeAsync(VIEW_TIMEOUT_MS);
            // The clear acted on the pointer recorded before that step, which is this one's.
            await expect(cleared).resolves.toBe(true);
            expect(localStorage.getItem(POINTER_KEY)).toBeNull();
          } finally {
            vi.useRealTimers();
            uninstallWebLocks();
          }
        },
      );

      it("a step that cannot restore the recorded pointer is skipped, takes no generation as seen, and the next one waits again", async () => {
        installFifoWebLocks();
        let refusing: { mockRestore: () => void } | null = null;
        try {
          const { capture } = await storeNativeA();
          const generation = (await get(GENERATION_KEY, getDraftStore())) as number;
          await setMany(
            [
              [GENERATION_KEY, generation + 1],
              [RECORD_KEY, { generation: generation + 1, value: null }],
            ],
            getDraftStore(),
          );
          const marker = localStorage.getItem(GENERATION_KEY);
          const removeItem = localStorage.removeItem.bind(localStorage);
          refusing = vi.spyOn(localStorage, "removeItem").mockImplementation((key: string) => {
            if (key === POINTER_KEY) throw new DOMException("full", "QuotaExceededError");
            removeItem(key);
          });
          vi.useFakeTimers({ toFake: ["setTimeout", "clearTimeout", "setInterval", "clearInterval"] });
          const waits = enteredViewWait();

          const first = Promise.resolve(clearActiveNativeDraftIfCurrent(capture));
          await untilReached(() => waits() > 0);
          expect(waits()).toBe(1);
          await vi.advanceTimersByTimeAsync(VIEW_TIMEOUT_MS);
          await expect(first).resolves.toBe(false);

          const second = Promise.resolve(clearActiveNativeDraftIfCurrent(capture));
          second.catch(() => {}); // the assertion below attaches after the advance
          const secondState = track(second);
          await untilReached(() => waits() > 1 || secondState() !== "pending");
          // The next step waits again: the skipped one took no generation as seen.
          expect(waits()).toBe(2);
          expect(localStorage.getItem(GENERATION_KEY)).toBe(marker);
          await vi.advanceTimersByTimeAsync(VIEW_TIMEOUT_MS);
          await expect(second).resolves.toBe(false);
          expect(localStorage.getItem(GENERATION_KEY)).toBe(marker);
          expect(localStorage.getItem(POINTER_KEY)).toBe(capture.raw);

          // Positive pair: once storage accepts the restore, a step waits, restores and proceeds.
          refusing.mockRestore();
          refusing = null;
          const third = Promise.resolve(clearActiveNativeDraftIfCurrent(capture));
          await untilReached(() => waits() > 2);
          expect(waits()).toBe(3);
          await vi.advanceTimersByTimeAsync(VIEW_TIMEOUT_MS);
          // The restored record holds no pointer, so there is nothing to clear.
          await expect(third).resolves.toBe(false);
          expect(localStorage.getItem(POINTER_KEY)).toBeNull();
          expect(localStorage.getItem(GENERATION_KEY)).toBe(String(await get(GENERATION_KEY, getDraftStore())));
        } finally {
          refusing?.mockRestore();
          vi.useRealTimers();
          uninstallWebLocks();
        }
      });

      it.each(["a publication", "a clear", "a refused republication"] as const)(
        "each step records the pointer it leaves with the generation it committed: %s",
        async (leg) => {
          installFifoWebLocks();
          try {
            const pointerB = nativePointer({ capabilityId: "cap-B", draftCode: "NAT002", seatIndex: 1 });
            let step: () => Promise<unknown>;
            if (leg === "a publication") {
              step = () => publishActiveNativeDraft(pointerB);
            } else {
              const { pointer, capture } = await storeNativeA();
              if (leg === "a clear") {
                step = () => Promise.resolve(clearActiveNativeDraftIfCurrent(capture));
              } else {
                localStorage.setItem(POINTER_KEY, JSON.stringify(pointerB));
                step = () => republishActiveNativeDraft(capture, nativePointer({ timestamp: pointer.timestamp + 1_000 }));
              }
            }
            const before = ((await get(GENERATION_KEY, getDraftStore())) as number | undefined) ?? 0;
            const left = await step();
            const committed = await get(GENERATION_KEY, getDraftStore());
            // Reach: the step committed one generation.
            expect(committed).toBe(before + 1);

            if (leg === "a clear") {
              expect(left).toBe(true);
              expect(localStorage.getItem(POINTER_KEY)).toBeNull();
            } else {
              if (leg === "a refused republication") expect(left).toMatchObject({ status: "superseded" });
              expect(storedPointer()).toMatchObject({ capabilityId: "cap-B" });
            }
            await expect(get(RECORD_KEY, getDraftStore())).resolves.toEqual({
              generation: committed,
              value: localStorage.getItem(POINTER_KEY),
            });
          } finally {
            uninstallWebLocks();
          }
        },
      );

      it.each(
        (["throw", "abort"] as const).flatMap((shape) =>
          (["once", "every"] as const).flatMap((times) =>
            (["clear", "republication"] as const).map((later) => [shape, times, later] as const),
          ),
        ),
      )(
        "a publication whose result write is refused is kept or reverted, and a lagging context's later step leaves what it reported: %s, %s, %s",
        async (shape, times, later) => {
          installFifoWebLocks();
          let fault: ReturnType<typeof refuseResultWrites> | null = null;
          try {
            const { pointer, capture: captureC } = await storeNativeA();
            const recordC = await storedNative("cap-C");
            const generation = (await get(GENERATION_KEY, getDraftStore())) as number;
            const refreshed = nativePointer({ timestamp: pointer.timestamp + 1_000 });
            const tab = await otherTab();
            const pointerB = await tab.saveNativeDraftCapability(capB);
            const recordB = await storedNative("cap-B");

            const armed = refuseResultWrites(times === "once" ? 1 : "every", shape);
            fault = armed;
            let publishedB: unknown = null;
            let viewAfterB: string | null = null;
            let recordAfterB: unknown;
            const deliver = await withholdFromView([POINTER_KEY, GENERATION_KEY], async () => {
              try {
                publishedB = await tab.publishActiveNativeDraft(pointerB).then(
                  () => "resolved",
                  (error: unknown) => error,
                );
              } finally {
                armed.restore();
              }
              viewAfterB = localStorage.getItem(POINTER_KEY);
              // Reach: the other context's step wrote its generation ahead.
              await expect(get(GENERATION_KEY, getDraftStore())).resolves.toBe(generation + 1);
              recordAfterB = await get(RECORD_KEY, getDraftStore());
            });
            // Reach: this tab's view shows neither that step's writes nor its generation.
            expect(localStorage.getItem(POINTER_KEY)).toBe(captureC.raw);
            expect(localStorage.getItem(GENERATION_KEY)).toBe(String(generation));
            expect(JSON.stringify(refreshed)).not.toBe(captureC.raw);

            vi.useFakeTimers({ toFake: ["setTimeout", "clearTimeout", "setInterval", "clearInterval"] });
            const waits = enteredViewWait();
            const step: Promise<unknown> = later === "clear"
              ? Promise.resolve(clearActiveNativeDraftIfCurrent(captureC))
              : republishActiveNativeDraft(captureC, refreshed);
            step.catch(() => {}); // its outcome is asserted after delivery
            const state = track(step);
            await untilReached(() => waits() > 0);
            expect(waits()).toBe(1);
            await vi.advanceTimersByTimeAsync(VIEW_TIMEOUT_MS - 1);
            // Reach: the step is still waiting on its view, under the lock.
            expect(state()).toBe("pending");
            expect((await navigator.locks.query()).held).toEqual([{ name: LOCK, mode: "exclusive" }]);
            await vi.advanceTimersByTimeAsync(1);
            await Promise.allSettled([step]);
            vi.useRealTimers();
            deliver();

            if (times === "once") {
              // The publication recorded its result on its second write and was kept.
              expect(storedPointer()).toMatchObject({ transport: "native", capabilityId: "cap-B" });
              await expect(storedNative("cap-B")).resolves.toEqual(recordB);
              expect(publishedB).toBe("resolved");
              if (later === "clear") await expect(step).resolves.toBe(false);
              else await expect(step).resolves.toMatchObject({ status: "superseded" });
            } else {
              // The publication could record no result, wrote back what it replaced and reported that.
              expect(publishedB).toMatchObject({ name: "ActiveDraftPointerUnavailableError", reason: "result-unrecorded" });
              await expect(storedNative("cap-C")).resolves.toEqual(recordC);
              if (later === "clear") {
                await expect(step).resolves.toBe(true);
                expect(localStorage.getItem(POINTER_KEY)).toBeNull();
              } else {
                const republished = await step as Awaited<ReturnType<typeof republishActiveNativeDraft>>;
                expect(republished.status).toBe("published");
                await republished.settled;
                expect(storedPointer()).toEqual(refreshed);
                await expect(storedNative("cap-C")).resolves.toEqual(recordC);
              }
            }

            // Reach: the fault refused the other context's result writes, and it tried twice.
            expect(armed.refused()).toBe(times === "once" ? 1 : 2);
            expect(armed.attempts()).toBe(2);
            if (shape === "abort") expect(armed.aborted()).toBe(armed.refused());
            // Reach: what that step left durably, and in its own view.
            if (times === "once") {
              expect(JSON.parse(viewAfterB!)).toMatchObject({ transport: "native", capabilityId: "cap-B" });
              expect(recordAfterB).toEqual({ generation: generation + 1, value: viewAfterB });
            } else {
              expect(viewAfterB).toBe(captureC.raw);
              expect(recordAfterB).toEqual({ generation, value: captureC.raw });
            }
            await expect(storedNative("cap-B")).resolves.toEqual(recordB);
          } finally {
            fault?.restore();
            vi.useRealTimers();
            uninstallWebLocks();
          }
        },
      );

      it("a result write that outlasts its bound is abandoned, so it never lands after its step is reported", async () => {
        installFifoWebLocks();
        let holding: { mockRestore: () => void } | null = null;
        const holds: Array<() => void> = [];
        const endHolds = () => {
          for (const end of holds.splice(0)) end();
        };
        try {
          const { capture } = await storeNativeA();
          const recordC = await storedNative("cap-C");
          const generation = (await get(GENERATION_KEY, getDraftStore())) as number;
          let attempts = 0;
          const put = IDBObjectStore.prototype.put;
          const writesAhead = new WeakSet<IDBTransaction>();
          holding = vi.spyOn(IDBObjectStore.prototype, "put").mockImplementation(function (
            this: IDBObjectStore,
            value: unknown,
            key?: IDBValidKey,
          ) {
            if (key === GENERATION_KEY) writesAhead.add(this.transaction);
            if (key !== RECORD_KEY || writesAhead.has(this.transaction)) return put.call(this, value, key);
            attempts += 1;
            // Holds the result write's transaction open with reads until the hold ends.
            const request = put.call(this, value, key);
            let open = true;
            const keepAlive = (objects: IDBObjectStore) => {
              if (open) objects.get("keep-alive").onsuccess = () => keepAlive(objects);
            };
            keepAlive(this);
            holds.push(() => {
              open = false;
            });
            return request;
          });
          vi.useFakeTimers({ toFake: ["setTimeout", "clearTimeout", "setInterval", "clearInterval"] });
          const published = Promise.resolve(publishActiveNativeDraft(nativePointer({ capabilityId: "cap-D" })));
          published.catch(() => {}); // its outcome is asserted after the record
          const state = track(published);
          await untilReached(() => attempts > 0 || state() !== "pending");
          await vi.advanceTimersByTimeAsync(GENERATION_IO_TIMEOUT_MS);
          await untilReached(() => attempts > 1 || state() !== "pending");
          await vi.advanceTimersByTimeAsync(GENERATION_IO_TIMEOUT_MS);
          await Promise.allSettled([published]);
          vi.useRealTimers();
          endHolds();
          holding.mockRestore();
          holding = null;
          await untilReached(() => false);

          // No result write the step gave up on landed after it.
          await expect(get(RECORD_KEY, getDraftStore())).resolves.toEqual({ generation, value: capture.raw });
          await expect(published).rejects.toBeInstanceOf(ActiveDraftPointerUnavailableError);
          await expect(published).rejects.toMatchObject({ reason: "result-unrecorded" });
          expect(localStorage.getItem(POINTER_KEY)).toBe(capture.raw);
          await expect(storedNative("cap-C")).resolves.toEqual(recordC);
          // Reach: two result writes were issued, and the write-ahead landed.
          expect(attempts).toBe(2);
          await expect(get(GENERATION_KEY, getDraftStore())).resolves.toBe(generation + 1);
        } finally {
          endHolds();
          holding?.mockRestore();
          vi.useRealTimers();
          uninstallWebLocks();
        }
      });

      it.each([
        "a publication",
        "a guest seat's publication",
        "a republication",
        "a clear",
        "a refused republication",
        "the revert itself fails",
      ] as const)(
        "a step whose result cannot be recorded writes back what it started from and takes no effect: %s",
        async (leg) => {
          installFifoWebLocks();
          let fault: ReturnType<typeof refuseResultWrites> | null = null;
          let refusing: { mockRestore: () => void } | null = null;
          try {
            const { pointer, capture } = await storeNativeA();
            const recordC = await storedNative("cap-C");
            const textB = JSON.stringify(nativePointer({ capabilityId: "cap-B", draftCode: "NAT002", seatIndex: 1 }));
            if (leg === "a refused republication") localStorage.setItem(POINTER_KEY, textB);
            const generation = (await get(GENERATION_KEY, getDraftStore())) as number;
            const refreshed = nativePointer({ timestamp: pointer.timestamp + 1_000 });
            const pointerD = nativePointer({ capabilityId: "cap-D" });
            // Reach: this tab's view is caught up, so the step runs at once.
            expect(localStorage.getItem(GENERATION_KEY)).toBe(String(generation));

            const run = (): Promise<unknown> => {
              switch (leg) {
                case "a publication":
                case "the revert itself fails":
                  return publishActiveNativeDraft(pointerD);
                case "a guest seat's publication":
                  return saveActiveDraftGuest(guestPublicationA);
                case "a republication":
                case "a refused republication":
                  return republishActiveNativeDraft(capture, refreshed);
                case "a clear":
                  return Promise.resolve(clearActiveNativeDraftIfCurrent(capture));
              }
            };
            let step: Promise<unknown>;
            try {
              if (leg === "the revert itself fails") {
                // The step's own pointer write lands; the write back after it throws.
                const setItem = localStorage.setItem.bind(localStorage);
                let pointerWrites = 0;
                refusing = vi.spyOn(localStorage, "setItem").mockImplementation((key: string, value: string) => {
                  if (key === POINTER_KEY && ++pointerWrites > 1) throw new DOMException("full", "QuotaExceededError");
                  setItem(key, value);
                });
              }
              fault = refuseResultWrites("every", "throw");
              step = run();
              step.catch(() => {}); // its outcome is asserted below
              await Promise.allSettled([step]);
            } finally {
              fault?.restore();
              refusing?.mockRestore();
              refusing = null;
            }

            switch (leg) {
              case "a publication":
              case "a guest seat's publication":
              case "a republication":
                await expect(step).rejects.toBeInstanceOf(ActiveDraftPointerUnavailableError);
                await expect(step).rejects.toMatchObject({ reason: "result-unrecorded" });
                expect(localStorage.getItem(POINTER_KEY)).toBe(capture.raw);
                if (leg !== "a republication") await expect(storedNative("cap-C")).resolves.toEqual(recordC);
                break;
              case "a clear":
                await expect(step).resolves.toBe(false);
                expect(localStorage.getItem(POINTER_KEY)).toBe(capture.raw);
                break;
              case "a refused republication":
                await expect(step).resolves.toMatchObject({ status: "superseded" });
                expect(localStorage.getItem(POINTER_KEY)).toBe(textB);
                await expect(get(RECORD_KEY, getDraftStore())).resolves.toEqual({ generation, value: textB });
                break;
              case "the revert itself fails":
                await expect(step).rejects.toBeInstanceOf(ActiveDraftPointerUnavailableError);
                await expect(step).rejects.toMatchObject({ reason: "result-unrecorded" });
                // It took no generation as seen, since its view still holds its own write.
                expect(localStorage.getItem(GENERATION_KEY)).toBe(String(generation));
                expect(storedPointer()).toEqual(pointerD);
                await expect(storedNative("cap-C")).resolves.toEqual(recordC);
                break;
            }

            // Reach: both result writes were refused after the write-ahead landed.
            if (leg === "a refused republication") expect(fault!.refused()).toBeGreaterThan(0);
            else expect(fault!.refused()).toBe(2);
            await expect(get(GENERATION_KEY, getDraftStore())).resolves.toBe(generation + 1);
            if (leg === "a republication") expect(JSON.stringify(refreshed)).not.toBe(capture.raw);
            if (leg !== "the revert itself fails") {
              // The view agrees with the durable record again, so the step published its generation.
              expect(localStorage.getItem(GENERATION_KEY)).toBe(String(generation + 1));
            } else {
              // A later clean step waits, restores the recorded pointer and acts on it.
              vi.useFakeTimers({ toFake: ["setTimeout", "clearTimeout", "setInterval", "clearInterval"] });
              const waits = enteredViewWait();
              const cleared = Promise.resolve(clearActiveNativeDraftIfCurrent(capture));
              await untilReached(() => waits() > 0);
              expect(waits()).toBe(1);
              await vi.advanceTimersByTimeAsync(VIEW_TIMEOUT_MS);
              await expect(cleared).resolves.toBe(true);
              expect(localStorage.getItem(POINTER_KEY)).toBeNull();
            }
          } finally {
            fault?.restore();
            vi.useRealTimers();
            uninstallWebLocks();
          }
        },
      );

      it("a refused pointer transaction rejects a publication and keeps the pointer for a clear", async () => {
        const { capture } = await storeNativeA();
        installRefusingWebLocks(new DOMException("denied", "SecurityError"));
        try {
          const request = vi.spyOn(navigator.locks, "request");
          const published = publishActiveNativeDraft(nativePointer({ capabilityId: "cap-D" }));
          await expect(published).rejects.toBeInstanceOf(ActiveDraftPointerUnavailableError);
          await expect(published).rejects.toMatchObject({ reason: "lock-refused" });
          expect(localStorage.getItem(POINTER_KEY)).toBe(capture.raw);
          await expect(Promise.resolve(clearActiveNativeDraftIfCurrent(capture))).resolves.toBe(false);
          expect(localStorage.getItem(POINTER_KEY)).toBe(capture.raw);
          // Reach: each step asked for the lock and was refused.
          expect(request).toHaveBeenCalledTimes(2);
        } finally {
          uninstallWebLocks();
        }
      });

      it("with no lock manager a pointer step runs before its promise settles", async () => {
        expect(globalThis.navigator.locks ?? null).toBeNull();
        const pointer = nativePointer();
        const step = withActiveDraftPointer((txn) => txn.publishNative(pointer));
        expect(storedPointer()).toEqual(pointer);
        await expect(step).resolves.toMatchObject({ status: "committed" });
      });

      it("a pointer write's record removal starts once its step's result is recorded and settles after its transaction ends", async () => {
        installFifoWebLocks();
        let release!: () => void;
        const gate = new Promise<void>((resolve) => {
          release = resolve;
        });
        let watching: { mockRestore: () => void } | null = null;
        try {
          await storeNativeA();
          // Reach: the pointer it replaces names a stored record.
          await expect(storedNative("cap-C")).resolves.toMatchObject({ capabilityId: "cap-C" });
          let recorded = false;
          const put = IDBObjectStore.prototype.put;
          const writesAhead = new WeakSet<IDBTransaction>();
          watching = vi.spyOn(IDBObjectStore.prototype, "put").mockImplementation(function (
            this: IDBObjectStore,
            value: unknown,
            key?: IDBValidKey,
          ) {
            if (key === GENERATION_KEY) writesAhead.add(this.transaction);
            if (key === RECORD_KEY && !writesAhead.has(this.transaction)) {
              // Once the step's result is recorded, a read-write transaction on the draft store,
              // kept alive until the gate opens, so a removal started after it waits behind it.
              this.transaction.addEventListener("complete", () => {
                recorded = true;
                let open = true;
                void gate.then(() => {
                  open = false;
                });
                void getDraftStore()("readwrite", (objects) => new Promise<void>((resolve) => {
                  const keepAlive = () => {
                    if (!open) {
                      resolve();
                      return;
                    }
                    objects.get("keep-alive").onsuccess = keepAlive;
                  };
                  keepAlive();
                }));
              });
            }
            return put.call(this, value, key);
          });
          const step = await withActiveDraftPointer((txn) => txn.publishNative(nativePointer({ capabilityId: "cap-D" })));
          // Reach: the step's result was recorded before it returned.
          expect(recorded).toBe(true);
          expect(step.status).toBe("committed");
          if (step.status !== "committed") return;
          const removal = track(step.value);
          expect((await navigator.locks.query()).held).toEqual([]);
          await untilReached(() => removal() !== "pending");
          expect(removal()).toBe("pending");
          // No draft-store read before the gate opens: it would wait behind the held transaction.
          release();
          await step.value;
          await expect(storedNative("cap-C")).resolves.toBeUndefined();
        } finally {
          release();
          watching?.mockRestore();
          uninstallWebLocks();
        }
      });

      it.each(["with no lock manager", "under the lock"] as const)(
        "a pointer capability refuses use after its step: %s",
        async (mode) => {
          if (mode === "under the lock") installFifoWebLocks();
          try {
            let captured: ActiveDraftPointerTxn | null = null;
            const first = nativePointer();
            await withActiveDraftPointer((txn) => {
              captured = txn;
              return txn.publishNative(first);
            });
            // Reach: the method wrote inside its step.
            expect(storedPointer()).toEqual(first);
            expect(() => captured!.publishNative(nativePointer({ capabilityId: "cap-D" }))).toThrow();
            expect(storedPointer()).toEqual(first);
          } finally {
            uninstallWebLocks();
          }
        },
      );

      it("a held saved-deck library lock does not delay a pointer step", async () => {
        installFifoWebLocks();
        let release!: () => void;
        const gate = new Promise<void>((resolve) => {
          release = resolve;
        });
        try {
          const { capture } = await storeNativeA();
          let entered = false;
          const holding = withSavedDeckLibrary(() => {
            entered = true;
            return gate;
          });
          await vi.waitFor(() => expect(entered).toBe(true));
          expect((await navigator.locks.query()).held).toEqual([{ name: "phase-saved-deck-library", mode: "exclusive" }]);
          const libraryGeneration = await readIdbGenerationForTests();
          const pointerGeneration = localStorage.getItem(GENERATION_KEY);

          await expect(Promise.resolve(clearActiveNativeDraftIfCurrent(capture))).resolves.toBe(true);
          expect(localStorage.getItem(POINTER_KEY)).toBeNull();
          // Its own counter moved; the library's did not.
          expect(localStorage.getItem(GENERATION_KEY)).not.toBe(pointerGeneration);
          await expect(readIdbGenerationForTests()).resolves.toBe(libraryGeneration);
          expect(isUserOwnedStorageKey(GENERATION_KEY)).toBe(false);
          release();
          await holding;
        } finally {
          release();
          uninstallWebLocks();
        }
      });
    });
  });
});
