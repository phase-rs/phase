import { act, cleanup, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { MemoryRouter } from "react-router";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

const harness = vi.hoisted(() => ({
  navigate: vi.fn(),
  hostSetup: null as Record<string, unknown> | null,
  lobby: null as Record<string, unknown> | null,
}));

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
vi.mock("../../components/menu/MyDecks", () => ({ MyDecks: () => <div data-testid="deck-select" /> }));
vi.mock("../../audio/useAudioContext", () => ({ useAudioContext: () => undefined }));
vi.mock("../../stores/cardDataStore", () => ({
  useCardDataStore: { getState: () => ({ warm: vi.fn() }) },
}));
vi.mock("../../stores/multiplayerDraftStore", () => ({
  useMultiplayerDraftStore: (selector: (state: Record<string, unknown>) => unknown) =>
    selector({ phase: "idle", roomCode: null, seats: [], joined: 0, joinDraft: vi.fn(), leave: vi.fn() }),
}));
vi.mock("../../stores/gameStore", () => ({
  useGameStore: { setState: vi.fn() },
  saveActiveGame: vi.fn(),
}));
vi.mock("../../services/savedDeckTransaction", () => ({ withSavedDeckLibraryOrSkip: vi.fn() }));
vi.mock("../../services/multiplayerSession", () => ({
  clearWsSession: vi.fn(),
  loadWsSession: vi.fn(() => null),
  saveWsSession: vi.fn(),
}));

const compat = vi.hoisted(() => ({ evaluateDeckCompatibility: vi.fn() }));
vi.mock("../../services/deckCompatibility", () => compat);

vi.mock("../../services/engineRuntime", () => ({
  deckSupplyForFormat: vi.fn(async (format: string) =>
    format === "Dandan" ? "HostPile" : format === "Momir" ? "EngineFixed" : "PlayerBuilt"),
}));

const pile = vi.hoisted(() => ({ pileSeatDeck: vi.fn() }));
vi.mock("../../services/pileSource", async (importOriginal) => {
  const actual = await importOriginal<typeof import("../../services/pileSource")>();
  pile.pileSeatDeck.mockImplementation(actual.pileSeatDeck);
  return { ...actual, pileSeatDeck: pile.pileSeatDeck };
});

import { OFFICIAL_MULTIPLAYER_SERVER_URL } from "../../config/multiplayerServer";
import { ACTIVE_DECK_KEY, STORAGE_KEY_PREFIX } from "../../constants/storage";
import { MultiplayerPage } from "../MultiplayerPage";
import { adHocLobbySource, useMultiplayerStore } from "../../stores/multiplayerStore";
import { LOBBY_PROTOCOL_VERSION, PROTOCOL_VERSION } from "../../adapter/ws-adapter";
import multiplayerEn from "../../i18n/locales/en/multiplayer.json";
import type { PileSource } from "../../services/pileSource";

const URL_A = "wss://anchor.example/ws";
const URL_B = "wss://chosen.example/ws";
const NOT_LOADED = multiplayerEn.page.couldNotLoadDeck;
const PILE = Array<string>(80).fill("Island");
const NAMED: PileSource = { type: "SavedDeck", name: "Pile A" };

const ensureSubscriptionSocket = vi.fn();
const startHosting = vi.fn();
const startP2PHostingSession = vi.fn(async (..._args: unknown[]) => true);
const showToast = vi.fn();
const lookupJoinTarget = vi.fn();
const resolveGuest = vi.fn();

function renderPage(entry: string | { pathname: string; state: unknown } = "/multiplayer?view=host-setup") {
  return render(
    <MemoryRouter initialEntries={[entry]}>
      <MultiplayerPage />
    </MemoryRouter>,
  );
}

async function submitHostSetup(serverUrl: string | null, overrides: Record<string, unknown> = {}) {
  await screen.findByTestId("host-setup");
  await act(async () => {
    await (harness.hostSetup!.onHost as (settings: unknown, serverUrl: string | null) => Promise<boolean>)(
      {
        displayName: "Tester",
        public: true,
        password: "",
        timerSeconds: null,
        formatConfig: { format: "Dandan", max_players: 2 },
        matchType: "Bo1",
        loopDetection: { type: "Off" },
        aiSeats: [],
        startWhenFull: false,
        ranked: false,
        roomName: "Test room",
        ...overrides,
      },
      serverUrl,
    );
  });
}

function brokerMode(mode: "LobbyOnly" | "Full" | null) {
  ensureSubscriptionSocket.mockImplementation(async (url: string) =>
    url === OFFICIAL_MULTIPLAYER_SERVER_URL
      ? (mode === null ? null : { serverInfo: { mode } })
      : { serverInfo: { mode: "Full", protocolVersion: PROTOCOL_VERSION, lobbyProtocolVersion: LOBBY_PROTOCOL_VERSION } },
  );
}

function hostedDecks(): string[][] {
  return [
    ...startHosting.mock.calls.map((call) => (call[1] as { main_deck: string[] }).main_deck),
    ...startP2PHostingSession.mock.calls.map((call) => (call[1] as { main_deck: string[] }).main_deck),
  ];
}

function gameNavigations(): string[] {
  return harness.navigate.mock.calls.map((call) => String(call[0])).filter((to) => to.startsWith("/game/"));
}

function saveDeck(name: string, cards: { name: string; count: number }[]) {
  localStorage.setItem(`${STORAGE_KEY_PREFIX}${name}`, JSON.stringify({ main: cards, sideboard: [] }));
}

function verdict(compatible: boolean, reason?: string) {
  return { selected_format_compatible: compatible, selected_format_reasons: reason ? [reason] : [], color_distribution: [] };
}

beforeEach(() => {
  vi.clearAllMocks();
  harness.hostSetup = null;
  harness.lobby = null;
  localStorage.clear();
  saveDeck("Pile A", [{ name: "Island", count: 80 }]);
  saveDeck("Active", [{ name: "Plains", count: 60 }]);
  vi.stubGlobal("navigator", { sendBeacon: vi.fn(() => true) });
  vi.stubGlobal("fetch", vi.fn(async () => ({ ok: false, status: 599 })));
  compat.evaluateDeckCompatibility.mockResolvedValue(verdict(true));
  brokerMode("LobbyOnly");
  useMultiplayerStore.setState({
    hostingServer: URL_A,
    connectionMode: null,
    userLobbySources: [],
    sourceStatus: new Map(),
    directorySources: [],
    disabledDirectorySources: [],
    displayName: "Tester",
    toasts: new Map(),
    serverInfo: null,
    formatConfig: null,
    ensureSubscriptionSocket,
    startHosting,
    startP2PHostingSession,
    showToast,
    lookupJoinTarget,
    resolveGuest,
  });
});

afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
});

