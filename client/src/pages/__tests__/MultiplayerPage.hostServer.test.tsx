import { act, cleanup, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { MemoryRouter } from "react-router";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

/** Render the real page and store, recording the hosting boundary calls. */
const harness = vi.hoisted(() => ({
  navigate: vi.fn(),
  /** The live props of each stubbed child, so the TEST decides when to invoke
   * a callback. Firing from the child's own mount effect is too early: the
   * page reads the active deck in its OWN mount effect, and a child's effect
   * runs first, so a submit from there routes to deck-select instead of
   * executing — which is a fixture artefact, not the behaviour under test. */
  hostSetup: null as Record<string, unknown> | null,
  lobby: null as Record<string, unknown> | null,
}));

/**
 * The page mounts the metrics lifecycle installer. Recording stub: an
 * unmocked module would register real page-lifecycle listeners bound to the
 * REAL official host (`vitest.config.ts` defines
 * `__OFFICIAL_MULTIPLAYER_SERVER_URL__` as the production URL).
 */
const metricsMocks = vi.hoisted(() => ({
  reportConnectOutcome: vi.fn(),
  flushMetricsNow: vi.fn(),
  installServerMetricsLifecycle: vi.fn(),
  metricsUrl: vi.fn(() => "https://metrics.test/servers/metrics"),
}));
vi.mock("../../services/serverMetrics", () => metricsMocks);

vi.mock("react-router", async (importOriginal) => ({
  ...(await importOriginal<typeof import("react-router")>()),
  useNavigate: () => harness.navigate,
}));

vi.mock("../../components/lobby/HostSetup", () => ({
  HostSetup: (props: Record<string, unknown>) => {
    harness.hostSetup = props;
    return <div data-testid="host-setup" />;
  },
}));

vi.mock("../../components/lobby/LobbyView", () => ({
  LobbyView: (props: Record<string, unknown>) => {
    harness.lobby = props;
    return <div data-testid="lobby" />;
  },
}));

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

vi.mock("../../services/deckParser", () => ({
  expandParsedDeck: () => ({ main_deck: ["Island"], sideboard: [], commander: [] }),
}));

vi.mock("../../services/deckCompatibility", () => ({
  evaluateDeckCompatibility: vi.fn(async () => ({
    selected_format_compatible: true,
    selected_format_reasons: [],
    color_distribution: [],
  })),
}));

vi.mock("../../services/multiplayerSession", () => ({
  clearWsSession: vi.fn(),
  loadWsSession: vi.fn(() => null),
  saveWsSession: vi.fn(),
}));

import { OFFICIAL_MULTIPLAYER_SERVER_URL } from "../../config/multiplayerServer";
import { MultiplayerPage } from "../MultiplayerPage";
import { useMultiplayerStore } from "../../stores/multiplayerStore";
import { LOBBY_PROTOCOL_VERSION, PROTOCOL_VERSION } from "../../adapter/ws-adapter";

/** The browsing anchor. */
const URL_A = "wss://anchor.example/ws";
/** The server the host-setup picker chose. */
const URL_B = "wss://chosen.example/ws";

const ensureSubscriptionSocket = vi.fn();
const startHosting = vi.fn();
const startP2PHostingSession = vi.fn(async () => true);

function renderPage(entry = "/multiplayer") {
  return render(
    <MemoryRouter initialEntries={[entry]}>
      <MultiplayerPage />
    </MemoryRouter>,
  );
}

/** Submit host-setup exactly as the real form does: the settings payload plus
 * the server this submit chose (`null` in P2P — see `HostSetup`'s prop doc). */
async function submitHostSetup(serverUrl: string | null): Promise<void> {
  await act(async () => {
    await (harness.hostSetup!.onHost as (
      settings: unknown,
      serverUrl: string | null,
    ) => Promise<boolean>)(
      {
        displayName: "Tester",
        public: true,
        password: "",
        timerSeconds: null,
        // Two seats, no AI seat: the all-AI local-game shortcut ahead of the
        // probe must not fire, or the row would never reach the seam.
        formatConfig: { format: "Commander", max_players: 2 },
        matchType: "Bo1",
        loopDetection: { type: "Off" },
        aiSeats: [],
        startWhenFull: false,
        ranked: false,
        roomName: "Test room",
      },
      serverUrl,
    );
  });
}

describe("MultiplayerPage host server", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    harness.hostSetup = null;
    harness.lobby = null;
    localStorage.setItem("active-deck", "Test Deck");
    // Egress guards — defence-in-depth; the real mitigation is the module
    // mock of `serverMetrics` above.
    vi.stubGlobal("navigator", { sendBeacon: vi.fn(() => true) });
    vi.stubGlobal("fetch", vi.fn(async () => ({ ok: false, status: 599 })));
    ensureSubscriptionSocket.mockImplementation(async (url: string) => ({
      serverInfo: {
        version: "test",
        buildCommit: "test",
        mode: url === OFFICIAL_MULTIPLAYER_SERVER_URL ? "LobbyOnly" : "Full",
        protocolVersion: PROTOCOL_VERSION,
        lobbyProtocolVersion: LOBBY_PROTOCOL_VERSION,
      },
    }));
    // zustand actions are plain state fields, so `setState` swaps them.
    useMultiplayerStore.setState({
      hostingServer: URL_A,
      // Reset per case: the mode is persisted store state now, so leg (ii)'s
      // flip to P2P would otherwise leak into whatever runs after it.
      connectionMode: null,
      userLobbySources: [],
      sourceStatus: new Map(),
      directorySources: [],
      disabledDirectorySources: [],
      displayName: "Tester",
      toasts: new Map(),
      serverInfo: null,
      ensureSubscriptionSocket,
      startHosting,
      startP2PHostingSession,
    });
  });

  afterEach(() => {
    cleanup();
    vi.unstubAllGlobals();
  });

  // V-U15g, leg (i)
  it("probes and hosts on the chosen server, never on the anchor", async () => {
    renderPage("/multiplayer?view=host-setup");
    await screen.findByTestId("host-setup");

    await submitHostSetup(URL_B);

    await waitFor(() => expect(startHosting).toHaveBeenCalled());
    expect(ensureSubscriptionSocket).toHaveBeenCalledWith(URL_B);
    expect(ensureSubscriptionSocket).not.toHaveBeenCalledWith(URL_A);
    // The same URL reaches the store: the probe target and the dial target are
    // one value, not two reads that could disagree.
    expect(startHosting).toHaveBeenCalledWith(expect.anything(), expect.anything(), URL_B);
    expect(navigator.sendBeacon).not.toHaveBeenCalled();
    expect(globalThis.fetch).not.toHaveBeenCalled();
  });

  it("registers P2P with the official broker when the anchor is a dedicated server", async () => {
    renderPage();
    await screen.findByTestId("lobby");

    // The production route into P2P host-setup with a NON-NULL anchor: open
    // Host Game from the lobby, then choose P2P on the screen that owns that
    // choice. Neither step touches `hostingServer`.
    act(() => {
      (harness.lobby!.onHostGame as () => void)();
    });
    await screen.findByTestId("host-setup");
    act(() => {
      (harness.hostSetup!.onConnectionModeChange as (m: string) => void)("p2p");
    });

    await submitHostSetup(null);

    await waitFor(() => expect(startP2PHostingSession).toHaveBeenCalled());
    expect(ensureSubscriptionSocket).toHaveBeenCalledWith(URL_A);
    expect(ensureSubscriptionSocket).toHaveBeenCalledWith(OFFICIAL_MULTIPLAYER_SERVER_URL);
    expect(startP2PHostingSession).toHaveBeenCalledWith(
      expect.objectContaining({ public: true }), expect.anything(),
      { brokerUrl: OFFICIAL_MULTIPLAYER_SERVER_URL, roomName: "Test room" },
    );
    expect(useMultiplayerStore.getState().hostingServer).toBe(URL_A);
    expect(startHosting).not.toHaveBeenCalled();
    expect(navigator.sendBeacon).not.toHaveBeenCalled();
    expect(globalThis.fetch).not.toHaveBeenCalled();
  });
  it("uses a known dedicated anchor's connected official broker without probing the game server", async () => {
    useMultiplayerStore.setState({
      connectionMode: "p2p",
      sourceStatus: new Map([[URL_A, {
        state: "open", playerCount: 0,
        serverInfo: {
          version: "test", buildCommit: "test", mode: "Full",
          protocolVersion: PROTOCOL_VERSION, lobbyProtocolVersion: LOBBY_PROTOCOL_VERSION,
        },
      }]]),
    });
    renderPage("/multiplayer?view=host-setup");
    await submitHostSetup(null);
    expect(ensureSubscriptionSocket).toHaveBeenCalledExactlyOnceWith(OFFICIAL_MULTIPLAYER_SERVER_URL);
    expect(startP2PHostingSession).toHaveBeenCalledWith(
      expect.objectContaining({ public: true }), expect.anything(),
      { brokerUrl: OFFICIAL_MULTIPLAYER_SERVER_URL, roomName: "Test room" },
    );
  });

  it("preserves a custom broker and passes its probed URL despite an anchor change", async () => {
    useMultiplayerStore.setState({ connectionMode: "p2p" });
    ensureSubscriptionSocket.mockImplementation(async () => {
      useMultiplayerStore.setState({ hostingServer: URL_B });
      return { serverInfo: { mode: "LobbyOnly" } };
    });
    renderPage("/multiplayer?view=host-setup");
    await submitHostSetup(null);
    expect(ensureSubscriptionSocket).toHaveBeenCalledExactlyOnceWith(URL_A);
    expect(startP2PHostingSession).toHaveBeenCalledWith(
      expect.anything(), expect.anything(), { brokerUrl: URL_A, roomName: "Test room" },
    );
  });

  it("requires explicit unlisted hosting when the official broker is unavailable", async () => {
    const user = userEvent.setup();
    useMultiplayerStore.setState({ connectionMode: "p2p" });
    ensureSubscriptionSocket.mockImplementation(async (url: string) => (
      url === URL_A ? { serverInfo: { mode: "Full" } } : null
    ));
    renderPage("/multiplayer?view=host-setup");
    await submitHostSetup(null);
    expect(screen.getByText(OFFICIAL_MULTIPLAYER_SERVER_URL)).toBeInTheDocument();
    expect(startP2PHostingSession).not.toHaveBeenCalled();
    await user.click(screen.getByRole("button", { name: "Continue without lobby" }));
    expect(startP2PHostingSession).toHaveBeenCalledWith(
      expect.anything(), expect.anything(), { brokerUrl: null, roomName: "Test room" },
    );
  });

  it("does not replace an unavailable custom broker using unrelated server metadata", async () => {
    useMultiplayerStore.setState({
      connectionMode: "p2p",
      serverInfo: {
        version: "test", buildCommit: "test", mode: "Full",
        protocolVersion: PROTOCOL_VERSION, lobbyProtocolVersion: LOBBY_PROTOCOL_VERSION,
      },
    });
    ensureSubscriptionSocket.mockResolvedValue(null);
    renderPage("/multiplayer?view=host-setup");
    await submitHostSetup(null);
    expect(screen.getByText(URL_A)).toBeInTheDocument();
    expect(ensureSubscriptionSocket).toHaveBeenCalledExactlyOnceWith(URL_A);
    expect(startP2PHostingSession).not.toHaveBeenCalled();
  });

  it("does not silently turn a dedicated hosting choice into P2P", async () => {
    const showToast = vi.fn();
    useMultiplayerStore.setState({ showToast });
    renderPage("/multiplayer?view=host-setup");
    await submitHostSetup(OFFICIAL_MULTIPLAYER_SERVER_URL);
    expect(startHosting).not.toHaveBeenCalled();
    expect(startP2PHostingSession).not.toHaveBeenCalled();
    expect(showToast).toHaveBeenCalledWith("Couldn't connect to the dedicated multiplayer server.");
  });

});
