import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { readFileSync } from "node:fs";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { useEffect, type ReactNode } from "react";
import { MemoryRouter, useNavigate, type NavigateFunction } from "react-router";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { draftProcedureFixture } from "../../adapter/__tests__/draftProcedureFixture";
import type { RestorableDraftSessionVerdict } from "../../adapter/draft-adapter";
import type { ActiveDraftPodLoadResult } from "../../services/draftPersistence";
import type { GuestDraftResumeOutcome } from "../../stores/multiplayerDraftStore";

const mocks = vi.hoisted(() => ({
  draftProcedure: vi.fn(),
  checkRestorableSession: vi.fn<(json: string) => Promise<RestorableDraftSessionVerdict>>(),
  loadActiveDraftPod: vi.fn(() => null),
  // The host-recovery probe `draftPodStore.resumeHostedPod` actually calls.
  // `absent` terminates it cleanly so this suite measures the kind-intent
  // effect rather than a recovery it never seeded.
  inspectActiveDraftPod: vi.fn<() => ActiveDraftPodLoadResult>(() => ({ type: "absent" })),
  clearActiveDraftPodIfCurrent: vi.fn(),
  persistedDraftHostSessionState: vi.fn(() => "live"),
  loadDraftHostSession: vi.fn(),
  clearActiveDraftPod: vi.fn(),
  saveActiveDraftPod: vi.fn(),
  // The lifecycle store itself stays real. These are only the external peer
  // and host collaborators the real wrapper reaches during recovery.
  recoveryHostRoom: vi.fn(),
  recoveryInitialize: vi.fn<(host: unknown) => Promise<void>>(async () => {}),
  guestResume: vi.fn<() => Promise<GuestDraftResumeOutcome>>(async () => "absent"),
  recoveryHosts: [] as Array<{
    dispose: ReturnType<typeof vi.fn>;
    onEvent: ReturnType<typeof vi.fn>;
  }>,
}));

vi.mock("../../network/connection", async (importOriginal) => ({
  ...(await importOriginal<typeof import("../../network/connection")>()),
  hostRoom: mocks.recoveryHostRoom,
}));

vi.mock("../../adapter/p2p-draft-host", () => ({
  P2PDraftHost: class {
    readonly dispose = vi.fn(async () => {});
    readonly onEvent = vi.fn(() => vi.fn());

    constructor() { mocks.recoveryHosts.push(this); }

    async initialize() { await mocks.recoveryInitialize(this); }
    async restoreFromPersisted() { return null; }
    getHostWorkspaceState() { return null; }
    async terminateDraft() { await this.dispose(); }
  },
}));

// The engine's per-kind procedure. `pod_size` is deliberately NOT 4 so a client
// literal cannot coincide with the adopted value.
// Only the adapter CLASS is replaced. The module's shape helpers
// (`setPackSequence`, `distinctJoined`, `isSharedStackDistribution`) are the
// boundary's own logic and the store calls them while this page renders —
// stubbing them away would make this suite answer questions about the mock.
vi.mock("../../adapter/draft-adapter", async (importOriginal) => ({
  ...(await importOriginal<typeof import("../../adapter/draft-adapter")>()),
  DraftAdapter: class {
    draftProcedure = mocks.draftProcedure;
    // The draft engine's check of a recovered pod's saved session.
    checkRestorableSession = mocks.checkRestorableSession;
  },
}));

vi.mock("../../services/draftPersistence", () => ({
  loadActiveDraftPod: mocks.loadActiveDraftPod,
  inspectActiveDraftPod: mocks.inspectActiveDraftPod,
  clearActiveDraftPodIfCurrent: mocks.clearActiveDraftPodIfCurrent,
  persistedDraftHostSessionState: mocks.persistedDraftHostSessionState,
  loadDraftHostSession: mocks.loadDraftHostSession,
  clearActiveDraftPod: mocks.clearActiveDraftPod,
  saveActiveDraftPod: mocks.saveActiveDraftPod,
}));

vi.mock("../../components/chrome/ScreenChrome", () => ({ ScreenChrome: () => null }));
vi.mock("../../components/menu/MenuShell", () => ({
  MenuShell: ({ children }: { children: ReactNode }) => <>{children}</>,
}));
vi.mock("../../components/draft/HostControls", () => {
  const emptyTopActions: readonly [] = [];
  return {
    HostControls: () => null,
    useHostDraftTopActions: (_options: { enabled: boolean }) => emptyTopActions,
  };
});
vi.mock("../../components/draft/SetSelector", () => ({ SetSelector: () => null }));

import { DraftPodPage } from "../DraftPodPage";
import { isHostedPodRecoveryInFlight, useDraftPodStore } from "../../stores/draftPodStore";
import { useMultiplayerDraftStore } from "../../stores/multiplayerDraftStore";
import { useConnectivityStore } from "../../stores/connectivityStore";

/** An engine-published `DraftPlayerView` slice containing the procedure the intro reads.
 *  `seats` is the pod's real size — `draftPodStore.config.podSize` is this client's
 *  own intent and is deliberately left on its default in every fixture below. */
function engineView(
  kind: string,
  seatCount: number,
  procedure: Partial<{
    pack_count: number;
    cards_per_pack: number;
    pack_sizes: number[];
    min_deck_size: number;
    launch_capability: "None" | "CommanderMultiplayer";
    commanders_required: number;
    distribution: unknown;
  }> = {},
) {
  return {
    kind,
    launch_capability: "None",
    distribution: "PickAndPass",
    commanders_required: 0,
    seats: Array.from({ length: seatCount }, (_, seat_index) => ({ seat_index })),
    pack_count: 4,
    cards_per_pack: 12,
    pack_sizes: [12, 12, 12, 12],
    min_deck_size: 45,
    ...procedure,
  };
}

function renderAt(path: string) {
  return render(
    <MemoryRouter initialEntries={[path]}>
      <DraftPodPage />
    </MemoryRouter>,
  );
}

/**
 * A saved draft session from the fixture draft-wasm's own test checks:
 * draft-core's serialization of a session, and the draft engine's answer to the
 * saved-session check.
 */
function savedSession(name: string): { text: string; verdict: RestorableDraftSessionVerdict } {
  const fixture = JSON.parse(readFileSync(resolve(
    dirname(fileURLToPath(import.meta.url)),
    "../../../../fixtures/draft-contract/fifo_legacy_admission.json",
  ), "utf8")) as { saved_sessions: Record<string, { verdict: RestorableDraftSessionVerdict; session: unknown }> };
  const saved = fixture.saved_sessions[name];
  if (!saved) throw new Error(`The fixture has no saved session named ${name}`);
  return { text: JSON.stringify(saved.session), verdict: saved.verdict };
}

function setupProcedure(kind: string) {
  const commander = kind === "CommanderDraft";
  return draftProcedureFixture({
    min_pod_size: commander ? 3 : 2,
    allowed_pod_sizes: commander ? [3, 4, 5, 6, 7, 8] : [2, 3, 4, 5, 6, 7, 8],
    cards_per_pick: commander ? 2 : 1,
    post_draft_play: commander ? "CompleteImmediately" : "TournamentPairings",
  });
}