describe("MultiplayerPage — host-supplied pile, hosting", () => {
  it("hosts Dandan's default pile on a server with no active deck", async () => {
    renderPage();
    await submitHostSetup(URL_B);
    await waitFor(() => expect(startHosting).toHaveBeenCalled());
    expect(hostedDecks()).toEqual([[]]);
    expect(showToast).not.toHaveBeenCalled();
    expect(screen.queryByTestId("deck-select")).toBeNull();
  });

  it("hosts Dandan's default pile over P2P with no active deck", async () => {
    useMultiplayerStore.setState({ connectionMode: "p2p" });
    renderPage();
    await submitHostSetup(null);
    await waitFor(() => expect(startP2PHostingSession).toHaveBeenCalled());
    expect(hostedDecks()).toEqual([[]]);
    expect(showToast).not.toHaveBeenCalled();
    expect(screen.queryByTestId("deck-select")).toBeNull();
  });

  it.each(["Dandan", "Momir"])("never submits an illegal active deck for %s", async (format) => {
    localStorage.setItem(ACTIVE_DECK_KEY, "Active");
    compat.evaluateDeckCompatibility.mockResolvedValue(verdict(false, "Not this deck"));
    renderPage();
    await submitHostSetup(URL_B, { formatConfig: { format, max_players: 2 } });
    await waitFor(() => expect(startHosting).toHaveBeenCalled());
    useMultiplayerStore.setState({ connectionMode: "p2p" });
    await submitHostSetup(null, { formatConfig: { format, max_players: 2 } });
    await waitFor(() => expect(startP2PHostingSession).toHaveBeenCalled());
    expect(hostedDecks()).toEqual([[], []]);
    expect(showToast).not.toHaveBeenCalled();
    expect(screen.queryByTestId("deck-select")).toBeNull();
  });

  it("hosts a named pile's expansion", async () => {
    renderPage();
    await submitHostSetup(URL_B, { pile: NAMED });
    await waitFor(() => expect(startHosting).toHaveBeenCalled());
    expect(hostedDecks()).toEqual([PILE]);
    expect(compat.evaluateDeckCompatibility).toHaveBeenCalledWith(
      expect.objectContaining({ main: [{ name: "Island", count: 80 }] }),
      expect.objectContaining({ selectedFormat: "Dandan" }),
    );
  });

  it("refuses a named pile the engine refuses, with its reason", async () => {
    compat.evaluateDeckCompatibility.mockResolvedValue(verdict(false, "Dandân deck must have exactly 80 cards"));
    renderPage();
    await submitHostSetup(URL_B, { pile: NAMED });
    useMultiplayerStore.setState({ connectionMode: "p2p" });
    await submitHostSetup(null, { pile: NAMED });
    expect(showToast).toHaveBeenCalledWith("Dandân deck must have exactly 80 cards");
    expect(startHosting).not.toHaveBeenCalled();
    expect(startP2PHostingSession).not.toHaveBeenCalled();
    expect(screen.queryByTestId("deck-select")).toBeNull();
  });

  it("continues without a lobby with the same pile", async () => {
    const user = userEvent.setup();
    useMultiplayerStore.setState({ connectionMode: "p2p" });
    brokerMode(null);
    renderPage();
    await submitHostSetup(null, { pile: NAMED });
    expect(startP2PHostingSession).not.toHaveBeenCalled();
    await user.click(screen.getByRole("button", { name: "Continue without lobby" }));
    await waitFor(() => expect(startP2PHostingSession).toHaveBeenCalled());
    expect(hostedDecks()).toEqual([PILE]);
  });

  it("starts an all-AI table locally, carrying a named pile and omitting the default", async () => {
    const aiSeats = [{ seatIndex: 1, difficulty: "Medium" }];
    renderPage();
    await submitHostSetup(URL_B, { aiSeats, pile: NAMED });
    await submitHostSetup(URL_B, { aiSeats });
    const [named, standard] = gameNavigations();
    expect(named).toContain("mode=ai");
    expect(named).toContain("pile=Pile%20A");
    expect(standard).toContain("mode=ai");
    expect(standard).not.toContain("pile=");
  });

  it("still sends a player-built host with no active deck to deck-select", async () => {
    renderPage();
    await submitHostSetup(URL_B, { formatConfig: { format: "Standard", max_players: 2 } });
    expect(await screen.findByTestId("deck-select")).toBeInTheDocument();
    expect(screen.queryByTestId("host-setup")).toBeNull();
    expect(showToast).not.toHaveBeenCalled();
    expect(startHosting).not.toHaveBeenCalled();
    expect(startP2PHostingSession).not.toHaveBeenCalled();
  });
});

