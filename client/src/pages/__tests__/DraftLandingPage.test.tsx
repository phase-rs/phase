import { act, cleanup, render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { MemoryRouter } from "react-router";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { ActiveDraftGuestMeta, ActiveDraftPodMeta } from "../../services/draftPersistence";
import type { ActiveQuickDraftMeta } from "../../services/quickDraftPersistence";
import { useConnectivityStore } from "../../stores/connectivityStore";

const mocks = vi.hoisted(() => ({
  navigate: vi.fn(),
  loadActiveQuickDraft: vi.fn<() => ActiveQuickDraftMeta | null>(() => null),
  clearActiveQuickDraft: vi.fn(),
  loadActiveDraftPod: vi.fn((): ActiveDraftPodMeta | null => null),
  loadActiveDraftGuest: vi.fn<() => ActiveDraftGuestMeta | null>(() => null),
  clearActiveDraftPod: vi.fn(),
  clearActiveDraftGuest: vi.fn(),
  loadGame: vi.fn(async () => null),
}));

vi.mock("react-router", async (importOriginal) => ({
  ...(await importOriginal<typeof import("react-router")>()),
  useNavigate: () => mocks.navigate,
}));

// `ScreenChrome` reaches `ChromeControls -> AccountControl`, which is unrelated
// to the tile grid under test.
vi.mock("../../components/chrome/ScreenChrome", () => ({ ScreenChrome: () => null }));

// The landing page renders a resume card off localStorage; pin both persistence
// reads so no ambient browser state decides what renders.
vi.mock("../../services/quickDraftPersistence", () => ({
  loadActiveQuickDraft: mocks.loadActiveQuickDraft,
  clearActiveQuickDraft: mocks.clearActiveQuickDraft,
}));
vi.mock("../../services/draftPersistence", () => ({
  loadActiveDraftPod: mocks.loadActiveDraftPod,
  loadActiveDraftGuest: mocks.loadActiveDraftGuest,
  clearActiveDraftPod: mocks.clearActiveDraftPod,
  clearActiveDraftGuest: mocks.clearActiveDraftGuest,
}));
vi.mock("../../services/gamePersistence", () => ({ loadGame: mocks.loadGame }));

// No store is mocked here, and none needs to be: `COMMANDER_DRAFT_ENTRY` and
// `draftKindLabels` come from the leaf module `components/draft/draftKind`, so this
// page value-imports no store at all. Its `lazy()` chunk therefore does not pull
// `multiplayerDraftStore -> draftPodHostAdapter -> p2p-draft-host -> network/
// connection` or the game loop. The slug and the labels asserted below are real.

import { DraftLandingPage } from "../DraftLandingPage";

function podMeta(overrides: Partial<ActiveDraftPodMeta> = {}): ActiveDraftPodMeta {
  return {
    id: "pod-1",
    roomCode: "ABCDE",
    kind: "CommanderDraft",
    podSize: 4,
    hostDisplayName: "Host",
    tournamentFormat: "Swiss",
    podPolicy: "Competitive",
    phase: "lobby",
    pickCount: 0,
    updatedAt: Date.now(),
    ...overrides,
  };
}

function quickDraftMeta(overrides: Partial<ActiveQuickDraftMeta> = {}): ActiveQuickDraftMeta {
  return {
    id: "quick-1",
    setCode: "otj",
    difficulty: 2,
    phase: "drafting",
    pickCount: 3,
    updatedAt: Date.now(),
    ...overrides,
  };
}

function guestPodMeta(overrides: Partial<ActiveDraftGuestMeta> = {}): ActiveDraftGuestMeta {
  return {
    roomCode: "ABCDE",
    displayName: "Guest",
    hostPeerId: "host-peer",
    timestamp: Date.now(),
    ...overrides,
  };
}

function renderPage() {
  return render(
    <MemoryRouter>
      <DraftLandingPage />
    </MemoryRouter>,
  );
}

function setConnectivity({ forcedOffline, browserOnline }: {
  forcedOffline?: boolean;
  browserOnline?: boolean;
}) {
  act(() => useConnectivityStore.setState((state) => ({
    forcedOffline: forcedOffline ?? state.forcedOffline,
    browserOnline: browserOnline ?? state.browserOnline,
  })));
}

describe("DraftLandingPage Commander Draft entry", () => {
  afterEach(() => {
    cleanup();
    useConnectivityStore.setState({ forcedOffline: false, browserOnline: true });
  });

  beforeEach(() => {
    mocks.navigate.mockClear();
    mocks.loadActiveQuickDraft.mockReturnValue(null);
    mocks.loadActiveDraftPod.mockReturnValue(null);
    mocks.loadActiveDraftGuest.mockReturnValue(null);
    mocks.clearActiveQuickDraft.mockClear();
    mocks.clearActiveDraftPod.mockClear();
    mocks.clearActiveDraftGuest.mockClear();
    useConnectivityStore.setState({ forcedOffline: false, browserOnline: true });
  });

  it("deep-links the Commander tile into pod setup", async () => {
    const user = userEvent.setup();
    renderPage();

    // Reach guard: the tile grid rendered, so an absent Commander tile below
    // would be a real absence rather than a failed render.
    expect(screen.getByRole("button", { name: /Pod Draft/ })).toBeInTheDocument();

    await user.click(screen.getByRole("button", { name: /Commander Draft/ }));

    // REVERT-FAILING: at BASE there is no fifth tile at all, and the `?kind=`
    // slug is the contract `DraftPodPage`'s entry effect reads.
    expect(mocks.navigate).toHaveBeenCalledWith("/draft-pod?kind=commander");
  });

  it("leaves the existing Pod Draft route alone", async () => {
    const user = userEvent.setup();
    renderPage();

    await user.click(screen.getByRole("button", { name: /Pod Draft/ }));

    expect(mocks.navigate).toHaveBeenCalledWith("/draft-pod");
  });

  it("disables only new multiplayer draft entry while forced offline", async () => {
    const user = userEvent.setup();
    setConnectivity({ forcedOffline: true });
    renderPage();

    const quick = screen.getByRole("button", { name: /Quick Draft/ });
    const sealed = screen.getByRole("button", { name: /Sealed/ });
    const cube = screen.getByRole("button", { name: /Cube Draft/ });
    expect(quick).toBeEnabled();
    expect(sealed).toBeEnabled();
    expect(cube).toBeEnabled();
    expect(screen.getByRole("button", { name: /Pod Draft/ })).toBeDisabled();
    expect(screen.getByRole("button", { name: /Commander Draft/ })).toBeDisabled();
    expect(screen.getAllByText("Starting a multiplayer draft is unavailable while offline. Reconnect or turn off Offline Mode to continue.")).toHaveLength(2);

    await user.click(quick);
    await user.click(sealed);
    await user.click(cube);

    expect(mocks.navigate.mock.calls).toEqual([
      ["/draft/quick"],
      ["/draft/quick?mode=sealed"],
      ["/draft/quick?mode=cube"],
    ]);
  });

  it("also disables new multiplayer draft entry when the browser reports offline", () => {
    setConnectivity({ browserOnline: false });
    renderPage();

    expect(screen.getByRole("button", { name: /Pod Draft/ })).toBeDisabled();
    expect(screen.getByRole("button", { name: /Commander Draft/ })).toBeDisabled();
    expect(screen.getByRole("button", { name: /Quick Draft/ })).toBeEnabled();
  });

  it("keeps a persisted quick draft resumable while offline", async () => {
    const user = userEvent.setup();
    mocks.loadActiveQuickDraft.mockReturnValue(quickDraftMeta());
    setConnectivity({ forcedOffline: true });
    renderPage();

    const resume = screen.getByRole("button", { name: /Outlaws of Thunder Junction/ });
    expect(resume).toBeEnabled();
    await user.click(resume);

    expect(mocks.navigate).toHaveBeenCalledWith("/draft/quick?resume=1");
    expect(mocks.clearActiveQuickDraft).not.toHaveBeenCalled();
  });

  it("keeps a persisted hosted pod resumable while offline", async () => {
    const user = userEvent.setup();
    mocks.loadActiveDraftPod.mockReturnValue(podMeta());
    setConnectivity({ forcedOffline: true });
    renderPage();

    const resume = screen.getByRole("button", { name: /Commander Pod/ });
    expect(resume).toBeEnabled();
    await user.click(resume);

    expect(mocks.navigate).toHaveBeenCalledWith("/draft-pod?entry=host");
    expect(mocks.clearActiveDraftPod).not.toHaveBeenCalled();
  });

  it("keeps a persisted guest pod reconnectable while offline", async () => {
    const user = userEvent.setup();
    mocks.loadActiveDraftGuest.mockReturnValue(guestPodMeta());
    setConnectivity({ forcedOffline: true });
    renderPage();

    const reconnect = screen.getByRole("button", { name: /Draft Pod.*Reconnect/ });
    expect(reconnect).toBeEnabled();
    await user.click(reconnect);

    expect(mocks.navigate).toHaveBeenCalledWith("/draft-pod?entry=guest");
    expect(mocks.clearActiveDraftGuest).not.toHaveBeenCalled();
  });

  it("labels a resumed Commander pod in prose, not as a raw enum", () => {
    mocks.loadActiveDraftPod.mockReturnValue(podMeta({ kind: "CommanderDraft" }));
    renderPage();

    // REVERT-FAILING: `t("landing.podLabel", { kind: meta.kind })` renders
    // "CommanderDraft Pod" — the raw wire enum — for this exact fixture.
    expect(screen.getByText("Commander Pod")).toBeInTheDocument();
    // Paired negative — non-vacuous because the positive above proves the card
    // rendered and this exact string is what BASE produces.
    expect(screen.queryByText("CommanderDraft Pod")).toBeNull();
  });

  it("still labels the pre-existing kinds from the same map", () => {
    mocks.loadActiveDraftPod.mockReturnValue(podMeta({ kind: "Premier" }));
    renderPage();

    // Sibling guard: the label map did not become Commander-specific.
    expect(screen.getByText("Premier Pod")).toBeInTheDocument();
  });
});
