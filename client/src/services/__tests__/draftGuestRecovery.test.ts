import "fake-indexeddb/auto";
import { clear, get, set } from "idb-keyval";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { parseWebSocketUrl } from "../../config/multiplayerServer";
import {
  canonicalNativeDraftEndpoint,
  clearActiveDraftGuestIfCurrent,
  clearDraftDeckSubmission,
  clearDraftGuestRecovery,
  clearNativeDraftCapability,
  getDraftStore,
  inspectActiveDraftGuest,
  inspectActiveNativeDraft,
  loadActiveDraftGuest,
  loadDraftDeckSubmission,
  loadDraftGuestCapability,
  loadDraftGuestSession,
  loadNativeDraftCapability,
  publishActiveNativeDraft,
  saveActiveDraftGuest,
  saveDraftDeckSubmission,
  saveDraftGuestSession,
  saveNativeDraftCapability,
} from "../draftPersistence";
import type {
  ActiveNativeDraftPointer,
  DraftGuestCapability,
  DraftGuestCapabilityOwner,
  NativeDraftCapabilityRecord,
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
  saveActiveDraftGuest({
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
  });
});
