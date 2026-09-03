import { act, cleanup, render, screen, waitFor } from "@testing-library/react";
import { MemoryRouter } from "react-router";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

/**
 * Which server a host action probes, and therefore hosts on.
 *
 * `executeAction` computes ONE `ensureSubscriptionSocket` probe before the
 * P2P/server branch, and the mode it learns from that probe is what decides
 * which leg runs — so the probe target is the whole claim. The two cases below
 * are each other's control: same page, same spy, opposite targets, the only
 * difference being the mode the action carries.
 *
 * Harness shape follows `MultiplayerPage.joinOrigin.test.tsx`: render the real
 * page, stub the child that invokes the callback under test, keep the real
 * store module via `importOriginal`, and mock `useNavigate`.
 */
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
  })),
}));

vi.mock("../../services/multiplayerSession", () => ({
  clearWsSession: vi.fn(),
  loadWsSession: vi.fn(() => null),
  saveWsSession: vi.fn(),
}));

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
    ensureSubscriptionSocket.mockResolvedValue({
      serverInfo: {
        version: "test",
        buildCommit: "test",
        mode: "Full",
        protocolVersion: PROTOCOL_VERSION,
        lobbyProtocolVersion: LOBBY_PROTOCOL_VERSION,
      },
    });
    // zustand actions are plain state fields, so `setState` swaps them.
    useMultiplayerStore.setState({
      hostingServer: URL_A,
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

  // V-U15g, leg (ii) — the regression guard on the P2P leg. `null` is what
  // `HostSetup` actually passes in P2P, and `null ?? hostingServer` IS the live
  // read this line has always made.
  it("probes the anchor when the action chose no server", async () => {
    renderPage();
    await screen.findByTestId("lobby");

    // The production route into P2P host-setup with a NON-NULL anchor: the
    // lobby's own "host a direct-code game" affordance, which flips the mode
    // without touching `hostingServer`.
    act(() => {
      (harness.lobby!.onHostP2P as () => void)();
    });
    await screen.findByTestId("host-setup");

    await submitHostSetup(null);

    await waitFor(() => expect(startP2PHostingSession).toHaveBeenCalled());
    expect(ensureSubscriptionSocket).toHaveBeenCalledWith(URL_A);
    expect(ensureSubscriptionSocket).not.toHaveBeenCalledWith(URL_B);
    expect(startHosting).not.toHaveBeenCalled();
    expect(navigator.sendBeacon).not.toHaveBeenCalled();
    expect(globalThis.fetch).not.toHaveBeenCalled();
  });
});
