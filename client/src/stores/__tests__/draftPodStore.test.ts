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
    joinDraft: vi.fn<(config: unknown) => Promise<boolean>>(async () => true),
  },
  // Shaped like the real store's source model: `configuredBackupEndpoint`
  // reads `hostingServer`, so a mock still carrying `serverAddress` would
  // feed it `undefined` and the assertions below would pass for the wrong
  // reason.
  multiplayerConfig: {
    hostingServer: "wss://phase.example/ws" as string | null,
    userLobbySources: [],
    sourceStatus: new Map(),
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
  DRAFT_OFFLINE_ERROR: "offline.startUnavailable",
  useMultiplayerDraftStore: {
    getState: () => mocks.multiplayerState,
  },
}));

vi.mock("../multiplayerStore", () => ({
  useMultiplayerStore: {
    getState: () => mocks.multiplayerConfig,
  },
}));

// `enterKind` reads the ENGINE's per-kind `DraftProcedure` through the adapter.
// Mocking the adapter is what lets the hostile fixture below return a pod size
// the client could not have guessed.
vi.mock("../../adapter/draft-adapter", async (importOriginal) => ({
  // The pure helpers (`distinctJoined`, `setPackSequence`) are the real ones:
  // they are the boundary's own shape logic, and stubbing them would let a
  // wrong pack sequence pass these tests. Only the adapter CLASS is replaced.
  ...(await importOriginal<typeof import("../../adapter/draft-adapter")>()),
  DraftAdapter: class {
    draftProcedure = mocks.draftProcedure;
  },
}));

import { useDraftPodStore } from "../draftPodStore";
import { useConnectivityStore } from "../connectivityStore";

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
  poolInput: { type: "Set" as const, data: { pools: [{ code: "TST" }], sequence: ["TST"] } },
};

