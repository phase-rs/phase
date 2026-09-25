import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import i18n from "i18next";
import type { DraftKind, DraftProcedure, TournamentFormat } from "../../adapter/draft-adapter";
import { draftProcedureFixture } from "../../adapter/__tests__/draftProcedureFixture";
import deMultiplayer from "../../i18n/locales/de/multiplayer.json";
import { resources, SUPPORTED_LNGS } from "../../i18n/resources";

const mocks = vi.hoisted(() => ({
  clearActiveDraftPod: vi.fn(),
  loadActiveDraftPod: vi.fn(),
  inspectActiveDraftPod: vi.fn(),
  clearActiveDraftPodIfCurrent: vi.fn(),
  loadDraftHostSession: vi.fn(),
  draftProcedure: vi.fn<(kind: DraftKind, tournamentFormat: TournamentFormat) => Promise<DraftProcedure>>(),
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
    // `adoptSavedDisplayName` reads the saved identity off this store, so the
    // mock carries the field under its real name. Empty is the real store's
    // own initial value, which keeps it out of the way of every other suite.
    displayName: "",
    userLobbySources: [],
    sourceStatus: new Map(),
    lastPodListingPublic: null as boolean | null,
    rememberPodListingPublic: vi.fn<(isPublic: boolean) => void>(),
    resolveP2PBroker: vi.fn<(anchor: string | null) => Promise<{
      url: string;
      socket: { serverInfo: { mode: string } } | null;
    }>>(),
  },
  openBrokerClient: vi.fn<(url: string) => Promise<{ close: () => void }>>(),
  brokerClose: vi.fn(),
}));

