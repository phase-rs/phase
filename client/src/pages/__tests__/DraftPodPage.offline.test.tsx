// @vitest-environment happy-dom

import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import { MemoryRouter, Route, Routes, useLocation } from "react-router";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { useConnectivityStore } from "../../stores/connectivityStore";
import { DRAFT_OFFLINE_ERROR } from "../../stores/multiplayerDraftStore";

const mocks = vi.hoisted(() => ({
  leave: vi.fn(async () => {}),
  resumeDraft: vi.fn<(options?: unknown) => Promise<"absent" | "offline">>().mockResolvedValue("absent"),
  resumeHostedPod: vi.fn<(options?: unknown) => Promise<"absent" | "offline">>().mockResolvedValue("absent"),
  reset: vi.fn(),
  enterKindForEntry: vi.fn(),
  refreshProcedure: vi.fn(async () => {}),
  peerConstructed: vi.fn(),
  webSocketConstructed: vi.fn(),
  recoveryAborted: vi.fn(),
  adapterDisposed: vi.fn(),
  configError: null as string | null,
  state: {
    role: null as "host" | "guest" | null,
    phase: "idle" as "idle" | "connecting" | "lobby",
    sideboardPrompt: null,
    playDrawPrompt: null,
    sideboardSubmitted: false,
    view: null,
    intergameWorkspaceState: null,
    error: null,
    seats: [],
    joined: 0,
    total: 0,
    roomCode: null,
    seatIndex: null,
    kickPlayer: vi.fn(),
  },
}));

vi.mock("../../stores/multiplayerDraftStore", async (importOriginal) => {
  const actual = await importOriginal<typeof import("../../stores/multiplayerDraftStore")>();
  const hook = Object.assign(
    <T,>(selector: (state: typeof mocks.state & {
      leave: typeof mocks.leave;
      resumeDraft: typeof mocks.resumeDraft;
    }) => T) => selector({ ...mocks.state, leave: mocks.leave, resumeDraft: mocks.resumeDraft }),
    { getState: () => ({ ...mocks.state, leave: mocks.leave, resumeDraft: mocks.resumeDraft }) },
  );
  return {
    ...actual,
    useMultiplayerDraftStore: hook,
    draftPodScreen: (state: typeof mocks.state) => state.phase,
    intergamePromptKey: () => null,
  };
});

vi.mock("../../stores/draftPodStore", () => ({
  useDraftPodStore: <T,>(selector: (state: {
    config: { kind: "Premier"; tournamentFormat: "Swiss"; podSize: number; podPolicy: "Casual"; setCode: string; setName: string; packs: never[] };
    setConfig: () => void;
    hostDisplayName: string;
    setHostDisplayName: () => void;
    guestDisplayName: string;
    setGuestDisplayName: () => void;
    joinCode: string;
    setJoinCode: () => void;
    createPod: () => void;
    joinPod: () => void;
    configError: string | null;
    loadingPool: boolean;
    poolMode: "set";
    setPoolMode: () => void;
    setDraftMode: "uniform";
    setSetDraftMode: () => void;
    setCubeForm: () => void;
    procedureCacheKey: null;
    allowedPodSizes: null;
    packDistribution: "Draft";
    packsPerPlayer: null;
    refreshProcedure: typeof mocks.refreshProcedure;
    reset: typeof mocks.reset;
    resumeHostedPod: typeof mocks.resumeHostedPod;
    enterKindForEntry: typeof mocks.enterKindForEntry;
  }) => T) => selector({
    config: { kind: "Premier", tournamentFormat: "Swiss", podSize: 8, podPolicy: "Casual", setCode: "", setName: "", packs: [] },
    setConfig: vi.fn(),
    hostDisplayName: "Host",
    setHostDisplayName: vi.fn(),
    guestDisplayName: "Guest",
    setGuestDisplayName: vi.fn(),
    joinCode: "",
    setJoinCode: vi.fn(),
    createPod: vi.fn(),
    joinPod: vi.fn(),
    configError: mocks.configError,
    loadingPool: false,
    poolMode: "set",
    setPoolMode: vi.fn(),
    setDraftMode: "uniform",
    setSetDraftMode: vi.fn(),
    setCubeForm: vi.fn(),
    procedureCacheKey: null,
    allowedPodSizes: null,
    packDistribution: "Draft",
    packsPerPlayer: null,
    refreshProcedure: mocks.refreshProcedure,
    reset: mocks.reset,
    resumeHostedPod: mocks.resumeHostedPod,
    enterKindForEntry: mocks.enterKindForEntry,
  }),
}));

