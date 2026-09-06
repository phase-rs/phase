import { beforeEach, describe, expect, it, vi } from "vitest";

// Mock idb-keyval before importing the module under test
const mockStore = new Map<string, unknown>();
const idb = vi.hoisted(() => ({
  set: vi.fn<(key: string, value: unknown) => Promise<void>>(),
}));
vi.mock("idb-keyval", () => ({
  createStore: vi.fn(() => "mock-store"),
  get: vi.fn((key: string) => Promise.resolve(mockStore.get(key) ?? undefined)),
  set: idb.set,
  del: vi.fn((key: string) => {
    mockStore.delete(key);
    return Promise.resolve();
  }),
}));

import {
  clearActiveDraftPod,
  clearActiveDraftPodIfCurrent,
  clearActiveDraftGuest,
  clearActiveDraftGuestIfCurrent,
  clearDraftGuestRecovery,
  clearDraftGuestSession,
  clearDraftDeckSubmission,
  clearDraftHostSession,
  loadActiveDraftPod,
  loadActiveDraftGuest,
  inspectActiveDraftPod,
  inspectActiveDraftGuest,
  loadDraftGuestSession,
  loadDraftDeckSubmission,
  loadDraftHostSession,
  loadDraftIntergameCommands,
  loadDraftSettlementOutbox,
  saveActiveDraftPod,
  saveActiveDraftGuest,
  saveDraftGuestSession,
  saveDraftDeckSubmission,
  saveDraftHostSession,
  persistedDraftHostSessionState,
  saveDraftIntergameCommands,
  saveDraftSettlementOutbox,
} from "../draftPersistence";
import { draftIntergameDigest } from "../intergameCommandLedger";
import type { PersistedDraftHostSession } from "../draftPersistence";
import {
  MAX_MATERIALIZED_VIRTUAL_BASICS,
  validateWorkspaceState,
  type DraftWorkspaceState,
} from "../../components/draft/workspace/types";

const validWorkspace = (): DraftWorkspaceState => ({
  schemaVersion: 1 as const,
  placements: {
    "card-1": { zone: "deck" as const, row: 0, column: 0, order: 0 },
  },
  virtualBasics: [{ instanceId: "basic-1", name: "Island" }],
});

