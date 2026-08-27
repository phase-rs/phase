import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

const mocks = vi.hoisted(() => ({
  clearActiveDraftPod: vi.fn(),
  loadActiveDraftPod: vi.fn(),
  inspectActiveDraftPod: vi.fn(),
  clearActiveDraftPodIfCurrent: vi.fn(),
  loadDraftHostSession: vi.fn(),
  persistedDraftHostSessionState: vi.fn(() => "live"),
  draftProcedure: vi.fn(),
  multiplayerState: {
    role: null as "host" | "guest" | null,
    phase: "idle",
    roomCode: null as string | null,
    hostDraft: vi.fn<(config: unknown) => Promise<boolean>>(async () => true),
  },
}));

vi.mock("../../services/draftPersistence", () => ({
  clearActiveDraftPod: mocks.clearActiveDraftPod,
  loadActiveDraftPod: mocks.loadActiveDraftPod,
  inspectActiveDraftPod: mocks.inspectActiveDraftPod,
  clearActiveDraftPodIfCurrent: mocks.clearActiveDraftPodIfCurrent,
  loadDraftHostSession: mocks.loadDraftHostSession,
  persistedDraftHostSessionState: mocks.persistedDraftHostSessionState,
}));

vi.mock("../multiplayerDraftStore", () => ({
  useMultiplayerDraftStore: {
    getState: () => mocks.multiplayerState,
  },
}));