vi.mock("../../services/draftPersistence", async (importOriginal) => ({
  ...(await importOriginal<typeof import("../../services/draftPersistence")>()),
  clearActiveDraftPod: mocks.clearActiveDraftPod,
  loadActiveDraftPod: mocks.loadActiveDraftPod,
  inspectActiveDraftPod: mocks.inspectActiveDraftPod,
  clearActiveDraftPodIfCurrent: mocks.clearActiveDraftPodIfCurrent,
  loadDraftHostSession: mocks.loadDraftHostSession,
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

vi.mock("../../services/brokerClient", async (importOriginal) => ({
  ...(await importOriginal<typeof import("../../services/brokerClient")>()),
  openBrokerClient: mocks.openBrokerClient,
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
  draftStarted: false,
  draftCode: "ABCDE",
  draftSessionJson: null,
  poolInput: { type: "Set" as const, data: { pools: [{ code: "TST" }], sequence: ["TST"] } },
};

describe("draftPodStore", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    mocks.draftProcedure.mockResolvedValue(draftProcedureFixture());
    mocks.multiplayerState.role = null;
    mocks.multiplayerState.phase = "idle";
    mocks.multiplayerState.roomCode = null;
    mocks.multiplayerState.hostDraft = vi.fn<(config: unknown) => Promise<boolean>>(async () => true);
    mocks.multiplayerState.joinDraft = vi.fn<(config: unknown) => Promise<boolean>>(async () => true);
    mocks.multiplayerConfig.hostingServer = "wss://phase.example/ws";
    mocks.multiplayerConfig.displayName = "";
    mocks.multiplayerConfig.lastPodListingPublic = null;
    mocks.multiplayerConfig.rememberPodListingPublic = vi.fn<(isPublic: boolean) => void>();
    mocks.multiplayerConfig.resolveP2PBroker = vi.fn(async () => ({
      url: "wss://broker.example/ws",
      socket: { serverInfo: { mode: "LobbyOnly" } },
    }));
    mocks.brokerClose = vi.fn();
    mocks.openBrokerClient.mockReset().mockImplementation(async () => ({ close: mocks.brokerClose }));
    mocks.inspectActiveDraftPod.mockReturnValue({
      type: "absent",
    });
    useConnectivityStore.setState({ forcedOffline: false, browserOnline: true });
    useDraftPodStore.getState().reset();
  });

  describe("offline orchestration boundary", () => {
    it.each([
      ["procedure entry", () => useDraftPodStore.getState().enterKind("Premier")],
      ["entry procedure", () => useDraftPodStore.getState().enterKindForEntry("Premier")],
      ["procedure refresh", () => useDraftPodStore.getState().refreshProcedure()],
      ["pod creation", () => useDraftPodStore.getState().createPod()],
      ["pod join", () => useDraftPodStore.getState().joinPod()],
      ["draft start", () => useDraftPodStore.getState().startDraft()],
      ["host recovery", () => useDraftPodStore.getState().resumeHostedPod()],
    ])("does not begin %s while effective offline", async (_label, run) => {
      useConnectivityStore.setState({ forcedOffline: true });
      useDraftPodStore.setState({ loadingPool: true });

      await run();

      expect(mocks.draftProcedure).not.toHaveBeenCalled();
      expect(mocks.multiplayerState.hostDraft).not.toHaveBeenCalled();
      expect(useDraftPodStore.getState()).toMatchObject({
        loadingPool: false,
        configError: "offline.startUnavailable",
      });
    });
  });

  describe("enterKind", () => {
    // Every axis but `pod_size` is inert here; only `pod_size` is read.
    function procedure(podSize: number, cubeMinDeckSize = 73): DraftProcedure {
      return draftProcedureFixture({
        pod_size: podSize,
        human_seats: 1,
        min_pod_size: 3,
        max_pod_size: 8,
        allowed_pod_sizes: [3, 4, 5, 6, 7, 8],
        packs_per_player: 3,
        cards_per_pick: 2,
        distribution: "PickAndPass",
        min_deck_size: 60,
        cube_min_deck_size: cubeMinDeckSize,
        post_draft_play: "CompleteImmediately",
        match_config: { match_type: "Bo1" },
      });
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
      expect(useDraftPodStore.getState().cubeMinDeckSize).toBe(73);
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

    it("drops a chaos selection when the entered kind shares one stack", async () => {
      // The host arranged a Chaos pod under a pick-and-pass kind, then changed
      // the kind. Nothing in the UI can reach `setSetDraftMode` again on the
      // way through, so publication is where the stale intent has to go.
      // A contract that ADMITS Chaos has to be in place first: an absent one
      // normalizes the selection away, which is the point of the rows below.
      useDraftPodStore.setState({ allowedSetLayouts: ["UniformByRound", "Chaos"] });
      useDraftPodStore.getState().setSetDraftMode("chaos");
      expect(useDraftPodStore.getState().setDraftMode).toBe("chaos");
      mocks.draftProcedure.mockResolvedValue({
        ...procedure(2),
        min_pod_size: 2,
        max_pod_size: 4,
        allowed_pod_sizes: [2, 3, 4],
        distribution: { SharedStackPiles: { pile_count: 3 } },
        // The CAPABILITY is what the store reads, not the distribution. Spread
        // over the fixture, so it has to be narrowed here exactly as
        // `DraftProcedure::allowed_set_layouts` narrows it for a shared stack.
        allowed_set_layouts: ["UniformByRound"],
      });

      await useDraftPodStore.getState().enterKind("Winston");

      expect(useDraftPodStore.getState().config.kind).toBe("Winston");
      expect(useDraftPodStore.getState().setDraftMode).toBe("uniform");
    });

    /**
     * THE PUBLISHED LIST DECIDES, NOT THE DISTRIBUTION.
     *
     * Every other row here supplies a procedure whose `distribution` and
     * `allowed_set_layouts` AGREE, because the fixture derives one from the
     * other. That makes them all blind to the change this pair exists for:
     * restore `setDraftModeFor(prev.packDistribution, ...)` and they stay green,
     * because the two inputs give the same answer on every consistent fixture.
     *
     * So these two SKEW them on purpose. Neither procedure is one the engine
     * would publish -- that is the point: they isolate which input the store
     * actually reads. Both legs red if the store goes back to asking the
     * distribution.
     */
    /**
     * AN ABSENT CONTRACT IS NOT PERMISSION.
     *
     * `allowedSetLayouts` is `null` until a procedure has been published for the
     * current selection. This used to keep the host's request through that
     * window, on the reasoning that the engine refuses at `StartDraft` anyway --
     * but "it will be refused later" is not a reason to hold a selection the
     * engine may never honour, and it is how a stale Chaos intent survived a
     * kind change to reach a control that could not be satisfied.
     */
    it("normalizes a chaos selection while no layout contract has been published", () => {
      useDraftPodStore.setState({ allowedSetLayouts: ["UniformByRound", "Chaos"] });
      useDraftPodStore.getState().setSetDraftMode("chaos");
      // Reach guard: with a permitting contract the selection really does stick,
      // so the normalization below is the ABSENCE doing it and not the action.
      expect(useDraftPodStore.getState().setDraftMode).toBe("chaos");

      useDraftPodStore.setState({ allowedSetLayouts: null });
      useDraftPodStore.getState().setSetDraftMode("chaos");

      expect(useDraftPodStore.getState().setDraftMode).toBe("uniform");
    });

    it("keeps chaos when the published list allows it, whatever the distribution says", async () => {
      // A contract that ADMITS Chaos has to be in place first: an absent one
      // normalizes the selection away, which is the point of the rows below.
      useDraftPodStore.setState({ allowedSetLayouts: ["UniformByRound", "Chaos"] });
      useDraftPodStore.getState().setSetDraftMode("chaos");
      mocks.draftProcedure.mockResolvedValue({
        ...procedure(2),
        allowed_pod_sizes: [2, 3, 4],
        distribution: { SharedStackPiles: { pile_count: 3 } },
        allowed_set_layouts: ["UniformByRound", "Chaos"],
      });

      await useDraftPodStore.getState().enterKind("Winston");

      expect(useDraftPodStore.getState().setDraftMode).toBe("chaos");
    });

    it("drops chaos when the published list omits it, whatever the distribution says", async () => {
      // A contract that ADMITS Chaos has to be in place first: an absent one
      // normalizes the selection away, which is the point of the rows below.
      useDraftPodStore.setState({ allowedSetLayouts: ["UniformByRound", "Chaos"] });
      useDraftPodStore.getState().setSetDraftMode("chaos");
      mocks.draftProcedure.mockResolvedValue({
        ...procedure(2),
        allowed_pod_sizes: [2, 3, 4],
        distribution: "PickAndPass",
        allowed_set_layouts: ["UniformByRound"],
      });

      await useDraftPodStore.getState().enterKind("Premier");

      expect(useDraftPodStore.getState().setDraftMode).toBe("uniform");
    });

    it("keeps a chaos selection for a kind that passes packs", async () => {
      // The paired positive for the row above, through the SAME entry point:
      // publication normalizes on the distribution, not on every entry.
      // A contract that ADMITS Chaos has to be in place first: an absent one
      // normalizes the selection away, which is the point of the rows below.
      useDraftPodStore.setState({ allowedSetLayouts: ["UniformByRound", "Chaos"] });
      useDraftPodStore.getState().setSetDraftMode("chaos");
      mocks.draftProcedure.mockResolvedValue(procedure(8));

      await useDraftPodStore.getState().enterKind("Premier");

      expect(useDraftPodStore.getState().setDraftMode).toBe("chaos");
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

    it("publishes entry cache and dependent normalization atomically", async () => {
      mocks.draftProcedure.mockResolvedValue({
        ...procedure(4),
        distribution: "AllAtOnce",
        allowed_pod_sizes: [3, 4],
      });
      useDraftPodStore.setState({
        poolMode: "cube",
        loadingPool: true,
        configError: "stale error",
      });
      const emissions: Array<ReturnType<typeof useDraftPodStore.getState>> = [];
      const unsubscribe = useDraftPodStore.subscribe((state) => emissions.push(state));

      await useDraftPodStore.getState().enterKind("Premier");
      unsubscribe();

      const published = emissions.filter((state) => state.procedureCacheKey !== null);
      expect(published).not.toHaveLength(0);
      expect(published.every((state) =>
        state.poolMode === "set"
        && state.config.podSize === 4
        && state.loadingPool === false
        && state.configError === null
      )).toBe(true);
    });

    it("ignores a stale kind response after a newer kind has loaded", async () => {
      let resolveCommander!: () => void;
      let resolvePremier!: () => void;
      mocks.draftProcedure.mockImplementation((kind: string) => new Promise((resolve) => {
        if (kind === "CommanderDraft") {
          resolveCommander = () => resolve({
            ...procedure(4, 91),
            post_draft_play: "CompleteImmediately",
          });
          return;
        }
        resolvePremier = () => resolve({
          ...procedure(6, 73),
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
        cubeMinDeckSize: 73,
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
        ...procedure(8, 73),
        min_pod_size: 2,
        allowed_pod_sizes: [2, 3, 4, 5, 6, 7, 8],
        packs_per_player: 6,
        post_draft_play: "TournamentPairings",
      });
      await refreshing;
      pending[0]!({
        ...procedure(6, 91),
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
        cubeMinDeckSize: 73,
      });
    });

    it("drops a pending procedure success after reset", async () => {
      let resolveProcedure!: (value: ReturnType<typeof procedure>) => void;
      mocks.draftProcedure.mockImplementation(() => new Promise((resolve) => {
        resolveProcedure = resolve;
      }));

      const entering = useDraftPodStore.getState().enterKind("CommanderDraft");
      useDraftPodStore.getState().reset();
      resolveProcedure(procedure(6, 83));
      await entering;

      expect(useDraftPodStore.getState()).toMatchObject({
        config: { kind: "Premier", podSize: 8 },
        procedureCacheKey: null,
        cubeMinDeckSize: null,
        configError: null,
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
        cubeMinDeckSize: null,
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

    it("does not allow a chaos selection when the procedure shares one stack", () => {
      // Paired positive FIRST, on the same action and the same store: a
      // pick-and-pass distribution keeps the host's chaos intent, so the
      // refusal below is the distribution's doing and not the action's.
      useDraftPodStore.setState({
        packDistribution: "PickAndPass",
        allowedSetLayouts: ["UniformByRound", "Chaos"],
        setDraftMode: "uniform",
      });
      useDraftPodStore.getState().setSetDraftMode("chaos");
      expect(useDraftPodStore.getState().setDraftMode).toBe("chaos");

      // A shared stack shuffles every booster together before the first
      // decision, so a per-(seat, round) set assignment describes nothing the
      // players can observe — and `DraftProcedure::validate_source` refuses
      // the pair outright. The engine's pile count is carried through rather
      // than invented: this is the tagged member, not a kind name.
      // The store reads the engine's published capability now, not the
      // distribution -- the distribution is carried alongside it only because
      // other selectors still read it.
      useDraftPodStore.setState({
        packDistribution: { SharedStackPiles: { pile_count: 3 } },
        allowedSetLayouts: ["UniformByRound"],
        setDraftMode: "uniform",
      });

      useDraftPodStore.getState().setSetDraftMode("chaos");

      expect(useDraftPodStore.getState().setDraftMode).toBe("uniform");
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
      let resolveProcedure!: (value: DraftProcedure | PromiseLike<DraftProcedure>) => void;
      mocks.draftProcedure.mockImplementationOnce(() => new Promise<DraftProcedure>((resolve) => {
        resolveProcedure = resolve;
      }));
      useDraftPodStore.setState({
        allowedPodSizes: [2, 3, 4, 5, 6, 7, 8],
        procedureCacheKey: { kind: "Premier", tournamentFormat: "Swiss" },
        packDistribution: "PickAndPass",
        packsPerPlayer: 3,
        cubeMinDeckSize: 73,
      });

      const refreshing = useDraftPodStore.getState().refreshProcedure();
      useDraftPodStore.getState().setConfig({ tournamentFormat: "SingleElimination" });

      // The previous Swiss cache is invalid immediately, before the format's
      // replacement request starts, so the selector has no stale values to show.
      expect(useDraftPodStore.getState()).toMatchObject({
        allowedPodSizes: null,
        procedureCacheKey: null,
        packDistribution: null,
        packsPerPlayer: null,
        cubeMinDeckSize: null,
      });
      expect(mocks.draftProcedure).toHaveBeenCalledWith("Premier", "Swiss");

      resolveProcedure({
        ...draftProcedureFixture(),
        pod_size: 8,
        human_seats: 1,
        min_pod_size: 2,
        max_pod_size: 8,
        allowed_pod_sizes: [2, 3, 4, 5, 6, 7, 8],
        packs_per_player: 3,
        cards_per_pick: 1,
        distribution: "PickAndPass",
        min_deck_size: 40,
        cube_min_deck_size: 83,
        post_draft_play: "TournamentPairings",
      });
      await refreshing;

      expect(useDraftPodStore.getState()).toMatchObject({
        config: { tournamentFormat: "SingleElimination" },
        allowedPodSizes: null,
        procedureCacheKey: null,
        packDistribution: null,
        packsPerPlayer: null,
        cubeMinDeckSize: null,
      });
    });

    it("drops an original A success after A to B to A before replacement starts", async () => {
      let resolveOriginal!: (value: DraftProcedure | PromiseLike<DraftProcedure>) => void;
      mocks.draftProcedure.mockImplementationOnce(() => new Promise((resolve) => {
        resolveOriginal = resolve;
      }));

      const original = useDraftPodStore.getState().refreshProcedure();
      useDraftPodStore.getState().setConfig({ tournamentFormat: "SingleElimination" });
      useDraftPodStore.getState().setConfig({ tournamentFormat: "Swiss" });
      expect(mocks.draftProcedure).toHaveBeenCalledTimes(1);

      resolveOriginal({
        ...draftProcedureFixture(),
        pod_size: 8,
        human_seats: 1,
        min_pod_size: 2,
        max_pod_size: 8,
        allowed_pod_sizes: [2, 3, 4, 5, 6, 7, 8],
        packs_per_player: 3,
        cards_per_pick: 1,
        distribution: "PickAndPass",
        min_deck_size: 40,
        cube_min_deck_size: 97,
        post_draft_play: "TournamentPairings",
      });
      await original;

      expect(useDraftPodStore.getState()).toMatchObject({
        procedureCacheKey: null,
        cubeMinDeckSize: null,
        configError: null,
      });
    });

    it("drops an original A failure after A to B to A before replacement starts", async () => {
      let rejectOriginal!: (error: Error) => void;
      mocks.draftProcedure.mockImplementationOnce(() => new Promise((_resolve, reject) => {
        rejectOriginal = reject;
      }));

      const original = useDraftPodStore.getState().refreshProcedure();
      useDraftPodStore.getState().setConfig({ tournamentFormat: "SingleElimination" });
      useDraftPodStore.getState().setConfig({ tournamentFormat: "Swiss" });
      expect(mocks.draftProcedure).toHaveBeenCalledTimes(1);

      rejectOriginal(new Error("original A failed late"));
      await original;

      expect(useDraftPodStore.getState()).toMatchObject({
        procedureCacheKey: null,
        configError: null,
      });
    });

    it("publishes refreshed cache and dependent normalization atomically", async () => {
      mocks.draftProcedure.mockResolvedValue({
        ...draftProcedureFixture(),
        pod_size: 4,
        human_seats: 1,
        min_pod_size: 3,
        max_pod_size: 4,
        allowed_pod_sizes: [3, 4],
        packs_per_player: 6,
        cards_per_pick: 1,
        distribution: "AllAtOnce",
        min_deck_size: 40,
        cube_min_deck_size: 83,
        post_draft_play: "TournamentPairings",
      });
      useDraftPodStore.setState((prev) => ({
        config: { ...prev.config, podSize: 8 },
        poolMode: "cube",
        pendingProcedureDefault: { kind: "Premier", tournamentFormat: "Swiss" },
        loadingPool: true,
        configError: "stale error",
      }));
      const emissions: Array<ReturnType<typeof useDraftPodStore.getState>> = [];
      const unsubscribe = useDraftPodStore.subscribe((state) => emissions.push(state));

      await useDraftPodStore.getState().refreshProcedure();
      unsubscribe();

      const published = emissions.filter((state) => state.procedureCacheKey !== null);
      expect(published).not.toHaveLength(0);
      expect(published.every((state) =>
        state.poolMode === "set"
        && state.config.podSize === 4
        && state.pendingProcedureDefault === null
        && state.loadingPool === false
        && state.configError === null
      )).toBe(true);
    });
  });

  describe("setListing", () => {
    it("merges into the existing listing rather than replacing it", () => {
      useDraftPodStore.getState().setListing({ isPublic: true, roomName: "Friday" });
      useDraftPodStore.getState().setListing({ password: "secret" });

      expect(useDraftPodStore.getState().listing).toEqual({
        isPublic: true,
        roomName: "Friday",
        password: "secret",
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
      mocks.loadDraftHostSession.mockResolvedValue({
        ...persistedSession,
        draftStarted: true,
        draftSessionJson: '{"status":"Complete"}',
      });

      const outcome = await useDraftPodStore.getState().resumeHostedPod({ routeToken: 2 });

      expect(outcome).toBe("terminal");
      expect(mocks.multiplayerState.hostDraft).not.toHaveBeenCalled();
      expect(mocks.clearActiveDraftPodIfCurrent).toHaveBeenCalled();
    });

    it("does not host when the recovery procedure lookup fails", async () => {
      mocks.inspectActiveDraftPod.mockReturnValue({ type: "present", meta: activeMeta, capture: { id: activeMeta.id, roomCode: activeMeta.roomCode, updatedAt: activeMeta.updatedAt } });
      mocks.loadDraftHostSession.mockResolvedValue(persistedSession);
      mocks.draftProcedure.mockRejectedValue(new Error("wasm unavailable"));

      await expect(useDraftPodStore.getState().resumeHostedPod()).resolves.toBe("invalid");

      expect(mocks.multiplayerState.hostDraft).not.toHaveBeenCalled();
      expect(useDraftPodStore.getState().configError).toBe("wasm unavailable");
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
      let resolveProcedure!: (value: DraftProcedure | PromiseLike<DraftProcedure>) => void;
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
        ...draftProcedureFixture(),
        pod_size: 8,
        human_seats: 1,
        min_pod_size: 2,
        max_pod_size: 8,
        allowed_pod_sizes: [2, 3, 4, 5, 6, 7, 8],
        packs_per_player: 3,
        cards_per_pick: 1,
        distribution: "PickAndPass",
        min_deck_size: 40,
        cube_min_deck_size: 89,
        post_draft_play: "TournamentPairings",
      });

      await expect(resuming).resolves.toBe("superseded");
      expect(mocks.multiplayerState.hostDraft).not.toHaveBeenCalled();
    });

    it("publishes every procedure cache axis for a resumed pod", async () => {
      mocks.inspectActiveDraftPod.mockReturnValue({ type: "present", meta: activeMeta, capture: { id: activeMeta.id, roomCode: activeMeta.roomCode, updatedAt: activeMeta.updatedAt } });
      mocks.loadDraftHostSession.mockResolvedValue(persistedSession);
      mocks.draftProcedure.mockResolvedValue({
        ...draftProcedureFixture(),
        pod_size: 8,
        human_seats: 1,
        min_pod_size: 2,
        max_pod_size: 8,
        allowed_pod_sizes: [2, 3, 4, 5, 6, 7, 8],
        packs_per_player: 6,
        cards_per_pick: 1,
        distribution: "PickAndPass",
        min_deck_size: 40,
        cube_min_deck_size: 89,
        post_draft_play: "TournamentPairings",
      });

      await expect(useDraftPodStore.getState().resumeHostedPod()).resolves.toBe("resumed");

      expect(useDraftPodStore.getState()).toMatchObject({
        allowedPodSizes: [2, 3, 4, 5, 6, 7, 8],
        packDistribution: "PickAndPass",
        packsPerPlayer: 6,
        cubeMinDeckSize: 89,
      });
    });

    it("publishes resumed cache and dependent normalization atomically", async () => {
      mocks.inspectActiveDraftPod.mockReturnValue({ type: "present", meta: activeMeta, capture: { id: activeMeta.id, roomCode: activeMeta.roomCode, updatedAt: activeMeta.updatedAt } });
      mocks.loadDraftHostSession.mockResolvedValue({
        ...persistedSession,
        poolInput: {
          type: "Cube" as const,
          data: {
            cube_list_text: "1 Lightning Bolt\n",
            cube_name: "Recovered Cube",
            cube_draft_settings: {
              pod_size: 8,
              pack_count: 3,
              cards_per_pack: 15,
              min_deck_size: 40,
              addable_cards: { policy: "StandardBasics" as const, custom: [] },
            },
          },
        },
      });
      mocks.draftProcedure.mockResolvedValue({
        ...draftProcedureFixture(),
        pod_size: 4,
        human_seats: 1,
        min_pod_size: 3,
        max_pod_size: 4,
        allowed_pod_sizes: [3, 4],
        packs_per_player: 6,
        cards_per_pick: 1,
        distribution: "AllAtOnce",
        min_deck_size: 40,
        cube_min_deck_size: 89,
        post_draft_play: "TournamentPairings",
      });
      useDraftPodStore.setState({
        pendingProcedureDefault: {
          kind: "CommanderDraft",
          tournamentFormat: "SingleElimination",
        },
      });
      const emissions: Array<ReturnType<typeof useDraftPodStore.getState>> = [];
      const unsubscribe = useDraftPodStore.subscribe((state) => emissions.push(state));

      await expect(useDraftPodStore.getState().resumeHostedPod()).resolves.toBe("resumed");
      unsubscribe();

      const published = emissions.filter((state) => state.procedureCacheKey !== null);
      expect(published).not.toHaveLength(0);
      expect(published.every((state) =>
        state.poolMode === "set"
        && state.config.podSize === 3
        && state.pendingProcedureDefault === null
        && state.loadingPool === false
        && state.configError === null
      )).toBe(true);
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

    it("does not list a resumed pod", async () => {
      useDraftPodStore.getState().setListing({ isPublic: true });
      mocks.inspectActiveDraftPod.mockReturnValue({ type: "present", meta: activeMeta, capture: { id: activeMeta.id, roomCode: activeMeta.roomCode, updatedAt: activeMeta.updatedAt } });
      // Seated within LOBBY_LISTING_MAX_SEATS so only the resume path's own
      // no-relist behaviour, not the seat gate, can keep this pod unlisted.
      mocks.loadDraftHostSession.mockResolvedValue({ ...persistedSession, podSize: 6 });

      const outcome = await useDraftPodStore.getState().resumeHostedPod();

      expect(outcome).toBe("resumed");
      // Reach guard: the resume really did reach hosting.
      expect(mocks.multiplayerState.hostDraft).toHaveBeenCalledOnce();
      const dispatched = mocks.multiplayerState.hostDraft.mock.calls[0]?.[0] as { listing?: unknown };
      expect(dispatched.listing).toBeUndefined();
      expect(mocks.multiplayerConfig.resolveP2PBroker).not.toHaveBeenCalled();
      expect(mocks.openBrokerClient).not.toHaveBeenCalled();
    });
  });

  describe("createPod (cube branch)", () => {
    it("rejects an incompatible pool mode without publishing a matching cache key", async () => {
      mocks.draftProcedure.mockResolvedValue({
        ...draftProcedureFixture(),
        pod_size: 8,
        human_seats: 1,
        min_pod_size: 2,
        max_pod_size: 8,
        allowed_pod_sizes: [8],
        packs_per_player: 6,
        cards_per_pick: 1,
        distribution: "AllAtOnce",
        min_deck_size: 40,
        cube_min_deck_size: 73,
        post_draft_play: "TournamentPairings",
      });
      useDraftPodStore.setState({
        poolMode: "cube",
        cubeForm: {
          cubeName: "C",
          cubeListText: "1 Lightning Bolt\n",
          settings: {
            pod_size: 8,
            pack_count: 1,
            cards_per_pack: 2,
            min_deck_size: 4,
            addable_cards: { policy: "StandardBasics", custom: [] },
          },
        },
        hostDisplayName: "Host",
      });
      const emissions: Array<ReturnType<typeof useDraftPodStore.getState>> = [];
      const unsubscribe = useDraftPodStore.subscribe((state) => emissions.push(state));

      await useDraftPodStore.getState().createPod();
      unsubscribe();

      expect(useDraftPodStore.getState().configError).toBe("This procedure requires a set pool");
      expect(emissions.some((state) =>
        state.procedureCacheKey?.kind === state.config.kind
        && state.procedureCacheKey.tournamentFormat === state.config.tournamentFormat
      )).toBe(false);
      expect(mocks.multiplayerState.hostDraft).not.toHaveBeenCalled();
    });

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
      let resolveCreateProcedure!: (procedure: DraftProcedure | PromiseLike<DraftProcedure>) => void;
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
        ...draftProcedureFixture(),
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
        ...draftProcedureFixture(),
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
        ...draftProcedureFixture(),
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

    it("publishes a matching create cache only with normalized dependent state", async () => {
      stubPools(["ISD"]);
      mocks.draftProcedure.mockResolvedValue({
        ...draftProcedureFixture(),
        pod_size: 4,
        human_seats: 1,
        min_pod_size: 4,
        max_pod_size: 4,
        allowed_pod_sizes: [4],
        packs_per_player: 3,
        cards_per_pick: 1,
        distribution: "PickAndPass",
        min_deck_size: 40,
        cube_min_deck_size: 73,
        post_draft_play: "TournamentPairings",
      });
      useDraftPodStore.setState((prev) => ({
        config: {
          ...prev.config,
          packs: [{ code: "ISD", name: "Innistrad" }],
          setCode: "ISD",
          podSize: 2,
        },
        poolMode: "set",
        hostDisplayName: "Host",
      }));
      const emissions: Array<ReturnType<typeof useDraftPodStore.getState>> = [];
      const unsubscribe = useDraftPodStore.subscribe((state) => emissions.push(state));

      await useDraftPodStore.getState().createPod();
      unsubscribe();

      const matchingPublications = emissions.filter((state) =>
        state.procedureCacheKey?.kind === state.config.kind
        && state.procedureCacheKey.tournamentFormat === state.config.tournamentFormat
      );
      expect(matchingPublications).not.toHaveLength(0);
      expect(matchingPublications.every((state) =>
        state.config.podSize === 4 && state.poolMode === "set"
      )).toBe(true);
    });

    /**
     * A set the host named with no pool data must fail creation by name rather
     * than shipping a sequence the engine will refuse mid-draft. Checked on a
     * LATER entry, since resolving only the first code would still pass.
     */
    it("refuses a pack list naming a set with no pool data", async () => {
      stubPools(["ISD"]);
      mocks.draftProcedure.mockResolvedValue({
        ...draftProcedureFixture(),
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

    it("stops creation when loadProcedure fails", async () => {
      mocks.draftProcedure.mockReset().mockRejectedValue(new Error("wasm unavailable"));
      useDraftPodStore.setState((prev) => ({
        config: {
          ...prev.config,
          packs: [{ code: "EOE", name: "Edge of Eternities" }],
          setCode: "EOE",
        },
        hostDisplayName: "Host",
      }));

      await useDraftPodStore.getState().createPod();

      expect(mocks.multiplayerState.hostDraft).not.toHaveBeenCalled();
      expect(useDraftPodStore.getState().configError).toBe("wasm unavailable");
    });

    it("clears an older creation's loading state when a newer procedure read fails", async () => {
      let resolveFetch!: (response: {
        ok: boolean;
        status: number;
        json: () => Promise<Record<string, unknown>>;
      }) => void;
      vi.stubGlobal("__DRAFT_POOLS_URL__", "/draft-pools.json");
      vi.stubGlobal("fetch", vi.fn(() => new Promise((resolve) => {
        resolveFetch = resolve;
      })));
      mocks.draftProcedure
        .mockReset()
        .mockResolvedValueOnce({
          ...draftProcedureFixture(),
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
        })
        .mockRejectedValueOnce(new Error("new procedure failed"));
      useDraftPodStore.setState((prev) => ({
        config: {
          ...prev.config,
          packs: [{ code: "ISD", name: "Innistrad" }],
          setCode: "ISD",
        },
        hostDisplayName: "Host",
      }));

      const olderCreation = useDraftPodStore.getState().createPod();
      await vi.waitFor(() => expect(useDraftPodStore.getState().loadingPool).toBe(true));
      await useDraftPodStore.getState().createPod();

      expect(useDraftPodStore.getState()).toMatchObject({
        loadingPool: false,
        configError: "new procedure failed",
      });
      resolveFetch({
        ok: true,
        status: 200,
        json: async () => ({ isd: { code: "ISD" } }),
      });
      await olderCreation;
      expect(useDraftPodStore.getState().loadingPool).toBe(false);
      expect(mocks.multiplayerState.hostDraft).not.toHaveBeenCalled();
    });
  });

  describe("createPod lobby listing", () => {
    afterEach(async () => {
      vi.unstubAllGlobals();
      await i18n.changeLanguage("en");
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

    /** A published procedure admitting every seat count up to the lobby's
     * ceiling, so `config.podSize` set through `setState` survives publication. */
    function listableProcedure(overrides: Partial<DraftProcedure> = {}): DraftProcedure {
      return {
        ...draftProcedureFixture(),
        pod_size: 6,
        human_seats: 1,
        min_pod_size: 2,
        max_pod_size: 8,
        allowed_pod_sizes: [2, 3, 4, 5, 6, 7, 8],
        packs_per_player: 3,
        cards_per_pick: 1,
        distribution: "PickAndPass",
        min_deck_size: 40,
        post_draft_play: "TournamentPairings",
        ...overrides,
      };
    }

    /** Configure a set pod of `podSize` seats. */
    function configureSetPod(podSize = 6): void {
      useDraftPodStore.setState((prev) => ({
        config: {
          ...prev.config,
          packs: [{ code: "TST", name: "Test Set" }],
          setCode: "TST",
          podSize,
        },
        hostDisplayName: "Host",
      }));
    }

    function dispatchedHostConfig(): {
      podSize: number;
      listing?: { broker: unknown; request: Record<string, unknown> };
    } {
      const [config] = mocks.multiplayerState.hostDraft.mock.calls[0] as [{
        podSize: number;
        listing?: { broker: unknown; request: Record<string, unknown> };
      }];
      return config;
    }

    it("sends a public registration for a listed pod", async () => {
      stubPools(["TST", "XYZ"]);
      mocks.draftProcedure.mockResolvedValue(listableProcedure());
      useDraftPodStore.setState((prev) => ({
        config: {
          ...prev.config,
          packs: [
            { code: "TST", name: "Test Set" },
            { code: "TST", name: "Test Set" },
            { code: "XYZ", name: "XYZ Set" },
          ],
          setCode: "TST+XYZ",
          podSize: 6,
        },
        hostDisplayName: "Host",
      }));
      useDraftPodStore.getState().setListing({ isPublic: true });

      await useDraftPodStore.getState().createPod();

      expect(mocks.multiplayerConfig.resolveP2PBroker).toHaveBeenCalledWith("wss://phase.example/ws");
      expect(mocks.openBrokerClient).toHaveBeenCalledWith("wss://broker.example/ws");
      const openedBroker = await mocks.openBrokerClient.mock.results[0]!.value;
      const dispatched = dispatchedHostConfig();
      expect(dispatched.listing?.broker).toBe(openedBroker);
      expect(dispatched.listing?.request).toEqual({
        displayName: "Host",
        public: true,
        password: null,
        timerSeconds: null,
        playerCount: 6,
        matchConfig: { match_type: "Bo1" },
        formatConfig: null,
        roomName: "Host's table",
        draftMetadata: { setCode: "TST+XYZ", draftKind: "Premier" },
        ranked: false,
      });
    });

    it("advertises the pod's own seat count, not the lobby's ceiling", async () => {
      stubPools(["TST"]);
      mocks.draftProcedure.mockResolvedValue(listableProcedure());
      configureSetPod(4);
      useDraftPodStore.getState().setListing({ isPublic: true });

      await useDraftPodStore.getState().createPod();

      expect(dispatchedHostConfig().listing?.request.playerCount).toBe(4);
    });

    it("labels a Chaos pod by its candidate sets", async () => {
      stubPools(["AAA", "BBB"]);
      mocks.draftProcedure.mockResolvedValue(listableProcedure());
      useDraftPodStore.setState((prev) => ({
        config: {
          ...prev.config,
          packs: [
            { code: "AAA", name: "Set AAA" },
            { code: "BBB", name: "Set BBB" },
          ],
          setCode: "AAA+BBB",
          podSize: 6,
        },
        hostDisplayName: "Host",
        setDraftMode: "chaos",
      }));
      useDraftPodStore.getState().setListing({ isPublic: true });

      await useDraftPodStore.getState().createPod();

      expect(dispatchedHostConfig().listing?.request.draftMetadata).toEqual({
        setCode: "Chaos:AAA+BBB",
        draftKind: "Premier",
      });
    });

    it("labels a Chaos pod's candidates without deduplicating repeated sets", async () => {
      stubPools(["AAA", "BBB"]);
      mocks.draftProcedure.mockResolvedValue(listableProcedure());
      useDraftPodStore.setState((prev) => ({
        config: {
          ...prev.config,
          packs: [
            { code: "AAA", name: "Set AAA" },
            { code: "AAA", name: "Set AAA" },
            { code: "BBB", name: "Set BBB" },
          ],
          setCode: "AAA+BBB",
          podSize: 6,
        },
        hostDisplayName: "Host",
        setDraftMode: "chaos",
      }));
      useDraftPodStore.getState().setListing({ isPublic: true });

      await useDraftPodStore.getState().createPod();

      expect(dispatchedHostConfig().listing?.request.draftMetadata).toEqual({
        setCode: "Chaos:AAA+AAA+BBB",
        draftKind: "Premier",
      });
    });

    it("labels a listed pod by its selected draft kind, not a hardcoded one", async () => {
      stubPools(["TST"]);
      mocks.draftProcedure.mockResolvedValue(listableProcedure());
      configureSetPod(6);
      useDraftPodStore.setState((prev) => ({ config: { ...prev.config, kind: "Sealed" } }));
      useDraftPodStore.getState().setListing({ isPublic: true });

      await useDraftPodStore.getState().createPod();

      expect(dispatchedHostConfig().listing?.request.draftMetadata).toEqual({
        setCode: "TST",
        draftKind: "Sealed",
      });
    });

    it("dispatches the listing snapshotted at createPod's start, not a later edit", async () => {
      stubPools(["TST"]);
      let resolveCreateProcedure!: (procedure: DraftProcedure | PromiseLike<DraftProcedure>) => void;
      mocks.draftProcedure.mockImplementationOnce(() => new Promise((resolve) => {
        resolveCreateProcedure = resolve;
      }));
      configureSetPod(6);
      useDraftPodStore.getState().setListing({ isPublic: true, roomName: "Before" });

      const creating = useDraftPodStore.getState().createPod();
      useDraftPodStore.getState().setListing({ roomName: "After" });
      resolveCreateProcedure(listableProcedure());
      await creating;

      expect(dispatchedHostConfig().listing?.request.roomName).toBe("Before");
    });

    it("lists a cube pod by its name and Pod Size", async () => {
      useDraftPodStore.setState((prev) => ({
        config: { ...prev.config, podSize: 6 },
        poolMode: "cube",
        cubeForm: {
          cubeName: "  My Cube  ",
          cubeListText: "1 Lightning Bolt\n",
          settings: {
            pod_size: 8,
            pack_count: 1,
            cards_per_pack: 2,
            min_deck_size: 4,
            addable_cards: { policy: "StandardBasics", custom: [] },
          },
        },
        hostDisplayName: "Host",
      }));
      useDraftPodStore.getState().setListing({ isPublic: true });

      await useDraftPodStore.getState().createPod();

      const dispatched = dispatchedHostConfig();
      expect(dispatched.listing?.request.draftMetadata).toEqual({
        setCode: "custom-cube",
        draftKind: "Premier",
        cubeName: "My Cube",
      });
      expect(dispatched.listing?.request.playerCount).toBe(6);
    });

    describe("room name default", () => {
      it.each([
        ["", "Host's table"],
        ["   ", "Host's table"],
        ["  Friday  ", "Friday"],
      ])("names the room %j", async (roomName, expected) => {
        stubPools(["TST"]);
        mocks.draftProcedure.mockResolvedValue(listableProcedure());
        configureSetPod(6);
        useDraftPodStore.getState().setListing({ isPublic: true, roomName });

        await useDraftPodStore.getState().createPod();

        expect(dispatchedHostConfig().listing?.request.roomName).toBe(expected);
      });

      it("names the room in the host's language when it defaults", async () => {
        i18n.addResourceBundle("de", "multiplayer", deMultiplayer, true, true);
        await i18n.changeLanguage("de");
        stubPools(["TST"]);
        mocks.draftProcedure.mockResolvedValue(listableProcedure());
        configureSetPod(6);
        useDraftPodStore.getState().setListing({ isPublic: true });

        await useDraftPodStore.getState().createPod();

        const roomName = dispatchedHostConfig().listing?.request.roomName;
        expect(roomName).toBe(
          i18n.t("multiplayer:hostSetup.roomNameDefaultPlaceholder", { name: "Host", lng: "de" }),
        );
        expect(roomName).not.toBe("Host's table");
      });
    });

    it.each([
      ["pw", "pw"],
      ["", null],
      [" pw ", " pw "],
    ])("carries the listing password %j", async (password, expected) => {
      stubPools(["TST"]);
      mocks.draftProcedure.mockResolvedValue(listableProcedure());
      configureSetPod(6);
      useDraftPodStore.getState().setListing({ isPublic: true, password });

      await useDraftPodStore.getState().createPod();

      expect(dispatchedHostConfig().listing?.request.password).toBe(expected);
    });

    describe("label bounds", () => {
      function configureCubePod(overrides: {
        hostDisplayName?: string;
        roomName?: string;
        cubeName?: string;
        password?: string;
      } = {}): void {
        useDraftPodStore.setState((prev) => ({
          config: { ...prev.config, podSize: 2 },
          poolMode: "cube",
          cubeForm: {
            cubeName: overrides.cubeName ?? "Cube",
            cubeListText: "1 Lightning Bolt\n",
            settings: {
              pod_size: 2,
              pack_count: 1,
              cards_per_pack: 2,
              min_deck_size: 4,
              addable_cards: { policy: "StandardBasics", custom: [] },
            },
          },
          hostDisplayName: overrides.hostDisplayName ?? "Host",
        }));
        useDraftPodStore.getState().setListing({
          isPublic: true,
          roomName: overrides.roomName ?? "",
          password: overrides.password ?? "",
        });
      }

      it.each([
        [
          "a display name past the bound",
          { hostDisplayName: "A".repeat(21) },
          "To list this pod in the lobby, use a display name of at most 20 characters.",
        ],
        [
          "a room name past the bound",
          { roomName: "A".repeat(41) },
          "To list this pod in the lobby, use a room name of at most 40 characters.",
        ],
        [
          "a cube name past the bound",
          { cubeName: "A".repeat(41) },
          "To list this pod in the lobby, use a cube name of at most 40 characters.",
        ],
        [
          "a password past the byte bound",
          // 65 code points, 130 UTF-8 bytes (2 bytes each) — over the 128-byte bound.
          { password: "é".repeat(65) },
          "To list this pod in the lobby, use a shorter password.",
        ],
        [
          "a password one byte past the byte bound",
          // 64 "é" (128 bytes) + one ASCII byte = 129 bytes — exactly LOBBY_PASSWORD_MAX_BYTES + 1.
          { password: "é".repeat(64) + "a" },
          "To list this pod in the lobby, use a shorter password.",
        ],
      ] as const)("refuses %s before contacting the lobby", async (_label, overrides, message) => {
        configureCubePod(overrides);

        await useDraftPodStore.getState().createPod();

        expect(useDraftPodStore.getState().configError).toBe(message);
        expect(mocks.multiplayerConfig.resolveP2PBroker).not.toHaveBeenCalled();
        expect(mocks.openBrokerClient).not.toHaveBeenCalled();
        expect(mocks.multiplayerState.hostDraft).not.toHaveBeenCalled();
      });

      it.each([
        // 20 code points, 40 UTF-16 units — code points are what the bound counts.
        ["a display name of 20 astral characters", { hostDisplayName: "🂡".repeat(20) }],
        ["a room name at the bound", { roomName: "A".repeat(40) }],
        ["a cube name at the bound", { cubeName: "A".repeat(40) }],
        // 21 code points, 42 UTF-16 units — code points are what the bound counts.
        ["a cube name of 21 astral characters", { cubeName: "🂡".repeat(21) }],
        // 64 code points, 128 UTF-8 bytes — at, not over, the byte bound.
        ["a password at the byte bound", { password: "é".repeat(64) }],
      ] as const)("lists a pod with %s", async (_label, overrides) => {
        configureCubePod(overrides);

        await useDraftPodStore.getState().createPod();

        expect(dispatchedHostConfig().listing).toBeDefined();
      });
    });

    it.each([
      [
        "the resolver finds no lobby",
        () => {
          mocks.multiplayerConfig.resolveP2PBroker = vi.fn(async () => ({
            url: "wss://broker.example/ws",
            socket: null,
          }));
        },
      ],
      [
        "the resolver settles on a full server",
        () => {
          mocks.multiplayerConfig.resolveP2PBroker = vi.fn(async () => ({
            url: "wss://broker.example/ws",
            socket: { serverInfo: { mode: "Full" } },
          }));
        },
      ],
      [
        "the broker connection itself is refused",
        () => {
          mocks.openBrokerClient.mockReset().mockRejectedValueOnce(new Error("refused"));
        },
      ],
    ] as const)("tells the host when the lobby cannot be reached (%s)", async (_label, arrange) => {
      stubPools(["TST"]);
      mocks.draftProcedure.mockResolvedValue(listableProcedure());
      configureSetPod(6);
      useDraftPodStore.getState().setListing({ isPublic: true });
      arrange();

      await useDraftPodStore.getState().createPod();

      expect(useDraftPodStore.getState().configError).toBe(
        "Couldn't reach the lobby to list this pod. Turn off “List in lobby” to host by room code.",
      );
      expect(mocks.multiplayerState.hostDraft).not.toHaveBeenCalled();
    });

    it.each([
      [true, 8],
      [false, 6],
    ] as const)(
      "remembers the host's listing choice of %s at %i seats when pod creation starts",
      async (isPublic, podSize) => {
        stubPools(["TST"]);
        mocks.draftProcedure.mockResolvedValue(listableProcedure());
        configureSetPod(podSize);
        useDraftPodStore.getState().setListing({ isPublic });

        await useDraftPodStore.getState().createPod();

        expect(mocks.multiplayerConfig.rememberPodListingPublic).toHaveBeenCalledOnce();
        expect(mocks.multiplayerConfig.rememberPodListingPublic).toHaveBeenCalledWith(isPublic);
      },
    );

    it("does not remember a listing choice when creation is refused for a missing display name", async () => {
      stubPools(["TST"]);
      mocks.draftProcedure.mockResolvedValue(listableProcedure());
      configureSetPod(6);
      useDraftPodStore.setState({ hostDisplayName: "" });
      useDraftPodStore.getState().setListing({ isPublic: true });

      await useDraftPodStore.getState().createPod();

      expect(mocks.multiplayerConfig.rememberPodListingPublic).not.toHaveBeenCalled();
      expect(useDraftPodStore.getState().configError).toBe("Enter a display name");
    });

    it.each(SUPPORTED_LNGS)(
      "names the listing control in the unreachable-lobby message in %s",
      async (lng) => {
        const label = (resources[lng].draft as { podSetup: { listInLobby: string } })
          .podSetup.listInLobby;
        i18n.addResourceBundle(lng, "draft", resources[lng].draft, true, true);
        i18n.addResource(lng, "draft", "podSetup.listInLobby", "Renamed control");
        await i18n.changeLanguage(lng);
        try {
          stubPools(["TST"]);
          mocks.draftProcedure.mockResolvedValue(listableProcedure());
          configureSetPod(6);
          useDraftPodStore.getState().setListing({ isPublic: true });
          mocks.openBrokerClient.mockReset().mockRejectedValueOnce(new Error("refused"));

          await useDraftPodStore.getState().createPod();

          expect(useDraftPodStore.getState().configError).toContain("Renamed control");
        } finally {
          i18n.addResource(lng, "draft", "podSetup.listInLobby", label);
        }
      },
    );

    it.each([7, 8])("creates a pod above the lobby's seat ceiling without listing it (%i seats)", async (podSize) => {
      stubPools(["TST"]);
      mocks.draftProcedure.mockResolvedValue(listableProcedure({ pod_size: podSize }));
      configureSetPod(podSize);
      useDraftPodStore.getState().setListing({ isPublic: true });

      await useDraftPodStore.getState().createPod();

      expect(mocks.multiplayerConfig.resolveP2PBroker).not.toHaveBeenCalled();
      expect(mocks.openBrokerClient).not.toHaveBeenCalled();
      expect(dispatchedHostConfig().listing).toBeUndefined();
      expect(useDraftPodStore.getState().listing.isPublic).toBe(true);
    });

    it("opens no lobby connection for a pod created with the initial listing state", async () => {
      stubPools(["TST"]);
      mocks.draftProcedure.mockResolvedValue(listableProcedure());
      configureSetPod(6);

      expect(useDraftPodStore.getState().listing.isPublic).toBe(false);

      await useDraftPodStore.getState().createPod();

      expect(mocks.multiplayerConfig.resolveP2PBroker).not.toHaveBeenCalled();
      expect(mocks.openBrokerClient).not.toHaveBeenCalled();
      expect(dispatchedHostConfig().listing).toBeUndefined();
    });

    it.each([
      [
        "hosting fails",
        () => {
          mocks.multiplayerState.hostDraft = vi.fn<(config: unknown) => Promise<boolean>>(async () => false);
        },
        "Unable to host draft pod",
      ],
      [
        "hosting throws",
        () => {
          mocks.multiplayerState.hostDraft = vi.fn<(config: unknown) => Promise<boolean>>(async () => {
            throw new Error("boom");
          });
        },
        "boom",
      ],
    ] as const)("closes the lobby connection when hosting does not start (%s)", async (_label, arrange, message) => {
      stubPools(["TST"]);
      mocks.draftProcedure.mockResolvedValue(listableProcedure());
      configureSetPod(6);
      useDraftPodStore.getState().setListing({ isPublic: true });
      arrange();

      await useDraftPodStore.getState().createPod();

      expect(mocks.brokerClose).toHaveBeenCalledOnce();
      expect(useDraftPodStore.getState().configError).toBe(message);
    });

    it("does not close the lobby connection once hosting starts", async () => {
      stubPools(["TST"]);
      mocks.draftProcedure.mockResolvedValue(listableProcedure());
      configureSetPod(6);
      useDraftPodStore.getState().setListing({ isPublic: true });

      await useDraftPodStore.getState().createPod();

      expect(mocks.brokerClose).not.toHaveBeenCalled();
    });

    it("closes the lobby connection if the app goes offline while it opens", async () => {
      stubPools(["TST"]);
      mocks.draftProcedure.mockResolvedValue(listableProcedure());
      configureSetPod(6);
      useDraftPodStore.getState().setListing({ isPublic: true });
      let resolveOpen!: (client: { close: () => void }) => void;
      mocks.openBrokerClient.mockReset().mockImplementationOnce(() => new Promise((resolve) => {
        resolveOpen = resolve;
      }));

      const creating = useDraftPodStore.getState().createPod();
      await vi.waitFor(() => expect(resolveOpen).toBeTypeOf("function"));
      useConnectivityStore.setState({ forcedOffline: true });
      resolveOpen({ close: mocks.brokerClose });

      await creating;

      expect(mocks.brokerClose).toHaveBeenCalledOnce();
      expect(mocks.multiplayerState.hostDraft).not.toHaveBeenCalled();
      expect(useDraftPodStore.getState().configError).toBe("offline.startUnavailable");
    });

    it("closes the lobby connection when pod setup is replaced while it opens", async () => {
      stubPools(["TST"]);
      mocks.draftProcedure.mockResolvedValue(listableProcedure());
      configureSetPod(6);
      useDraftPodStore.getState().setListing({ isPublic: true });
      let resolveOpen!: (client: { close: () => void }) => void;
      mocks.openBrokerClient.mockReset().mockImplementationOnce(() => new Promise((resolve) => {
        resolveOpen = resolve;
      }));

      const creating = useDraftPodStore.getState().createPod();
      await vi.waitFor(() => expect(resolveOpen).toBeTypeOf("function"));
      useDraftPodStore.getState().reset();
      resolveOpen({ close: mocks.brokerClose });

      await creating;

      expect(mocks.brokerClose).toHaveBeenCalledOnce();
      expect(mocks.multiplayerState.hostDraft).not.toHaveBeenCalled();
    });

    it("does not report a lobby failure for pod setup replaced while the lobby connection opens", async () => {
      stubPools(["TST"]);
      mocks.draftProcedure.mockResolvedValue(listableProcedure());
      configureSetPod(6);
      useDraftPodStore.getState().setListing({ isPublic: true });
      let rejectOpen!: (err: Error) => void;
      mocks.openBrokerClient.mockReset().mockImplementationOnce(() => new Promise((_resolve, reject) => {
        rejectOpen = reject;
      }));

      const creating = useDraftPodStore.getState().createPod();
      await vi.waitFor(() => expect(rejectOpen).toBeTypeOf("function"));
      useDraftPodStore.getState().reset();
      rejectOpen(new Error("lobby unreachable"));

      await creating;

      expect(useDraftPodStore.getState().configError).toBeNull();
      expect(mocks.multiplayerState.hostDraft).not.toHaveBeenCalled();
    });

    it.each([
      ["no lobby is available", null],
      ["a lobby is available", { serverInfo: { mode: "LobbyOnly" } }],
    ] as const)("does not report a lobby failure for pod setup replaced while the lobby is chosen (%s)", async (_label, socket) => {
      stubPools(["TST"]);
      mocks.draftProcedure.mockResolvedValue(listableProcedure());
      configureSetPod(6);
      useDraftPodStore.getState().setListing({ isPublic: true });
      let resolveBroker!: (result: { url: string; socket: { serverInfo: { mode: string } } | null }) => void;
      mocks.multiplayerConfig.resolveP2PBroker = vi.fn(() => new Promise((resolve) => {
        resolveBroker = resolve;
      }));

      const creating = useDraftPodStore.getState().createPod();
      await vi.waitFor(() => expect(mocks.multiplayerConfig.resolveP2PBroker).toHaveBeenCalledOnce());
      useDraftPodStore.getState().reset();
      resolveBroker({ url: "wss://broker.example/ws", socket });

      await creating;

      expect(useDraftPodStore.getState().configError).toBeNull();
      expect(mocks.openBrokerClient).not.toHaveBeenCalled();
    });

    it("does not open the lobby connection for a config edited while the broker is chosen", async () => {
      stubPools(["TST"]);
      mocks.draftProcedure.mockResolvedValue(listableProcedure());
      configureSetPod(6);
      useDraftPodStore.getState().setListing({ isPublic: true });
      let resolveBroker!: (result: { url: string; socket: { serverInfo: { mode: string } } | null }) => void;
      mocks.multiplayerConfig.resolveP2PBroker = vi.fn(() => new Promise((resolve) => {
        resolveBroker = resolve;
      }));

      const creating = useDraftPodStore.getState().createPod();
      await vi.waitFor(() => expect(mocks.multiplayerConfig.resolveP2PBroker).toHaveBeenCalledOnce());
      useDraftPodStore.getState().setConfig({ podSize: 4 });
      resolveBroker({ url: "wss://broker.example/ws", socket: { serverInfo: { mode: "LobbyOnly" } } });

      await creating;

      expect(mocks.openBrokerClient).not.toHaveBeenCalled();
      expect(mocks.multiplayerState.hostDraft).not.toHaveBeenCalled();
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

    function procedure(): DraftProcedure {
      return draftProcedureFixture();
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

    it("keeps a held online creation from hosting after a later offline creation", async () => {
      let resolveResponse!: (response: { ok: boolean; status: number; json: () => Promise<Record<string, unknown>> }) => void;
      vi.stubGlobal("__DRAFT_POOLS_URL__", "/draft-pools.json");
      vi.stubGlobal("fetch", vi.fn(() => new Promise((resolve) => { resolveResponse = resolve; })));
      configureSetPod();

      const olderCreation = useDraftPodStore.getState().createPod();
      await vi.waitFor(() => expect(useDraftPodStore.getState().loadingPool).toBe(true));

      useConnectivityStore.setState({ forcedOffline: true });
      await useDraftPodStore.getState().createPod();
      expect(useDraftPodStore.getState()).toMatchObject({
        loadingPool: false,
        configError: "offline.startUnavailable",
      });

      useConnectivityStore.setState({ forcedOffline: false });
      resolveResponse({ ok: true, status: 200, json: async () => ({ tst: { code: "TST" } }) });
      await olderCreation;

      expect(mocks.multiplayerState.hostDraft).not.toHaveBeenCalled();
    });

    it("starts a fresh host recovery after an offline resume retires a held attempt", async () => {
      let resolveFirstLoad!: (session: typeof persistedSession) => void;
      mocks.inspectActiveDraftPod.mockReturnValue({
        type: "present",
        meta: activeMeta,
        capture: { id: activeMeta.id, roomCode: activeMeta.roomCode, updatedAt: activeMeta.updatedAt },
      });
      mocks.loadDraftHostSession
        .mockImplementationOnce(() => new Promise((resolve) => { resolveFirstLoad = resolve; }))
        .mockResolvedValueOnce(persistedSession);

      const firstResume = useDraftPodStore.getState().resumeHostedPod();
      await vi.waitFor(() => expect(mocks.loadDraftHostSession).toHaveBeenCalledOnce());

      useConnectivityStore.setState({ forcedOffline: true });
      await expect(useDraftPodStore.getState().resumeHostedPod()).resolves.toBe("offline");

      useConnectivityStore.setState({ forcedOffline: false });
      const recovered = useDraftPodStore.getState().resumeHostedPod();
      await vi.waitFor(() => expect(mocks.loadDraftHostSession).toHaveBeenCalledTimes(2));
      resolveFirstLoad(persistedSession);

      await expect(firstResume).resolves.toBe("superseded");
      await expect(recovered).resolves.toBe("resumed");
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

  // ── Saved-identity seeding ───────────────────────────────────────────────
  //
  // Every assertion here is REVERT-FAILING as a group: BASE has no
  // `adoptSavedDisplayName`, so the call is a TypeError rather than a wrong
  // value. The per-test notes below say what each one discriminates BEYOND
  // that — i.e. what a present-but-wrong implementation would fail on.
  describe("adoptSavedDisplayName", () => {
    it("seeds both pod name fields from the saved identity", () => {
      mocks.multiplayerConfig.displayName = "Alice";

      useDraftPodStore.getState().adoptSavedDisplayName();

      expect(useDraftPodStore.getState()).toMatchObject({
        hostDisplayName: "Alice",
        guestDisplayName: "Alice",
      });
    });

    it.each([
      ["host", "hostDisplayName", () => useDraftPodStore.getState().setHostDisplayName("Bea")],
      ["guest", "guestDisplayName", () => useDraftPodStore.getState().setGuestDisplayName("Bea")],
    ])("leaves a name the %s already typed alone", (_seat, field, type) => {
      mocks.multiplayerConfig.displayName = "Alice";
      type();

      useDraftPodStore.getState().adoptSavedDisplayName();

      // Discriminates an unconditional `set`: that would overwrite "Bea" here.
      expect(useDraftPodStore.getState()[field as "hostDisplayName" | "guestDisplayName"]).toBe("Bea");
    });

    it("leaves an already-populated host field alone", () => {
      // `setState` is the shortcut, so this measures the guard on a non-empty
      // `hostDisplayName` — NOT a production ordering. The ordering the page
      // really produces is the test below.
      useDraftPodStore.setState({ hostDisplayName: "Restored Host" });
      mocks.multiplayerConfig.displayName = "Alice";

      useDraftPodStore.getState().adoptSavedDisplayName();

      expect(useDraftPodStore.getState().hostDisplayName).toBe("Restored Host");
      // The guest field was empty, so the same call still seeds it — this is
      // one action over two independent fields, not an all-or-nothing gate.
      expect(useDraftPodStore.getState().guestDisplayName).toBe("Alice");
    });

    it.each([
      ["never set", ""],
      ["whitespace only", "   "],
    ])("leaves both fields empty when the saved identity is %s", (_label, saved) => {
      mocks.multiplayerConfig.displayName = saved;

      useDraftPodStore.getState().adoptSavedDisplayName();

      // Discriminates a seed that skips the emptiness check: "   " would land
      // in both fields and read as filled while `createPod`/`joinPod` still
      // reject it, since both trim before validating.
      expect(useDraftPodStore.getState()).toMatchObject({
        hostDisplayName: "",
        guestDisplayName: "",
      });
    });

    it("skips a persisted identity that is not a string", () => {
      // `multiplayerStore`'s persist `merge` normalizes `lastHostConfig`,
      // `userLobbySources`, `disabledDirectorySources`, `hostingServer` and
      // `connectionMode` under the comment "Persisted state is external
      // input", but spreads `displayName` through unvalidated — so a corrupt
      // or hand-edited localStorage blob really can hydrate a non-string here.
      // The cast is how this suite reaches that state; the store's own type
      // says it cannot happen.
      (mocks.multiplayerConfig as { displayName: unknown }).displayName = 42;

      // Seeding is cosmetic, so it must not be able to take the page down.
      // `PodSetup` calls this from a mount effect, and `App.tsx` wraps every
      // route in `ErrorBoundary` — so an unguarded throw here replaces the
      // Draft Pod screen with that boundary's fallback. Measured with the guard
      // removed and `displayName` set to 42: rendering the page bare throws a
      // TypeError out of the `.trim()`.
      expect(() => useDraftPodStore.getState().adoptSavedDisplayName()).not.toThrow();
      expect(useDraftPodStore.getState()).toMatchObject({
        hostDisplayName: "",
        guestDisplayName: "",
      });
    });

    it("yields to a host session restored after it", async () => {
      // The ordering the page actually produces: `PodSetup`'s mount effect
      // seeds first, and `resumeHostedPod` — whose `set` sits behind an
      // `await` — lands after it. The restored name wins because that path
      // assigns `hostDisplayName` unconditionally, NOT because of the
      // empty-field guard, which by then has nothing left to protect.
      mocks.multiplayerConfig.displayName = "Alice";
      mocks.inspectActiveDraftPod.mockReturnValue({
        type: "present",
        meta: activeMeta,
        capture: { id: activeMeta.id, roomCode: activeMeta.roomCode, updatedAt: activeMeta.updatedAt },
      });
      mocks.loadDraftHostSession.mockResolvedValue(persistedSession);

      useDraftPodStore.getState().adoptSavedDisplayName();
      // Reach guard: the seed really did land, so the assertion after the
      // resume is measuring an overwrite rather than a seed that never ran.
      expect(useDraftPodStore.getState().hostDisplayName).toBe("Alice");

      await expect(useDraftPodStore.getState().resumeHostedPod()).resolves.toBe("resumed");

      expect(useDraftPodStore.getState().hostDisplayName).toBe(persistedSession.hostDisplayName);
    });

    it("reads the identity at call time, not at store creation", () => {
      // The name is editable — `PlayerIdentityBanner`, and Preferences →
      // Multiplayer — long after this module is evaluated. Discriminates a
      // seed captured into `initialState`.
      mocks.multiplayerConfig.displayName = "Renamed After Load";

      useDraftPodStore.getState().adoptSavedDisplayName();

      expect(useDraftPodStore.getState().hostDisplayName).toBe("Renamed After Load");
    });
  });
});
