import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { draftProcedureFixture } from "../../adapter/__tests__/draftProcedureFixture";

const {
  clearDraftHostSession,
  hostRoom,
  loadDraftHostSession,
  saveActiveDraftPod,
  saveDraftHostSession,
} = vi.hoisted(() => ({
  clearDraftHostSession: vi.fn(async () => {}),
  hostRoom: vi.fn(),
  loadDraftHostSession: vi.fn(async () => null),
  saveActiveDraftPod: vi.fn(),
  saveDraftHostSession: vi.fn(async () => {}),
}));

vi.mock("../../adapter/draft-adapter", async (importOriginal) => {
  const actual = await importOriginal<typeof import("../../adapter/draft-adapter")>();
  return {
    ...actual,
    DraftAdapter: vi.fn().mockImplementation(function () {
      return {
        draftProcedure: vi.fn(async () => draftProcedureFixture({
          pod_size: 8,
          human_seats: 8,
          min_pod_size: 2,
          max_pod_size: 8,
          allowed_pod_sizes: [2, 3, 4, 5, 6, 7, 8],
          packs_per_player: 3,
          cards_per_pick: 1,
          distribution: "PickAndPass",
          min_deck_size: 40,
          match_config: { match_type: "Bo1" },
        })),
      };
    }),
  };
});

vi.mock("../../network/connection", async (importOriginal) => {
  const actual = await importOriginal<typeof import("../../network/connection")>();
  return { ...actual, hostRoom };
});

vi.mock("../../services/draftPersistence", async (importOriginal) => {
  const actual = await importOriginal<typeof import("../../services/draftPersistence")>();
  return {
    ...actual,
    clearDraftHostSession,
    loadDraftHostSession,
    saveActiveDraftPod,
    saveDraftHostSession,
  };
});

import { useMultiplayerDraftStore } from "../multiplayerDraftStore";

describe("multiplayerDraftStore recovered-host persistence fence", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    let hostNumber = 0;
    hostRoom.mockImplementation(async () => {
      hostNumber++;
      return {
        roomCode: "ABCDE",
        peerId: `phase2-ABCDE-${hostNumber}`,
        peer: { destroy: vi.fn() } as never,
        onGuestConnected: vi.fn(() => vi.fn()),
        destroy: vi.fn(),
      };
    });
  });

  afterEach(async () => {
    await useMultiplayerDraftStore.getState().leave(true);
  });

  it("holds a replacement recovery until a route-aborted host's in-flight save drains", async () => {
    let releaseStaleSave!: () => void;
    saveDraftHostSession.mockImplementationOnce(
      () => new Promise<void>((resolve) => {
        releaseStaleSave = resolve;
      }),
    );
    const config = {
      poolInput: { type: "Set" as const, data: { pools: [{ code: "TST" }], sequence: ["TST"] } },
      kind: "Premier" as const,
      podSize: 8,
      hostDisplayName: "Host",
      tournamentFormat: "Swiss" as const,
      podPolicy: "Competitive" as const,
      persistenceId: "shared-recovery",
    };
    const controller = new AbortController();

    await expect(useMultiplayerDraftStore.getState().hostDraft({
      ...config,
      signal: controller.signal,
    })).resolves.toBe(true);
    await Promise.resolve();
    expect(saveDraftHostSession).toHaveBeenCalledOnce();

    controller.abort();
    const replacement = useMultiplayerDraftStore.getState().hostDraft(config);
    await Promise.resolve();
    await Promise.resolve();

    expect(hostRoom).toHaveBeenCalledOnce();
    expect(saveDraftHostSession).toHaveBeenCalledOnce();

    releaseStaleSave();

    await expect(replacement).resolves.toBe(true);
    expect(hostRoom).toHaveBeenCalledTimes(2);
    expect(saveDraftHostSession).toHaveBeenCalledTimes(2);
  });

  it("holds a replacement recovery until a route-aborted delayed hostRoom is destroyed", async () => {
    const staleHostResult = {
      roomCode: "ABCDE",
      peerId: "phase2-ABCDE-stale",
      peer: { destroy: vi.fn() } as never,
      onGuestConnected: vi.fn(() => vi.fn()),
      destroy: vi.fn(),
    };
    let resolveStaleHostRoom!: (result: typeof staleHostResult) => void;
    hostRoom.mockReset();
    hostRoom.mockImplementationOnce(
      () => new Promise<typeof staleHostResult>((resolve) => {
        resolveStaleHostRoom = resolve;
      }),
    );
    hostRoom.mockImplementationOnce(async () => {
      expect(staleHostResult.destroy).toHaveBeenCalledOnce();
      return {
        roomCode: "ABCDE",
        peerId: "phase2-ABCDE-current",
        peer: { destroy: vi.fn() } as never,
        onGuestConnected: vi.fn(() => vi.fn()),
        destroy: vi.fn(),
      };
    });
    const config = {
      poolInput: { type: "Set" as const, data: { pools: [{ code: "TST" }], sequence: ["TST"] } },
      kind: "Premier" as const,
      podSize: 8,
      hostDisplayName: "Host",
      tournamentFormat: "Swiss" as const,
      podPolicy: "Competitive" as const,
      persistenceId: "shared-recovery",
    };
    const controller = new AbortController();
    const stale = useMultiplayerDraftStore.getState().hostDraft({
      ...config,
      signal: controller.signal,
    });
    await Promise.resolve();
    await Promise.resolve();
    await Promise.resolve();

    controller.abort();
    const replacement = useMultiplayerDraftStore.getState().hostDraft(config);
    await Promise.resolve();
    await Promise.resolve();

    expect(hostRoom).toHaveBeenCalledOnce();
    resolveStaleHostRoom(staleHostResult);

    await expect(stale).resolves.toBe(false);
    await expect(replacement).resolves.toBe(true);
    expect(hostRoom).toHaveBeenCalledTimes(2);
  });
});
