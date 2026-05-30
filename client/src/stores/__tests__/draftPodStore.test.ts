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
  poolInput: { type: "Set" as const, data: { set_pool_json: "{}" } },
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

    it("restores cube poolMode + setName from a persisted cube snapshot", async () => {
      const cubeSession = {
        ...persistedSession,
        poolInput: {
          type: "Cube" as const,
          data: {
            cube_list_text: "1 Lightning Bolt\n",
            cube_name: "My Cube",
            cube_draft_settings: {
              pod_size: 2,
              pack_count: 1,
              cards_per_pack: 2,
              min_deck_size: 4,
              addable_cards: { policy: "StandardBasics" as const, custom: [] },
            },
          },
        },
      };
      mocks.loadActiveDraftPod.mockReturnValue(activeMeta);
      mocks.loadDraftHostSession.mockResolvedValue(cubeSession);

      await useDraftPodStore.getState().resumeHostedPod();

      const state = useDraftPodStore.getState();
      expect(state.poolMode).toBe("cube");
      expect(state.config.setName).toBe("My Cube");
      expect(state.config.setCode).toBe("custom-cube");
      expect(state.cubeForm?.cubeName).toBe("My Cube");
      expect(state.cubeForm?.cubeListText).toBe("1 Lightning Bolt\n");

      // The hostConfig dispatched to multiplayerDraftStore must mirror the
      // persisted Cube source 1:1 so the host re-initializes onto the same
      // cube content rather than falling back to "{}".
      const dispatched = mocks.multiplayerState.hostDraft.mock.calls[0]?.[0];
      expect(dispatched.poolInput.type).toBe("Cube");
    });
  });

  describe("createPod (cube branch)", () => {
    it("rejects an empty cube list with a config error", async () => {
      useDraftPodStore.setState({
        poolMode: "cube",
        cubeForm: {
          cubeName: "C",
          cubeListText: "   ",
          settings: {
            pod_size: 2,
            pack_count: 1,
            cards_per_pack: 2,
            min_deck_size: 4,
            addable_cards: { policy: "StandardBasics", custom: [] },
          },
        },
        hostDisplayName: "Host",
      });

      await useDraftPodStore.getState().createPod();

      expect(useDraftPodStore.getState().configError).toBeTruthy();
      expect(mocks.multiplayerState.hostDraft).not.toHaveBeenCalled();
    });

    it("dispatches a Cube poolInput hostConfig when cubeForm is valid", async () => {
      useDraftPodStore.setState({
        poolMode: "cube",
        cubeForm: {
          cubeName: "Test Cube",
          cubeListText: "1 Lightning Bolt\n",
          settings: {
            pod_size: 2,
            pack_count: 1,
            cards_per_pack: 2,
            min_deck_size: 4,
            addable_cards: { policy: "StandardBasics", custom: [] },
          },
        },
        hostDisplayName: "Host",
      });

      await useDraftPodStore.getState().createPod();

      expect(mocks.multiplayerState.hostDraft).toHaveBeenCalledOnce();
      const dispatched = mocks.multiplayerState.hostDraft.mock.calls[0]?.[0];
      expect(dispatched.poolInput.type).toBe("Cube");
      expect(dispatched.poolInput.data.cube_name).toBe("Test Cube");
      expect(dispatched.poolInput.data.cube_list_text).toBe("1 Lightning Bolt\n");
    });
  });
});