vi.mock("../../components/chrome/ScreenChrome", () => ({
  ScreenChrome: ({ onBack }: { onBack?: () => void }) => onBack ? <button onClick={onBack}>Back</button> : null,
}));
vi.mock("../../components/chrome/ShellContext", () => ({
  useDraftShellChrome: vi.fn(),
  useInShell: () => false,
}));
vi.mock("../../components/draft/HostControls", () => ({ HostControls: () => null, useHostDraftTopActions: () => [] }));
vi.mock("../../components/draft/SetSelector", () => ({ SetSelector: () => null }));
vi.mock("../../components/draft/CubeSetupPanel", () => ({ CubeSetupPanel: () => null }));
vi.mock("peerjs", () => ({
  default: class {
    constructor(...args: unknown[]) {
      mocks.peerConstructed(...args);
    }
  },
}));

import { DraftPodPage } from "../DraftPodPage";

function LocationProbe() {
  const location = useLocation();
  return <output data-testid="location">{location.pathname}</output>;
}

function renderPage(entry = "/draft/pod") {
  return render(
    <MemoryRouter initialEntries={[entry]}>
      <LocationProbe />
      <Routes>
        <Route path="/" element={<div>Home</div>} />
        <Route path="/draft/pod" element={<DraftPodPage />} />
      </Routes>
    </MemoryRouter>,
  );
}

function deferred<T>() {
  let resolve!: (value: T | PromiseLike<T>) => void;
  const promise = new Promise<T>((resolvePromise) => {
    resolve = resolvePromise;
  });
  return { promise, resolve };
}

function expectNoColdAdmissionWork() {
  expect(mocks.resumeHostedPod).not.toHaveBeenCalled();
  expect(mocks.resumeDraft).not.toHaveBeenCalled();
  expect(mocks.refreshProcedure).not.toHaveBeenCalled();
  expect(mocks.enterKindForEntry).not.toHaveBeenCalled();
  expect(mocks.peerConstructed).not.toHaveBeenCalled();
  expect(mocks.webSocketConstructed).not.toHaveBeenCalled();
}

