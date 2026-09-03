import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import { useEffect } from "react";
import { MemoryRouter, Route, Routes, useLocation } from "react-router";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { useConnectivityStore } from "../../stores/connectivityStore";

const mocks = vi.hoisted(() => ({
  audio: vi.fn(),
  warm: vi.fn().mockResolvedValue(undefined),
  lobbyMounted: vi.fn(),
  lobbyUnmounted: vi.fn(),
  clearWsSession: vi.fn(),
  webSocketConstructed: vi.fn(),
  peerConstructed: vi.fn(),
  multiplayerState: {
    startHosting: vi.fn(),
    startP2PHostingSession: vi.fn(),
    showToast: vi.fn(),
    cancelHosting: vi.fn(),
    clearPendingGameRoute: vi.fn(),
    closeBroker: vi.fn(),
    closeSubscriptionSocket: vi.fn(),
    clearAllToasts: vi.fn(),
    serverAddress: "wss://example.test/ws",
    formatConfig: null,
    compatibilityPlayerCount: null,
    resolveGuest: vi.fn(),
    lookupJoinTarget: vi.fn(),
    displayName: "Player",
  },
  draftState: {
    role: null as "host" | "guest" | null,
    phase: "lobby",
    roomCode: "ABCDE",
    joinDraft: vi.fn(),
    leave: vi.fn(),
    reset: vi.fn(),
    seats: [],
    joined: 1,
    total: 2,
    error: null,
  },
  gameState: {
    gameId: null as string | null,
    gameMode: null as string | null,
    gameState: null as { waiting_for?: { type?: string } } | null,
    adapter: null as object | null,
    reset: vi.fn(),
  },
}));

vi.mock("../../audio/useAudioContext", () => ({
  useAudioContext: mocks.audio,
}));

vi.mock("../../components/chrome/DiscordBadge", () => ({ DiscordBadge: () => null }));
vi.mock("../../components/chrome/ScreenChrome", () => ({ ScreenChrome: () => null }));
vi.mock("../../components/chrome/ShellContext", () => ({ useInShell: () => false }));
vi.mock("../../components/lobby/BrokerOfflinePrompt", () => ({ BrokerOfflinePrompt: () => null }));
vi.mock("../../components/lobby/HostSetup", () => ({
  HostSetup: ({ onHost }: { onHost: (settings: { formatConfig: { format: string } }) => Promise<boolean> }) => (
    <button onClick={() => { void onHost({ formatConfig: { format: "FreeForAll" } }); }}>Host setup</button>
  ),
}));
vi.mock("../../components/lobby/JoinErrorDialog", () => ({ JoinErrorDialog: () => null }));
vi.mock("../../components/lobby/LobbyView", () => ({
  LobbyView: () => {
    useEffect(() => {
      mocks.lobbyMounted();
      return () => mocks.lobbyUnmounted();
    }, []);
    return <div data-testid="lobby-view">Lobby</div>;
  },
}));
vi.mock("../../components/lobby/PlayerIdentityBanner", () => ({ PlayerIdentityBanner: () => null }));
vi.mock("../../components/lobby/ServerOfflinePrompt", () => ({ ServerOfflinePrompt: () => null }));
vi.mock("../../components/multiplayer/ConnectionToast", () => ({ ConnectionToast: () => null }));
vi.mock("../../components/menu/MenuParticles", () => ({ MenuParticles: () => null }));
vi.mock("../../components/menu/MyDecks", () => ({ MyDecks: () => <div>Deck select</div> }));
vi.mock("../../services/deckCompatibility", () => ({ evaluateDeckCompatibility: vi.fn() }));
vi.mock("../../services/deckParser", () => ({ expandParsedDeck: vi.fn() }));
vi.mock("../../services/multiplayerSession", () => ({ clearWsSession: mocks.clearWsSession }));
vi.mock("peerjs", () => ({
  default: class {
    constructor(...args: unknown[]) {
      mocks.peerConstructed(...args);
    }
  },
}));
vi.mock("../../stores/cardDataStore", () => ({
  useCardDataStore: { getState: () => ({ warm: mocks.warm }) },
}));
vi.mock("../../stores/gameStore", () => ({
  useGameStore: {
    getState: () => mocks.gameState,
    setState: vi.fn(),
  },
  isAuthorityRemote: (gameMode: string | null) => gameMode === "online",
  saveActiveGame: vi.fn(),
}));
vi.mock("../../stores/multiplayerStore", async (importOriginal) => {
  const actual = await importOriginal<typeof import("../../stores/multiplayerStore")>();
  const useMultiplayerStore = Object.assign(
    <T,>(selector: (state: typeof mocks.multiplayerState) => T) => selector(mocks.multiplayerState),
    { getState: () => mocks.multiplayerState },
  );
  return { ...actual, findLobbyGameByCode: vi.fn(), useMultiplayerStore };
});
vi.mock("../../stores/multiplayerDraftStore", async (importOriginal) => ({
  ...(await importOriginal<typeof import("../../stores/multiplayerDraftStore")>()),
  useMultiplayerDraftStore: Object.assign(
    <T,>(selector: (state: typeof mocks.draftState) => T) => selector(mocks.draftState),
    { getState: () => mocks.draftState },
  ),
}));