describe("MultiplayerPage — host-supplied pile, vanished pile", () => {
  it("reports an unreadable pile at the host-deck read and starts nothing", async () => {
    pile.pileSeatDeck.mockResolvedValueOnce(null);
    renderPage();
    await submitHostSetup(URL_B, { pile: NAMED });
    expect(pile.pileSeatDeck).toHaveBeenCalledTimes(1);
    expect(showToast).toHaveBeenCalledWith(NOT_LOADED);
    expect(startHosting).not.toHaveBeenCalled();
  });

  it("reports a pile that vanishes before continuing without a lobby", async () => {
    const user = userEvent.setup();
    useMultiplayerStore.setState({ connectionMode: "p2p" });
    brokerMode(null);
    pile.pileSeatDeck.mockResolvedValueOnce({ ...(await import("../../services/pileSource")).emptySeatDeck(), main_deck: PILE });
    pile.pileSeatDeck.mockResolvedValueOnce(null);
    renderPage();
    await submitHostSetup(null, { pile: NAMED });
    const proceed = screen.getByRole("button", { name: "Continue without lobby" });
    await user.click(proceed);
    await waitFor(() => expect(showToast).toHaveBeenCalledWith(NOT_LOADED));
    expect(pile.pileSeatDeck).toHaveBeenCalledTimes(2);
    expect(startP2PHostingSession).not.toHaveBeenCalled();
  });

  it("reports a named pile that is not saved", async () => {
    renderPage();
    await submitHostSetup(URL_B, { pile: { type: "SavedDeck", name: "Gone" } });
    expect(showToast).toHaveBeenCalledWith(NOT_LOADED);
    expect(startHosting).not.toHaveBeenCalled();
    expect(startP2PHostingSession).not.toHaveBeenCalled();
  });
});