describe("DraftPodPage offline admission", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    mocks.leave.mockResolvedValue(undefined);
    mocks.resumeDraft.mockResolvedValue("absent");
    mocks.resumeHostedPod.mockResolvedValue("absent");
    mocks.refreshProcedure.mockResolvedValue(undefined);
    mocks.state.role = null;
    mocks.state.phase = "idle";
    mocks.configError = null;
    vi.stubGlobal("WebSocket", class {
      constructor(...args: unknown[]) {
        mocks.webSocketConstructed(...args);
      }
    });
    useConnectivityStore.setState({ forcedOffline: false, browserOnline: true });
  });

  afterEach(() => {
    cleanup();
    vi.unstubAllGlobals();
    useConnectivityStore.setState({ forcedOffline: false, browserOnline: true });
  });

  it.each([
    ["bare", "/draft/pod"],
    ["host", "/draft/pod?entry=host"],
    ["guest", "/draft/pod?entry=guest"],
    ["Commander Draft", "/draft/pod?kind=commander"],
    ["persisted resume", "/draft/pod?resume=1"],
  ] as const)("blocks every cold %s route for both effective-offline causes", (_routeLabel, entry) => {
    for (const connectivity of [
      { forcedOffline: true, browserOnline: true },
      { forcedOffline: false, browserOnline: false },
    ]) {
      useConnectivityStore.setState(connectivity);
      const rendered = renderPage(entry);

      expect(screen.getByText("Multiplayer Draft is unavailable while offline.")).toBeInTheDocument();
      expectNoColdAdmissionWork();

      rendered.unmount();
      vi.clearAllMocks();
    }
  });

  it.each([
    ["forced offline", { forcedOffline: true, browserOnline: true }],
    ["browser offline", { forcedOffline: false, browserOnline: false }],
  ] as const)("blocks a cold bare route before recovery work and admits it once after %s reconnect", (_label, connectivity) => {
    useConnectivityStore.setState(connectivity);
    renderPage();

    expect(screen.getByText("Multiplayer Draft is unavailable while offline.")).toBeInTheDocument();
    expectNoColdAdmissionWork();

    act(() => useConnectivityStore.setState({ forcedOffline: false, browserOnline: true }));

    expect(mocks.resumeHostedPod).toHaveBeenCalledTimes(1);
    expect(mocks.refreshProcedure).toHaveBeenCalledTimes(1);
  });

  it("does not fall through from a held host recovery that settles offline", async () => {
    const hostRecovery = deferred<"offline">();
    mocks.resumeHostedPod.mockImplementationOnce(() => hostRecovery.promise);
    renderPage();

    await vi.waitFor(() => expect(mocks.resumeHostedPod).toHaveBeenCalledTimes(1));
    expect(mocks.resumeDraft).not.toHaveBeenCalled();

    await act(async () => {
      hostRecovery.resolve("offline");
      await hostRecovery.promise;
    });

    expect(mocks.resumeHostedPod).toHaveBeenCalledTimes(1);
    expect(mocks.resumeDraft).not.toHaveBeenCalled();
  });

  it("does not retry a held guest recovery that settles offline", async () => {
    const guestRecovery = deferred<"offline">();
    mocks.resumeHostedPod.mockResolvedValueOnce("absent");
    mocks.resumeDraft.mockImplementationOnce(() => guestRecovery.promise);
    renderPage();

    await vi.waitFor(() => expect(mocks.resumeDraft).toHaveBeenCalledTimes(1));
    expect(mocks.resumeHostedPod).toHaveBeenCalledTimes(1);

    await act(async () => {
      guestRecovery.resolve("offline");
      await guestRecovery.promise;
    });

    expect(mocks.resumeHostedPod).toHaveBeenCalledTimes(1);
    expect(mocks.resumeDraft).toHaveBeenCalledTimes(1);
  });

  it("keeps a live connecting pod mounted offline, explains the block, and preserves Back", async () => {
    mocks.state.role = "host";
    mocks.state.phase = "connecting";
    renderPage();

    act(() => useConnectivityStore.setState({ forcedOffline: true }));

    expect(screen.getByText("Reconnect or turn off Offline Mode to host, join, start, or watch a multiplayer draft.")).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: /Host/i })).not.toBeInTheDocument();
    expect(screen.queryByRole("button", { name: /Join/i })).not.toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "Back" }));

    expect(mocks.leave).toHaveBeenCalledWith(false);
  });

  it.each([
    ["connecting", "connecting"],
    ["lobby", "lobby"],
  ] as const)("keeps a live %s pod mounted across online-to-offline without teardown", async (_label, phase) => {
    const heldRecovery = deferred<"absent">();
    const abort = vi.spyOn(AbortController.prototype, "abort");
    mocks.state.role = "host";
    mocks.state.phase = phase;
    mocks.resumeHostedPod.mockImplementationOnce((request) => {
      const signal = (request as { signal?: AbortSignal } | undefined)?.signal;
      signal?.addEventListener("abort", () => {
        mocks.recoveryAborted();
        mocks.adapterDisposed();
        heldRecovery.resolve("absent");
      });
      return heldRecovery.promise;
    });
    renderPage();

    await vi.waitFor(() => expect(mocks.resumeHostedPod).toHaveBeenCalledTimes(1));
    act(() => useConnectivityStore.setState({ forcedOffline: true }));

    expect(screen.getByText("Reconnect or turn off Offline Mode to host, join, start, or watch a multiplayer draft.")).toBeInTheDocument();
    expect(mocks.leave).not.toHaveBeenCalled();
    expect(mocks.reset).not.toHaveBeenCalled();
    expect(abort).not.toHaveBeenCalled();
    expect(mocks.recoveryAborted).not.toHaveBeenCalled();
    expect(mocks.adapterDisposed).not.toHaveBeenCalled();
    abort.mockRestore();
  });

  it("translates only the stable offline sentinel at both setup error surfaces", () => {
    mocks.configError = DRAFT_OFFLINE_ERROR;
    renderPage();

    fireEvent.click(screen.getByRole("button", { name: /Host a Pod/ }));
    expect(screen.getByText("Starting a multiplayer draft is unavailable while offline. Reconnect or turn off Offline Mode to continue.")).toBeInTheDocument();

    cleanup();
    renderPage();
    fireEvent.click(screen.getByRole("button", { name: /Join a Pod/ }));
    expect(screen.getByText("Starting a multiplayer draft is unavailable while offline. Reconnect or turn off Offline Mode to continue.")).toBeInTheDocument();
  });

  it("leaves ordinary setup failures verbatim", () => {
    mocks.configError = "The room has ended.";
    renderPage();

    fireEvent.click(screen.getByRole("button", { name: /Join a Pod/ }));

    expect(screen.getByText("The room has ended.")).toBeInTheDocument();
  });
});