describe("draftPodStore", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    mocks.draftProcedure.mockResolvedValue({
      pod_size: 8,
      human_seats: 1,
      min_pod_size: 2,
      max_pod_size: 8,
      allowed_pod_sizes: [2, 3, 4, 5, 6, 7, 8],
      packs_per_player: 3,
      cards_per_pick: 1,
      distribution: "PickAndPass",
      min_deck_size: 40,
      post_draft_play: "TournamentPairings",
      match_config: { best_of: 3 },
    });
    mocks.multiplayerState.role = null;
    mocks.multiplayerState.phase = "idle";
    mocks.multiplayerState.roomCode = null;
    mocks.multiplayerState.hostDraft = vi.fn<(config: unknown) => Promise<boolean>>(async () => true);
    mocks.multiplayerState.joinDraft = vi.fn<(config: unknown) => Promise<boolean>>(async () => true);
    mocks.multiplayerConfig.hostingServer = "wss://phase.example/ws";
    mocks.persistedDraftHostSessionState.mockReturnValue("live");
    mocks.inspectActiveDraftPod.mockReturnValue({
      type: "absent",
    });
    useConnectivityStore.setState({ forcedOffline: false, browserOnline: true });
    useDraftPodStore.getState().reset();
  });

  describe("offline orchestration boundary", () => {
    it.each([
      ["procedure entry", () => useDraftPodStore.getState().enterKind("Premier")],
      ["procedure refresh", () => useDraftPodStore.getState().refreshProcedure()],
      ["pod creation", () => useDraftPodStore.getState().createPod()],
      ["pod join", () => useDraftPodStore.getState().joinPod()],
      ["draft start", () => useDraftPodStore.getState().startDraft()],
      ["host recovery", () => useDraftPodStore.getState().resumeHostedPod()],
    ])("does not begin %s while effective offline", async (_label, run) => {
      useConnectivityStore.setState({ forcedOffline: true });

      await run();

      expect(mocks.draftProcedure).not.toHaveBeenCalled();
      expect(mocks.multiplayerState.hostDraft).not.toHaveBeenCalled();
      expect(useDraftPodStore.getState().configError).toBe("offline.startUnavailable");
    });
  });

  describe("enterKind", () => {
    // Every axis but `pod_size` is inert here; only `pod_size` is read.
    function procedure(podSize: number) {
      return {
        pod_size: podSize,
        human_seats: 1,
        min_pod_size: 3,
        max_pod_size: 8,
        allowed_pod_sizes: [3, 4, 5, 6, 7, 8],
        packs_per_player: 3,
        cards_per_pick: 2,
        distribution: "PickAndPass",
        min_deck_size: 60,
        post_draft_play: "CompleteImmediately",
        match_config: { best_of: 1 },
      };
    }

    it("applies the kind and adopts the engine's pod-size default", async () => {
      mocks.draftProcedure.mockResolvedValue(procedure(4));

      await useDraftPodStore.getState().enterKind("CommanderDraft");

      // Reach guard: the engine read really happened, so `podSize` below is an
      // adopted value rather than a constant that coincides with it.
      expect(mocks.draftProcedure).toHaveBeenCalledWith("CommanderDraft", "Swiss");
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

    it("uses the procedure distribution to select a set pool", async () => {
      // The kind is deliberately not the old all-at-once kind. This proves the
      // client follows the engine-published distribution rather than inferring
      // pool behavior from a kind name.
      mocks.draftProcedure.mockResolvedValue({
        ...procedure(8),
        distribution: "AllAtOnce",
      });
      useDraftPodStore.getState().setPoolMode("cube");

      await useDraftPodStore.getState().enterKind("Premier");

      expect(useDraftPodStore.getState().config.kind).toBe("Premier");
      expect(useDraftPodStore.getState().poolMode).toBe("set");
    });

    it("ignores a stale kind response after a newer kind has loaded", async () => {
      let resolveCommander!: () => void;
      let resolvePremier!: () => void;
      mocks.draftProcedure.mockImplementation((kind: string) => new Promise((resolve) => {
        if (kind === "CommanderDraft") {
          resolveCommander = () => resolve({
            ...procedure(4),
            post_draft_play: "CompleteImmediately",
          });
          return;
        }
        resolvePremier = () => resolve({
          ...procedure(6),
        min_pod_size: 2,
        allowed_pod_sizes: [2, 3, 4, 5, 6, 7, 8],
        post_draft_play: "TournamentPairings",
        });
      }));

      const commander = useDraftPodStore.getState().enterKind("CommanderDraft");
      const premier = useDraftPodStore.getState().enterKind("Premier");

      resolvePremier();
      await premier;
      resolveCommander();
      await commander;

      expect(useDraftPodStore.getState()).toMatchObject({
        config: { kind: "Premier", podSize: 6 },
        allowedPodSizes: [2, 3, 4, 5, 6, 7, 8],
        packDistribution: "PickAndPass",
        packsPerPlayer: 3,
      });
    });

    it("does not let an older same-kind entry overwrite a newer refresh", async () => {
      const pending: Array<(value: ReturnType<typeof procedure>) => void> = [];
      mocks.draftProcedure.mockImplementation(() => new Promise((resolve) => {
        pending.push(resolve);
      }));
      useDraftPodStore.getState().setConfig({ podSize: 4 });

      const entering = useDraftPodStore.getState().enterKind("Premier");
      const refreshing = useDraftPodStore.getState().refreshProcedure();

      expect(pending).toHaveLength(2);
      pending[1]!({
        ...procedure(8),
        min_pod_size: 2,
        allowed_pod_sizes: [2, 3, 4, 5, 6, 7, 8],
        packs_per_player: 6,
        post_draft_play: "TournamentPairings",
      });
      await refreshing;
      pending[0]!({
        ...procedure(6),
        min_pod_size: 3,
        allowed_pod_sizes: [3, 4, 5, 6, 7, 8],
        packs_per_player: 4,
        post_draft_play: "CompleteImmediately",
      });
      await entering;

      // The newer refresh keeps both its full cache and the host-selected
      // size. Without a request identity, the older entry adopts 6 here.
      expect(useDraftPodStore.getState()).toMatchObject({
        config: { kind: "Premier", podSize: 4 },
        allowedPodSizes: [2, 3, 4, 5, 6, 7, 8],
        packDistribution: "PickAndPass",
        packsPerPlayer: 6,
      });
    });

    it("drops a pending procedure failure after reset", async () => {
      let rejectProcedure!: (error: Error) => void;
      mocks.draftProcedure.mockImplementation(() => new Promise((_resolve, reject) => {
        rejectProcedure = reject;
      }));

      const entering = useDraftPodStore.getState().enterKind("CommanderDraft");
      useDraftPodStore.getState().reset();
      rejectProcedure(new Error("stale wasm failure"));
      await entering;

      expect(useDraftPodStore.getState()).toMatchObject({
        config: { kind: "Premier", podSize: 8 },
        allowedPodSizes: null,
        packDistribution: null,
        packsPerPlayer: null,
        configError: null,
      });
    });

    it("does not allow cube selection when the procedure distributes all packs at once", () => {
      // A hostile Premier procedure catches a reintroduction of `kind ===
      // \"Sealed\"` into the pool-mode reducer.
      useDraftPodStore.setState({ packDistribution: "AllAtOnce", poolMode: "set" });

      useDraftPodStore.getState().setPoolMode("cube");

      expect(useDraftPodStore.getState().poolMode).toBe("set");
    });
  });

  describe("setConfig", () => {
    it("records host intent without reinterpreting tournament policy", () => {
      useDraftPodStore.getState().setConfig({
        tournamentFormat: "SingleElimination",
        podSize: 2,
      });

      expect(useDraftPodStore.getState().config).toMatchObject({
        tournamentFormat: "SingleElimination",
        podSize: 2,
      });
    });

    it("drops a delayed procedure response after its tournament format changes", async () => {
      let resolveProcedure!: (value: unknown) => void;
      mocks.draftProcedure.mockImplementationOnce(() => new Promise<unknown>((resolve) => {
        resolveProcedure = resolve;
      }));
      useDraftPodStore.setState({
        allowedPodSizes: [2, 3, 4, 5, 6, 7, 8],
        procedureCacheKey: { kind: "Premier", tournamentFormat: "Swiss" },
      });

      const refreshing = useDraftPodStore.getState().refreshProcedure();
      useDraftPodStore.getState().setConfig({ tournamentFormat: "SingleElimination" });

      // The previous Swiss cache is invalid immediately, before the format's
      // replacement request starts, so the selector has no stale values to show.
      expect(useDraftPodStore.getState()).toMatchObject({
        allowedPodSizes: null,
        procedureCacheKey: null,
      });
      expect(mocks.draftProcedure).toHaveBeenCalledWith("Premier", "Swiss");

      resolveProcedure({
        pod_size: 8,
        human_seats: 1,
        min_pod_size: 2,
        max_pod_size: 8,
        allowed_pod_sizes: [2, 3, 4, 5, 6, 7, 8],
        packs_per_player: 3,
        cards_per_pick: 1,
        distribution: "PickAndPass",
        min_deck_size: 40,
        post_draft_play: "TournamentPairings",
        match_config: { best_of: 3 },
      });
      await refreshing;

      expect(useDraftPodStore.getState()).toMatchObject({
        config: { tournamentFormat: "SingleElimination" },
        allowedPodSizes: null,
        procedureCacheKey: null,
      });
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

    it("does not publish a persisted pod after a newer procedure request", async () => {
      let resolveSession!: (session: typeof persistedSession) => void;
      mocks.inspectActiveDraftPod.mockReturnValue({ type: "present", meta: activeMeta, capture: { id: activeMeta.id, roomCode: activeMeta.roomCode, updatedAt: activeMeta.updatedAt } });
      mocks.loadDraftHostSession.mockReturnValue(new Promise((resolve) => {
        resolveSession = resolve;
      }));

      const resuming = useDraftPodStore.getState().resumeHostedPod();
      await useDraftPodStore.getState().enterKind("CommanderDraft");
      resolveSession(persistedSession);

      await expect(resuming).resolves.toBe("superseded");
      expect(useDraftPodStore.getState().config.kind).toBe("CommanderDraft");
      expect(mocks.multiplayerState.hostDraft).not.toHaveBeenCalled();
    });

    it.each([
      ["resets", () => useDraftPodStore.getState().reset()],
      ["starts a newer procedure", () => useDraftPodStore.getState().refreshProcedure()],
    ])("does not host when recovery's procedure lookup is superseded by %s", async (_reason, supersede) => {
      let resolveProcedure!: (value: Record<string, unknown>) => void;
      mocks.inspectActiveDraftPod.mockReturnValue({ type: "present", meta: activeMeta, capture: { id: activeMeta.id, roomCode: activeMeta.roomCode, updatedAt: activeMeta.updatedAt } });
      mocks.loadDraftHostSession.mockResolvedValue(persistedSession);
      mocks.draftProcedure.mockImplementationOnce(() => new Promise((resolve) => {
        resolveProcedure = resolve;
      }));

      const resuming = useDraftPodStore.getState().resumeHostedPod();
      await Promise.resolve();
      expect(mocks.draftProcedure).toHaveBeenCalledOnce();
      supersede();
      resolveProcedure({
        pod_size: 8,
        human_seats: 1,
        min_pod_size: 2,
        max_pod_size: 8,
        allowed_pod_sizes: [2, 3, 4, 5, 6, 7, 8],
        packs_per_player: 3,
        cards_per_pick: 1,
        distribution: "PickAndPass",
        min_deck_size: 40,
        post_draft_play: "TournamentPairings",
        match_config: { best_of: 3 },
      });

      await expect(resuming).resolves.toBe("superseded");
      expect(mocks.multiplayerState.hostDraft).not.toHaveBeenCalled();
    });

    it("publishes every procedure cache axis for a resumed pod", async () => {
      mocks.inspectActiveDraftPod.mockReturnValue({ type: "present", meta: activeMeta, capture: { id: activeMeta.id, roomCode: activeMeta.roomCode, updatedAt: activeMeta.updatedAt } });
      mocks.loadDraftHostSession.mockResolvedValue(persistedSession);
      mocks.draftProcedure.mockResolvedValue({
        pod_size: 8,
        human_seats: 1,
        min_pod_size: 2,
        max_pod_size: 8,
        allowed_pod_sizes: [2, 3, 4, 5, 6, 7, 8],
        packs_per_player: 6,
        cards_per_pick: 1,
        distribution: "PickAndPass",
        min_deck_size: 40,
        post_draft_play: "TournamentPairings",
        match_config: { best_of: 3 },
      });

      await expect(useDraftPodStore.getState().resumeHostedPod()).resolves.toBe("resumed");

      expect(useDraftPodStore.getState()).toMatchObject({
        allowedPodSizes: [2, 3, 4, 5, 6, 7, 8],
        packDistribution: "PickAndPass",
        packsPerPlayer: 6,
      });
    });

    it("does not report recovery as resumed when host initialization fails", async () => {
      mocks.inspectActiveDraftPod.mockReturnValue({ type: "present", meta: activeMeta, capture: { id: activeMeta.id, roomCode: activeMeta.roomCode, updatedAt: activeMeta.updatedAt } });
      mocks.loadDraftHostSession.mockResolvedValue(persistedSession);
      mocks.multiplayerState.hostDraft = vi.fn<(config: unknown) => Promise<boolean>>(async () => false);

      await expect(useDraftPodStore.getState().resumeHostedPod({ routeToken: 4 })).resolves.toBe("invalid");
      expect(mocks.clearActiveDraftPodIfCurrent).not.toHaveBeenCalled();
    });

    it.each([
      ["offline", () => {
        useConnectivityStore.setState({ forcedOffline: true });
        throw new Error("IndexedDB unavailable");
      }, "offline", "offline.startUnavailable"],
      ["ordinary", () => { throw new Error("IndexedDB unavailable"); }, "invalid", "IndexedDB unavailable"],
    ])("maps a rejected host session read by current ownership before %s handling", async (_label, reject, outcome, error) => {
      mocks.inspectActiveDraftPod.mockReturnValue({ type: "present", meta: activeMeta, capture: { id: activeMeta.id, roomCode: activeMeta.roomCode, updatedAt: activeMeta.updatedAt } });
      mocks.loadDraftHostSession.mockImplementationOnce(async () => reject());

      await expect(useDraftPodStore.getState().resumeHostedPod()).resolves.toBe(outcome);

      expect(mocks.multiplayerState.hostDraft).not.toHaveBeenCalled();
      expect(mocks.clearActiveDraftPodIfCurrent).not.toHaveBeenCalled();
      expect(useDraftPodStore.getState().configError).toBe(error);
    });

    it.each([
      ["forced offline", { forcedOffline: true, browserOnline: true }],
      ["browser offline", { forcedOffline: false, browserOnline: false }],
    ] as const)("returns offline after a fulfilled hosted session read becomes %s", async (_label, connectivity) => {
      let resolveSession!: (session: typeof persistedSession) => void;
      mocks.inspectActiveDraftPod.mockReturnValue({
        type: "present",
        meta: activeMeta,
        capture: { id: activeMeta.id, roomCode: activeMeta.roomCode, updatedAt: activeMeta.updatedAt },
      });
      mocks.loadDraftHostSession.mockImplementationOnce(() => new Promise((resolve) => {
        resolveSession = resolve;
      }));

      const resuming = useDraftPodStore.getState().resumeHostedPod();
      await vi.waitFor(() => expect(mocks.loadDraftHostSession).toHaveBeenCalledOnce());
      useConnectivityStore.setState(connectivity);
      resolveSession(persistedSession);

      await expect(resuming).resolves.toBe("offline");
      expect(mocks.clearActiveDraftPodIfCurrent).not.toHaveBeenCalled();
      expect(mocks.draftProcedure).not.toHaveBeenCalled();
      expect(mocks.multiplayerState.hostDraft).not.toHaveBeenCalled();
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

    /**
     * A host who refreshes mid-lobby must land back on the pack sequence they
     * arranged — order included — not on an unlabelled pod they have to
     * rebuild.
     */
    it("restores the pack sequence from a persisted multi-set snapshot", async () => {
      mocks.inspectActiveDraftPod.mockReturnValue({ type: "present", meta: activeMeta, capture: { id: activeMeta.id, roomCode: activeMeta.roomCode, updatedAt: activeMeta.updatedAt } });
      mocks.loadDraftHostSession.mockResolvedValue({
        ...persistedSession,
        poolInput: {
          type: "Set" as const,
          data: {
            pools: [{ code: "ISD" }, { code: "DKA" }],
            sequence: ["ISD", "DKA", "ISD"],
          },
        },
      });

      await useDraftPodStore.getState().resumeHostedPod();

      const state = useDraftPodStore.getState();
      expect(state.poolMode).toBe("set");
      expect(state.config.packs.map((pack) => pack.code)).toEqual(["ISD", "DKA", "ISD"]);
      // The label dedupes, mirroring the engine's own `DraftSource::set_code`.
      expect(state.config.setCode).toBe("ISD+DKA");
    });

    it("restores a Chaos candidate selection and re-hosts its private source unchanged", async () => {
      const chaosSession = {
        ...persistedSession,
        poolInput: {
          type: "Chaos" as const,
          data: {
            pools: [{ code: "ISD" }, { code: "DKA" }],
            candidate_codes: ["ISD", "DKA"],
          },
        },
      };
      mocks.inspectActiveDraftPod.mockReturnValue({ type: "present", meta: activeMeta, capture: { id: activeMeta.id, roomCode: activeMeta.roomCode, updatedAt: activeMeta.updatedAt } });
      mocks.loadDraftHostSession.mockResolvedValue(chaosSession);

      await useDraftPodStore.getState().resumeHostedPod();

      const state = useDraftPodStore.getState();
      expect(state.poolMode).toBe("set");
      expect(state.setDraftMode).toBe("chaos");
      expect(state.config.packs.map((pack) => pack.code)).toEqual(["ISD", "DKA"]);
      const dispatched = mocks.multiplayerState.hostDraft.mock.calls[0]?.[0] as {
        poolInput: { type: string; data: { candidate_codes: string[] } };
      };
      expect(dispatched.poolInput).toEqual(chaosSession.poolInput);
    });

    /**
     * A pod persisted before multi-set pods existed carries one serialized pool
     * and no sequence. It must still resume — draft-wasm promotes that spelling
     * to the single-set pod it always meant — rather than being discarded.
     */
    it("resumes a pre-multi-set snapshot with no pack sequence", async () => {
      mocks.inspectActiveDraftPod.mockReturnValue({ type: "present", meta: activeMeta, capture: { id: activeMeta.id, roomCode: activeMeta.roomCode, updatedAt: activeMeta.updatedAt } });
      mocks.loadDraftHostSession.mockResolvedValue({
        ...persistedSession,
        poolInput: { type: "Set" as const, data: { set_pool_json: '{"code":"TST"}' } },
      });

      const outcome = await useDraftPodStore.getState().resumeHostedPod();

      expect(outcome).toBe("resumed");
      expect(mocks.multiplayerState.hostDraft).toHaveBeenCalledOnce();
      const state = useDraftPodStore.getState();
      expect(state.poolMode).toBe("set");
      expect(state.config.packs).toEqual([]);
      expect(state.config.setName).toBe("Draft Pod");
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
      expect((dispatched as { backupEndpoint?: string }).backupEndpoint).toBe("https://phase.example");
    });

    it("surfaces a current false host result for cube creation", async () => {
      mocks.multiplayerState.hostDraft.mockResolvedValueOnce(false);
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
      expect(useDraftPodStore.getState().configError).toBe("Unable to host draft pod");
    });
  });

  describe("createPod (set branch)", () => {
    afterEach(() => {
      vi.unstubAllGlobals();
    });

    /** Stub the pool fetch with a `draft-pools.json` carrying `codes`. */
    function stubPools(codes: string[]): void {
      vi.stubGlobal("__DRAFT_POOLS_URL__", "/draft-pools.json");
      const pools = Object.fromEntries(codes.map((code) => [code.toLowerCase(), { code }]));
      vi.stubGlobal(
        "fetch",
        vi.fn(async () => ({ ok: true, status: 200, json: async () => pools })),
      );
    }

    function hostedPoolInput(): { type: string; data: { pools: unknown[]; sequence: string[] } } {
      const [config] = mocks.multiplayerState.hostDraft.mock.calls[0] as [
        { poolInput: { type: string; data: { pools: unknown[]; sequence: string[] } } },
      ];
      return config.poolInput;
    }

    it("abandons a stale creation before fetching pools or hosting", async () => {
      let resolveCreateProcedure!: (procedure: Record<string, unknown>) => void;
      const fetchMock = vi.fn();
      vi.stubGlobal("fetch", fetchMock);
      mocks.draftProcedure.mockImplementationOnce(() => new Promise((resolve) => {
        resolveCreateProcedure = resolve;
      }));
      useDraftPodStore.setState((prev) => ({
        config: {
          ...prev.config,
          packs: [{ code: "ISD", name: "Innistrad" }],
          setCode: "ISD",
        },
        hostDisplayName: "Host",
      }));

      const creating = useDraftPodStore.getState().createPod();
      await useDraftPodStore.getState().enterKind("CommanderDraft");
      resolveCreateProcedure({
        pod_size: 8,
        human_seats: 8,
        min_pod_size: 2,
        max_pod_size: 8,
        allowed_pod_sizes: [2, 3, 4, 5, 6, 7, 8],
        packs_per_player: 3,
        cards_per_pick: 1,
        distribution: "PickAndPass",
        min_deck_size: 40,
        post_draft_play: "TournamentPairings",
        match_config: { best_of: 3 },
      });

      await creating;
      expect(fetchMock).not.toHaveBeenCalled();
      expect(mocks.multiplayerState.hostDraft).not.toHaveBeenCalled();
    });

    /**
     * THE multiplayer multi-set claim at the store: the ORDER the host arranged
     * reaches the host adapter intact, and each distinct set's pool crosses the
     * boundary exactly once no matter how many boosters it fills.
     */
    it("ships the host's pack order and one pool per distinct set", async () => {
      stubPools(["ISD", "DKA"]);
      mocks.draftProcedure.mockResolvedValue({
        pod_size: 8,
        human_seats: 1,
        min_pod_size: 2,
        max_pod_size: 8,
        allowed_pod_sizes: [2, 3, 4, 5, 6, 7, 8],
        packs_per_player: 6,
        cards_per_pick: 1,
        distribution: "PickAndPass",
        min_deck_size: 40,
        post_draft_play: "TournamentPairings",
        match_config: { best_of: 3 },
      });
      useDraftPodStore.setState((prev) => ({
        config: {
          ...prev.config,
          packs: [
            { code: "ISD", name: "Innistrad" },
            { code: "DKA", name: "Dark Ascension" },
            { code: "ISD", name: "Innistrad" },
          ],
          setCode: "ISD+DKA",
        },
        hostDisplayName: "Host",
      }));

      await useDraftPodStore.getState().createPod();

      const poolInput = hostedPoolInput();
      expect(poolInput.type).toBe("Set");
      expect(poolInput.data.sequence).toEqual(["ISD", "DKA", "ISD"]);
      // Deduped, and in first-appearance order — the sequence is what repeats.
      expect(poolInput.data.pools).toEqual([{ code: "ISD" }, { code: "DKA" }]);
      const [hostConfig] = mocks.multiplayerState.hostDraft.mock.calls[0] as [
        { backupEndpoint?: string },
      ];
      expect(hostConfig.backupEndpoint).toBe("https://phase.example");
      expect(useDraftPodStore.getState()).toMatchObject({
        allowedPodSizes: [2, 3, 4, 5, 6, 7, 8],
        packDistribution: "PickAndPass",
        packsPerPlayer: 6,
      });
    });

    it("uses the procedure's exact seat set before hosting", async () => {
      stubPools(["ISD"]);
      mocks.draftProcedure.mockResolvedValue({
        pod_size: 8,
        human_seats: 1,
        min_pod_size: 2,
        max_pod_size: 8,
        allowed_pod_sizes: [8],
        packs_per_player: 3,
        cards_per_pick: 1,
        distribution: "PickAndPass",
        min_deck_size: 40,
        post_draft_play: "TournamentPairings",
        match_config: { best_of: 3 },
      });
      useDraftPodStore.setState((prev) => ({
        config: {
          ...prev.config,
          packs: [{ code: "ISD", name: "Innistrad" }],
          setCode: "ISD",
          podSize: 2,
          tournamentFormat: "SingleElimination",
        },
        hostDisplayName: "Host",
      }));

      await useDraftPodStore.getState().createPod();

      expect(useDraftPodStore.getState().allowedPodSizes).toEqual([8]);
      expect(mocks.multiplayerState.hostDraft).toHaveBeenCalledWith(
        expect.objectContaining({ podSize: 8 }),
      );
    });

    /**
     * A set the host named with no pool data must fail creation by name rather
     * than shipping a sequence the engine will refuse mid-draft. Checked on a
     * LATER entry, since resolving only the first code would still pass.
     */
    it("refuses a pack list naming a set with no pool data", async () => {
      stubPools(["ISD"]);
      mocks.draftProcedure.mockResolvedValue({
        pod_size: 8,
        human_seats: 1,
        min_pod_size: 2,
        max_pod_size: 8,
        allowed_pod_sizes: [2, 3, 4, 5, 6, 7, 8],
        packs_per_player: 3,
        cards_per_pick: 1,
        distribution: "PickAndPass",
        min_deck_size: 40,
        post_draft_play: "TournamentPairings",
        match_config: { best_of: 3 },
      });
      useDraftPodStore.setState((prev) => ({
        config: {
          ...prev.config,
          packs: [
            { code: "ISD", name: "Innistrad" },
            { code: "NOPE", name: "Missing" },
          ],
          setCode: "ISD+NOPE",
        },
        hostDisplayName: "Host",
      }));

      await useDraftPodStore.getState().createPod();

      expect(mocks.multiplayerState.hostDraft).not.toHaveBeenCalled();
      expect(useDraftPodStore.getState().configError).toBe("No pool data for set: NOPE");
    });

    it("refuses an empty pack list", async () => {
      stubPools(["ISD"]);
      useDraftPodStore.setState((prev) => ({
        config: { ...prev.config, packs: [], setCode: "" },
        hostDisplayName: "Host",
      }));

      await useDraftPodStore.getState().createPod();

      expect(mocks.multiplayerState.hostDraft).not.toHaveBeenCalled();
      expect(useDraftPodStore.getState().configError).toBe("Select a set first");
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
        config: {
          ...prev.config,
          packs: [{ code: "EOE", name: "Edge of Eternities" }],
          setCode: "EOE",
        },
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

  describe("offline deferred orchestration settlement", () => {
    afterEach(() => {
      vi.unstubAllGlobals();
    });

    function configureSetPod() {
      useDraftPodStore.setState((prev) => ({
        config: {
          ...prev.config,
          packs: [{ code: "TST", name: "Test Set" }],
          setCode: "TST",
        },
        hostDisplayName: "Host",
      }));
    }

    function procedure() {
      return {
        pod_size: 8,
        human_seats: 1,
        min_pod_size: 2,
        max_pod_size: 8,
        allowed_pod_sizes: [2, 3, 4, 5, 6, 7, 8],
        packs_per_player: 3,
        cards_per_pick: 1,
        distribution: "PickAndPass",
        min_deck_size: 40,
        post_draft_play: "TournamentPairings",
        match_config: { best_of: 3 },
      };
    }

    it.each([
      ["fulfillment", (resolve: (value: ReturnType<typeof procedure>) => void, _reject: (reason: Error) => void) => resolve(procedure())],
      ["rejection", (_resolve: (value: ReturnType<typeof procedure>) => void, reject: (reason: Error) => void) => reject(new Error("wasm unavailable"))],
    ])("keeps an offline procedure %s from starting pool work", async (_label, settle) => {
      let resolveProcedure!: (value: ReturnType<typeof procedure>) => void;
      let rejectProcedure!: (reason: Error) => void;
      mocks.draftProcedure.mockImplementationOnce(() => new Promise((resolve, reject) => {
        resolveProcedure = resolve;
        rejectProcedure = reject;
      }));
      const fetchMock = vi.fn();
      vi.stubGlobal("fetch", fetchMock);
      configureSetPod();

      const creating = useDraftPodStore.getState().createPod();
      await Promise.resolve();
      expect(mocks.draftProcedure).toHaveBeenCalledOnce();
      useConnectivityStore.setState({ forcedOffline: true });
      settle(resolveProcedure, rejectProcedure);

      await creating;
      expect(fetchMock).not.toHaveBeenCalled();
      expect(mocks.multiplayerState.hostDraft).not.toHaveBeenCalled();
      expect(useDraftPodStore.getState().configError).toBe("offline.startUnavailable");
    });

    it("stops after a held pool response becomes offline", async () => {
      let resolveResponse!: (response: { ok: boolean; status: number; json: () => Promise<Record<string, unknown>> }) => void;
      vi.stubGlobal("__DRAFT_POOLS_URL__", "/draft-pools.json");
      const fetchMock = vi.fn(() => new Promise((resolve) => { resolveResponse = resolve; }));
      vi.stubGlobal("fetch", fetchMock);
      configureSetPod();

      const creating = useDraftPodStore.getState().createPod();
      await vi.waitFor(() => expect(fetchMock).toHaveBeenCalledOnce());
      useConnectivityStore.setState({ browserOnline: false });
      resolveResponse({ ok: true, status: 200, json: async () => ({ tst: { code: "TST" } }) });

      await creating;
      expect(mocks.multiplayerState.hostDraft).not.toHaveBeenCalled();
      expect(useDraftPodStore.getState()).toMatchObject({ loadingPool: false, configError: "offline.startUnavailable" });
    });

    it("stops after a held pool JSON parse becomes offline", async () => {
      let resolveJson!: (value: Record<string, unknown>) => void;
      vi.stubGlobal("__DRAFT_POOLS_URL__", "/draft-pools.json");
      vi.stubGlobal("fetch", vi.fn(async () => ({
        ok: true,
        status: 200,
        json: () => new Promise((resolve) => { resolveJson = resolve; }),
      })));
      configureSetPod();

      const creating = useDraftPodStore.getState().createPod();
      await vi.waitFor(() => expect(resolveJson).toBeTypeOf("function"));
      useConnectivityStore.setState({ forcedOffline: true });
      resolveJson({ tst: { code: "TST" } });

      await creating;
      expect(mocks.multiplayerState.hostDraft).not.toHaveBeenCalled();
      expect(useDraftPodStore.getState()).toMatchObject({ loadingPool: false, configError: "offline.startUnavailable" });
    });

    it.each([
      ["response", { browserOnline: false }],
      ["JSON", { forcedOffline: true }],
    ] as const)("maps a rejected pool %s read to offline after connectivity changes", async (stage, connectivity) => {
      let rejectRead!: (reason: Error) => void;
      vi.stubGlobal("__DRAFT_POOLS_URL__", "/draft-pools.json");
      if (stage === "response") {
        vi.stubGlobal("fetch", vi.fn(() => new Promise((_resolve, reject) => { rejectRead = reject; })));
      } else {
        vi.stubGlobal("fetch", vi.fn(async () => ({
          ok: true,
          status: 200,
          json: () => new Promise((_resolve, reject) => { rejectRead = reject; }),
        })));
      }
      configureSetPod();

      const creating = useDraftPodStore.getState().createPod();
      await vi.waitFor(() => expect(rejectRead).toBeTypeOf("function"));
      useConnectivityStore.setState(connectivity);
      rejectRead(new Error(`${stage} unavailable`));

      await creating;
      expect(mocks.multiplayerState.hostDraft).not.toHaveBeenCalled();
      expect(useDraftPodStore.getState()).toMatchObject({ loadingPool: false, configError: "offline.startUnavailable" });
    });

    it("maps a current false host result to offline after set-pool creation", async () => {
      let resolveHost!: (value: boolean) => void;
      vi.stubGlobal("__DRAFT_POOLS_URL__", "/draft-pools.json");
      vi.stubGlobal("fetch", vi.fn(async () => ({ ok: true, status: 200, json: async () => ({ tst: { code: "TST" } }) })));
      mocks.multiplayerState.hostDraft.mockImplementationOnce(() => new Promise((resolve) => {
        resolveHost = resolve;
      }));
      configureSetPod();

      const creating = useDraftPodStore.getState().createPod();
      await vi.waitFor(() => expect(mocks.multiplayerState.hostDraft).toHaveBeenCalledOnce());
      useConnectivityStore.setState({ forcedOffline: true });
      resolveHost(false);

      await creating;
      expect(useDraftPodStore.getState().configError).toBe("offline.startUnavailable");
    });

    it("maps a current false guest join result to offline", async () => {
      let resolveJoin!: (value: boolean) => void;
      mocks.multiplayerState.joinDraft.mockImplementationOnce(() => new Promise((resolve) => {
        resolveJoin = resolve;
      }));
      useDraftPodStore.setState({ joinCode: "ABCDE", guestDisplayName: "Alice" });

      const joining = useDraftPodStore.getState().joinPod();
      await vi.waitFor(() => expect(mocks.multiplayerState.joinDraft).toHaveBeenCalledOnce());
      useConnectivityStore.setState({ browserOnline: false });
      resolveJoin(false);

      await joining;
      expect(useDraftPodStore.getState().configError).toBe("offline.startUnavailable");
    });

    it("retires a stale pool spinner when a newer public orchestration starts", async () => {
      let resolveResponse!: (response: { ok: boolean; status: number; json: () => Promise<Record<string, unknown>> }) => void;
      vi.stubGlobal("__DRAFT_POOLS_URL__", "/draft-pools.json");
      const fetchMock = vi.fn(() => new Promise((resolve) => { resolveResponse = resolve; }));
      vi.stubGlobal("fetch", fetchMock);
      configureSetPod();

      const creating = useDraftPodStore.getState().createPod();
      await vi.waitFor(() => expect(fetchMock).toHaveBeenCalledOnce());
      expect(useDraftPodStore.getState().loadingPool).toBe(true);

      await useDraftPodStore.getState().refreshProcedure();
      expect(useDraftPodStore.getState().loadingPool).toBe(false);
      resolveResponse({ ok: true, status: 200, json: async () => ({ tst: { code: "TST" } }) });
      await creating;

      expect(mocks.multiplayerState.hostDraft).not.toHaveBeenCalled();
    });

    it.each(["fulfillment", "rejection"] as const)("keeps a stale procedure %s from overwriting a newer offline join", async (settlement) => {
      let resolveProcedure!: (value: ReturnType<typeof procedure>) => void;
      let rejectProcedure!: (reason: Error) => void;
      mocks.draftProcedure.mockImplementationOnce(() => new Promise((resolve, reject) => {
        resolveProcedure = resolve;
        rejectProcedure = reject;
      }));
      let resolveJoin!: (value: boolean) => void;
      mocks.multiplayerState.joinDraft.mockImplementationOnce(() => new Promise((resolve) => {
        resolveJoin = resolve;
      }));

      const entering = useDraftPodStore.getState().enterKind("CommanderDraft");
      await vi.waitFor(() => expect(mocks.draftProcedure).toHaveBeenCalledOnce());
      useDraftPodStore.getState().setJoinCode("ABCDE");
      useDraftPodStore.getState().setGuestDisplayName("Alice");
      const joining = useDraftPodStore.getState().joinPod();
      await vi.waitFor(() => expect(mocks.multiplayerState.joinDraft).toHaveBeenCalledOnce());

      useConnectivityStore.setState({ forcedOffline: true });
      resolveJoin(false);
      await joining;
      const newerOffline = useDraftPodStore.getState();
      expect(newerOffline).toMatchObject({
        config: { kind: "CommanderDraft" },
        loadingPool: false,
        configError: "offline.startUnavailable",
      });

      if (settlement === "fulfillment") resolveProcedure(procedure());
      else rejectProcedure(new Error("stale procedure failure"));
      await entering;

      expect(useDraftPodStore.getState()).toMatchObject({
        config: newerOffline.config,
        loadingPool: false,
        configError: "offline.startUnavailable",
      });
      expect(mocks.multiplayerState.hostDraft).not.toHaveBeenCalled();
    });
  });
});