describe("draftPersistence", () => {
  beforeEach(() => {
    mockStore.clear();
    localStorage.clear();
    idb.set.mockReset();
    idb.set.mockImplementation((key, value) => {
      mockStore.set(key, value);
      return Promise.resolve();
    });
  });

  describe("host session", () => {
    const testSession: PersistedDraftHostSession = {
      persistenceId: "test-draft-1",
      roomCode: "ABCDE",
      kind: "Premier",
      podSize: 8,
      hostDisplayName: "Alice",
      tournamentFormat: "Swiss",
      podPolicy: "Competitive",
      seatTokens: { 0: "host-token", 1: "guest-1-token" },
      seatNames: { 0: "Alice", 1: "Bob" },
      kickedTokens: [],
      draftStarted: true,
      draftCode: "draft-12345678",
      draftSessionJson: '{"status":"Drafting"}',
      poolInput: { type: "Set", data: { pools: [{ code: "TST" }], sequence: ["TST"] } },
    };

    it("saves and loads a host session", async () => {
      await saveDraftHostSession("test-draft-1", testSession);
      const loaded = await loadDraftHostSession("test-draft-1");
      expect(loaded).toEqual({ ...testSession, perSeatWorkspaceSnapshots: {} });
    });

    it("retains validated absolute reconnect deadlines", async () => {
      const deadline = Date.now() + 60_000;
      await saveDraftHostSession("deadline-session", {
        ...testSession,
        reconnectDeadlines: { 1: deadline },
      });

      await expect(loadDraftHostSession("deadline-session")).resolves.toMatchObject({
        reconnectDeadlines: { 1: deadline },
      });
    });

    it("rejects malformed reconnect deadlines", async () => {
      mockStore.set("phase-draft-host:bad-deadline", {
        ...testSession,
        reconnectDeadlines: { 1: "later" },
      });

      await expect(loadDraftHostSession("bad-deadline")).resolves.toBeNull();
    });

    it("returns null for non-existent session", async () => {
      const loaded = await loadDraftHostSession("nonexistent");
      expect(loaded).toBeNull();
    });

    it("rethrows a host write failure so callers cannot continue on a lost receipt", async () => {
      idb.set.mockRejectedValueOnce(new Error("IndexedDB unavailable"));

      await expect(saveDraftHostSession("write-failure", testSession))
        .rejects.toThrow("IndexedDB unavailable");
    });

    it("clears a host session", async () => {
      await saveDraftHostSession("test-draft-1", testSession);
      await clearDraftHostSession("test-draft-1");
      const loaded = await loadDraftHostSession("test-draft-1");
      expect(loaded).toBeNull();
    });

    it("overwrites existing session on re-save", async () => {
      await saveDraftHostSession("test-draft-1", testSession);
      const updated = { ...testSession, draftStarted: false };
      await saveDraftHostSession("test-draft-1", updated);
      const loaded = await loadDraftHostSession("test-draft-1");
      expect(loaded!.draftStarted).toBe(false);
    });

    it("accepts a live pre-draft lobby before it has generated a draft code", async () => {
      const lobby = { ...testSession, draftStarted: false, draftSessionJson: null, draftCode: "" };
      await saveDraftHostSession(lobby.persistenceId, lobby);

      await expect(loadDraftHostSession(lobby.persistenceId)).resolves.toEqual({
        ...lobby,
        perSeatWorkspaceSnapshots: {},
      });
      expect(persistedDraftHostSessionState(lobby)).toBe("live");
    });

    it("returns null for legacy snapshots missing poolInput (C6 shape guard)", async () => {
      // Simulate a pre-#1253 snapshot with the flat setPoolJson field.
      const legacy = {
        ...testSession,
        // Intentionally drop poolInput; carry the legacy field instead.
        setPoolJson: '{"code":"LEGACY"}',
      } as unknown as PersistedDraftHostSession;
      // Bypass the typed write — direct mockStore population mirrors how a
      // pre-migration snapshot would have been written to IDB.
      mockStore.set("phase-draft-host:legacy", legacy);
      delete (mockStore.get("phase-draft-host:legacy") as Record<string, unknown>).poolInput;

      const loaded = await loadDraftHostSession("legacy");
      expect(loaded).toBeNull();
    });

    it("loads a Cube poolInput snapshot intact", async () => {
      const cubeSession: PersistedDraftHostSession = {
        ...testSession,
        poolInput: {
          type: "Cube",
          data: {
            cube_list_text: "1 Lightning Bolt\n",
            cube_name: "Test Cube",
            cube_draft_settings: {
              pod_size: 2,
              pack_count: 1,
              cards_per_pack: 2,
              min_deck_size: 4,
              addable_cards: { policy: "StandardBasics", custom: [] },
            },
          },
        },
      };
      await saveDraftHostSession("cube-1", cubeSession);
      const loaded = await loadDraftHostSession("cube-1");
      expect(loaded).toEqual({ ...cubeSession, perSeatWorkspaceSnapshots: {} });
      expect(loaded?.poolInput.type).toBe("Cube");
    });

    it("retains a Chaos candidate input for host-local recovery", async () => {
      const chaosSession: PersistedDraftHostSession = {
        ...testSession,
        poolInput: {
          type: "Chaos",
          data: {
            pools: [{ code: "ISD" }, { code: "DKA" }],
            candidate_codes: ["ISD", "DKA"],
          },
        },
      };
      await saveDraftHostSession("chaos-1", chaosSession);

      await expect(loadDraftHostSession("chaos-1")).resolves.toEqual({
        ...chaosSession,
        perSeatWorkspaceSnapshots: {},
      });
    });

    it("retains durable deck-submission receipts alongside workspace snapshots", async () => {
      const session = {
        ...testSession,
        deckSubmissionReceipts: [{
          seat: 1,
          submissionId: "submission-1",
          payloadFingerprint: "[[\\\"Island\\\",1]]",
        }],
        perSeatWorkspaceSnapshots: { 1: validWorkspace() },
      };
      await saveDraftHostSession("receipt-and-workspace", session);

      await expect(loadDraftHostSession("receipt-and-workspace")).resolves.toEqual(session);
    });

    it("rejects persisted Set, Chaos, and Cube snapshots missing data used by resume", async () => {
      mockStore.set("phase-draft-host:bad-set", {
        ...testSession,
        poolInput: { type: "Set", data: {} },
      });
      mockStore.set("phase-draft-host:bad-cube", {
        ...testSession,
        poolInput: {
          type: "Cube",
          data: {
            cube_list_text: "1 Lightning Bolt",
            cube_name: "Cube",
            cube_draft_settings: { pod_size: 8 },
          },
        },
      });
      mockStore.set("phase-draft-host:bad-chaos", {
        ...testSession,
        poolInput: { type: "Chaos", data: { pools: [], candidate_codes: [] } },
      });

      await expect(loadDraftHostSession("bad-set")).resolves.toBeNull();
      await expect(loadDraftHostSession("bad-cube")).resolves.toBeNull();
      await expect(loadDraftHostSession("bad-chaos")).resolves.toBeNull();
    });

    /**
     * The guard admits both spellings a Set pod may be stored in: the live
     * `SetPackSequence`, and the single serialized pool a pre-multi-set host
     * wrote. Rejecting either would discard a resumable pod as corrupt.
     */
    it("accepts both the pack-sequence and legacy single-pool Set snapshots", async () => {
      mockStore.set("phase-draft-host:sequence", {
        ...testSession,
        poolInput: {
          type: "Set",
          data: { pools: [{ code: "ISD" }, { code: "DKA" }], sequence: ["ISD", "DKA", "ISD"] },
        },
      });
      mockStore.set("phase-draft-host:legacy", {
        ...testSession,
        poolInput: { type: "Set", data: { set_pool_json: '{"code":"TST"}' } },
      });

      const sequence = await loadDraftHostSession("sequence");
      expect(sequence?.poolInput).toEqual({
        type: "Set",
        data: { pools: [{ code: "ISD" }, { code: "DKA" }], sequence: ["ISD", "DKA", "ISD"] },
      });
      await expect(loadDraftHostSession("legacy")).resolves.not.toBeNull();
    });

    /**
     * A sequence that names no booster, or whose entries are not set codes, has
     * no pod to restore — draft-wasm would refuse it, so the snapshot is junk.
     */
    it("rejects a Set snapshot whose pack sequence is empty or mistyped", async () => {
      mockStore.set("phase-draft-host:empty-seq", {
        ...testSession,
        poolInput: { type: "Set", data: { pools: [{ code: "ISD" }], sequence: [] } },
      });
      mockStore.set("phase-draft-host:bad-seq", {
        ...testSession,
        poolInput: { type: "Set", data: { pools: [{ code: "ISD" }], sequence: [7] } },
      });

      await expect(loadDraftHostSession("empty-seq")).resolves.toBeNull();
      await expect(loadDraftHostSession("bad-seq")).resolves.toBeNull();
    });

    it("round-trips per-seat workspace snapshots", async () => {
      const session = {
        ...testSession,
        perSeatWorkspaceSnapshots: { 0: validWorkspace() },
      };
      await saveDraftHostSession("workspace", session);

      const loaded = await loadDraftHostSession("workspace");

      expect(loaded?.perSeatWorkspaceSnapshots).toEqual(session.perSeatWorkspaceSnapshots);
      expect(loaded).not.toBe(session);
    });

    it("normalizes a missing workspace map without mutating the stored session", async () => {
      mockStore.set("phase-draft-host:missing-workspace", testSession);

      const loaded = await loadDraftHostSession("missing-workspace");

      expect(loaded?.perSeatWorkspaceSnapshots).toEqual({});
      expect(testSession.perSeatWorkspaceSnapshots).toBeUndefined();
    });

    it.each([
      ["array map", []],
      ["custom-prototype map", Object.create({ inherited: true })],
      ["invalid snapshot", { 0: { ...validWorkspace(), schemaVersion: 2 } }],
    ])("returns null for a malformed workspace map: %s", async (_label, value) => {
      mockStore.set("phase-draft-host:malformed-workspace", {
        ...testSession,
        perSeatWorkspaceSnapshots: value,
      });

      expect(await loadDraftHostSession("malformed-workspace")).toBeNull();
    });

    it.each(["", " ", "-1", "+1", "01", "1.0", "1e0", "0.5", "abc", String(Number.MAX_SAFE_INTEGER + 1)])(
      "returns null for non-canonical workspace seat key %j",
      async (key) => {
        const snapshots = Object.create(null) as Record<string, unknown>;
        snapshots[key] = validWorkspace();
        mockStore.set("phase-draft-host:bad-seat", {
          ...testSession,
          perSeatWorkspaceSnapshots: snapshots,
        });

        expect(await loadDraftHostSession("bad-seat")).toBeNull();
      },
    );

    it("accepts the largest canonical safe-integer seat key", async () => {
      const key = String(Number.MAX_SAFE_INTEGER);
      mockStore.set("phase-draft-host:max-seat", {
        ...testSession,
        perSeatWorkspaceSnapshots: { [key]: validWorkspace() },
      });

      const loaded = await loadDraftHostSession("max-seat");

      expect(loaded?.perSeatWorkspaceSnapshots?.[Number.MAX_SAFE_INTEGER]).toEqual(validWorkspace());
    });

    it("saves and loads active host resume metadata", () => {
      saveActiveDraftPod({
        id: "test-draft-1",
        roomCode: "ABCDE",
        kind: "Premier",
        podSize: 8,
        hostDisplayName: "Alice",
        tournamentFormat: "Swiss",
        podPolicy: "Competitive",
        phase: "drafting",
        pickCount: 12,
        updatedAt: Date.now(),
      });

      const loaded = loadActiveDraftPod();

      expect(loaded?.roomCode).toBe("ABCDE");
      expect(loaded?.phase).toBe("drafting");
      expect(loaded?.pickCount).toBe(12);
    });

    it("clears active host resume metadata", () => {
      saveActiveDraftPod({
        id: "test-draft-1",
        roomCode: "ABCDE",
        kind: "Premier",
        podSize: 8,
        hostDisplayName: "Alice",
        tournamentFormat: "Swiss",
        podPolicy: "Competitive",
        phase: "lobby",
        pickCount: 0,
        updatedAt: Date.now(),
      });

      clearActiveDraftPod();

      expect(loadActiveDraftPod()).toBeNull();
    });

    it("does not clear metadata replaced while an older resume was loading", () => {
      const older = {
        id: "test-draft-1", roomCode: "ABCDE", updatedAt: Date.now(),
      };
      saveActiveDraftPod({
        id: older.id,
        roomCode: older.roomCode,
        kind: "Premier",
        podSize: 8,
        hostDisplayName: "Alice",
        tournamentFormat: "Swiss",
        podPolicy: "Competitive",
        phase: "drafting",
        pickCount: 1,
        updatedAt: older.updatedAt,
      });
      saveActiveDraftPod({
        id: "new-draft",
        roomCode: "FGHJK",
        kind: "Premier",
        podSize: 8,
        hostDisplayName: "Alice",
        tournamentFormat: "Swiss",
        podPolicy: "Competitive",
        phase: "lobby",
        pickCount: 0,
        updatedAt: older.updatedAt + 1,
      });

      clearActiveDraftPodIfCurrent(older);

      expect(loadActiveDraftPod()).toMatchObject({ id: "new-draft", roomCode: "FGHJK" });
    });

    it("classifies only live snapshot states as host-resumable", () => {
      expect(persistedDraftHostSessionState({ ...testSession, draftStarted: false, draftSessionJson: null })).toBe("live");
      expect(persistedDraftHostSessionState(testSession)).toBe("live");
      expect(persistedDraftHostSessionState({ ...testSession, draftSessionJson: '{"status":"Complete"}' })).toBe("terminal");
      expect(persistedDraftHostSessionState({ ...testSession, draftSessionJson: '{"status":"Abandoned"}' })).toBe("invalid");
      expect(persistedDraftHostSessionState({ ...testSession, draftSessionJson: "not json" })).toBe("invalid");
    });

    it("reports malformed active metadata as invalid", () => {
      localStorage.setItem("phase-active-draft-pod", JSON.stringify({ id: "draft", roomCode: "lower", updatedAt: 1 }));

      expect(inspectActiveDraftPod()).toMatchObject({ type: "invalid" });
    });
  });

  describe("validateWorkspaceState", () => {
    it("accepts exact state and preserves nonblank whitespace", () => {
      const raw = {
        schemaVersion: 1,
        placements: {
          " card-1 ": { zone: "sideboard", row: 1, column: 19, order: Number.MAX_SAFE_INTEGER },
        },
        virtualBasics: [{ instanceId: " basic-1 ", name: " Island " }],
      };

      const result = validateWorkspaceState(raw);

      expect("error" in result).toBe(false);
      if (!("error" in result)) {
        expect(result.placements[" card-1 "].order).toBe(Number.MAX_SAFE_INTEGER);
        expect(result.virtualBasics[0]).toEqual({ instanceId: " basic-1 ", name: " Island " });
      }
    });

    it("accepts a virtual instance that also has a placement", () => {
      const raw = validWorkspace();
      raw.placements["basic-1"] = { zone: "deck", row: 0, column: 0, order: 1 };

      expect("error" in validateWorkspaceState(raw)).toBe(false);
    });

    it("accepts more placements than the virtual-instance limit", () => {
      const placements = Object.fromEntries(
        Array.from({ length: MAX_MATERIALIZED_VIRTUAL_BASICS + 1 }, (_, index) => [
          `card-${index}`,
          { zone: "deck", row: 0, column: 0, order: index },
        ]),
      );

      expect("error" in validateWorkspaceState({
        schemaVersion: 1,
        placements,
        virtualBasics: [],
      })).toBe(false);
    });

    it("rejects a placement overflow before inspecting any placement values", () => {
      const placements: Record<string, unknown> = {
        first: { zone: "deck", row: 0, column: 0, order: 0 },
        second: { zone: "deck", row: 0, column: 0, order: 1 },
      };
      Object.defineProperty(placements, "overflow", {
        enumerable: true,
        get: () => {
          throw new Error("placement value was read");
        },
      });

      expect(validateWorkspaceState({
        schemaVersion: 1,
        placements,
        virtualBasics: [],
      }, { maxPlacementCount: 2 })).toEqual({ error: "placements cannot exceed 2 entries" });
    });

    it("accepts the virtual-instance limit and rejects one more", () => {
      const virtualBasics = Array.from(
        { length: MAX_MATERIALIZED_VIRTUAL_BASICS },
        (_, index) => ({ instanceId: `basic-${index}`, name: "Island" }),
      );

      expect("error" in validateWorkspaceState({
        schemaVersion: 1,
        placements: {},
        virtualBasics,
      })).toBe(false);
      expect(validateWorkspaceState({
        schemaVersion: 1,
        placements: {},
        virtualBasics: [...virtualBasics, { instanceId: "too-many", name: "Island" }],
      })).toHaveProperty("error");
    });

    it.each([
      ["wrong schema", { ...validWorkspace(), schemaVersion: 2 }],
      ["extra workspace field", { ...validWorkspace(), extra: true }],
      ["array placements", { ...validWorkspace(), placements: [] }],
      ["blank placement key", { ...validWorkspace(), placements: { " ": { zone: "deck", row: 0, column: 0, order: 0 } } }],
      ["extra placement field", { ...validWorkspace(), placements: { card: { zone: "deck", row: 0, column: 0, order: 0, extra: true } } }],
      ["invalid zone", { ...validWorkspace(), placements: { card: { zone: "hand", row: 0, column: 0, order: 0 } } }],
      ["invalid row", { ...validWorkspace(), placements: { card: { zone: "deck", row: 2, column: 0, order: 0 } } }],
      ["invalid column", { ...validWorkspace(), placements: { card: { zone: "deck", row: 0, column: 20, order: 0 } } }],
      ["unsafe order", { ...validWorkspace(), placements: { card: { zone: "deck", row: 0, column: 0, order: Number.MAX_SAFE_INTEGER + 1 } } }],
      ["blank virtual ID", { ...validWorkspace(), virtualBasics: [{ instanceId: "\t", name: "Island" }] }],
      ["blank virtual name", { ...validWorkspace(), virtualBasics: [{ instanceId: "basic", name: "\n" }] }],
      ["duplicate virtual ID", { ...validWorkspace(), virtualBasics: [{ instanceId: "basic", name: "Island" }, { instanceId: "basic", name: "Plains" }] }],
      ["extra virtual field", { ...validWorkspace(), virtualBasics: [{ instanceId: "basic", name: "Island", extra: true }] }],
    ])("rejects malformed state: %s", (_label, raw) => {
      expect(validateWorkspaceState(raw)).toHaveProperty("error");
    });

    it("rejects custom prototypes, symbols, and non-enumerable extras", () => {
      const custom = Object.create({ custom: true });
      Object.assign(custom, validWorkspace());
      expect(validateWorkspaceState(custom)).toHaveProperty("error");

      const symbolState = validWorkspace() as unknown as Record<PropertyKey, unknown>;
      symbolState[Symbol("extra")] = true;
      expect(validateWorkspaceState(symbolState)).toHaveProperty("error");

      const hiddenState = validWorkspace();
      Object.defineProperty(hiddenState, "hidden", { value: true });
      expect(validateWorkspaceState(hiddenState)).toHaveProperty("error");
    });

    it("does not mutate the supplied state", () => {
      const raw = validWorkspace();
      const before = structuredClone(raw);

      validateWorkspaceState(raw);

      expect(raw).toEqual(before);
    });
  });

  it("persists a held intergame command until its receipt", async () => {
    const payload = { type: "ChoosePlayDraw" as const, playFirst: true };
    const command = {
      commandId: "command-1",
      matchId: "traditional-1",
      gameNumber: 2,
      seat: 1,
      payload,
      launchPayload: { matchId: "traditional-1", seat: 1 },
      launchDigest: draftIntergameDigest({ matchId: "traditional-1", seat: 1 }),
      payloadDigest: draftIntergameDigest(payload),
      status: "Pending" as const,
    };
    await saveDraftIntergameCommands(command.matchId, [command]);
    await expect(loadDraftIntergameCommands(command.matchId)).resolves.toEqual([command]);
  });

  it("does not replay a legacy settlement across a renewed match binding", async () => {
    const binding = {
      podId: "draft-1", matchId: "match-1", round: 1, sessionKey: "session-1",
      lease: "lease-1", nonce: "nonce-1", revision: 0, matchAuthoritySeat: 0,
    };
    await saveDraftSettlementOutbox({ binding, receiptId: "receipt-1", winnerSeat: 1 });

    await expect(loadDraftSettlementOutbox({ ...binding, revision: 1, lease: "lease-2" })).resolves.toBeNull();
    await expect(loadDraftSettlementOutbox(binding)).resolves.toMatchObject({ receiptId: "receipt-1" });
  });

  describe("guest session", () => {
    it("canonicalizes a deck submission room code for replay and removal", async () => {
      await saveDraftDeckSubmission("phase2-HOST1", {
        draftCode: "draft-xyz",
        roomCode: " abcde ",
        draftToken: "token-abc",
        submissionId: "submission-1",
        mainDeck: ["Island"],
        commanders: ["Kenrith, the Returned King"],
      });

      await expect(loadDraftDeckSubmission("phase2-HOST1", {
        roomCode: "abcde",
        draftToken: "token-abc",
      })).resolves.toMatchObject({ roomCode: "ABCDE" });
      await clearDraftDeckSubmission("phase2-HOST1", "submission-1");
      await expect(loadDraftDeckSubmission("phase2-HOST1")).resolves.toBeNull();
    });

    it("saves and loads a guest session", async () => {
      await saveDraftGuestSession("phase2-HOST1", {
        draftToken: "token-abc",
        seatIndex: 3,
        draftCode: "draft-xyz",
        roomCode: "ABCDE",
        displayName: "Alice",
      });

      const loaded = await loadDraftGuestSession("phase2-HOST1");
      expect(loaded).not.toBeNull();
      expect(loaded!.draftToken).toBe("token-abc");
      expect(loaded!.seatIndex).toBe(3);
      expect(loaded!.draftCode).toBe("draft-xyz");
      expect(loaded!.hostPeerId).toBe("phase2-HOST1");
      expect(loaded!.roomCode).toBe("ABCDE");
      expect(loaded!.displayName).toBe("Alice");
    });

    it("returns null for expired session", async () => {
      // Save with a timestamp in the past
      await saveDraftGuestSession("phase2-OLD", {
        draftToken: "old-token",
        seatIndex: 1,
        draftCode: "draft-old",
        roomCode: "ABCDE",
        displayName: "Alice",
      });

      // Manually patch the stored timestamp to simulate expiry
      const key = "phase-draft-guest:phase2-OLD";
      const stored = mockStore.get(key) as Record<string, unknown>;
      stored.timestamp = Date.now() - 5 * 60 * 60 * 1000; // 5 hours ago
      mockStore.set(key, stored);

      const loaded = await loadDraftGuestSession("phase2-OLD");
      expect(loaded).toBeNull();
    });

    it("returns null for non-existent session", async () => {
      const loaded = await loadDraftGuestSession("nonexistent");
      expect(loaded).toBeNull();
    });

    it("refuses a token record stored under a different host peer key", async () => {
      mockStore.set("phase-draft-guest:phase2-EXPECTED", {
        hostPeerId: "phase2-OTHER",
        draftToken: "cross-key-token",
        seatIndex: 1,
        draftCode: "draft-xyz",
        roomCode: "ABCDE",
        displayName: "Alice",
        timestamp: Date.now(),
      });

      await expect(loadDraftGuestSession("phase2-EXPECTED", {
        roomCode: "ABCDE",
        displayName: "Alice",
      })).resolves.toBeNull();
    });

    it("clears a guest session", async () => {
      await saveDraftGuestSession("phase2-CLEAR", {
        draftToken: "token-clear",
        seatIndex: 0,
        draftCode: "draft-clear",
        roomCode: "ABCDE",
        displayName: "Alice",
      });
      await clearDraftGuestSession("phase2-CLEAR");
      const loaded = await loadDraftGuestSession("phase2-CLEAR");
      expect(loaded).toBeNull();
    });

    it("uses a non-secret room-code locator to bind the IndexedDB capability", async () => {
      await saveDraftGuestSession("phase2-HOST1", {
        draftToken: "token-abc",
        seatIndex: 3,
        draftCode: "draft-xyz",
        roomCode: "ABCDE",
        displayName: "Alice",
      });
      saveActiveDraftGuest({ roomCode: "ABCDE", displayName: "Alice", hostPeerId: "phase2-HOST1" });

      expect(loadActiveDraftGuest()).toMatchObject({
        roomCode: "ABCDE",
        displayName: "Alice",
        hostPeerId: "phase2-HOST1",
      });
      expect(localStorage.getItem("phase-active-draft-guest")).not.toContain("token-abc");
      await expect(loadDraftGuestSession("phase2-HOST1", {
        roomCode: "ABCDE",
        displayName: "Alice",
      })).resolves.toMatchObject({ draftToken: "token-abc" });
      await expect(loadDraftGuestSession("phase2-HOST1", {
        roomCode: "ABCDE",
        displayName: "Mallory",
      })).resolves.toBeNull();
    });

    it("clears both recovery records for an explicit terminal removal", async () => {
      await saveDraftGuestSession("phase2-HOST1", {
        draftToken: "token-abc",
        seatIndex: 3,
        draftCode: "draft-xyz",
        roomCode: "ABCDE",
        displayName: "Alice",
      });
      saveActiveDraftGuest({ roomCode: "ABCDE", displayName: "Alice", hostPeerId: "phase2-HOST1" });

      await clearDraftGuestRecovery("phase2-HOST1");
      expect(loadActiveDraftGuest()).toBeNull();
      await expect(loadDraftGuestSession("phase2-HOST1")).resolves.toBeNull();
      clearActiveDraftGuest();
    });

    it("inspects guest locators without mutation and only clears the exact captured locator", () => {
      saveActiveDraftGuest({ roomCode: "ABCDE", displayName: "Alice", hostPeerId: "phase2-HOST1" });
      const active = inspectActiveDraftGuest();
      expect(active.type).toBe("present");
      expect(localStorage.getItem("phase-active-draft-guest")).not.toBeNull();
      if (active.type !== "present") throw new Error("expected guest locator");

      saveActiveDraftGuest({ roomCode: "FGHJK", displayName: "Bob", hostPeerId: "phase2-HOST2" });
      clearActiveDraftGuestIfCurrent(active.capture);

      expect(inspectActiveDraftGuest()).toMatchObject({
        type: "present",
        meta: { roomCode: "FGHJK", hostPeerId: "phase2-HOST2" },
      });
    });

    it.each([
      ["malformed", "{not-json"],
      ["expired", JSON.stringify({
        roomCode: "ABCDE",
        displayName: "Alice",
        hostPeerId: "phase2-HOST1",
        timestamp: Date.now() - 5 * 60 * 60 * 1000,
      })],
    ])("keeps a %s guest locator intact during inspection, then load performs ordinary cleanup", (_label, raw) => {
      const removeItem = vi.spyOn(localStorage, "removeItem");
      localStorage.setItem("phase-active-draft-guest", raw);

      const inspected = inspectActiveDraftGuest();

      expect(inspected.type).toBe("invalid");
      expect(localStorage.getItem("phase-active-draft-guest")).toBe(raw);
      expect(removeItem).not.toHaveBeenCalledWith("phase-active-draft-guest");

      expect(loadActiveDraftGuest()).toBeNull();
      expect(removeItem).toHaveBeenCalledWith("phase-active-draft-guest");
      expect(localStorage.getItem("phase-active-draft-guest")).toBeNull();

      removeItem.mockRestore();
    });

    it("does not clear a same-timestamp locator whose display identity changed", () => {
      const timestamp = Date.now();
      localStorage.setItem("phase-active-draft-guest", JSON.stringify({
        roomCode: "ABCDE",
        displayName: "Alice",
        hostPeerId: "phase2-HOST1",
        timestamp,
      }));
      const active = inspectActiveDraftGuest();
      if (active.type !== "present") throw new Error("expected guest locator");

      localStorage.setItem("phase-active-draft-guest", JSON.stringify({
        roomCode: "ABCDE",
        displayName: "Alicia",
        hostPeerId: "phase2-HOST1",
        timestamp,
      }));
      clearActiveDraftGuestIfCurrent(active.capture);

      expect(inspectActiveDraftGuest()).toMatchObject({
        type: "present",
        meta: { displayName: "Alicia", timestamp },
      });
    });
  });
});
