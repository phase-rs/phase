import { beforeEach, describe, expect, it, vi } from "vitest";

const mocks = vi.hoisted(() => ({
  clearActiveDraftPod: vi.fn(),
  loadActiveDraftPod: vi.fn(),
  loadDraftHostSession: vi.fn(),
  multiplayerState: {
    role: null as "host" | "guest" | null,
    phase: "idle",
    roomCode: null as string | null,
    hostDraft: vi.fn(),
  },
}));

vi.mock("../../services/draftPersistence", () => ({
  clearActiveDraftPod: mocks.clearActiveDraftPod,
  loadActiveDraftPod: mocks.loadActiveDraftPod,
  loadDraftHostSession: mocks.loadDraftHostSession,
}));

vi.mock("../multiplayerDraftStore", () => ({
  useMultiplayerDraftStore: {
    getState: () => mocks.multiplayerState,
  },
}));

import { useDraftPodStore } from "../draftPodStore";

const activeMeta = {
  id: "draft-1",
  roomCode: "ABCDE",
  kind: "Premier" as const,
  podSize: 8,
  hostDisplayName: "Host",
  tournamentFormat: "Swiss" as const,
  podPolicy: "Competitive" as const,
  phase: "matchInProgress" as const,
  pickCount: 42,
  updatedAt: Date.now(),
};

const persistedSession = {
  persistenceId: "draft-1",
  roomCode: "ABCDE",
  kind: "Premier" as const,
  podSize: 8,
  hostDisplayName: "Host",
  tournamentFormat: "Swiss" as const,
  podPolicy: "Competitive" as const,
  seatTokens: { 0: "host" },
  seatNames: { 0: "Host" },
  kickedTokens: [],
  draftStarted: true,
  draftCode: "ABCDE",
  draftSessionJson: "{}",
  setPoolJson: "{}",
};

describe("draftPodStore", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    mocks.multiplayerState.role = null;
    mocks.multiplayerState.phase = "idle";
    mocks.multiplayerState.roomCode = null;
    mocks.multiplayerState.hostDraft = vi.fn(async () => {});
    useDraftPodStore.getState().reset();
  });

  describe("resumeHostedPod", () => {
    it("deduplicates concurrent resume calls for the same hosted pod", async () => {
      let resolveSession!: (session: typeof persistedSession) => void;
      const sessionPromise = new Promise<typeof persistedSession>((resolve) => {
        resolveSession = resolve;
      });
      mocks.loadActiveDraftPod.mockReturnValue(activeMeta);
      mocks.loadDraftHostSession.mockReturnValue(sessionPromise);

      const first = useDraftPodStore.getState().resumeHostedPod();
      const second = useDraftPodStore.getState().resumeHostedPod();
      resolveSession(persistedSession);
      await Promise.all([first, second]);

      expect(mocks.loadDraftHostSession).toHaveBeenCalledTimes(1);
      expect(mocks.multiplayerState.hostDraft).toHaveBeenCalledTimes(1);
    });

    it("does not re-host when the saved pod is already live in memory", async () => {
      mocks.multiplayerState.role = "host";
      mocks.multiplayerState.phase = "matchInProgress";
      mocks.multiplayerState.roomCode = "ABCDE";
      mocks.loadActiveDraftPod.mockReturnValue(activeMeta);

      await useDraftPodStore.getState().resumeHostedPod();

      expect(mocks.loadDraftHostSession).not.toHaveBeenCalled();
      expect(mocks.multiplayerState.hostDraft).not.toHaveBeenCalled();
    });

    it("retries resume when matching host state is not live", async () => {
      mocks.multiplayerState.role = "host";
      mocks.multiplayerState.phase = "error";
      mocks.multiplayerState.roomCode = "ABCDE";
      mocks.loadActiveDraftPod.mockReturnValue(activeMeta);
      mocks.loadDraftHostSession.mockResolvedValue(persistedSession);

      await useDraftPodStore.getState().resumeHostedPod();

      expect(mocks.loadDraftHostSession).toHaveBeenCalledOnce();
      expect(mocks.multiplayerState.hostDraft).toHaveBeenCalledOnce();
    });
  });
});