// `enterKind` reads the ENGINE's per-kind `DraftProcedure` through the adapter.
// Mocking the adapter is what lets the hostile fixture below return a pod size
// the client could not have guessed.
vi.mock("../../adapter/draft-adapter", () => ({
  DraftAdapter: class {
    draftProcedure = mocks.draftProcedure;
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
    mocks.multiplayerState.hostDraft = vi.fn<(config: unknown) => Promise<boolean>>(async () => true);
    mocks.persistedDraftHostSessionState.mockReturnValue("live");
    mocks.inspectActiveDraftPod.mockReturnValue({
      type: "absent",
    });
    useDraftPodStore.getState().reset();
  });

  describe("enterKind", () => {
    // Every axis but `pod_size` is inert here; only `pod_size` is read.
    function procedure(podSize: number) {
      return {
        pod_size: podSize,
        human_seats: 1,
        min_pod_size: 3,
        packs_per_player: 3,
        cards_per_pick: 2,
        min_deck_size: 60,
        match_config: { best_of: 1 },
      };
    }

    it("applies the kind and adopts the engine's pod-size default", async () => {
      mocks.draftProcedure.mockResolvedValue(procedure(4));

      await useDraftPodStore.getState().enterKind("CommanderDraft");

      // Reach guard: the engine read really happened, so `podSize` below is an
      // adopted value rather than a constant that coincides with it.
      expect(mocks.draftProcedure).toHaveBeenCalledWith("CommanderDraft");
      // REVERT-FAILING: no `enterKind` exists at BASE.
      expect(useDraftPodStore.getState().config).toMatchObject({
        kind: "CommanderDraft",
        podSize: 4,
      });
    });

    it("adopts a pod size no client literal could have produced", async () => {
      // HOSTILE / ANTI-HARDCODE: a hardcoded `4` passes every other case here
      // and fails only this one. CR 903.13 fixes no pod size — 4 is the
      // engine's product default, not an invariant, so it must be read.
      mocks.draftProcedure.mockResolvedValue(procedure(6));

      await useDraftPodStore.getState().enterKind("CommanderDraft");

      expect(useDraftPodStore.getState().config.podSize).toBe(6);
    });

    it("keeps the kind when the engine read fails", async () => {
      const before = useDraftPodStore.getState().config.podSize;
      mocks.draftProcedure.mockRejectedValue(new Error("wasm unavailable"));

      await useDraftPodStore.getState().enterKind("CommanderDraft");

      const state = useDraftPodStore.getState();
      expect(state.config.kind).toBe("CommanderDraft");
      expect(state.config.podSize).toBe(before);
      expect(state.configError).toBe("wasm unavailable");
    });

    it("routes through setConfig rather than bypassing its normalization", async () => {
      // Sibling: `setConfig` forces `poolMode: "set"` for Sealed. If `enterKind`
      // wrote `config` directly, this stays "cube".
      mocks.draftProcedure.mockResolvedValue(procedure(8));
      useDraftPodStore.getState().setPoolMode("cube");

      await useDraftPodStore.getState().enterKind("Sealed");

      expect(useDraftPodStore.getState().config.kind).toBe("Sealed");
      expect(useDraftPodStore.getState().poolMode).toBe("set");
    });
  });

  describe("resumeHostedPod", () => {
    it("returns absent silently without changing setup state", async () => {
      const outcome = await useDraftPodStore.getState().resumeHostedPod({ silent: true, routeToken: 1 });

      expect(outcome).toBe("absent");
      expect(useDraftPodStore.getState().configError).toBeNull();
    });

    it("treats a completed persisted snapshot as terminal and never re-hosts it", async () => {
      mocks.inspectActiveDraftPod.mockReturnValue({ type: "present", meta: activeMeta, capture: { id: activeMeta.id, roomCode: activeMeta.roomCode, updatedAt: activeMeta.updatedAt } });
      mocks.loadDraftHostSession.mockResolvedValue(persistedSession);
      mocks.persistedDraftHostSessionState.mockReturnValue("terminal");

      const outcome = await useDraftPodStore.getState().resumeHostedPod({ routeToken: 2 });

      expect(outcome).toBe("terminal");
      expect(mocks.multiplayerState.hostDraft).not.toHaveBeenCalled();
      expect(mocks.clearActiveDraftPodIfCurrent).toHaveBeenCalled();
    });

    it("uses the snapshot rather than stale complete metadata as resume authority", async () => {
      const staleMeta = { ...activeMeta, phase: "complete" as const };
      mocks.inspectActiveDraftPod.mockReturnValue({ type: "present", meta: staleMeta, capture: { id: staleMeta.id, roomCode: staleMeta.roomCode, updatedAt: staleMeta.updatedAt } });
      mocks.loadDraftHostSession.mockResolvedValue(persistedSession);

      const outcome = await useDraftPodStore.getState().resumeHostedPod({ routeToken: 3 });

      expect(outcome).toBe("resumed");
      expect(mocks.multiplayerState.hostDraft).toHaveBeenCalledOnce();
      expect(mocks.clearActiveDraftPodIfCurrent).not.toHaveBeenCalled();
    });

    it("does not report recovery as resumed when host initialization fails", async () => {
      mocks.inspectActiveDraftPod.mockReturnValue({ type: "present", meta: activeMeta, capture: { id: activeMeta.id, roomCode: activeMeta.roomCode, updatedAt: activeMeta.updatedAt } });
      mocks.loadDraftHostSession.mockResolvedValue(persistedSession);
      mocks.multiplayerState.hostDraft = vi.fn<(config: unknown) => Promise<boolean>>(async () => false);

      await expect(useDraftPodStore.getState().resumeHostedPod({ routeToken: 4 })).resolves.toBe("invalid");
      expect(mocks.clearActiveDraftPodIfCurrent).not.toHaveBeenCalled();
    });

    it("deduplicates concurrent resume calls for the same hosted pod", async () => {
      let resolveSession!: (session: typeof persistedSession) => void;
      const sessionPromise = new Promise<typeof persistedSession>((resolve) => {
        resolveSession = resolve;
      });
      mocks.inspectActiveDraftPod.mockReturnValue({ type: "present", meta: activeMeta, capture: { id: activeMeta.id, roomCode: activeMeta.roomCode, updatedAt: activeMeta.updatedAt } });
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
      mocks.inspectActiveDraftPod.mockReturnValue({ type: "present", meta: activeMeta, capture: { id: activeMeta.id, roomCode: activeMeta.roomCode, updatedAt: activeMeta.updatedAt } });

      await useDraftPodStore.getState().resumeHostedPod();

      expect(mocks.loadDraftHostSession).not.toHaveBeenCalled();
      expect(mocks.multiplayerState.hostDraft).not.toHaveBeenCalled();
    });

    it("retries resume when matching host state is not live", async () => {
      mocks.multiplayerState.role = "host";
      mocks.multiplayerState.phase = "error";
      mocks.multiplayerState.roomCode = "ABCDE";
      mocks.inspectActiveDraftPod.mockReturnValue({ type: "present", meta: activeMeta, capture: { id: activeMeta.id, roomCode: activeMeta.roomCode, updatedAt: activeMeta.updatedAt } });
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
      mocks.inspectActiveDraftPod.mockReturnValue({ type: "present", meta: activeMeta, capture: { id: activeMeta.id, roomCode: activeMeta.roomCode, updatedAt: activeMeta.updatedAt } });
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
      const dispatched = mocks.multiplayerState.hostDraft.mock.calls[0]?.[0] as {
        poolInput: { type: string };
      };
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
      const dispatched = mocks.multiplayerState.hostDraft.mock.calls[0]?.[0] as {
        poolInput: { type: string; data: { cube_name: string; cube_list_text: string } };
      };
      expect(dispatched.poolInput.type).toBe("Cube");
      expect(dispatched.poolInput.data.cube_name).toBe("Test Cube");
      expect(dispatched.poolInput.data.cube_list_text).toBe("1 Lightning Bolt\n");
    });
  });

  describe("createPod (set branch)", () => {
    afterEach(() => {
      vi.unstubAllGlobals();
    });

    it("keeps the loadProcedure failure visible past the pool fetch", async () => {
      // `__DRAFT_POOLS_URL__` is a vite define that `vitest.config.ts` does not
      // declare, so it is a free identifier here and must be supplied, or the
      // fetch throws and the set branch's own catch overwrites the message
      // under test.
      vi.stubGlobal("__DRAFT_POOLS_URL__", "/draft-pools.json");
      vi.stubGlobal(
        "fetch",
        vi.fn(async () => ({ ok: true, status: 200, json: async () => ({ eoe: {} }) })),
      );
      mocks.draftProcedure.mockRejectedValue(new Error("wasm unavailable"));
      useDraftPodStore.setState((prev) => ({
        config: { ...prev.config, setCode: "EOE" },
        hostDisplayName: "Host",
      }));

      await useDraftPodStore.getState().createPod();

      // Reach guard: creation ran to completion, so the assertion below reads a
      // message that survived the whole set-pool path rather than one left by an
      // early return. Without this, a `return` added to the catch would pass too.
      expect(mocks.multiplayerState.hostDraft).toHaveBeenCalledOnce();
      // REVERT-FAILING: restore `configError: null` to the `loadingPool` write
      // and this reads `null` -- the catch's message is erased three statements
      // later, so a `draftProcedure` failure is silent on the branch Sealed
      // always takes.
      expect(useDraftPodStore.getState().configError).toBe("wasm unavailable");
    });
  });
});
