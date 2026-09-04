// @vitest-environment happy-dom

import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import { MemoryRouter, Route, Routes, useLocation, useNavigate } from "react-router";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { useConnectivityStore } from "../../stores/connectivityStore";
import { useDraftSpectatorStore } from "../../stores/draftSpectatorStore";

const mocks = vi.hoisted(() => ({
  leave: vi.fn(),
  watchDraft: vi.fn(),
}));

vi.mock("../../audio/useAudioContext", () => ({ useAudioContext: vi.fn() }));
vi.mock("../../components/chrome/ScreenChrome", () => ({
  ScreenChrome: ({ onBack }: { onBack?: () => void }) => (
    <button type="button" onClick={onBack}>Chrome back</button>
  ),
}));
vi.mock("../../components/draft/DraftSpectatorDashboard", () => ({
  DraftSpectatorDashboard: () => <div data-testid="spectator-dashboard">Dashboard</div>,
}));
vi.mock("../../components/menu/MenuParticles", () => ({ MenuParticles: () => null }));
vi.mock("../../components/menu/MenuShell", () => ({
  MenuPanel: ({ children }: { children: React.ReactNode }) => <div>{children}</div>,
}));

import { DraftSpectatorPage } from "../DraftSpectatorPage";

function LocationProbe() {
  const location = useLocation();
  return <output data-testid="location">{location.pathname}{location.search}</output>;
}

function RemoveSpectatorCode() {
  const navigate = useNavigate();
  return (
    <button type="button" onClick={() => navigate("/draft-spectator")}>Remove spectator code</button>
  );
}

function renderPage(entry: string) {
  return render(
    <MemoryRouter initialEntries={[entry]}>
      <LocationProbe />
      <Routes>
        <Route path="/multiplayer" element={<div>Lobby</div>} />
        <Route path="/draft-spectator" element={<DraftSpectatorPage />} />
      </Routes>
    </MemoryRouter>,
  );
}

function renderPageWithRouteControl(entry: string) {
  return render(
    <MemoryRouter initialEntries={[entry]}>
      <Routes>
        <Route
          path="/draft-spectator"
          element={(
            <>
              <RemoveSpectatorCode />
              <DraftSpectatorPage />
            </>
          )}
        />
      </Routes>
    </MemoryRouter>,
  );
}

function setSpectatorState({
  draftCode = null,
  session = null,
  view = null,
  status = session ? "connected" : "idle",
  error = null,
}: {
  draftCode?: string | null;
  session?: object | null;
  view?: object | null;
  status?: "idle" | "connecting" | "connected" | "error";
  error?: string | null;
} = {}) {
  useDraftSpectatorStore.setState({
    draftCode,
    session: session as never,
    view: view as never,
    status,
    error,
    watchDraft: mocks.watchDraft,
    leave: mocks.leave,
  });
}

