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
 * the user sees rather than routing around it; and arriving with no anchor
 * repairs it instead of leaving the lobby pointed at nothing. Harness shape
 * follows `MultiplayerPage.hostServer.test.tsx`: render the real page, stub
 * the children, keep the real store module.
 *
 * The switch is read off HOST SETUP, not the lobby: the transport configures a
 * game being created, so that is the only surface that renders it.
 */
const harness = vi.hoisted(() => ({
  navigate: vi.fn(),
  /** Live props of the stubbed lobby — the route into host setup. */
  lobby: null as Record<string, unknown> | null,
  /** Live props of the stubbed host setup, so a test reads the mode the page
   * actually handed the switch — not the store field behind it. */
  hostSetup: null as Record<string, unknown> | null,
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

vi.mock("../../components/lobby/HostSetup", () => ({
  HostSetup: (props: Record<string, unknown>) => {
    harness.hostSetup = props;
    return <div data-testid="host-setup" />;
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

/** Walk the production route to the switch: lobby → Host Game. */
async function openHostSetup() {
  await screen.findByTestId("lobby");
  act(() => {
    (harness.lobby!.onHostGame as () => void)();
  });
  await screen.findByTestId("host-setup");
}

function switchMode(mode: string) {
  act(() => {
    (harness.hostSetup!.onConnectionModeChange as (m: string) => void)(mode);
  });
}

describe("MultiplayerPage connection mode", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    harness.lobby = null;
    harness.hostSetup = null;
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
    await openHostSetup();
    expect(harness.hostSetup!.connectionMode).toBe("server");

    switchMode("p2p");
    expect(harness.hostSetup!.connectionMode).toBe("p2p");
    // The anchor is untouched: it is also the P2P broker target, and the lobby
    // browses through it in either mode, so the switch must never clear it.
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
    await openHostSetup();

    expect(harness.hostSetup!.connectionMode).toBe("p2p");
  });

  it("repairs a missing anchor on arrival, in either mode", async () => {
    // The legacy state: a blob persisted while the picker still offered its
    // "None" row. Nothing else can produce it any more. The repair is no
    // longer tied to picking server mode — the lobby browses, joins and
    // spectates through the anchor whatever the hosting choice is, so an
    // anchorless page is not a reachable state at all.
    // `connectionMode` is null too: these blobs predate the switch entirely,
    // so "None" is the only record of the transport this player wanted.
    useMultiplayerStore.setState({ hostingServer: null, connectionMode: null });
    expect(useMultiplayerStore.getState().hostingServer).toBeNull();

    renderPage();
    await screen.findByTestId("lobby");

    expect(useMultiplayerStore.getState().hostingServer).toBe(
      DEFAULT_MULTIPLAYER_SERVER_URL,
    );
    // The anchor repair must not read as a transport change: seeding it while
    // the mode derived from it would move a deliberate P2P-only player onto
    // the official server without them touching anything.
    expect(useMultiplayerStore.getState().connectionMode).toBe("p2p");
    await openHostSetup();
    expect(harness.hostSetup!.connectionMode).toBe("p2p");
  });

  it("flips the switch when the offline prompt offers direct codes", async () => {
    const user = userEvent.setup();
    renderPage();
    await screen.findByTestId("lobby");

    act(() => {
      (harness.lobby!.onServerOffline as () => void)();
    });

    await user.click(screen.getByRole("button", { name: "Use direct code" }));

    expect(useMultiplayerStore.getState().connectionMode).toBe("p2p");
    // It chooses a mode, not a server.
    expect(useMultiplayerStore.getState().hostingServer).toBe(ANCHOR_URL);

    // The prompt must not route AROUND the control: the switch the user next
    // sees on Host Game reports the mode the prompt chose for them.
    await openHostSetup();
    expect(harness.hostSetup!.connectionMode).toBe("p2p");
  });

  /**
   * Regression. The lobby is no longer gated by the connection mode, so the two
   * guards that used to make "Use direct code" an escape hatch are gone:
   * `LobbyView` short-circuited its subscription effect in P2P, and this page
   * suppressed `onServerOffline` in P2P. What replaces them is identity
   * stability. `LobbyView` lists this callback in its subscription effect's
   * dependency array, so an inline arrow re-ran that effect on every render of
   * this page — re-dialling sources that were still down and re-opening the
   * modal on the very re-render its own dismissal caused. Neither button could
   * close it.
   */
  it("hands the lobby an offline callback that survives answering the prompt", async () => {
    const user = userEvent.setup();
    renderPage();
    await screen.findByTestId("lobby");
    const first = harness.lobby!.onServerOffline;

    act(() => {
      (harness.lobby!.onServerOffline as () => void)();
    });
    // Opening the prompt is a page state change, so the lobby has re-rendered.
    expect(screen.getByRole("button", { name: "Use direct code" })).toBeInTheDocument();
    expect(harness.lobby!.onServerOffline).toBe(first);

    await user.click(screen.getByRole("button", { name: "Use direct code" }));
    expect(
      screen.queryByRole("button", { name: "Use direct code" }),
    ).not.toBeInTheDocument();
    // Unchanged across the answer's own re-render too — which is precisely what
    // stops `LobbyView`'s effect re-running and re-arming what was just closed.
    expect(harness.lobby!.onServerOffline).toBe(first);
  });
});