describe("DraftPodPage ?kind= mode entry", () => {
  afterEach(cleanup);

  beforeEach(async () => {
    vi.clearAllMocks();
    await useMultiplayerDraftStore.getState().leave(true);
    useMultiplayerDraftStore.getState().reset();
    mocks.guestResume.mockResolvedValue("absent");
    useMultiplayerDraftStore.setState({
      phase: "idle", role: null, roomCode: null, view: null,
      resumeDraft: mocks.guestResume,
    } as never);
    mocks.recoveryHostRoom.mockReset();
    mocks.recoveryInitialize.mockResolvedValue(undefined);
    mocks.recoveryHosts.length = 0;
    useConnectivityStore.setState({ forcedOffline: false, browserOnline: true });
    mocks.loadActiveDraftPod.mockReturnValue(null);
    mocks.inspectActiveDraftPod.mockReturnValue({ type: "absent" });
    mocks.persistedDraftHostSessionState.mockReturnValue("live");
    mocks.loadDraftHostSession.mockResolvedValue({ type: "absent" });
    mocks.checkRestorableSession.mockReset().mockResolvedValue({ type: "Admitted" });
    mocks.draftProcedure.mockResolvedValue(draftProcedureFixture({
      pod_size: 6,
      human_seats: 1,
      min_pod_size: 3,
      max_pod_size: 8,
      allowed_pod_sizes: [3, 4, 5, 6, 7, 8],
      packs_per_player: 3,
      cards_per_pick: 2,
      distribution: "PickAndPass",
      min_deck_size: 60,
      cube_min_deck_size: 53,
      post_draft_play: "CompleteImmediately",
      match_config: { match_type: "Bo1" },
    }));
    useDraftPodStore.getState().reset();
  });

  it("applies the deep-linked kind and the engine's pod size", async () => {
    renderAt("/draft-pod?kind=commander");

    // REVERT-FAILING: BASE has no `?kind=` effect, so `kind` stays "Premier".
    // The `podSize: 6` half additionally fails any hardcoded client default.
    await waitFor(() =>
      expect(useDraftPodStore.getState().config).toMatchObject({
        kind: "CommanderDraft",
        podSize: 6,
      }),
    );
    expect(mocks.draftProcedure).toHaveBeenCalledWith("CommanderDraft", "Swiss");
  });

  it("renders an actual current host incompatibility with a host-only retry", async () => {
    const capture = { id: "draft-1", roomCode: "ABCDE", updatedAt: 1 };
    mocks.inspectActiveDraftPod.mockReturnValue({
      type: "present",
      meta: {
        ...capture,
        kind: "Premier",
        podSize: 8,
        hostDisplayName: "Host",
        tournamentFormat: "Swiss",
        podPolicy: "Competitive",
        phase: "lobby",
        pickCount: 0,
      },
      capture,
    });
    const saved = savedSession("fifo_drafting");
    const refused = {
      persistenceId: capture.id, roomCode: capture.roomCode, kind: "Premier" as const, podSize: 8,
      hostDisplayName: "Host", tournamentFormat: "Swiss" as const, podPolicy: "Competitive" as const,
      seatTokens: { 0: "host" }, seatNames: { 0: "Host" }, kickedTokens: [],
      draftStarted: true, draftCode: capture.roomCode, draftSessionJson: saved.text,
      poolInput: { type: "Set" as const, data: { pools: [{ code: "TST" }], sequence: ["TST"] } },
    };
    mocks.loadDraftHostSession.mockResolvedValue({ type: "admitted", session: refused, raw: refused });
    mocks.checkRestorableSession.mockResolvedValue(saved.verdict);

    const user = userEvent.setup();
    renderAt("/draft-pod?entry=host");

    await waitFor(() => expect(screen.getByText("This draft is incompatible with your protocol version.")).toBeInTheDocument());
    await user.click(screen.getByRole("button", { name: "Try Reconnecting" }));

    // The explicit host route never calls the guest capability resume path,
    // including from an incompatible recovery CTA.
    expect(mocks.guestResume).not.toHaveBeenCalled();
  });

  it("keeps a stale full-capture host retry inert and lets an intentional guest route retire its error", async () => {
    const captureA = { id: "draft-1", roomCode: "ABCDE", updatedAt: 1 };
    const captureB = { id: "draft-2", roomCode: "ABCDE", updatedAt: 2 };
    const metaFor = (capture: typeof captureA) => ({
      ...capture,
      kind: "Premier" as const,
      podSize: 8,
      hostDisplayName: "Host",
      tournamentFormat: "Swiss" as const,
      podPolicy: "Competitive" as const,
      phase: "lobby" as const,
      pickCount: 0,
    });
    mocks.inspectActiveDraftPod.mockReturnValue({
      type: "present",
      meta: metaFor(captureA),
      capture: captureA,
    });
    const saved = savedSession("fifo_drafting");
    const refused = {
      persistenceId: captureA.id, roomCode: captureA.roomCode, kind: "Premier" as const, podSize: 8,
      hostDisplayName: "Host", tournamentFormat: "Swiss" as const, podPolicy: "Competitive" as const,
      seatTokens: { 0: "host" }, seatNames: { 0: "Host" }, kickedTokens: [],
      draftStarted: true, draftCode: captureA.roomCode, draftSessionJson: saved.text,
      poolInput: { type: "Set" as const, data: { pools: [{ code: "TST" }], sequence: ["TST"] } },
    };
    mocks.loadDraftHostSession.mockResolvedValue({ type: "admitted", session: refused, raw: refused });
    mocks.checkRestorableSession.mockResolvedValue(saved.verdict);

    const user = userEvent.setup();
    const rendered = renderAt("/draft-pod?entry=host");
    await waitFor(() => expect(screen.getByText("This draft is incompatible with your protocol version.")).toBeInTheDocument());

    // The same room code alone is insufficient: once B replaces A, A's
    // rendered CTA cannot re-read B or invoke another recovery attempt.
    mocks.inspectActiveDraftPod.mockReturnValue({
      type: "present",
      meta: metaFor(captureB),
      capture: captureB,
    });
    await user.click(screen.getByRole("button", { name: "Try Reconnecting" }));
    expect(mocks.loadDraftHostSession).toHaveBeenCalledOnce();
    expect(mocks.guestResume).not.toHaveBeenCalled();
    // The remounted setup screen requests its procedure. Hold it until the
    // retired failure's setup error has been read.
    const healthy = mocks.draftProcedure.getMockImplementation()!;
    let releaseProcedure!: () => void;
    mocks.draftProcedure.mockReturnValueOnce(new Promise((resolve) => {
      releaseProcedure = () => resolve(healthy());
    }));

    rendered.rerender(
      <MemoryRouter key="intentional-guest" initialEntries={["/draft-pod?entry=guest"]}>
        <DraftPodPage />
      </MemoryRouter>,
    );
    await waitFor(() => expect(mocks.guestResume).toHaveBeenCalledOnce());
    expect(screen.queryByText("This draft is incompatible with your protocol version.")).toBeNull();
    expect(useDraftPodStore.getState().hostRecoveryFailure).toBeNull();
    // Until the guest route's setup procedure arrives, the retired failure
    // leaves its key as the setup error, and the join form shows it translated.
    expect(useDraftPodStore.getState().configError).toBe("podError.incompatible");
    await user.click(screen.getByRole("button", { name: /Join a Pod/ }));
    expect(screen.getByPlaceholderText("Enter room code...")).toBeInTheDocument();
    expect(screen.queryByText("podError.incompatible")).toBeNull();
    expect(screen.getByText("This draft is incompatible with your protocol version.")).toBeInTheDocument();
    expect(mocks.draftProcedure).toHaveBeenCalledTimes(2);
    expect(mocks.draftProcedure).toHaveBeenLastCalledWith("Premier", "Swiss");

    releaseProcedure();

    await waitFor(() => expect(useDraftPodStore.getState().procedureCacheKey).toEqual({
      kind: "Premier",
      tournamentFormat: "Swiss",
    }));
    expect(useDraftPodStore.getState().configError).toBeNull();
    expect(screen.queryByText("This draft is incompatible with your protocol version.")).toBeNull();
    expect(screen.queryByText("podError.incompatible")).toBeNull();
  });

  it.each(["absent", "invalid"] as const)("loads the setup procedure on a fresh guest route whose guest resume is %s", async (outcome) => {
    mocks.guestResume.mockResolvedValue(outcome);
    const user = userEvent.setup();
    renderAt("/draft-pod?entry=guest");

    await waitFor(() => expect(mocks.guestResume).toHaveBeenCalledOnce());
    expect(mocks.draftProcedure).toHaveBeenCalledOnce();
    expect(mocks.draftProcedure).toHaveBeenCalledWith("Premier", "Swiss");
    expect(mocks.draftProcedure.mock.invocationCallOrder[0]).toBeLessThan(mocks.guestResume.mock.invocationCallOrder[0]);
    expect(screen.getByRole("button", { name: /Host a Pod/ })).toBeInTheDocument();

    await waitFor(() => expect(useDraftPodStore.getState().procedureCacheKey).toEqual({
      kind: "Premier",
      tournamentFormat: "Swiss",
    }));
    expect(useDraftPodStore.getState().allowedPodSizes).toEqual([3, 4, 5, 6, 7, 8]);
    await user.click(screen.getByRole("button", { name: /Host a Pod/ }));
    expect(screen.getByText("Draft Type")).toBeInTheDocument();
    expect(screen.queryByText("Loading set pool data...")).toBeNull();
    expect(screen.getByRole("button", { name: "Pod Size" })).toBeEnabled();
  });

  it("routes every B recovery outcome through the real persistence → pod → lifecycle → wrapper handoff without retiring A early", async () => {
    const hostResult = (roomCode: string) => ({
      roomCode,
      peerId: `peer-${roomCode}`,
      // These deliberately model the installed owner's credentials and
      // outbound queue. B must not destroy or mutate A on a non-commit path.
      peer: { credential: `credential-${roomCode}`, outbox: [`outbox-${roomCode}`], destroy: vi.fn() },
      onGuestConnected: vi.fn(() => vi.fn()),
      destroy: vi.fn(),
    });
    const session = (id: string, roomCode: string) => ({
      persistenceId: id, roomCode, kind: "Premier" as const, podSize: 4,
      hostDisplayName: "Host", tournamentFormat: "Swiss" as const, podPolicy: "Competitive" as const,
      seatTokens: { 0: "host" }, seatNames: { 0: "Host" }, kickedTokens: [],
      draftStarted: false, draftCode: roomCode, draftSessionJson: null,
      poolInput: { type: "Set" as const, data: { pools: [{ code: "TST" }], sequence: ["TST"] } },
    });
    const configFor = (persisted: ReturnType<typeof session>) => ({
      poolInput: persisted.poolInput, kind: persisted.kind, podSize: persisted.podSize,
      hostDisplayName: persisted.hostDisplayName, tournamentFormat: persisted.tournamentFormat,
      podPolicy: persisted.podPolicy, persistenceId: persisted.persistenceId,
      preferredRoomCode: persisted.roomCode,
    });
    const metaFor = (persisted: ReturnType<typeof session>, updatedAt: number) => ({
      id: persisted.persistenceId, roomCode: persisted.roomCode, updatedAt,
      kind: persisted.kind, podSize: persisted.podSize, hostDisplayName: persisted.hostDisplayName,
      tournamentFormat: persisted.tournamentFormat, podPolicy: persisted.podPolicy,
      phase: "lobby" as const, pickCount: 0,
    });
    const ownerA = session("owner-a", "AAAAA");
    const ownerB = session("owner-b", "BBBBB");
    const resultA = hostResult(ownerA.roomCode);
    mocks.loadDraftHostSession.mockResolvedValueOnce({ type: "absent" });
    mocks.recoveryHostRoom.mockResolvedValueOnce(resultA);
    await expect(useMultiplayerDraftStore.getState().hostDraft(configFor(ownerA))).resolves.toEqual({ status: "opened" });
    const hostA = mocks.recoveryHosts[0]!;
    const aUnsubscribes = hostA.onEvent.mock.results.map((result) => result.value as ReturnType<typeof vi.fn>);
    const admittedA = engineView("Premier", 4);
    useMultiplayerDraftStore.setState({
      view: admittedA, workspaceState: { schemaVersion: 1, placements: {}, virtualBasics: [] },
      mainDeck: ["Island"], submittedDeck: ["Island"],
    } as never);
    const assertAOwns = () => {
      expect(useMultiplayerDraftStore.getState()).toMatchObject({
        role: "host", hostPersistenceId: ownerA.persistenceId, view: admittedA,
        mainDeck: ["Island"], submittedDeck: ["Island"],
      });
      expect(hostA.dispose).not.toHaveBeenCalled();
      for (const unsubscribe of aUnsubscribes) expect(unsubscribe).not.toHaveBeenCalled();
      expect(resultA.destroy).not.toHaveBeenCalled();
      expect(resultA.peer.destroy).not.toHaveBeenCalled();
      expect(resultA.peer).toMatchObject({ credential: "credential-AAAAA", outbox: ["outbox-AAAAA"] });
      // B's raw persistence input remains a refusal/read-only observation;
      // neither its failed attempt nor a same-code replacement may clear A.
      expect(mocks.clearActiveDraftPodIfCurrent).not.toHaveBeenCalled();
      expect(mocks.guestResume).not.toHaveBeenCalled();
    };
    let current = {
      type: "present" as const,
      meta: metaFor(ownerB, 20),
      capture: { id: ownerB.persistenceId, roomCode: ownerB.roomCode, updatedAt: 20 },
    };
    mocks.inspectActiveDraftPod.mockImplementation(() => current);

    // Each route mounts the real page. The persistence/pod short circuits are
    // intentionally not simulated by a replacement lifecycle action.
    const saved = savedSession("fifo_drafting");
    const refusedB = { ...ownerB, draftStarted: true, draftSessionJson: saved.text };
    mocks.loadDraftHostSession.mockResolvedValueOnce({ type: "admitted", session: refusedB, raw: refusedB });
    mocks.checkRestorableSession.mockResolvedValue(saved.verdict);
    let rendered = renderAt("/draft-pod?entry=host");
    await waitFor(() => expect(screen.getByText("This draft is incompatible with your protocol version.")).toBeInTheDocument());
    assertAOwns();
    rendered.unmount();
    useDraftPodStore.getState().reset();

    mocks.loadDraftHostSession.mockRejectedValueOnce(new Error("storage read failed"));
    rendered = renderAt("/draft-pod?entry=host");
    await waitFor(() => expect(useDraftPodStore.getState().hostRecoveryFailure?.kind).toBe("readFailure"));
    assertAOwns();
    rendered.unmount();
    useDraftPodStore.getState().reset();

    useConnectivityStore.setState({ forcedOffline: true });
    rendered = renderAt("/draft-pod?entry=host");
    await waitFor(() => expect(useDraftPodStore.getState().configError).toBe("offline.startUnavailable"));
    assertAOwns();
    rendered.unmount();
    useConnectivityStore.setState({ forcedOffline: false });
    useDraftPodStore.getState().reset();

    let resolveAllocation!: (value: ReturnType<typeof hostResult>) => void;
    mocks.loadDraftHostSession.mockResolvedValueOnce({ type: "admitted", session: ownerB, raw: ownerB });
    mocks.recoveryHostRoom.mockImplementationOnce(() => new Promise<ReturnType<typeof hostResult>>((resolve) => { resolveAllocation = resolve; }));
    rendered = renderAt("/draft-pod?entry=host");
    await waitFor(() => expect(mocks.recoveryHostRoom).toHaveBeenCalledTimes(2));
    rendered.unmount(); // route-owned AbortSignal aborts B during hostRoom allocation
    resolveAllocation(hostResult(ownerB.roomCode));
    await waitFor(assertAOwns);
    useDraftPodStore.getState().reset();

    let resolveStale!: (value: ReturnType<typeof hostResult>) => void;
    mocks.loadDraftHostSession.mockResolvedValueOnce({ type: "admitted", session: ownerB, raw: ownerB });
    mocks.recoveryHostRoom.mockImplementationOnce(() => new Promise<ReturnType<typeof hostResult>>((resolve) => { resolveStale = resolve; }));
    rendered = renderAt("/draft-pod?entry=host");
    await waitFor(() => expect(mocks.recoveryHostRoom).toHaveBeenCalledTimes(3));
    current = {
      type: "present", meta: metaFor(session("owner-c", "BBBBB"), 21),
      capture: { id: "owner-c", roomCode: "BBBBB", updatedAt: 21 },
    };
    resolveStale(hostResult(ownerB.roomCode));
    await waitFor(assertAOwns);
    rendered.unmount();
    current = {
      type: "present", meta: metaFor(ownerB, 20),
      capture: { id: ownerB.persistenceId, roomCode: ownerB.roomCode, updatedAt: 20 },
    };
    useDraftPodStore.getState().reset();

    mocks.loadDraftHostSession.mockResolvedValueOnce({ type: "admitted", session: ownerB, raw: ownerB });
    mocks.recoveryHostRoom.mockResolvedValueOnce(hostResult(ownerB.roomCode));
    mocks.recoveryInitialize.mockRejectedValueOnce(new Error("B initialization failed"));
    rendered = renderAt("/draft-pod?entry=host");
    await waitFor(() => expect(useDraftPodStore.getState().hostRecoveryFailure?.kind).toBe("initializationFailure"));
    expect(useDraftPodStore.getState().configError).toBe("podError.connection");
    assertAOwns();
    rendered.unmount();
    useDraftPodStore.getState().reset();

    let finishB!: () => void;
    mocks.loadDraftHostSession.mockResolvedValueOnce({ type: "admitted", session: ownerB, raw: ownerB });
    mocks.recoveryHostRoom.mockResolvedValueOnce(hostResult(ownerB.roomCode));
    mocks.recoveryInitialize.mockImplementationOnce(() => new Promise<void>((resolve) => { finishB = resolve; }));
    rendered = renderAt("/draft-pod?entry=host");
    await waitFor(() => expect(mocks.recoveryHosts).toHaveLength(3));
    // B exists only provisionally while its wrapper initialization is held.
    assertAOwns();
    finishB();
    await waitFor(() => expect(useMultiplayerDraftStore.getState().hostPersistenceId).toBe(ownerB.persistenceId));
    expect(hostA.dispose).toHaveBeenCalledOnce();
    expect(resultA.destroy).toHaveBeenCalledOnce();
    rendered.unmount();
  });

  describe("host recovery while the setup screen mounts", () => {
    const savedPod = {
      persistenceId: "owner-b", roomCode: "BBBBB", kind: "Premier" as const, podSize: 4,
      hostDisplayName: "Host", tournamentFormat: "Swiss" as const, podPolicy: "Competitive" as const,
      seatTokens: { 0: "host" }, seatNames: { 0: "Host" }, kickedTokens: [],
      draftStarted: false, draftCode: "BBBBB", draftSessionJson: null,
      poolInput: { type: "Set" as const, data: { pools: [{ code: "TST" }], sequence: ["TST"] } },
    };
    const capture = { id: savedPod.persistenceId, roomCode: savedPod.roomCode, updatedAt: 20 };
    const admitted = { type: "admitted" as const, session: savedPod, raw: savedPod };
    const mounts = { count: 0 };
    const nav: { current: NavigateFunction | null } = { current: null };

    function NavProbe() {
      nav.current = useNavigate();
      return null;
    }

    function MountProbe({ children }: { children: ReactNode }) {
      useEffect(() => {
        mounts.count += 1;
      }, []);
      return <>{children}</>;
    }

    function renderInPlace(entries: string[], index: number) {
      return render(
        <MemoryRouter initialEntries={entries} initialIndex={index}>
          <NavProbe />
          <MountProbe><DraftPodPage /></MountProbe>
        </MemoryRouter>,
      );
    }

    // Each fails one stage of the first attempt and returns what lets the
    // next attempt pass that stage.
    function failProcedureLoad(): () => void {
      const healthy = mocks.draftProcedure.getMockImplementation()!;
      mocks.draftProcedure.mockRejectedValue(new Error("draft wasm failed to load"));
      return () => {
        mocks.draftProcedure.mockImplementation(healthy);
      };
    }
    function failPeerInitialization(): () => void {
      mocks.recoveryInitialize.mockRejectedValueOnce(new Error("peer server unavailable"));
      return () => {};
    }
    function failSavedPodRead(): () => void {
      mocks.loadDraftHostSession.mockRejectedValueOnce(new Error("storage read failed"));
      return () => {};
    }

    beforeEach(() => {
      mounts.count = 0;
      nav.current = null;
      mocks.inspectActiveDraftPod.mockReturnValue({
        type: "present",
        meta: {
          ...capture,
          kind: savedPod.kind,
          podSize: savedPod.podSize,
          hostDisplayName: savedPod.hostDisplayName,
          tournamentFormat: savedPod.tournamentFormat,
          podPolicy: savedPod.podPolicy,
          phase: "lobby",
          pickCount: 0,
        },
        capture,
      });
      // A reset also drops any one-time result an earlier row left unconsumed.
      mocks.loadDraftHostSession.mockReset().mockResolvedValue(admitted);
      mocks.recoveryInitialize.mockReset().mockResolvedValue(undefined);
      mocks.recoveryHostRoom.mockImplementation(async () => ({
        roomCode: savedPod.roomCode,
        peerId: `peer-${savedPod.roomCode}`,
        peer: { destroy: vi.fn() },
        onGuestConnected: vi.fn(() => vi.fn()),
        destroy: vi.fn(),
      }));
    });

    it.each([
      ["the procedure load", "initializationFailure", failProcedureLoad],
      ["peer initialization", "initializationFailure", failPeerInitialization],
      ["the saved-pod read", "readFailure", failSavedPodRead],
    ] as const)("recovers the host when Try Reconnecting follows a failure from %s", async (_source, kind, failFirst) => {
      const heal = failFirst();
      const user = userEvent.setup();
      renderAt("/draft-pod?entry=host");
      const retry = await screen.findByRole("button", { name: "Try Reconnecting" });
      expect(useDraftPodStore.getState().hostRecoveryFailure?.kind).toBe(kind);
      expect(useMultiplayerDraftStore.getState().phase).toBe("idle");
      const hostsBefore = mocks.recoveryHosts.length;
      heal();

      await user.click(retry);

      await waitFor(() => expect(mocks.loadDraftHostSession).toHaveBeenCalledTimes(2));
      await waitFor(() => expect(useMultiplayerDraftStore.getState().role).toBe("host"));
      expect(useMultiplayerDraftStore.getState().hostPersistenceId).toBe(savedPod.persistenceId);
      expect(mocks.recoveryHosts).toHaveLength(hostsBefore + 1);
      expect(screen.queryByRole("button", { name: "Try Reconnecting" })).toBeNull();
    });

    it("shows the failure screen again when Try Reconnecting re-reads a persisting incompatibility", async () => {
      const saved = savedSession("fifo_drafting");
      const refused = { ...savedPod, draftStarted: true, draftSessionJson: saved.text };
      mocks.loadDraftHostSession.mockResolvedValue({ type: "admitted", session: refused, raw: refused });
      mocks.checkRestorableSession.mockResolvedValue(saved.verdict);
      const user = userEvent.setup();
      renderAt("/draft-pod?entry=host");
      const retry = await screen.findByRole("button", { name: "Try Reconnecting" });
      expect(useDraftPodStore.getState().hostRecoveryFailure?.kind).toBe("incompatible");
      expect(useMultiplayerDraftStore.getState().phase).toBe("idle");

      await user.click(retry);

      await waitFor(() => expect(mocks.loadDraftHostSession).toHaveBeenCalledTimes(2));
      expect(await screen.findByRole("button", { name: "Try Reconnecting" })).toBeInTheDocument();
      expect(useDraftPodStore.getState().hostRecoveryFailure?.kind).toBe("incompatible");
      expect(screen.getByText("This draft is incompatible with your protocol version.")).toBeInTheDocument();
    });

    it("recovers the host when the retry's saved-pod read settles after the setup screen has rendered", async () => {
      failSavedPodRead();
      const user = userEvent.setup();
      renderAt("/draft-pod?entry=host");
      const retry = await screen.findByRole("button", { name: "Try Reconnecting" });
      expect(useDraftPodStore.getState().hostRecoveryFailure?.kind).toBe("readFailure");
      expect(useMultiplayerDraftStore.getState().phase).toBe("idle");
      let settleRead!: (result: typeof admitted) => void;
      mocks.loadDraftHostSession.mockReturnValueOnce(new Promise((resolve) => {
        settleRead = resolve;
      }));

      await user.click(retry);

      await waitFor(() => expect(mocks.loadDraftHostSession).toHaveBeenCalledTimes(2));
      expect(screen.getByRole("button", { name: /Host a Pod/ })).toBeInTheDocument();
      settleRead(admitted);
      await waitFor(() => expect(useMultiplayerDraftStore.getState().role).toBe("host"));
      expect(useMultiplayerDraftStore.getState().hostPersistenceId).toBe(savedPod.persistenceId);
      expect(mocks.recoveryHosts).toHaveLength(1);
      expect(screen.queryByRole("button", { name: "Try Reconnecting" })).toBeNull();
    });

    describe.each([
      ["the procedure load", failProcedureLoad],
      ["the saved-pod read", failSavedPodRead],
      ["peer initialization", failPeerInitialization],
    ] as const)("after a host-entry failure from %s", (_source, failFirst) => {
      it.each([
        ["a push to /draft-pod?resume=1", ["/draft-pod?entry=host"], 0, "/draft-pod?resume=1"],
        ["going back to /draft-pod", ["/draft-pod", "/draft-pod?entry=host"], 1, -1],
      ] as const)("recovers the host on %s without a remount", async (_navigation, entries, index, to) => {
        const heal = failFirst();
        renderInPlace([...entries], index);
        await screen.findByRole("button", { name: "Try Reconnecting" });
        const hostsBefore = mocks.recoveryHosts.length;
        heal();

        await act(async () => {
          if (to === -1) nav.current!(-1);
          else nav.current!(to);
        });

        await waitFor(() => expect(mocks.loadDraftHostSession).toHaveBeenCalledTimes(2));
        expect(mounts.count).toBe(1);
        await waitFor(() => expect(useMultiplayerDraftStore.getState().role).toBe("host"));
        expect(mocks.recoveryHosts).toHaveLength(hostsBefore + 1);
        expect(screen.queryByRole("button", { name: "Try Reconnecting" })).toBeNull();
      });
    });

    it("loads a newly chosen kind's procedure when the kind changes during a first-entry recovery", async () => {
      let settleRead!: (result: typeof admitted) => void;
      mocks.loadDraftHostSession.mockReturnValueOnce(new Promise((resolve) => {
        settleRead = resolve;
      }));
      const user = userEvent.setup();
      renderAt("/draft-pod?entry=host");
      await waitFor(() => expect(mocks.loadDraftHostSession).toHaveBeenCalledOnce());
      await waitFor(() => expect(mocks.draftProcedure).toHaveBeenCalledWith("Premier", "Swiss"));
      await user.click(screen.getByRole("button", { name: /Host a Pod/ }));
      expect(isHostedPodRecoveryInFlight()).toBe(true);

      await user.click(screen.getByRole("radio", { name: "Commander" }));

      await waitFor(() => expect(useDraftPodStore.getState().procedureCacheKey).toEqual({
        kind: "CommanderDraft",
        tournamentFormat: "Swiss",
      }));
      expect(mocks.draftProcedure).toHaveBeenCalledWith("CommanderDraft", "Swiss");
      await act(async () => {
        settleRead(admitted);
        await new Promise((resolve) => setTimeout(resolve, 50));
      });
      expect(mocks.recoveryHosts).toHaveLength(0);
      expect(useMultiplayerDraftStore.getState().role).toBeNull();
    });

    it("keeps the retry's own message when its saved-pod read finds the saved pod gone", async () => {
      failSavedPodRead();
      const user = userEvent.setup();
      renderAt("/draft-pod?entry=host");
      const retry = await screen.findByRole("button", { name: "Try Reconnecting" });
      expect(useDraftPodStore.getState().hostRecoveryFailure?.kind).toBe("readFailure");
      mocks.loadDraftHostSession.mockResolvedValueOnce({ type: "absent" });

      await user.click(retry);

      await waitFor(() => expect(mocks.loadDraftHostSession).toHaveBeenCalledTimes(2));
      await user.click(screen.getByRole("button", { name: /Host a Pod/ }));
      expect(screen.getByText("Draft Type")).toBeInTheDocument();
      await waitFor(() => expect(useDraftPodStore.getState().configError).toBe("Saved draft pod was not found"));
      expect(screen.getByText("Saved draft pod was not found")).toBeInTheDocument();
    });

    describe("a recovery that ends with no host and no failure", () => {
      const procedureKey = { kind: "Premier", tournamentFormat: "Swiss" };
      const endings = [
        ["absent", { type: "absent" }, "Saved draft pod was not found"],
        ["invalid", { type: "invalid" }, "Saved draft pod is invalid"],
        ["terminal", { type: "terminal" }, "Saved draft pod is complete"],
        ["another pod's", { ...admitted, session: { ...savedPod, roomCode: "OTHER" } }, "Saved draft pod is invalid"],
      ] as const;

      it.each(endings)("publishes the procedure and keeps the message when a retry finds it %s", async (_name, loaded, message) => {
        failSavedPodRead();
        const user = userEvent.setup();
        renderAt("/draft-pod?entry=host");
        const retry = await screen.findByRole("button", { name: "Try Reconnecting" });
        expect(useDraftPodStore.getState().procedureCacheKey).toBeNull();
        mocks.loadDraftHostSession.mockResolvedValueOnce(loaded);

        await user.click(retry);

        await waitFor(() => expect(useDraftPodStore.getState().procedureCacheKey).toEqual(procedureKey));
        expect(useDraftPodStore.getState().packsPerPlayer).not.toBeNull();
        expect(useDraftPodStore.getState().configError).toBe(message);
        expect(useDraftPodStore.getState().hostRecoveryFailure).toBeNull();
        expect(useMultiplayerDraftStore.getState().role).toBeNull();
        await user.click(screen.getByRole("button", { name: /Host a Pod/ }));
        expect(screen.getByText(message)).toBeInTheDocument();
        expect(screen.queryByText("Loading pool...")).toBeNull();
      });

      it.each(endings)("publishes the procedure and keeps the message when an entry finds it %s", async (_name, loaded, message) => {
        mocks.loadDraftHostSession.mockResolvedValueOnce(loaded);

        renderAt("/draft-pod?entry=host");

        await waitFor(() => expect(useDraftPodStore.getState().procedureCacheKey).toEqual(procedureKey));
        expect(useDraftPodStore.getState().configError).toBe(message);
        expect(mocks.loadDraftHostSession).toHaveBeenCalledOnce();
        expect(useMultiplayerDraftStore.getState().role).toBeNull();
      });

      it("publishes the procedure with no message on an automatic entry", async () => {
        mocks.loadDraftHostSession.mockResolvedValueOnce({ type: "terminal" });

        renderAt("/draft-pod");

        await waitFor(() => expect(useDraftPodStore.getState().procedureCacheKey).toEqual(procedureKey));
        expect(useDraftPodStore.getState().configError).toBeNull();
      });

      it("keeps the retry's message when the procedure itself cannot load", async () => {
        failSavedPodRead();
        const user = userEvent.setup();
        renderAt("/draft-pod?entry=host");
        const retry = await screen.findByRole("button", { name: "Try Reconnecting" });
        mocks.loadDraftHostSession.mockResolvedValueOnce({ type: "terminal" });
        const calls = mocks.draftProcedure.mock.calls.length;
        const healthy = mocks.draftProcedure.getMockImplementation()!;
        mocks.draftProcedure.mockRejectedValue(new Error("draft wasm failed to load"));

        await user.click(retry);

        await waitFor(() => expect(mocks.draftProcedure.mock.calls.length).toBeGreaterThan(calls));
        await act(async () => { await new Promise((resolve) => setTimeout(resolve, 20)); });
        expect(useDraftPodStore.getState().configError).toBe("Saved draft pod is complete");
        expect(useDraftPodStore.getState().procedureCacheKey).toBeNull();
        mocks.draftProcedure.mockImplementation(healthy);
      });

      it("lets a newer pod operation win over the procedure it was loading", async () => {
        failSavedPodRead();
        const user = userEvent.setup();
        renderAt("/draft-pod?entry=host");
        const retry = await screen.findByRole("button", { name: "Try Reconnecting" });
        mocks.loadDraftHostSession.mockResolvedValueOnce({ type: "terminal" });
        const healthy = mocks.draftProcedure.getMockImplementation()!;
        let release!: () => void;
        mocks.draftProcedure.mockImplementationOnce((...args: Parameters<typeof healthy>) =>
          new Promise((resolve) => { release = () => resolve(healthy(...args)); }));

        await user.click(retry);
        await waitFor(() => expect(release).toBeDefined());
        act(() => useDraftPodStore.getState().setConfig({ kind: "CommanderDraft" }));
        await waitFor(() => expect(useDraftPodStore.getState().procedureCacheKey?.kind).toBe("CommanderDraft"));
        await act(async () => { release(); await new Promise((resolve) => setTimeout(resolve, 20)); });

        expect(useDraftPodStore.getState().procedureCacheKey?.kind).toBe("CommanderDraft");
      });
    });

    it("loads the setup procedure when a remounted page meets an aborted recovery that has not settled", async () => {
      let settleRead!: (result: typeof admitted) => void;
      mocks.loadDraftHostSession.mockReturnValueOnce(new Promise((resolve) => {
        settleRead = resolve;
      }));
      const rendered = renderAt("/draft-pod?entry=host");
      await waitFor(() => expect(mocks.loadDraftHostSession).toHaveBeenCalledOnce());
      expect(useDraftPodStore.getState().procedureCacheKey).toBeNull();
      mocks.inspectActiveDraftPod.mockReturnValue({ type: "absent" });

      rendered.rerender(
        <MemoryRouter key="remounted" initialEntries={["/draft-pod"]}>
          <DraftPodPage />
        </MemoryRouter>,
      );

      await waitFor(() => expect(mocks.guestResume).toHaveBeenCalledOnce());
      expect(mocks.loadDraftHostSession).toHaveBeenCalledOnce();
      await waitFor(() => expect(useDraftPodStore.getState().procedureCacheKey).toEqual({
        kind: "Premier",
        tournamentFormat: "Swiss",
      }));
      expect(useDraftPodStore.getState().allowedPodSizes).toEqual([3, 4, 5, 6, 7, 8]);
      settleRead(admitted);
    });

    it("ends a host recovered through Try Reconnecting when its page route ends", async () => {
      failSavedPodRead();
      const user = userEvent.setup();
      const rendered = renderAt("/draft-pod?entry=host");
      await user.click(await screen.findByRole("button", { name: "Try Reconnecting" }));
      await waitFor(() => expect(useMultiplayerDraftStore.getState().role).toBe("host"));
      expect(mocks.recoveryHosts).toHaveLength(1);

      rendered.unmount();

      await waitFor(() => expect(useMultiplayerDraftStore.getState().role).toBeNull());
      await waitFor(() => expect(mocks.recoveryHosts[0]!.dispose).toHaveBeenCalled());
    });

    it("ends a host recovered on entering the page when its page route ends", async () => {
      const rendered = renderAt("/draft-pod?entry=host");
      await waitFor(() => expect(useMultiplayerDraftStore.getState().role).toBe("host"));
      expect(mocks.recoveryHosts).toHaveLength(1);

      rendered.unmount();

      await waitFor(() => expect(useMultiplayerDraftStore.getState().role).toBeNull());
      await waitFor(() => expect(mocks.recoveryHosts[0]!.dispose).toHaveBeenCalled());
    });
  });

  it("offers Commander Draft in the pod kind selector", async () => {
    const user = userEvent.setup();
    renderAt("/draft-pod?kind=commander");

    await user.click(screen.getByRole("button", { name: /Host a Pod/ }));

    // Reach guard: the pre-existing radios rendered, so an absent Commander
    // radio would be a real absence.
    expect(screen.getByRole("radio", { name: "Premier" })).toBeInTheDocument();
    // REVERT-FAILING: BASE's radio group has three members and its description
    // map is `Partial` with a `?? ""` fallback, so both of these are absent.
    await waitFor(() =>
      expect(screen.getByRole("radio", { name: "Commander" })).toBeChecked(),
    );
    expect(
      screen.getByText(
        "Each player drafts two cards at a time and builds a 60-card Commander deck, then the pod plays one multiplayer game.",
      ),
    ).toBeInTheDocument();
  });

  it("applies the deep-linked Winston kind through the same slug map", async () => {
    renderAt("/draft-pod?kind=winston");

    // REVERT-FAILING: with no `winston` entry in `DRAFT_KIND_ENTRIES` the slug
    // resolves to `null` and the kind stays "Premier". `podSize: 6` is the
    // fixture's, so a hardcoded client default fails the second half.
    await waitFor(() =>
      expect(useDraftPodStore.getState().config).toMatchObject({
        kind: "Winston",
        podSize: 6,
      }),
    );
    expect(mocks.draftProcedure).toHaveBeenCalledWith("Winston", "Swiss");
  });

  it("offers Winston in the pod kind selector", async () => {
    const user = userEvent.setup();
    renderAt("/draft-pod?kind=winston");

    await user.click(screen.getByRole("button", { name: /Host a Pod/ }));

    // Reach guard: the pre-existing radios rendered.
    expect(screen.getByRole("radio", { name: "Premier" })).toBeInTheDocument();
    await waitFor(() => expect(screen.getByRole("radio", { name: "Winston" })).toBeChecked());
    expect(
      screen.getByText(
        "Two to four players draft one shared face-down stack through three piles: on your turn, look at a pile and take it or decline and add a card to it.",
      ),
    ).toBeInTheDocument();
  });

  it("ignores an unknown ?kind= slug instead of failing the route", async () => {
    const user = userEvent.setup();
    renderAt("/draft-pod?kind=nonesuch");

    await user.click(screen.getByRole("button", { name: /Host a Pod/ }));

    expect(useDraftPodStore.getState().config.kind).toBe("Premier");
    expect(screen.getByRole("radio", { name: "Winston" })).not.toBeChecked();
    await waitFor(() => expect(mocks.draftProcedure).toHaveBeenCalledWith("Premier", "Swiss"));
    // The witness that no kind-intent effect fired, as in the bare-route case:
    // the store keeps its own default rather than adopting the fixture's 6.
    expect(useDraftPodStore.getState().config.podSize).toBe(8);
  });

  it("leaves a bare /draft-pod on the Premier default", async () => {
    const user = userEvent.setup();
    renderAt("/draft-pod");

    await user.click(screen.getByRole("button", { name: /Host a Pod/ }));

    // First production branch reached: `searchParams.get("kind") !== COMMANDER_DRAFT_ENTRY`.
    expect(useDraftPodStore.getState().config.kind).toBe("Premier");
    expect(screen.getByRole("radio", { name: "Commander" })).not.toBeChecked();
    // The witness for "the kind-intent effect did not fire" is `podSize`, not
    // whether `draftProcedure` was called: setup re-reads the engine's per-kind
    // axes on every kind change, so the CALL is no longer specific to
    // `enterKind`. Adopting the kind's `pod_size` still is, and the fixture
    // publishes 6 against the store's 8 default precisely so a stray
    // `enterKind` cannot hide behind a coincidence.
    await waitFor(() => expect(mocks.draftProcedure).toHaveBeenCalledWith("Premier", "Swiss"));
    expect(useDraftPodStore.getState().config.podSize).toBe(8);
  });

  it.each([
    {
      description: "normal Swiss pod",
      kind: "Premier",
      tournamentFormat: "Swiss",
      expectedSizes: [2, 3, 4, 5, 6, 7, 8],
    },
    {
      description: "Commander Swiss pod",
      kind: "CommanderDraft",
      tournamentFormat: "Swiss",
      expectedSizes: [3, 4, 5, 6, 7, 8],
    },
    {
      description: "normal single-elimination pod",
      kind: "Premier",
      tournamentFormat: "SingleElimination",
      expectedSizes: [8],
    },
    {
      description: "Commander single-elimination pod",
      kind: "CommanderDraft",
      tournamentFormat: "SingleElimination",
      expectedSizes: [3, 4, 5, 6, 7, 8],
    },
  ])("offers exactly the legal seat counts for a $description", async ({
    kind,
    tournamentFormat,
    expectedSizes,
  }) => {
    const user = userEvent.setup();
    mocks.draftProcedure.mockImplementation(async (
      requestedKind: string,
      requestedTournamentFormat: string,
    ) => draftProcedureFixture({
      pod_size: 8,
      human_seats: 1,
      min_pod_size: requestedKind === "CommanderDraft" ? 3 : 2,
      max_pod_size: 8,
      allowed_pod_sizes: requestedTournamentFormat === "SingleElimination"
        && requestedKind !== "CommanderDraft"
        ? [8]
        : requestedKind === "CommanderDraft"
          ? [3, 4, 5, 6, 7, 8]
          : [2, 3, 4, 5, 6, 7, 8],
      packs_per_player: 3,
      cards_per_pick: requestedKind === "CommanderDraft" ? 2 : 1,
      distribution: "PickAndPass",
      min_deck_size: 60,
      cube_min_deck_size: 53,
      post_draft_play: requestedKind === "CommanderDraft"
        ? "CompleteImmediately"
        : "TournamentPairings",
      match_config: { match_type: "Bo1" },
    }));

    renderAt("/draft-pod");
    await user.click(screen.getByRole("button", { name: /Host a Pod/ }));

    // Premier's procedure offers the format radios; Commander's does not.
    if (tournamentFormat === "SingleElimination") {
      await user.click(await screen.findByRole("radio", { name: "Single Elimination" }));
    }
    if (kind === "CommanderDraft") {
      await user.click(screen.getByRole("radio", { name: "Commander" }));
    }

    await waitFor(() => expect(useDraftPodStore.getState().allowedPodSizes).toEqual(expectedSizes));
    const podSizeSelect = screen.getByRole("button", { name: "Pod Size" });
    expect(podSizeSelect).toBeEnabled();
    await user.click(podSizeSelect);

    expect(screen.getAllByRole("option").map((option) => option.textContent)).toEqual(
      expectedSizes.map((size) => `${size} players`),
    );
  });

  it("waits for Commander procedure data instead of offering the previous kind's floor", async () => {
    let resolveCommanderProcedure!: () => void;
    mocks.draftProcedure.mockImplementation((kind: string) => {
      if (kind !== "CommanderDraft") {
        return Promise.resolve(draftProcedureFixture({
          pod_size: 8,
          human_seats: 1,
          min_pod_size: 2,
          max_pod_size: 8,
          allowed_pod_sizes: [2, 3, 4, 5, 6, 7, 8],
          packs_per_player: 3,
          cards_per_pick: 1,
          distribution: "PickAndPass",
          min_deck_size: 60,
          cube_min_deck_size: 53,
          post_draft_play: "TournamentPairings",
          match_config: { match_type: "Bo1" },
        }));
      }

      return new Promise((resolve) => {
        resolveCommanderProcedure = () => resolve(draftProcedureFixture({
          pod_size: 4,
          human_seats: 1,
          min_pod_size: 3,
          max_pod_size: 8,
          allowed_pod_sizes: [3, 4, 5, 6, 7, 8],
          packs_per_player: 3,
          cards_per_pick: 2,
          distribution: "PickAndPass",
          min_deck_size: 60,
          cube_min_deck_size: 73,
          post_draft_play: "CompleteImmediately",
          match_config: { match_type: "Bo1" },
        }));
      });
    });

    const user = userEvent.setup();
    renderAt("/draft-pod");
    await user.click(screen.getByRole("button", { name: /Host a Pod/ }));
    await waitFor(() => expect(useDraftPodStore.getState().allowedPodSizes).toEqual([2, 3, 4, 5, 6, 7, 8]));

    await user.click(screen.getByRole("radio", { name: "Commander" }));

    const podSizeSelect = screen.getByRole("button", { name: "Pod Size" });
    expect(useDraftPodStore.getState().allowedPodSizes).toBeNull();
    expect(podSizeSelect).toBeDisabled();
    expect(screen.queryByRole("option", { name: "2 players" })).toBeNull();
    await waitFor(() => expect(resolveCommanderProcedure).toBeTypeOf("function"));

    resolveCommanderProcedure();

    await waitFor(() => expect(useDraftPodStore.getState().allowedPodSizes).toEqual([3, 4, 5, 6, 7, 8]));
    expect(podSizeSelect).toBeEnabled();
    await user.click(podSizeSelect);
    expect(screen.getAllByRole("option").map((option) => option.textContent)).toEqual([
      "3 players",
      "4 players",
      "5 players",
      "6 players",
      "7 players",
      "8 players",
    ]);
  });

  it("hides the tournament format and round-based policy copy for a procedure that completes immediately", async () => {
    const pendingCommander: Array<() => void> = [];
    mocks.draftProcedure.mockImplementation((kind: string) => kind === "CommanderDraft"
      ? new Promise((resolve) => pendingCommander.push(() => resolve(setupProcedure(kind))))
      : Promise.resolve(setupProcedure(kind)));

    const user = userEvent.setup();
    renderAt("/draft-pod");
    await user.click(screen.getByRole("button", { name: /Host a Pod/ }));
    await waitFor(() => expect(screen.getByRole("radio", { name: "Swiss" })).toBeChecked());
    expect(screen.getByText("Tournament Format")).toBeInTheDocument();
    expect(screen.getByText(/automatic round advancement/)).toBeInTheDocument();

    await user.click(screen.getByRole("radio", { name: "Commander" }));
    await waitFor(() => expect(pendingCommander.length).toBeGreaterThan(0));
    // The previous kind's published procedure must not keep the block up while Commander loads.
    expect(screen.queryByRole("radio", { name: "Swiss" })).toBeNull();
    expect(screen.queryByText("Tournament Format")).toBeNull();

    act(() => pendingCommander.forEach((resolve) => resolve()));
    await waitFor(() => expect(useDraftPodStore.getState().allowedPodSizes).toEqual([3, 4, 5, 6, 7, 8]));
    expect(screen.getByRole("radio", { name: "Commander" })).toBeChecked();
    expect(screen.queryByRole("radio", { name: "Swiss" })).toBeNull();
    expect(screen.queryByRole("radio", { name: "Single Elimination" })).toBeNull();
    expect(screen.queryByText("Tournament Format")).toBeNull();
    expect(screen.getByText("Timed picks and automatic picks on timeout.")).toBeInTheDocument();
    expect(screen.queryByText(/automatic round advancement/)).toBeNull();

    await user.click(screen.getByRole("radio", { name: "Casual" }));
    expect(screen.getByText("Untimed picks and host controls for resolving issues.")).toBeInTheDocument();
    expect(screen.queryByText(/deck changes between rounds/)).toBeNull();

    await user.click(screen.getByRole("radio", { name: "Premier" }));
    await waitFor(() => expect(screen.getByRole("radio", { name: "Swiss" })).toBeChecked());
    expect(screen.getByText("Tournament Format")).toBeInTheDocument();
    expect(screen.getByText(/deck changes between rounds/)).toBeInTheDocument();
  });

  it("keeps the tournament format block while a format change reloads the procedure", async () => {
    const pendingElimination: Array<() => void> = [];
    mocks.draftProcedure.mockImplementation((kind: string, tournamentFormat: string) =>
      tournamentFormat === "SingleElimination"
        ? new Promise((resolve) => pendingElimination.push(() => resolve(setupProcedure(kind))))
        : Promise.resolve(setupProcedure(kind)));

    const user = userEvent.setup();
    renderAt("/draft-pod");
    await user.click(screen.getByRole("button", { name: /Host a Pod/ }));
    await waitFor(() => expect(screen.getByRole("radio", { name: "Swiss" })).toBeChecked());

    await user.click(screen.getByRole("radio", { name: "Single Elimination" }));
    await waitFor(() => expect(pendingElimination.length).toBeGreaterThan(0));
    expect(useDraftPodStore.getState().allowedPodSizes).toBeNull();
    expect(screen.getByRole("radio", { name: "Single Elimination" })).toBeChecked();

    act(() => pendingElimination.forEach((resolve) => resolve()));
    await waitFor(() =>
      expect(useDraftPodStore.getState().procedureCacheKey?.tournamentFormat).toBe("SingleElimination"));
    expect(screen.getByRole("radio", { name: "Single Elimination" })).toBeChecked();
  });

  it("keeps the real cube panel mounted across pending floor changes", async () => {
    let resolveHigher!: () => void;
    let resolveLower!: () => void;
    mocks.draftProcedure.mockImplementation((kind: string) => {
      const base = draftProcedureFixture({
        pod_size: 8,
        human_seats: 1,
        min_pod_size: 2,
        max_pod_size: 8,
        allowed_pod_sizes: [2, 3, 4, 5, 6, 7, 8],
        packs_per_player: 3,
        cards_per_pick: 1,
        distribution: "PickAndPass",
        min_deck_size: 40,
        cube_min_deck_size: 53,
        post_draft_play: "TournamentPairings",
        match_config: { match_type: "Bo1" },
      });
      if (kind === "CommanderDraft") {
        return new Promise((resolve) => {
          resolveHigher = () => resolve({
            ...base,
            cube_min_deck_size: 73,
            min_pod_size: 3,
            cards_per_pick: 2,
            post_draft_play: "CompleteImmediately",
          });
        });
      }
      if (kind === "Traditional") {
        return new Promise((resolve) => {
          resolveLower = () => resolve({ ...base, cube_min_deck_size: 61 });
        });
      }
      return Promise.resolve(base);
    });

    const user = userEvent.setup();
    renderAt("/draft-pod");
    await user.click(screen.getByRole("button", { name: /Host a Pod/ }));
    await waitFor(() => expect(useDraftPodStore.getState().cubeMinDeckSize).toBe(53));
    await user.click(screen.getByRole("button", { name: "Cube" }));

    const minimum = screen.getByRole("spinbutton", { name: "Min Deck" });
    const cubeList = screen.getByPlaceholderText(/1 Lightning Bolt/);
    fireEvent.change(minimum, { target: { value: "67" } });
    await user.type(cubeList, "1 Opt");

    await user.click(screen.getByRole("radio", { name: "Commander" }));
    expect(screen.getByRole("spinbutton", { name: "Min Deck" })).toBe(minimum);
    expect(screen.getByRole("button", { name: "Start Cube Draft" })).toBeDisabled();
    expect(cubeList).toHaveValue("1 Opt");
    resolveHigher();
    await waitFor(() => expect(minimum).toHaveValue(73));

    await user.click(screen.getByRole("radio", { name: "Traditional" }));
    expect(screen.getByRole("spinbutton", { name: "Min Deck" })).toBe(minimum);
    expect(screen.getByRole("button", { name: "Start Cube Draft" })).toBeDisabled();
    expect(cubeList).toHaveValue("1 Opt");
    resolveLower();
    await waitFor(() => expect(minimum).toHaveValue(67));

    await user.click(screen.getByRole("button", { name: "Start Cube Draft" }));
    expect(useDraftPodStore.getState().cubeForm).toMatchObject({
      cubeListText: "1 Opt",
      settings: { min_deck_size: 67 },
    });
  });

  it("leaves cube setup only after an all-at-once procedure resolves", async () => {
    mocks.draftProcedure.mockImplementation(async (kind: string) => draftProcedureFixture({
      pod_size: 8,
      human_seats: 1,
      min_pod_size: 2,
      max_pod_size: 8,
      allowed_pod_sizes: [2, 3, 4, 5, 6, 7, 8],
      packs_per_player: kind === "Sealed" ? 6 : 3,
      cards_per_pick: 1,
      distribution: kind === "Sealed" ? "AllAtOnce" : "PickAndPass",
      min_deck_size: 40,
      cube_min_deck_size: 1,
      post_draft_play: "TournamentPairings",
      match_config: { match_type: "Bo1" },
    }));

    const user = userEvent.setup();
    renderAt("/draft-pod");
    await user.click(screen.getByRole("button", { name: /Host a Pod/ }));
    await waitFor(() => expect(useDraftPodStore.getState().cubeMinDeckSize).toBe(1));
    await user.click(screen.getByRole("button", { name: "Cube" }));
    expect(screen.getByPlaceholderText(/1 Lightning Bolt/)).toBeInTheDocument();

    await user.click(screen.getByRole("radio", { name: "Sealed" }));

    await waitFor(() => expect(useDraftPodStore.getState().packDistribution).toBe("AllAtOnce"));
    expect(useDraftPodStore.getState().poolMode).toBe("set");
    expect(screen.queryByPlaceholderText(/1 Lightning Bolt/)).toBeNull();
  });

  it("lets the persisted session win over a URL kind intent", async () => {
    // HOSTILE multi-authority: `?resume=1` names a persisted pod whose kind is
    // the higher authority; the URL intent must not overwrite it. First
    // production branch reached: `searchParams.get("resume") === "1"`.
    renderAt("/draft-pod?resume=1&kind=commander");

    // `inspectActiveDraftPod` is the probe `resumeHostedPod` runs; witnessing
    // it is what proves the resume path — not the kind-intent effect — took
    // this route.
    await waitFor(() => expect(mocks.inspectActiveDraftPod).toHaveBeenCalled());
    expect(useDraftPodStore.getState().config.kind).toBe("Premier");
    // As above: `enterKind` is witnessed by the pod size it would have adopted
    // (6), not by `draftProcedure` going uncalled — the setup screen re-reads
    // the engine's axes for whatever kind is selected.
    expect(useDraftPodStore.getState().config.podSize).toBe(8);
  });

  it("picks the intro variant from the ENGINE-published launch capability", () => {
    // A guest's local `draftPodStore.config` is never populated from the host's
    // kind, so the intro must read the engine capability. This fixture is exactly that
    // case: the store is left on its "Premier" default.
    useMultiplayerDraftStore.setState({ phase: "drafting", view: engineView("Premier", 4, {
      min_deck_size: 63,
      launch_capability: "CommanderMultiplayer",
    }) } as never);
    renderAt("/draft-pod");

    expect(useDraftPodStore.getState().config.kind).toBe("Premier");
    // REVERT-FAILING: BASE renders `<DraftIntro mode="pod" .../>` unconditionally.
    expect(
      screen.getByText(
        "Open 4 packs; each pack contains 12 cards — pick two cards, pass the rest",
      ),
    ).toBeInTheDocument();
    expect(
      screen.getByText(
        "After drafting, build a Commander deck of at least 63 cards and play one multiplayer game",
      ),
    ).toBeInTheDocument();
  });

  it("describes a shared stack rather than passing packs, on the engine's distribution", () => {
    // THE ORDERING CASE. A Winston pod's `launch_capability` is "None", exactly
    // like an ordinary pod's, so a capability-first test falls through to the
    // pod copy and tells the players to open packs and pass them — a procedure
    // this format does not have. The fixture keeps the capability at its
    // default so only the distribution can be doing the work, and the store is
    // left on its "Premier" default so nothing can be reading a kind label.
    useMultiplayerDraftStore.setState({ phase: "drafting", view: engineView("Winston", 2, {
      pack_count: 3,
      cards_per_pack: 15,
      pack_sizes: [15, 15, 15],
      min_deck_size: 40,
      distribution: { SharedStackPiles: { pile_count: 3 } },
    }) } as never);
    renderAt("/draft-pod");

    expect(screen.getByText("Winston Draft")).toBeInTheDocument();
    expect(
      screen.getByText(
        "Every player's 3 packs are opened without anyone looking, and every card is shuffled together into one face-down stack",
      ),
    ).toBeInTheDocument();
    // The pile count comes from the engine's own `pile_count`, not a literal.
    expect(
      screen.getByText(
        "3 one-card piles are dealt off the top. On your turn, look at the first pile: take it, or decline it and move to the next",
      ),
    ).toBeInTheDocument();
    expect(
      screen.getByText(
        "Turns alternate until no cards are left; then build a deck of at least 40 cards and play tournament matches",
      ),
    ).toBeInTheDocument();

    // And none of the pack-passing copy survives — the actual complaint.
    expect(screen.queryByText(/pick one, pass the rest/)).toBeNull();
    expect(screen.queryByText(/passing direction/)).toBeNull();
  });

  it("still describes a pack-passing pod for every other distribution", () => {
    // The paired negative, on the same helper: an ordinary pod keeps its copy,
    // so the row above is the distribution doing the work rather than the
    // Winston branch swallowing everything.
    useMultiplayerDraftStore.setState({ phase: "drafting", view: engineView("Premier", 8, { min_deck_size: 40 }) } as never);
    renderAt("/draft-pod");

    expect(screen.getByText("Pod Draft")).toBeInTheDocument();
    expect(
      screen.getByText("Open 4 packs; each pack contains 12 cards — pick one, pass the rest"),
    ).toBeInTheDocument();
    expect(screen.queryByText(/face-down stack/)).toBeNull();
  });

  it("counts the pod from the ENGINE-published seats, not the local config", () => {
    // GUEST FIXTURE, multi-authority: the engine publishes a 4-seat Commander pod
    // while `draftPodStore.config.podSize` sits on this client's own `8` default,
    // which a guest never overwrites. The two authorities disagree by construction.
    useMultiplayerDraftStore.setState({ phase: "drafting", view: engineView("Premier", 4, {
      launch_capability: "CommanderMultiplayer",
    }) } as never);
    renderAt("/draft-pod");

    // Reach guard on the WRONG authority: the config really does still say 8, so
    // the assertion below distinguishes the two sources rather than agreeing with
    // both. (Also proves the intro mounted at all, via the sentence itself.)
    expect(useDraftPodStore.getState().config.podSize).toBe(8);
    // REVERT-FAILING: `podSize={podSize}` off `config.podSize` renders
    // "You're drafting with 8 players in a pod" for this exact fixture.
    expect(
      screen.getByText("You're drafting with 4 players in a pod"),
    ).toBeInTheDocument();
    expect(
      screen.queryByText("You're drafting with 8 players in a pod"),
    ).toBeNull();
  });

  it("renders no intro until the engine has published a view", () => {
    // `statusChanged("drafting")` can land before the first `viewUpdated`, so the
    // seat count is briefly unknown. Nothing is rendered rather than the component
    // default of 8 — the count is engine state, and a guess here is unfalsifiable.
    useMultiplayerDraftStore.setState({ phase: "drafting", view: null });
    renderAt("/draft-pod");

    expect(screen.queryByText(/You're drafting with/)).toBeNull();
    // Non-vacuous: the same fixture with a view DOES render the sentence (test
    // above), so this null is the view gate, not a failed render.
    expect(screen.queryByRole("button", { name: "Start Drafting" })).toBeNull();
  });

  it("keeps the pod intro for the other pod kinds", () => {
    useMultiplayerDraftStore.setState({ phase: "drafting", view: engineView("Premier", 8) } as never);
    renderAt("/draft-pod");

    expect(
      screen.getByText("Open 4 packs; each pack contains 12 cards — pick one, pass the rest"),
    ).toBeInTheDocument();
    expect(
      screen.getByText(
        "After drafting, build a deck of at least 45 cards and play tournament matches",
      ),
    ).toBeInTheDocument();
    // Non-vacuous: the positive above proves the intro mounted.
    expect(
      screen.queryByText(
        "Open 4 packs; each pack contains 12 cards — pick two cards, pass the rest",
      ),
    ).toBeNull();
  });

  it("renders mixed pack sizes from the engine-published view", () => {
    useMultiplayerDraftStore.setState({ phase: "drafting", view: engineView("Premier", 8, {
      pack_count: 3,
      cards_per_pack: 15,
      pack_sizes: [15, 14, 15],
    }) } as never);
    renderAt("/draft-pod");

    expect(
      screen.getByText(
        "Open 3 packs of mixed sizes, in this order: 15 cards, 14 cards, and 15 cards — pick one, pass the rest",
      ),
    ).toBeInTheDocument();
  });
});