describe("DraftSpectatorPage offline admission", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    mocks.watchDraft.mockResolvedValue(undefined);
    setSpectatorState();
    useConnectivityStore.setState({ forcedOffline: false, browserOnline: true });
  });

  afterEach(() => {
    cleanup();
    useConnectivityStore.setState({ forcedOffline: false, browserOnline: true });
  });

  it.each([
    ["forced offline", { forcedOffline: true, browserOnline: true }],
    ["browser offline", { forcedOffline: false, browserOnline: false }],
  ] as const)("retains an exact normalized spectator session while %s", async (_label, connectivity) => {
    setSpectatorState({ draftCode: "ABC123", session: {}, view: {} });
    useConnectivityStore.setState(connectivity);

    renderPage("/draft-spectator?code=%20abc123%20");

    expect(screen.getByTestId("spectator-dashboard")).toBeInTheDocument();
    expect(mocks.watchDraft).not.toHaveBeenCalled();
    expect(mocks.leave).not.toHaveBeenCalled();
  });

  it.each([
    ["forced offline", { forcedOffline: true, browserOnline: true }],
    ["browser offline", { forcedOffline: false, browserOnline: false }],
  ] as const)("does not leave or close an exact online session when transitioning to %s", async (_label, connectivity) => {
    const session = { close: vi.fn() };
    setSpectatorState({ draftCode: "ABC123", session, view: {} });

    renderPage("/draft-spectator?code=ABC123");

    expect(mocks.leave).not.toHaveBeenCalled();
    expect(session.close).not.toHaveBeenCalled();
    await act(async () => {
      useConnectivityStore.setState(connectivity);
    });

    expect(screen.getByTestId("spectator-dashboard")).toBeInTheDocument();
    expect(mocks.leave).not.toHaveBeenCalled();
    expect(session.close).not.toHaveBeenCalled();
  });

  it("does not re-watch or leave an exact session when reconnecting", async () => {
    setSpectatorState({ draftCode: "ABC123", session: {}, view: {} });
    useConnectivityStore.setState({ forcedOffline: true });
    renderPage("/draft-spectator?code=abc123");

    await act(async () => {
      useConnectivityStore.setState({ forcedOffline: false });
    });

    expect(screen.getByTestId("spectator-dashboard")).toBeInTheDocument();
    expect(mocks.watchDraft).not.toHaveBeenCalled();
    expect(mocks.leave).not.toHaveBeenCalled();
  });

  it("does not replace a watch that synchronously publishes its requested code", async () => {
    mocks.watchDraft.mockImplementation((requestedCode: string) => {
      useDraftSpectatorStore.setState({
        draftCode: requestedCode,
        status: "connecting",
        session: null,
        view: null,
      });
      return new Promise<void>(() => {});
    });

    renderPage("/draft-spectator?code=ABC123");

    await vi.waitFor(() => expect(mocks.watchDraft).toHaveBeenCalledTimes(1));
    // Neither route carries a `server` param, so the origin rides along as
    // `undefined` and the store falls back to the hosting server.
    expect(mocks.watchDraft).toHaveBeenCalledWith("ABC123", undefined);
    expect(mocks.leave).not.toHaveBeenCalled();
  });

  it.each([
    ["mismatched", "/draft-spectator?code=FGHIJK"],
    ["missing", "/draft-spectator"],
  ] as const)("does not expose or close a prior session for a %s offline route", async (_label, entry) => {
    setSpectatorState({ draftCode: "ABC123", session: {}, view: {} });
    useConnectivityStore.setState({ browserOnline: false });

    renderPage(entry);

    expect(screen.queryByTestId("spectator-dashboard")).not.toBeInTheDocument();
    expect(screen.getByText("Watching a multiplayer draft is unavailable while offline. Reconnect or turn off Offline Mode to continue.")).toBeInTheDocument();
    expect(mocks.watchDraft).not.toHaveBeenCalled();
    expect(mocks.leave).not.toHaveBeenCalled();
  });

  it("replaces one valid mismatched route exactly once after reconnecting", async () => {
    setSpectatorState({ draftCode: "ABC123", session: {}, view: {} });
    useConnectivityStore.setState({ forcedOffline: true });
    renderPage("/draft-spectator?code=fghijk");

    await act(async () => {
      useConnectivityStore.setState({ forcedOffline: false });
    });
    expect(mocks.watchDraft).toHaveBeenCalledTimes(1);
    expect(mocks.watchDraft).toHaveBeenCalledWith("FGHIJK", undefined);
  });

  it("never watches a missing route after reconnecting", async () => {
    useConnectivityStore.setState({ forcedOffline: true });
    renderPage("/draft-spectator");

    await act(async () => {
      useConnectivityStore.setState({ forcedOffline: false });
    });

    expect(mocks.watchDraft).not.toHaveBeenCalled();
  });

  it.each([
    ["an active session", { session: {}, view: {} }],
    ["a held watch", { session: null, view: null, status: "connecting" as const }],
  ])("retires %s when an online route loses its code", (_label, state) => {
    setSpectatorState({ draftCode: "ABC123", ...state });
    renderPageWithRouteControl("/draft-spectator?code=ABC123");
    vi.clearAllMocks();

    fireEvent.click(screen.getByRole("button", { name: "Remove spectator code" }));

    expect(mocks.leave).toHaveBeenCalledTimes(1);
    expect(screen.queryByTestId("spectator-dashboard")).not.toBeInTheDocument();
  });

  it("preserves an active session when an offline route loses its code", async () => {
    setSpectatorState({ draftCode: "ABC123", session: {}, view: {} });
    renderPageWithRouteControl("/draft-spectator?code=ABC123");

    await act(async () => {
      useConnectivityStore.setState({ browserOnline: false });
    });
    vi.clearAllMocks();
    fireEvent.click(screen.getByRole("button", { name: "Remove spectator code" }));

    expect(mocks.leave).not.toHaveBeenCalled();
    expect(screen.queryByTestId("spectator-dashboard")).not.toBeInTheDocument();
  });

  it("does not render a prior route's error while offline", () => {
    setSpectatorState({
      draftCode: "ABC123",
      status: "error",
      error: "Prior draft failed",
    });
    useConnectivityStore.setState({ browserOnline: false });

    renderPage("/draft-spectator?code=FGHIJK");

    expect(screen.queryByText("Prior draft failed")).not.toBeInTheDocument();
    expect(screen.getByText("Watching a multiplayer draft is unavailable while offline. Reconnect or turn off Offline Mode to continue.")).toBeInTheDocument();
  });

  it("leaves exactly once when the page actually unmounts while offline", () => {
    setSpectatorState({ draftCode: "ABC123", session: {}, view: {} });
    useConnectivityStore.setState({ forcedOffline: true });
    const page = renderPage("/draft-spectator?code=ABC123");

    page.unmount();

    expect(mocks.leave).toHaveBeenCalledTimes(1);
  });

  it.each([
    ["Chrome back", "Chrome back"],
    ["panel back", "Back to lobby"],
  ] as const)("%s leaves and navigates exactly once", (_label, control) => {
    setSpectatorState({ draftCode: "ABC123", session: {}, view: {} });
    renderPage("/draft-spectator?code=ABC123");

    fireEvent.click(screen.getByRole("button", { name: control }));

    expect(mocks.leave).toHaveBeenCalledTimes(1);
    expect(screen.getByTestId("location")).toHaveTextContent("/multiplayer");
  });
});