describe("MultiplayerPage — host-supplied pile, joining", () => {
  const origin = () => adHocLobbySource(URL_A);

  async function joinFromLobby(code: string, format?: string) {
    renderPage("/multiplayer");
    await screen.findByTestId("lobby");
    await act(async () => {
      await (harness.lobby!.onJoinGame as (...args: unknown[]) => Promise<void>)(code, origin(), undefined, format);
    });
  }

  function lookupAnswers(format: string, isP2P: boolean) {
    lookupJoinTarget.mockResolvedValue({ ok: true, info: { format_config: { format }, is_p2p: isP2P, draft_metadata: null } });
    resolveGuest.mockResolvedValue({ ok: true, peerInfo: { host_peer_id: "phase2-HOSTPEER" } });
  }

  it("joins a listed Dandan P2P room without a deck, carrying the format", async () => {
    lookupAnswers("Dandan", true);
    await joinFromLobby("GAME01");
    await waitFor(() => expect(gameNavigations()).toHaveLength(1));
    expect(gameNavigations()[0]).toMatch(/mode=p2p-join&code=HOSTPEER&format=Dandan$/);
    expect(screen.queryByTestId("deck-select")).toBeNull();
  });

  it("joins a listed Dandan server room without a deck, carrying the format", async () => {
    lookupAnswers("Dandan", false);
    await joinFromLobby("GAME01");
    await waitFor(() => expect(gameNavigations()).toHaveLength(1));
    const params = new URLSearchParams(gameNavigations()[0].split("?")[1]);
    expect(params.get("mode")).toBe("join");
    expect(params.get("format")).toBe("Dandan");
    expect(screen.queryByTestId("deck-select")).toBeNull();
  });

  it("still asks for a deck before joining a listed Standard room", async () => {
    lookupAnswers("Standard", false);
    await joinFromLobby("GAME01");
    expect(await screen.findByTestId("deck-select")).toBeInTheDocument();
    expect(gameNavigations()).toEqual([]);
  });

  it("dials a raw room code at once when there is no active deck", async () => {
    await joinFromLobby("ABCDE");
    await waitFor(() => expect(gameNavigations()).toHaveLength(1));
    expect(gameNavigations()[0]).toMatch(/mode=p2p-join&code=ABCDE$/);
    expect(screen.queryByTestId("deck-select")).toBeNull();
  });

  it("asks for a deck before a raw room code when an active deck exists", async () => {
    localStorage.setItem(ACTIVE_DECK_KEY, "Active");
    await joinFromLobby("ABCDE");
    expect(await screen.findByTestId("deck-select")).toBeInTheDocument();
    expect(gameNavigations()).toEqual([]);
  });

  it("re-dials a Dandan room that refused a deck, carrying the format", async () => {
    renderPage({
      pathname: "/multiplayer",
      state: { deckRejected: true, reason: "Deck rejected", format: "Dandan", joinCode: "ABCDE", server: URL_A },
    });
    await waitFor(() => expect(gameNavigations()).toHaveLength(1));
    expect(gameNavigations()[0]).toMatch(/mode=p2p-join&code=ABCDE&format=Dandan$/);
    expect(showToast).not.toHaveBeenCalled();
    expect(screen.queryByTestId("deck-select")).toBeNull();
  });

  it("still toasts and asks for a deck when a Standard room refused one", async () => {
    renderPage({
      pathname: "/multiplayer",
      state: { deckRejected: true, reason: "Deck rejected", format: "Standard", joinCode: "ABCDE", server: URL_A },
    });
    expect(await screen.findByTestId("deck-select")).toBeInTheDocument();
    expect(showToast).toHaveBeenCalledWith("Deck rejected");
    expect(gameNavigations()).toEqual([]);
  });
});

describe("MultiplayerPage — host-supplied pile, host-setup chrome", () => {
  async function renderHostSetupFor(format: string) {
    useMultiplayerStore.setState({ formatConfig: { format, min_players: 2 } as never });
    renderPage();
    await screen.findByTestId("host-setup");
    await act(async () => {
      await new Promise((resolve) => setTimeout(resolve, 300));
    });
  }

  it("shows no active-deck controls for Dandan and never blocks hosting on the active deck", async () => {
    localStorage.setItem(ACTIVE_DECK_KEY, "Active");
    compat.evaluateDeckCompatibility.mockResolvedValue(verdict(false, "Not this deck"));
    await renderHostSetupFor("Dandan");
    expect(screen.queryByText(multiplayerEn.page.activeDeck)).toBeNull();
    expect(screen.queryByText(/Not legal in/)).toBeNull();
    expect(harness.hostSetup!.hostDisabled).toBe(false);
  });

  it("shows no pick-deck warning for Dandan without an active deck", async () => {
    await renderHostSetupFor("Dandan");
    expect(screen.queryByText(multiplayerEn.page.noDeckWarning)).toBeNull();
  });

  it("keeps the active-deck banner, chip and warning for Standard", async () => {
    localStorage.setItem(ACTIVE_DECK_KEY, "Active");
    compat.evaluateDeckCompatibility.mockResolvedValue(verdict(false, "Not this deck"));
    await renderHostSetupFor("Standard");
    expect(screen.getByText(multiplayerEn.page.activeDeck)).toBeInTheDocument();
    expect(await screen.findByText(/Not legal in Standard/)).toBeInTheDocument();
    expect(harness.hostSetup!.hostDisabled).toBe(true);
    cleanup();

    localStorage.removeItem(ACTIVE_DECK_KEY);
    await renderHostSetupFor("Standard");
    expect(screen.getByText(multiplayerEn.page.noDeckWarning)).toBeInTheDocument();
  });
});
