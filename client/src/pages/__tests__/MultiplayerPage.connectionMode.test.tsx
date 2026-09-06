import { act, cleanup, render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { MemoryRouter } from "react-router";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

/**
 * The connection mode is an explicit, persisted choice — not a shadow of the
 * hosting server.
 *
 * Three claims live here: a stored choice OUTRANKS the `hostingServer`-derived
 * fallback across a remount; the server-offline prompt flips the same switch
 * the user sees rather than routing around it; and entering server mode with
 * no anchor repairs the anchor instead of leaving the lobby pointed at
 * nothing. Harness shape follows `MultiplayerPage.hostServer.test.tsx`: render
 * the real page, stub the children, keep the real store module.
 */
const harness = vi.hoisted(() => ({
  navigate: vi.fn(),
  /** Live props of the stubbed lobby, so a test reads the mode the page
   * actually handed the switch — not the store field behind it. */
  lobby: null as Record<string, unknown> | null,
}));

/**
 * The page mounts the metrics lifecycle installer. Left unmocked it would
 * register listeners bound to the REAL official host (`vitest.config.ts`
 * defines `__OFFICIAL_MULTIPLAYER_SERVER_URL__` as the production URL).
 */
vi.mock("../../services/serverMetrics", () => ({
  reportConnectOutcome: vi.fn(),
  flushMetricsNow: vi.fn(),
  installServerMetricsLifecycle: vi.fn(),
  metricsUrl: vi.fn(() => "https://metrics.test/servers/metrics"),
}));

vi.mock("react-router", async (importOriginal) => ({
  ...(await importOriginal<typeof import("react-router")>()),
  useNavigate: () => harness.navigate,
}));

vi.mock("../../components/lobby/LobbyView", () => ({
  LobbyView: (props: Record<string, unknown>) => {
    harness.lobby = props;
    return <div data-testid="lobby" />;
  },
}));

vi.mock("../../components/lobby/HostSetup", () => ({ HostSetup: () => null }));
vi.mock("../../components/chrome/ScreenChrome", () => ({ ScreenChrome: () => null }));
vi.mock("../../components/chrome/ShellContext", () => ({ useInShell: () => false }));
vi.mock("../../components/menu/MenuParticles", () => ({ MenuParticles: () => null }));
vi.mock("../../components/menu/MyDecks", () => ({ MyDecks: () => null }));
vi.mock("../../audio/useAudioContext", () => ({ useAudioContext: () => undefined }));

vi.mock("../../stores/cardDataStore", () => ({
  useCardDataStore: { getState: () => ({ warm: vi.fn() }) },
}));

vi.mock("../../stores/multiplayerDraftStore", () => ({
  useMultiplayerDraftStore: (selector: (state: Record<string, unknown>) => unknown) =>
    selector({
      phase: "idle",
      roomCode: null,
      seats: [],
      joined: 0,
      joinDraft: vi.fn(),
      leave: vi.fn(),
    }),
}));

vi.mock("../../stores/gameStore", () => ({
  useGameStore: { setState: vi.fn() },
  saveActiveGame: vi.fn(),
}));

vi.mock("../../constants/storage", () => ({
  ACTIVE_DECK_KEY: "active-deck",
  loadActiveDeck: () => ({ main: ["Island"], sideboard: [] }),
  touchDeckPlayed: vi.fn(),
}));

vi.mock("../../services/multiplayerSession", () => ({
  clearWsSession: vi.fn(),
  loadWsSession: vi.fn(() => null),
  saveWsSession: vi.fn(),
}));

import { MultiplayerPage } from "../MultiplayerPage";
import { useMultiplayerStore } from "../../stores/multiplayerStore";
import { DEFAULT_MULTIPLAYER_SERVER_URL } from "../../config/multiplayerServer";

const ANCHOR_URL = "wss://anchor.example/ws";

function renderPage() {
  return render(
    <MemoryRouter initialEntries={["/multiplayer"]}>
      <MultiplayerPage />
    </MemoryRouter>,
  );
}

describe("MultiplayerPage connection mode", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    harness.lobby = null;
    useMultiplayerStore.setState({
      hostingServer: ANCHOR_URL,
      connectionMode: null,
      userLobbySources: [],
      sourceStatus: new Map(),
      directorySources: [],
      disabledDirectorySources: [],
      toasts: new Map(),
    });
  });

  afterEach(cleanup);

  it("keeps a stored P2P choice across a remount, even with a server anchor", async () => {
    // Reach-guard: with nothing stored, the anchor-derived fallback applies —
    // so the assertion after the choice measures the precedence and not a
    // page that is always in P2P.
    renderPage();
    await screen.findByTestId("lobby");
    expect(harness.lobby!.connectionMode).toBe("server");

    act(() => {
      (harness.lobby!.onConnectionModeChange as (mode: string) => void)("p2p");
    });
    expect(harness.lobby!.connectionMode).toBe("p2p");
    // The anchor is untouched: it is also the P2P broker target, so the switch
    // must never clear it.
    expect(useMultiplayerStore.getState().hostingServer).toBe(ANCHOR_URL);
    // The choice reaches storage. Without this the whole design is inert on a
    // reload, and the in-memory remount below would pass on a value that only
    // survives because the module was never re-evaluated.
    expect(
      JSON.parse(localStorage.getItem("phase-multiplayer") ?? "{}").state
        ?.connectionMode,
    ).toBe("p2p");

    cleanup();
    renderPage();
    await screen.findByTestId("lobby");

    expect(harness.lobby!.connectionMode).toBe("p2p");
  });

  it("repairs a missing anchor when the user picks server mode", async () => {
    // The legacy state: a blob persisted while the picker still offered its
    // "None" row. Nothing else can produce it any more.
    useMultiplayerStore.setState({ hostingServer: null });

    renderPage();
    await screen.findByTestId("lobby");
    expect(harness.lobby!.connectionMode).toBe("p2p");

    act(() => {
      (harness.lobby!.onConnectionModeChange as (mode: string) => void)("server");
    });

    expect(harness.lobby!.connectionMode).toBe("server");
    expect(useMultiplayerStore.getState().hostingServer).toBe(
      DEFAULT_MULTIPLAYER_SERVER_URL,
    );
  });

  it("flips the switch when the offline prompt offers direct codes", async () => {
    const user = userEvent.setup();
    renderPage();
    await screen.findByTestId("lobby");
    expect(harness.lobby!.connectionMode).toBe("server");

    act(() => {
      (harness.lobby!.onServerOffline as () => void)();
    });

    await user.click(screen.getByRole("button", { name: "Use direct code" }));

    // The prompt must not route AROUND the control: after it, the switch the
    // user is looking at reports P2P, and the choice is stored like any other.
    expect(harness.lobby!.connectionMode).toBe("p2p");
    expect(useMultiplayerStore.getState().connectionMode).toBe("p2p");
    // It chooses a mode, not a server.
    expect(useMultiplayerStore.getState().hostingServer).toBe(ANCHOR_URL);
  });
});