import { isMultiplayerGameLive } from "../../pwa/multiplayerGuard";
import { MultiplayerPage } from "../MultiplayerPage";

function LocationProbe() {
  const location = useLocation();
  return <output data-testid="location">{location.pathname}{location.search}</output>;
}

function renderPage(entry = "/multiplayer") {
  return render(
    <MemoryRouter initialEntries={[entry]}>
      <LocationProbe />
      <Routes>
        <Route path="/" element={<div>Home page</div>} />
        <Route path="/multiplayer" element={<MultiplayerPage />} />
      </Routes>
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

function expectNoOfflineTeardown() {
  expect(mocks.clearWsSession).not.toHaveBeenCalled();
  expect(mocks.multiplayerState.cancelHosting).not.toHaveBeenCalled();
  expect(mocks.multiplayerState.clearPendingGameRoute).not.toHaveBeenCalled();
  expect(mocks.multiplayerState.closeBroker).not.toHaveBeenCalled();
  expect(mocks.multiplayerState.closeSubscriptionSocket).not.toHaveBeenCalled();
  expect(mocks.draftState.leave).not.toHaveBeenCalled();
  expect(mocks.draftState.reset).not.toHaveBeenCalled();
  expect(mocks.gameState.reset).not.toHaveBeenCalled();
}

function expectNoTransportConstruction() {
  expect(mocks.webSocketConstructed).not.toHaveBeenCalled();
  expect(mocks.peerConstructed).not.toHaveBeenCalled();
}

describe("MultiplayerPage offline entry", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    localStorage.clear();
    mocks.draftState.phase = "lobby";
    mocks.draftState.role = null;
    mocks.gameState.gameId = null;
    mocks.gameState.gameMode = null;
    mocks.gameState.gameState = null;
    mocks.gameState.adapter = null;
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

  it("blocks a forced-offline cold entry before the lobby or its warmup mounts", () => {
    setConnectivity({ forcedOffline: true });
    renderPage();

    expect(screen.getByText("Multiplayer is unavailable while offline.")).toBeInTheDocument();
    expect(screen.getByText("Reconnect or turn off Offline Mode to host or join a multiplayer game.")).toBeInTheDocument();
    expect(screen.queryByTestId("lobby-view")).not.toBeInTheDocument();
    expect(mocks.lobbyMounted).not.toHaveBeenCalled();
    expect(mocks.warm).not.toHaveBeenCalled();
    expect(mocks.audio).not.toHaveBeenCalled();
    expect(mocks.multiplayerState.startHosting).not.toHaveBeenCalled();
    expectNoOfflineTeardown();
    expectNoTransportConstruction();
  });

  it("also blocks browser-reported offline entry and keeps the local Home action available", () => {
    setConnectivity({ browserOnline: false });
    renderPage();

    expect(screen.getByText("Multiplayer is unavailable while offline.")).toBeInTheDocument();
    expect(mocks.lobbyMounted).not.toHaveBeenCalled();
    expectNoOfflineTeardown();
    expectNoTransportConstruction();

    fireEvent.click(screen.getByRole("button", { name: "Home" }));
    expect(screen.getByTestId("location")).toHaveTextContent("/");
  });

  it("unmounts ordinary lobby content offline and remounts a clean lobby after reconnect", () => {
    renderPage();
    expect(screen.getByTestId("lobby-view")).toBeInTheDocument();
    expect(mocks.lobbyMounted).toHaveBeenCalledTimes(1);

    setConnectivity({ forcedOffline: true });
    expect(screen.getByText("Multiplayer is unavailable while offline.")).toBeInTheDocument();
    expect(mocks.lobbyUnmounted).toHaveBeenCalledTimes(1);
    expectNoOfflineTeardown();

    setConnectivity({ forcedOffline: false });
    expect(screen.getByTestId("lobby-view")).toBeInTheDocument();
    expect(mocks.lobbyMounted).toHaveBeenCalledTimes(2);
  });

  it("drops a transient host-setup view while offline and returns to a clean lobby", () => {
    renderPage("/multiplayer?view=host-setup");
    expect(screen.getByText("Host setup")).toBeInTheDocument();

    setConnectivity({ forcedOffline: true });
    expect(screen.getByText("Multiplayer is unavailable while offline.")).toBeInTheDocument();

    setConnectivity({ forcedOffline: false });
    expect(screen.getByTestId("lobby-view")).toBeInTheDocument();
  });

  it("drops deck-select and its pending host operation while offline before returning to lobby", () => {
    renderPage("/multiplayer?view=host-setup");
    fireEvent.click(screen.getByRole("button", { name: "Host setup" }));
    expect(screen.getByText("Deck select")).toBeInTheDocument();

    setConnectivity({ forcedOffline: true });
    expect(screen.getByText("Multiplayer is unavailable while offline.")).toBeInTheDocument();
    expectNoOfflineTeardown();

    setConnectivity({ forcedOffline: false });
    expect(screen.getByTestId("lobby-view")).toBeInTheDocument();
    expect(mocks.multiplayerState.startHosting).not.toHaveBeenCalled();
    expect(mocks.multiplayerState.startP2PHostingSession).not.toHaveBeenCalled();
  });

  it("keeps a draft-lobby view across offline mode without leaving or rejoining", () => {
    renderPage("/multiplayer?view=draft-lobby");
    expect(screen.getByRole("button", { name: "Leave Draft" })).toBeInTheDocument();

    setConnectivity({ forcedOffline: true });
    expect(screen.getByText("Multiplayer is unavailable while offline.")).toBeInTheDocument();
    expectNoOfflineTeardown();
    expect(mocks.draftState.joinDraft).not.toHaveBeenCalled();

    setConnectivity({ forcedOffline: false });
    expect(screen.getByRole("button", { name: "Leave Draft" })).toBeInTheDocument();
    expect(mocks.draftState.joinDraft).not.toHaveBeenCalled();
  });

  it("does not admit an offline route from a live sibling game", () => {
    mocks.gameState.gameId = "remote-game";
    mocks.gameState.gameMode = "online";
    mocks.gameState.adapter = {};
    expect(isMultiplayerGameLive()).toBe(true);
    setConnectivity({ forcedOffline: true });
    renderPage();

    expect(screen.getByText("Multiplayer is unavailable while offline.")).toBeInTheDocument();
    expect(screen.queryByTestId("lobby-view")).not.toBeInTheDocument();
    expect(mocks.lobbyMounted).not.toHaveBeenCalled();
    expect(mocks.warm).not.toHaveBeenCalled();
    expect(mocks.audio).not.toHaveBeenCalled();
    expectNoOfflineTeardown();
    expectNoTransportConstruction();
  });

  it("does not admit an offline route from a live sibling draft", () => {
    mocks.draftState.role = "guest";
    mocks.draftState.phase = "drafting";
    expect(isMultiplayerGameLive()).toBe(true);
    setConnectivity({ forcedOffline: true });
    renderPage();

    expect(screen.getByText("Multiplayer is unavailable while offline.")).toBeInTheDocument();
    expect(screen.queryByTestId("lobby-view")).not.toBeInTheDocument();
    expect(mocks.lobbyMounted).not.toHaveBeenCalled();
    expect(mocks.warm).not.toHaveBeenCalled();
    expect(mocks.audio).not.toHaveBeenCalled();
    expectNoOfflineTeardown();
    expectNoTransportConstruction();
  });
});
