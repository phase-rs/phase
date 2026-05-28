import { beforeEach, describe, expect, it, vi } from "vitest";

// Mock idb-keyval before importing the module under test
const mockStore = new Map<string, unknown>();
vi.mock("idb-keyval", () => ({
  createStore: vi.fn(() => "mock-store"),
  get: vi.fn((key: string) => Promise.resolve(mockStore.get(key) ?? undefined)),
  set: vi.fn((key: string, value: unknown) => {
    mockStore.set(key, value);
    return Promise.resolve();
  }),
  del: vi.fn((key: string) => {
    mockStore.delete(key);
    return Promise.resolve();
  }),
}));

import {
  clearActiveDraftPod,
  clearDraftGuestSession,
  clearDraftHostSession,
  loadActiveDraftPod,
  loadDraftGuestSession,
  loadDraftHostSession,
  saveActiveDraftPod,
  saveDraftGuestSession,
  saveDraftHostSession,
} from "../draftPersistence";
import type { PersistedDraftHostSession } from "../draftPersistence";

describe("draftPersistence", () => {
  beforeEach(() => {
    mockStore.clear();
    localStorage.clear();
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
      poolInput: { type: "Set", data: { set_pool_json: '{"code":"TST"}' } },
    };

    it("saves and loads a host session", async () => {
      await saveDraftHostSession("test-draft-1", testSession);
      const loaded = await loadDraftHostSession("test-draft-1");
      expect(loaded).toEqual(testSession);
    });

    it("returns null for non-existent session", async () => {
      const loaded = await loadDraftHostSession("nonexistent");
      expect(loaded).toBeNull();
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
      expect(loaded).toEqual(cubeSession);
      expect(loaded?.poolInput.type).toBe("Cube");
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
  });

  describe("guest session", () => {
    it("saves and loads a guest session", async () => {
      await saveDraftGuestSession("phase2-HOST1", {
        draftToken: "token-abc",
        seatIndex: 3,
        draftCode: "draft-xyz",
      });

      const loaded = await loadDraftGuestSession("phase2-HOST1");
      expect(loaded).not.toBeNull();
      expect(loaded!.draftToken).toBe("token-abc");
      expect(loaded!.seatIndex).toBe(3);
      expect(loaded!.draftCode).toBe("draft-xyz");
      expect(loaded!.hostPeerId).toBe("phase2-HOST1");
    });

    it("returns null for expired session", async () => {
      // Save with a timestamp in the past
      await saveDraftGuestSession("phase2-OLD", {
        draftToken: "old-token",
        seatIndex: 1,
        draftCode: "draft-old",
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

    it("clears a guest session", async () => {
      await saveDraftGuestSession("phase2-CLEAR", {
        draftToken: "token-clear",
        seatIndex: 0,
        draftCode: "draft-clear",
      });
      await clearDraftGuestSession("phase2-CLEAR");
      const loaded = await loadDraftGuestSession("phase2-CLEAR");
      expect(loaded).toBeNull();
    });
  });
});
