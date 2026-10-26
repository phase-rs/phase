import { act, cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { createMemoryRouter, MemoryRouter, RouterProvider } from "react-router";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type * as WasmAdapter from "../../adapter/wasm-adapter";
import type * as Scryfall from "../../services/scryfall";
import { detectAndParseDeck } from "../../services/deckParser";
import type * as PhaseSocket from "../../services/openPhaseSocket";
import type * as Connection from "../../network/connection";
import type * as Broker from "../../services/brokerClient";
import { P2PHostAdapter } from "../../adapter/p2p-adapter";
import { submitParsedDeck } from "../../services/deckSubmission";
import type { ParsedDeck } from "../../services/deckParser";

import type { ComponentProps } from "react";
import type * as HostSetupModule from "../../components/lobby/HostSetup";
import type * as AiDeckCatalog from "../../services/aiDeckCatalog";
import type * as EngineRuntime from "../../services/engineRuntime";
import type * as Storage from "../../constants/storage";
import { STORAGE_KEY_PREFIX } from "../../constants/storage";
import type { AiDeckCandidate } from "../../services/aiDeckCatalog";
import { hostLinkSearch } from "../multiplayerPageState";
import type * as ServiceWorker from "../../pwa/registerServiceWorker";
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
  deckText: "Deck\n1 Island",
  deckExtras: {} as Partial<ParsedDeck>,
  chrome: null as Record<string, unknown> | null,
  realHostSetup: false,
  candidates: [] as AiDeckCandidate[],
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

const build = vi.hoisted(() => ({ checkDeployedBuild: vi.fn() }));
vi.mock("../../pwa/registerServiceWorker", async (importOriginal) => ({
  ...await importOriginal<typeof ServiceWorker>(),
  checkDeployedBuild: build.checkDeployedBuild,
}));

vi.mock("react-router", async (importOriginal) => ({
  ...(await importOriginal<typeof import("react-router")>()),
  useNavigate: () => harness.navigate,
}));

vi.mock("../../components/lobby/HostSetup", async (importOriginal) => {
  const actual = await importOriginal<typeof HostSetupModule>();
  return {
    HostSetup: (props: ComponentProps<typeof HostSetupModule.HostSetup>) => {
      harness.hostSetup = props as unknown as Record<string, unknown>;
      return harness.realHostSetup ? <actual.HostSetup {...props} /> : <div data-testid="host-setup" />;
    },
  };
});

vi.mock("../../services/engineRuntime", async (importOriginal) => ({
  ...await importOriginal<typeof EngineRuntime>(),
  bestOfThreeCeilingForFormat: vi.fn(async () => "Bo1"),
  deckSupplyForFormat: vi.fn(async (format: string) => format === "Dandan" ? "HostPile" : "PlayerBuilt"),
}));
vi.mock("../../services/aiDeckCatalog", async (importOriginal) => ({
  ...await importOriginal<typeof AiDeckCatalog>(),
  useAiDeckCatalog: () => ({ candidates: harness.candidates, allCandidates: harness.candidates, loading: false, error: null }),
}));

vi.mock("../../components/lobby/LobbyView", () => ({
  LobbyView: (props: Record<string, unknown>) => {
    harness.lobby = props;
    return <div data-testid="lobby" />;
  },
}));

vi.mock("../../components/chrome/ScreenChrome", () => ({
  ScreenChrome: (props: Record<string, unknown>) => { harness.chrome = props; return null; },
}));
vi.mock("../../components/chrome/ShellContext", () => ({ useInShell: () => false }));
vi.mock("../../components/menu/MenuParticles", () => ({ MenuParticles: () => null }));
vi.mock("../../components/menu/MyDecks", () => ({
  MyDecks: ({ onSelectDeck }: { onSelectDeck: (name: string) => void }) => (
    <button onClick={() => {
      harness.deckText = "Deck\n3 Forest (BBB) 002";
      onSelectDeck("New Deck");
    }}>Select New Deck</button>
  ),
}));
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

vi.mock("../../constants/storage", async (importOriginal) => ({
  ...await importOriginal<typeof Storage>(),
  ACTIVE_DECK_KEY: "active-deck",
  loadActiveDeck: () => ({ ...detectAndParseDeck(harness.deckText), ...harness.deckExtras }),
  loadActiveDeckBracket: () => null,
  touchDeckPlayed: vi.fn(),
}));

const art = vi.hoisted(() => ({ face: vi.fn(), printings: vi.fn(), bounds: vi.fn() }));
vi.mock("../../adapter/wasm-adapter", async (importOriginal) => ({
  ...await importOriginal<typeof WasmAdapter>(),
  getSharedAdapter: () => ({ getCardFaceData: art.face, checkDeckPrintingsBounds: art.bounds }),
}));
vi.mock("../../services/scryfall", async (importOriginal) => ({
  ...await importOriginal<typeof Scryfall>(),
  getCardPrintings: art.printings,
}));

const transport = vi.hoisted(() => ({ send: vi.fn(), apply: vi.fn(), register: vi.fn() }));
vi.mock("../../services/openPhaseSocket", async (importOriginal) => ({
  ...await importOriginal<typeof PhaseSocket>(),
  openPhaseSocket: vi.fn(async () => ({
    serverInfo: { mode: "Full", protocolVersion: 14 },
    ws: { readyState: 1, send: transport.send, close: vi.fn(), addEventListener: vi.fn(), removeEventListener: vi.fn() },
  })),
}));
vi.mock("../../network/connection", async (importOriginal) => ({
  ...await importOriginal<typeof Connection>(),
  hostRoom: vi.fn(async () => ({
    peer: { id: "test-peer", destroy: vi.fn() }, destroy: vi.fn(), roomCode: "ABCDE", onGuestConnected: vi.fn(),
  })),
}));
vi.mock("../../services/brokerClient", async (importOriginal) => ({
  ...await importOriginal<typeof Broker>(),
  openBrokerClient: vi.fn(async () => ({
    registerHost: transport.register, unregister: vi.fn(async () => undefined), close: vi.fn(), updateMetadata: vi.fn(),
  })),
}));
vi.mock("../../adapter/p2p-adapter", () => ({
  P2PHostAdapter: vi.fn().mockImplementation(function () {
    return {
      onEvent: vi.fn(), initialize: vi.fn(async () => undefined), applySeatMutation: transport.apply,
      getPlayerSlots: () => [], dispose: vi.fn(),
    };
  }),
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
import { FORMAT_DEFAULTS, useMultiplayerStore } from "../../stores/multiplayerStore";
import { LOBBY_PROTOCOL_VERSION, PROTOCOL_VERSION } from "../../adapter/ws-adapter";
import multiplayerEn from "../../i18n/locales/en/multiplayer.json";
import { usePreferencesStore } from "../../stores/preferencesStore";
const realHosting = {
  startHosting: useMultiplayerStore.getState().startHosting,
  startP2PHostingSession: useMultiplayerStore.getState().startP2PHostingSession,
};

/** The browsing anchor. */
const URL_A = "wss://anchor.example/ws";
/** The server the host-setup picker chose. */
const URL_B = "wss://chosen.example/ws";

const ensureSubscriptionSocket = vi.fn();
const startHosting = vi.fn();
const startP2PHostingSession = vi.fn<typeof realHosting.startP2PHostingSession>(async () => true);

function renderPage(entry = "/multiplayer") {
  return render(
    <MemoryRouter initialEntries={[entry]}>
      <MultiplayerPage />
    </MemoryRouter>,
  );
}

// The client's ES2021 lib has no Promise.withResolvers.
function heldMetadata() {
  let resolve!: (face: { scryfall_oracle_id: string }) => void;
  const promise = new Promise<{ scryfall_oracle_id: string }>((done) => { resolve = done; });
  return { promise, resolve };
}

/** Submit host-setup exactly as the real form does: the settings payload plus
 * the server this submit chose (`null` in P2P — see `HostSetup`'s prop doc). */
async function submitHostSetup(
  serverUrl: string | null,
  overrides: Record<string, unknown> = {},
  heldPreparation = false,
): Promise<void> {
  const submit = async () => {
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
        ...overrides,
      },
      serverUrl,
    );
  };
  if (heldPreparation) await submit();
  else await act(submit);
}

function prepareRealSetup(mode: "server" | "p2p" = "server") {
  harness.realHostSetup = true;
  harness.deckText = "Deck\n2 Island (AAA) 001";
  harness.candidates = [{
    id: "saved:AI", name: "AI Deck", source: { type: "saved" },
    deck: detectAndParseDeck("1 Forest (BBB) 002"), coveragePct: 100, archetype: null, bracket: null,
  }];
  useMultiplayerStore.setState({
    connectionMode: mode, hostingServer: URL_B,
    formatConfig: FORMAT_DEFAULTS.FreeForAll,
    lastHostConfig: {
      format: "FreeForAll", formatConfig: FORMAT_DEFAULTS.FreeForAll, savedCustomFormatId: null,
      playerCount: 3, matchType: "Bo1", loopDetection: { type: "Off" },
      isPublic: true, startWhenFull: false, ranked: false, aiSeats: [],
    },
    userLobbySources: [{ url: URL_B, name: "Test", origin: "user", kind: "Full" }],
    sourceStatus: new Map([[URL_B, { state: "open", playerCount: 0, serverInfo: {
      version: "test", buildCommit: "test", mode: "Full",
      protocolVersion: PROTOCOL_VERSION, lobbyProtocolVersion: LOBBY_PROTOCOL_VERSION,
    } }]]),
  });
}

function deferred<T>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((done) => { resolve = done; });
  return { promise, resolve };
}

function changeDeckButton() {
  return within(screen.getByRole("button", { name: "Edit" }).parentElement!)
    .getByRole("button", { name: "Change" });
}

describe("MultiplayerPage host server", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    sessionStorage.removeItem("phase:bot-link");
    build.checkDeployedBuild.mockReset().mockResolvedValue("current");
    harness.deckText = "Deck\n1 Island";
    harness.deckExtras = {};
    harness.realHostSetup = false;
    harness.candidates = [];
    usePreferencesStore.setState({ artChain: [], artOverrides: {} });
    art.face.mockResolvedValue({ scryfall_oracle_id: "island" });
    art.printings.mockResolvedValue([
      { id: "a", set: "aaa", collector_number: "001", released_at: "2000-01-01", frame_effects: [], faces: [] },
      { id: "b", set: "bbb", collector_number: "002", released_at: "2001-01-01", frame_effects: [], faces: [] },
    ]);
    art.bounds.mockResolvedValue(undefined);
    transport.apply.mockResolvedValue(undefined);
    transport.register.mockResolvedValue({ gameCode: "ABCDE", playerToken: "token" });
    useMultiplayerStore.setState({ hostingStatus: "idle", hostGameCode: null, hostSession: null, playerSlots: [] });
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
    useMultiplayerStore.getState().cancelHosting();
    vi.restoreAllMocks();
    vi.unstubAllGlobals();
  });

  describe("cross-component cancellation", () => {
    it.each(["not-found", "invalid"] as const)(
      "recovers the same host form after a %s join link cancels held AI preparation",
      async (arrival) => {
        prepareRealSetup();
        const lookupJoinTarget = vi.fn().mockResolvedValue({ ok: false, reason: "not_found", message: "Missing" });
        useMultiplayerStore.setState({ lookupJoinTarget });
        const router = createMemoryRouter([{ path: "/multiplayer", element: <MultiplayerPage /> }], {
          initialEntries: ["/multiplayer?view=host-setup"],
        });
        render(<RouterProvider router={router} />);
        const user = userEvent.setup();
        const host = screen.getByRole("button", { name: "Host Game" });
        const form = host.closest("form");
        await waitFor(() => expect(host).toBeEnabled());
        await user.click(screen.getAllByRole("button", { name: "Human" })[0]);
        const remembered = useMultiplayerStore.getState().lastHostConfig;
        const old = heldMetadata();
        art.face.mockReturnValueOnce(old.promise);
        await user.click(host);
        await waitFor(() => expect(art.face).toHaveBeenCalledWith("Forest"));
        await act(async () => {
          await router.navigate(arrival === "not-found"
            ? `/multiplayer?join=AB12CD@${URL_B}`
            : "/multiplayer?join=invalid");
        });
        if (arrival === "not-found") {
          await screen.findByText(multiplayerEn.page.waitingForHostTitle);
          expect(lookupJoinTarget).toHaveBeenCalledOnce();
        } else {
          expect(useMultiplayerStore.getState().toasts.get("generic")?.message).toBe(multiplayerEn.page.invalidGameLink);
        }
        const navigated = harness.navigate.mock.calls.length; // The link's URL strip only.
        await act(async () => { old.resolve({ scryfall_oracle_id: "forest" }); });
        expect(startHosting).not.toHaveBeenCalled();
        expect(startP2PHostingSession).not.toHaveBeenCalled();
        expect(harness.navigate).toHaveBeenCalledTimes(navigated);
        expect(useMultiplayerStore.getState().lastHostConfig).toBe(remembered);
        if (arrival === "not-found") await user.click(screen.getByRole("button", { name: "Dismiss" }));
        expect(host.closest("form")).toBe(form);
        expect(screen.getByRole("button", { name: "Host Game" })).toBe(host);
        expect(host).toBeEnabled();
        await user.click(host);
        await waitFor(() => expect(startHosting).toHaveBeenCalledOnce());
        expect(startHosting.mock.calls[0][0].aiSeats[0].deck).toMatchObject({
          type: "DeckList", data: { main_deck: ["Forest"], printings: [
            { section: "main_deck", name: "Forest", count: 1,
              printing: { set_code: "bbb", collector_number: "002" } },
          ] },
        });
        expect(harness.navigate).toHaveBeenCalledTimes(navigated + 1);
        expect(harness.navigate).toHaveBeenLastCalledWith("/");
      },
    );

    describe("pile-source cancellation", () => {
      beforeEach(() => {
        prepareRealSetup();
        useMultiplayerStore.setState({
          formatConfig: FORMAT_DEFAULTS.Dandan,
          lastHostConfig: {
            ...useMultiplayerStore.getState().lastHostConfig!,
            format: "Dandan", formatConfig: FORMAT_DEFAULTS.Dandan, playerCount: 2,
          },
        });
        localStorage.setItem(`${STORAGE_KEY_PREFIX}Pile A`, JSON.stringify(detectAndParseDeck("2 Island (AAA) 001")));
        localStorage.setItem(`${STORAGE_KEY_PREFIX}Pile B`, JSON.stringify(detectAndParseDeck("3 Forest (BBB) 002")));
      });
      afterEach(() => {
        localStorage.removeItem(`${STORAGE_KEY_PREFIX}Pile A`);
        localStorage.removeItem(`${STORAGE_KEY_PREFIX}Pile B`);
      });

      it.each(["", "Pile B"])("abandons held pile A for %s and hosts the new source from the same form", async (next) => {
        const user = userEvent.setup();
        renderPage("/multiplayer?view=host-setup");
        const picker = await screen.findByRole("combobox", { name: "Pile" });
        await user.selectOptions(picker, "Pile A");
        const host = screen.getByRole("button", { name: "Host Game" });
        const form = host.closest("form");
        await waitFor(() => expect(host).toBeEnabled());
        const old = heldMetadata();
        art.face.mockReturnValueOnce(old.promise);
        await user.click(host);
        await waitFor(() => expect(art.face).toHaveBeenCalledWith("Island"));
        expect(host).toBeDisabled();
        await user.selectOptions(picker, next);
        // Release at selection time, not only after the obsolete promise settles.
        await waitFor(() => expect(host).toBeEnabled());
        await act(async () => { old.resolve({ scryfall_oracle_id: "island" }); });
        expect(startHosting).not.toHaveBeenCalled();
        expect(startP2PHostingSession).not.toHaveBeenCalled();
        expect(harness.navigate).not.toHaveBeenCalled();
        expect(host.closest("form")).toBe(form);
        expect(picker).toHaveValue(next);
        await user.click(host);
        await waitFor(() => expect(startHosting).toHaveBeenCalledOnce());
        expect(startHosting.mock.calls[0][0].pile).toEqual(next
          ? { type: "SavedDeck", name: next } : { type: "Default" });
        expect(startHosting.mock.calls[0][1].main_deck).toEqual(next ? ["Forest", "Forest", "Forest"] : []);
        expect(startHosting.mock.calls[0][1].printings).toEqual(next ? [
          { section: "main_deck", name: "Forest", count: 3, printing: { set_code: "bbb", collector_number: "002" } },
        ] : undefined);
        expect(harness.navigate).toHaveBeenCalledExactlyOnceWith("/");
      });

      it("keeps a newer pile submission busy when the cancelled predecessor settles", async () => {
        const user = userEvent.setup();
        renderPage("/multiplayer?view=host-setup");
        const picker = await screen.findByRole("combobox", { name: "Pile" });
        await user.selectOptions(picker, "Pile A");
        const host = screen.getByRole("button", { name: "Host Game" });
        await waitFor(() => expect(host).toBeEnabled());
        const old = heldMetadata();
        art.face.mockReturnValueOnce(old.promise);
        await user.click(host);
        await waitFor(() => expect(art.face).toHaveBeenCalledWith("Island"));
        await user.selectOptions(picker, "Pile B");
        await waitFor(() => expect(host).toBeEnabled());
        const successor = heldMetadata();
        art.face.mockReturnValueOnce(successor.promise);
        await user.click(host);
        await waitFor(() => expect(art.face).toHaveBeenCalledWith("Forest"));
        await act(async () => { old.resolve({ scryfall_oracle_id: "island" }); });
        expect(startHosting).not.toHaveBeenCalled();
        expect(harness.navigate).not.toHaveBeenCalled();
        expect(host).toHaveTextContent("Opening…");
        expect(host).toBeDisabled();
        await user.click(host);
        expect(art.face).toHaveBeenCalledTimes(2);
        await act(async () => { successor.resolve({ scryfall_oracle_id: "forest" }); });
        await waitFor(() => expect(startHosting).toHaveBeenCalledOnce());
        expect(startHosting.mock.calls[0][1]).toMatchObject({
          main_deck: ["Forest", "Forest", "Forest"],
          printings: [{ section: "main_deck", name: "Forest", count: 3,
            printing: { set_code: "bbb", collector_number: "002" } }],
        });
        expect(harness.navigate).toHaveBeenCalledExactlyOnceWith("/");
      });

      it("preserves a live submission when the picker reports the same source again", async () => {
        const user = userEvent.setup();
        renderPage("/multiplayer?view=host-setup");
        const picker = await screen.findByRole("combobox", { name: "Pile" });
        await user.selectOptions(picker, "Pile A");
        const host = screen.getByRole("button", { name: "Host Game" });
        await waitFor(() => expect(host).toBeEnabled());
        const held = heldMetadata();
        art.face.mockReturnValueOnce(held.promise);
        await user.click(host);
        await waitFor(() => expect(art.face).toHaveBeenCalledWith("Island"));
        // A same-source notification is a new object with changed legality,
        // not a new pile choice. Exercise the real picker callback while held.
        fireEvent.change(picker, { target: { value: "Pile A" } });
        expect(host).toHaveTextContent("Opening…");
        expect(host).toBeDisabled();
        await act(async () => { held.resolve({ scryfall_oracle_id: "island" }); });
        await waitFor(() => expect(startHosting).toHaveBeenCalledOnce());
        expect(startHosting.mock.calls[0][1]).toMatchObject({
          main_deck: ["Island", "Island"],
          printings: [{ section: "main_deck", name: "Island", count: 2,
            printing: { set_code: "aaa", collector_number: "001" } }],
        });
        expect(harness.navigate).toHaveBeenCalledExactlyOnceWith("/");
      });
    });

    it.each(["server", "p2p", "offline-prompt", "offline-continue"] as const)(
      "cancels delegated human preparation on Change (%s)",
      async (route) => {
        for (const selectBeforeRelease of [false, true]) {
          prepareRealSetup(route === "server" ? "server" : "p2p");
          const user = userEvent.setup();
          const submitName = route === "server" ? "Host Game" : "Host P2P Game";
          if (route.startsWith("offline")) ensureSubscriptionSocket.mockResolvedValue(null);
          const mounted = renderPage("/multiplayer?view=host-setup");
          await waitFor(() => expect(screen.getByRole("button", { name: submitName })).toBeEnabled());
          if (route === "offline-continue") {
            await user.click(screen.getByRole("button", { name: submitName }));
            await screen.findByRole("button", { name: "Continue without lobby" });
          }
          const old = heldMetadata();
          art.face.mockClear().mockReturnValueOnce(old.promise);
          await user.click(screen.getByRole("button", {
            name: route === "offline-continue" ? "Continue without lobby" : submitName,
          }));
          await waitFor(() => expect(art.face).toHaveBeenCalledWith("Island"));
          expect(startHosting).not.toHaveBeenCalled();
          expect(startP2PHostingSession).not.toHaveBeenCalled();
          await user.click(changeDeckButton());
          if (selectBeforeRelease) await user.click(screen.getByRole("button", { name: "Select New Deck" }));
          await act(async () => { old.resolve({ scryfall_oracle_id: "island" }); });
          expect(startHosting).not.toHaveBeenCalled();
          expect(startP2PHostingSession).not.toHaveBeenCalled();
          expect(harness.navigate).not.toHaveBeenCalled();
          expect(screen.queryByRole("button", { name: "Continue without lobby" })).not.toBeInTheDocument();
          if (!selectBeforeRelease) await user.click(screen.getByRole("button", { name: "Select New Deck" }));
          await waitFor(() => expect(screen.getByRole("button", { name: submitName })).toBeEnabled());
          await user.click(screen.getByRole("button", { name: submitName }));
          if (route.startsWith("offline")) {
            await user.click(await screen.findByRole("button", { name: "Continue without lobby" }));
          }
          const host = route === "server" ? startHosting : startP2PHostingSession;
          await waitFor(() => expect(host).toHaveBeenCalledOnce());
          expect(host.mock.calls[0][1]).toMatchObject({
            main_deck: ["Forest", "Forest", "Forest"],
            printings: [{ section: "main_deck", name: "Forest", count: 3,
              printing: { set_code: "bbb", collector_number: "002" } }],
          });
          if (route.startsWith("offline")) expect(host.mock.calls[0][2]).toMatchObject({ brokerUrl: null });
          expect(harness.navigate).toHaveBeenCalledExactlyOnceWith("/");
          mounted.unmount();
          startHosting.mockClear();
          startP2PHostingSession.mockClear();
          harness.navigate.mockClear();
        }
      },
    );

    it("cancels delegated human preparation before Edit navigation", async () => {
      prepareRealSetup();
      const old = heldMetadata();
      art.face.mockReturnValueOnce(old.promise);
      const mounted = renderPage("/multiplayer?view=host-setup");
      const user = userEvent.setup();
      await waitFor(() => expect(screen.getByRole("button", { name: "Host Game" })).toBeEnabled());
      await user.click(screen.getByRole("button", { name: "Host Game" }));
      await waitFor(() => expect(art.face).toHaveBeenCalledWith("Island"));
      await user.click(screen.getByRole("button", { name: "Edit" }));
      expect(harness.navigate).toHaveBeenCalledExactlyOnceWith(expect.stringContaining("/deck-builder?"));
      await act(async () => { old.resolve({ scryfall_oracle_id: "island" }); });
      expect(startHosting).not.toHaveBeenCalled();
      expect(harness.navigate).toHaveBeenCalledTimes(1);
      mounted.unmount();
      renderPage("/multiplayer?view=host-setup");
      await waitFor(() => expect(screen.getByRole("button", { name: "Host Game" })).toBeEnabled());
      await user.click(screen.getByRole("button", { name: "Host Game" }));
      await waitFor(() => expect(startHosting).toHaveBeenCalledOnce());
      expect(startHosting.mock.calls[0][1].main_deck).toEqual(["Island", "Island"]);
    });

    it("cancels AI preparation on Pick Deck and accepts a fresh selected-deck submission", async () => {
      prepareRealSetup();
      localStorage.removeItem("active-deck");
      const mounted = renderPage("/multiplayer?view=host-setup");
      const user = userEvent.setup();
      await waitFor(() => expect(screen.getByRole("button", { name: "Host Game" })).toBeEnabled());
      await user.click(screen.getAllByRole("button", { name: "Human" })[0]);
      const remembered = useMultiplayerStore.getState().lastHostConfig;
      const old = heldMetadata();
      art.face.mockReturnValueOnce(old.promise);
      await user.click(screen.getByRole("button", { name: "Host Game" }));
      await waitFor(() => expect(art.face).toHaveBeenCalledWith("Forest"));
      await user.click(screen.getByRole("button", { name: multiplayerEn.page.pickDeck }));
      await act(async () => { old.resolve({ scryfall_oracle_id: "forest" }); });
      expect(useMultiplayerStore.getState().lastHostConfig).toBe(remembered);
      expect(startHosting).not.toHaveBeenCalled();
      expect(startP2PHostingSession).not.toHaveBeenCalled();
      expect(harness.navigate).not.toHaveBeenCalled();
      await user.click(screen.getByRole("button", { name: "Select New Deck" }));
      await waitFor(() => expect(screen.getByRole("button", { name: "Host Game" })).toBeEnabled());
      await user.click(screen.getAllByRole("button", { name: "Human" })[0]);
      await user.click(screen.getByRole("button", { name: "Host Game" }));
      await waitFor(() => expect(startHosting).toHaveBeenCalledOnce());
      expect(startHosting.mock.calls[0][1].main_deck).toEqual(["Forest", "Forest", "Forest"]);
      expect(startHosting.mock.calls[0][0].aiSeats[0].deck).toMatchObject({
        type: "DeckList", data: { main_deck: ["Forest"] },
      });
      expect(harness.navigate).toHaveBeenCalledExactlyOnceWith("/");
      mounted.unmount();
    });

    it.each(["host-build", "join-lookup"] as const)(
      "rejects old AI preparation before persistence and delegation after %s arrival",
      async (arrival) => {
        prepareRealSetup();
        const remember = vi.spyOn(useMultiplayerStore.getState(), "rememberHostConfig");
        const router = createMemoryRouter([{ path: "/multiplayer", element: <MultiplayerPage /> }], {
          initialEntries: ["/multiplayer?view=host-setup"],
        });
        const mounted = render(<RouterProvider router={router} />);
        const user = userEvent.setup();
        await waitFor(() => expect(screen.getByRole("button", { name: "Host Game" })).toBeEnabled());
        await user.click(screen.getAllByRole("button", { name: "Human" })[0]);
        const remembered = useMultiplayerStore.getState().lastHostConfig;
        const old = heldMetadata();
        art.face.mockReturnValueOnce(old.promise);
        await user.click(screen.getByRole("button", { name: "Host Game" }));
        await waitFor(() => expect(art.face).toHaveBeenCalledWith("Forest"));
        const checked = deferred<"current">();
        const lookup = deferred<{ ok: false; reason: "not_found"; message: string }>();
        const lookupJoinTarget = vi.fn().mockReturnValue(lookup.promise);
        useMultiplayerStore.setState({ lookupJoinTarget });
        if (arrival === "host-build") build.checkDeployedBuild.mockReturnValueOnce(checked.promise);
        const link = arrival === "host-build"
          ? `/multiplayer?${hostLinkSearch({
            code: "AB12CD", format: "FreeForAll", playerCount: 3, roomName: "New room", serverUrl: URL_B,
          })}`
          : `/multiplayer?join=AB12CD@${URL_B}`;
        await act(async () => { await router.navigate(link); });
        await waitFor(() => expect(build.checkDeployedBuild).toHaveBeenCalledOnce());
        if (arrival === "join-lookup") await waitFor(() => expect(lookupJoinTarget).toHaveBeenCalledOnce());
        const navigated = harness.navigate.mock.calls.length; // URL strip, not game navigation.
        await act(async () => { old.resolve({ scryfall_oracle_id: "forest" }); });
        expect(remember).not.toHaveBeenCalled();
        expect(useMultiplayerStore.getState().lastHostConfig).toBe(remembered);
        expect(startHosting).not.toHaveBeenCalled();
        expect(startP2PHostingSession).not.toHaveBeenCalled();
        expect(harness.navigate).toHaveBeenCalledTimes(navigated);
        if (arrival === "host-build") {
          await act(async () => { checked.resolve("current"); });
          await waitFor(() => expect(harness.hostSetup?.seed).toMatchObject({ code: "AB12CD" }));
          await waitFor(() => expect(screen.getByRole("button", { name: "Host Game" })).toBeEnabled());
          await user.click(screen.getByRole("button", { name: "Host Game" }));
          await waitFor(() => expect(startHosting).toHaveBeenCalledOnce());
          expect(startHosting.mock.calls[0][0]).toMatchObject({ requestedCode: "AB12CD" });
        } else {
          await act(async () => { lookup.resolve({ ok: false, reason: "not_found", message: "Missing" }); });
          expect(await screen.findByText(multiplayerEn.page.waitingForHostTitle)).toBeInTheDocument();
          expect(startHosting).not.toHaveBeenCalled();
          expect(remember).not.toHaveBeenCalled();
        }
        mounted.unmount();
        remember.mockRestore();
        // Same AI input with no superseding link must persist and host once.
        startHosting.mockClear();
        prepareRealSetup();
        const liveRemember = vi.spyOn(useMultiplayerStore.getState(), "rememberHostConfig");
        renderPage("/multiplayer?view=host-setup");
        await waitFor(() => expect(screen.getByRole("button", { name: "Host Game" })).toBeEnabled());
        await user.click(screen.getAllByRole("button", { name: "Human" })[0]);
        await user.click(screen.getByRole("button", { name: "Host Game" }));
        await waitFor(() => expect(startHosting).toHaveBeenCalledOnce());
        expect(liveRemember).toHaveBeenCalledOnce();
        expect(startHosting.mock.calls[0][0].aiSeats[0].deck).toMatchObject({
          type: "DeckList", data: { main_deck: ["Forest"], printings: [
            { section: "main_deck", name: "Forest", count: 1,
              printing: { set_code: "bbb", collector_number: "002" } },
          ] },
        });
        liveRemember.mockRestore();
      },
    );
  });

  it.each([false, true])("F1 real setup cancels delegated preparation on mode switch; successor AI held=%s", async (holdSuccessorAi) => {
    harness.realHostSetup = true;
    harness.deckText = "Deck\n2 Island (AAA) 001";
    harness.candidates = [{
      id: "saved:AI", name: "AI Deck", source: { type: "saved" },
      deck: detectAndParseDeck("1 Forest (BBB) 002"), coveragePct: 100, archetype: null, bracket: null,
    }];
    useMultiplayerStore.setState({
      connectionMode: "server", hostingServer: URL_B,
      formatConfig: FORMAT_DEFAULTS.FreeForAll,
      lastHostConfig: {
        format: "FreeForAll", formatConfig: FORMAT_DEFAULTS.FreeForAll, savedCustomFormatId: null,
        playerCount: 3, matchType: "Bo1", loopDetection: { type: "Off" },
        isPublic: true, startWhenFull: false, ranked: false, aiSeats: [],
      },
      userLobbySources: [{ url: URL_B, name: "Test", origin: "user", kind: "Full" }],
      sourceStatus: new Map([[URL_B, { state: "open", playerCount: 0, serverInfo: {
        version: "test", buildCommit: "test", mode: "Full",
        protocolVersion: PROTOCOL_VERSION, lobbyProtocolVersion: LOBBY_PROTOCOL_VERSION,
      } }]]),
    });
    const old = heldMetadata();
    art.face.mockReturnValueOnce(old.promise);
    renderPage("/multiplayer?view=host-setup");
    const user = userEvent.setup();
    await waitFor(() => expect(screen.getByRole("button", { name: "Host Game" })).toBeEnabled());
    await user.click(screen.getByRole("button", { name: "Host Game" }));
    await waitFor(() => expect(art.face).toHaveBeenCalledWith("Island"));
    expect(startHosting).not.toHaveBeenCalled();
    await user.click(screen.getByRole("button", { name: "You host (P2P)" }));
    await waitFor(() => expect(screen.getByRole("button", { name: "Host P2P Game" })).toBeEnabled());
    const successor = heldMetadata();
    if (holdSuccessorAi) {
      await user.click(screen.getAllByRole("button", { name: "Human" })[0]);
      art.face.mockReturnValueOnce(successor.promise);
      await user.click(screen.getByRole("button", { name: "Host P2P Game" }));
      await waitFor(() => expect(art.face).toHaveBeenCalledWith("Forest"));
    }
    await act(async () => { old.resolve({ scryfall_oracle_id: "island" }); });
    expect(startHosting).not.toHaveBeenCalled();
    expect(startP2PHostingSession).not.toHaveBeenCalled();
    expect(harness.navigate).not.toHaveBeenCalled();
    if (holdSuccessorAi) {
      expect(screen.getByRole("button", { name: "Opening…" })).toBeDisabled();
      await act(async () => { successor.resolve({ scryfall_oracle_id: "forest" }); });
    } else {
      await user.click(screen.getByRole("button", { name: "Host P2P Game" }));
    }
    await waitFor(() => expect(startP2PHostingSession).toHaveBeenCalledOnce());
    expect(startHosting).not.toHaveBeenCalled();
    expect(startP2PHostingSession.mock.calls[0][1].printings).toEqual([
      { section: "main_deck", name: "Island", count: 2, printing: { set_code: "aaa", collector_number: "001" } },
    ]);
    if (holdSuccessorAi) expect(startP2PHostingSession.mock.calls[0][0].aiSeats[0].deck).toMatchObject({
      type: "DeckList", data: { printings: [
        { section: "main_deck", name: "Forest", count: 1, printing: { set_code: "bbb", collector_number: "002" } },
      ] },
    });
    expect(harness.navigate).toHaveBeenCalledExactlyOnceWith("/");
  });

  // V-U15g, leg (i)
  it("honors the form's P2P fallback even before the saved dedicated preference updates", async () => {
    useMultiplayerStore.setState({ connectionMode: "server" });
    renderPage("/multiplayer?view=host-setup");
    await submitHostSetup(null);
    expect(startP2PHostingSession).toHaveBeenCalledWith(
      expect.anything(), expect.anything(),
      { brokerUrl: OFFICIAL_MULTIPLAYER_SERVER_URL, roomName: "Test room" },
    );
    expect(startHosting).not.toHaveBeenCalled();
  });

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

  it("registers a Discord P2P host on the official broker, not a custom anchor", async () => {
    useMultiplayerStore.setState({ connectionMode: "p2p" });
    ensureSubscriptionSocket.mockImplementation(async () => ({ serverInfo: { mode: "LobbyOnly" } }));
    renderPage("/multiplayer?view=host-setup");
    await submitHostSetup(null, { requestedCode: "AB12CD" });
    expect(ensureSubscriptionSocket).toHaveBeenCalledExactlyOnceWith(OFFICIAL_MULTIPLAYER_SERVER_URL);
    expect(startP2PHostingSession).toHaveBeenCalledWith(
      expect.objectContaining({ requestedCode: "AB12CD" }),
      expect.anything(),
      { brokerUrl: OFFICIAL_MULTIPLAYER_SERVER_URL, roomName: "Test room" },
    );
  });

  it("hosts a Discord dedicated-server game on its seeded server, never the broker", async () => {
    const SEED = "wss://seed.example/ws";
    renderPage("/multiplayer?view=host-setup");
    await submitHostSetup(SEED, { requestedCode: "AB12CD" });
    await waitFor(() => expect(startHosting).toHaveBeenCalled());
    expect(startHosting).toHaveBeenCalledWith(
      expect.objectContaining({ requestedCode: "AB12CD" }), expect.anything(), SEED,
    );
    expect(ensureSubscriptionSocket).not.toHaveBeenCalledWith(OFFICIAL_MULTIPLAYER_SERVER_URL);
    expect(startP2PHostingSession).not.toHaveBeenCalled();
  });

  it("offers a Discord host no unlisted fallback when the official broker is down", async () => {
    useMultiplayerStore.setState({ connectionMode: "p2p" });
    ensureSubscriptionSocket.mockResolvedValue(null);
    renderPage("/multiplayer?view=host-setup");
    await submitHostSetup(null, { requestedCode: "AB12CD" });
    // Reach guard: the probe ran against the official broker.
    expect(ensureSubscriptionSocket).toHaveBeenCalledWith(OFFICIAL_MULTIPLAYER_SERVER_URL);
    expect(useMultiplayerStore.getState().toasts.get("generic")?.message).toBe(
      multiplayerEn.page.botLinkBrokerUnreachable,
    );
    expect(screen.queryByRole("button", { name: "Continue without lobby" })).not.toBeInTheDocument();
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

  describe("owner submission handovers (V6)", () => {
    const expected = [{ section: "main_deck", name: "Island", count: 2, printing: { set_code: "aaa", collector_number: "001" } }];

    it("preserves the route's existing section loss and freezes the deck across a later broker probe", async () => {
      harness.deckText = "Deck\n2 Island (AAA) 001\nSideboard\n1 Forest";
      harness.deckExtras = {
        commander: ["Commander"], planar_deck: ["Plane"], scheme_deck: ["Scheme"],
        companion: "Companion", signature_spell: ["Spell"], sticker_sheets: ["Stickers"],
      };
      useMultiplayerStore.setState(realHosting);
      let release!: (socket: unknown) => void;
      const probe = new Promise((resolve) => { release = resolve; });
      ensureSubscriptionSocket.mockReturnValueOnce(probe);
      renderPage("/multiplayer?view=host-setup");
      const pending = submitHostSetup(URL_B, {}, true);
      await waitFor(() => expect(ensureSubscriptionSocket).toHaveBeenCalledWith(URL_B));
      expect(art.bounds).toHaveBeenCalledOnce();
      expect(transport.send).not.toHaveBeenCalled();
      act(() => usePreferencesStore.setState({ artOverrides: { island: { scryfallId: "b", setCode: "bbb", collectorNumber: "002" } } }));
      await act(async () => { release({ serverInfo: { mode: "Full" } }); await pending; });
      await waitFor(() => expect(transport.send).toHaveBeenCalledOnce());
      const frame = JSON.parse(transport.send.mock.calls[0][0]);
      expect(frame.data.deck).toEqual({
        main_deck: ["Island", "Island"], sideboard: ["Forest"], commander: ["Commander"],
        planar_deck: ["Plane"], scheme_deck: ["Scheme"], bracket_tier: "core", printings: expected,
      });
    });

    it("leaves the all-AI shortcut selection-only for the provider's later submission", async () => {
      harness.deckText = "Deck\n2 Island (AAA) 001";
      renderPage("/multiplayer?view=host-setup");
      await submitHostSetup(URL_B, {
        aiSeats: [{ seatIndex: 1, difficulty: "Hard", deckName: "Chosen", deckId: "saved:Chosen" }],
      });
      expect(harness.navigate).toHaveBeenCalledWith(expect.stringContaining("mode=ai"));
      expect(art.face).not.toHaveBeenCalled();
      expect(startHosting).not.toHaveBeenCalled();
      expect(startP2PHostingSession).not.toHaveBeenCalled();
    });

    it.each(["server", "p2p", "offline"] as const)("carries the real prepared owner and AI choices through %s hosting", async (route) => {
      harness.deckText = "Deck\n2 Island (AAA) 001";
      const ai = await submitParsedDeck(detectAndParseDeck("Deck\n1 Island (BBB) 002"));
      useMultiplayerStore.setState(realHosting);
      if (route === "offline") ensureSubscriptionSocket.mockResolvedValue(null);
      renderPage("/multiplayer?view=host-setup");
      await submitHostSetup(route === "server" ? URL_B : null, {
        formatConfig: { format: "Commander", max_players: 3 },
        aiSeats: [{ seatIndex: 1, difficulty: "Hard", deckName: null, deck: { type: "DeckList", data: ai } }],
      });
      if (route === "offline") {
        expect(P2PHostAdapter).not.toHaveBeenCalled();
        await userEvent.click(screen.getByRole("button", { name: "Continue without lobby" }));
      }
      if (route === "server") {
        await waitFor(() => expect(transport.send).toHaveBeenCalled());
        const frame = JSON.parse(transport.send.mock.calls[0][0]);
        expect(frame.data.deck).toEqual({
          main_deck: ["Island", "Island"], sideboard: [], commander: [], planar_deck: [], scheme_deck: [],
          bracket_tier: "core", printings: expected,
        });
        expect(frame.data.ai_seats[0].deck.data.printings).toEqual([
          { section: "main_deck", name: "Island", count: 1, printing: { set_code: "bbb", collector_number: "002" } },
        ]);
      } else {
        await waitFor(() => expect(P2PHostAdapter).toHaveBeenCalledOnce());
        expect(vi.mocked(P2PHostAdapter).mock.calls[0][0]).toMatchObject({ player: { printings: expected } });
        expect(transport.apply).toHaveBeenCalledWith({
          type: "SetKind", data: { seatIndex: 1, kind: { type: "Ai", data: { difficulty: "Hard", deck: { type: "DeckList", data: ai } } } },
        });
        expect(transport.register).toHaveBeenCalledTimes(route === "offline" ? 0 : 1);
      }
    });

    it.each(["server", "p2p", "offline"] as const)("binds %s preferences before held metadata and preserves the captured payload", async (route) => {
      harness.deckText = "Deck\n2 Island (AAA) 001";
      renderPage("/multiplayer?view=host-setup");
      if (route === "offline") {
        ensureSubscriptionSocket.mockResolvedValue(null);
        await submitHostSetup(null);
      }
      const held = heldMetadata();
      art.face.mockClear().mockReturnValueOnce(held.promise);
      const pending = route === "offline"
        ? userEvent.click(screen.getByRole("button", { name: "Continue without lobby" }))
        : submitHostSetup(route === "server" ? URL_B : null, {}, true);
      await waitFor(() => expect(art.face).toHaveBeenCalled());
      expect(startHosting).not.toHaveBeenCalled();
      expect(startP2PHostingSession).not.toHaveBeenCalled();
      act(() => usePreferencesStore.setState({ artOverrides: { island: { scryfallId: "b", setCode: "bbb", collectorNumber: "002" } } }));
      await act(async () => { held.resolve({ scryfall_oracle_id: "island" }); await pending; });
      const send = route === "server" ? startHosting : startP2PHostingSession;
      await waitFor(() => expect(send).toHaveBeenCalledOnce());
      expect(send.mock.calls[0][1].printings).toEqual(expected);
      expect(harness.navigate).toHaveBeenCalledWith("/");
    });

    it.each(["server", "p2p", "offline"] as const)("drops stale %s preparation on Back, unmount and supersession; the live successor sends", async (route) => {
      for (const stop of ["back", "unmount", "supersede"] as const) {
        startHosting.mockClear();
        startP2PHostingSession.mockClear();
        harness.navigate.mockClear();
        harness.deckText = "Deck\n2 Island (AAA) 001";
        const rendered = renderPage("/multiplayer?view=host-setup");
        if (route === "offline") {
          ensureSubscriptionSocket.mockResolvedValue(null);
          await submitHostSetup(null);
        }
        const held = heldMetadata();
        art.face.mockClear().mockReturnValueOnce(held.promise);
        const pending = route === "offline"
          ? userEvent.click(screen.getByRole("button", { name: "Continue without lobby" }))
          : submitHostSetup(route === "server" ? URL_B : null, {}, true);
        await waitFor(() => expect(art.face).toHaveBeenCalled());
        if (stop === "back") act(() => (harness.chrome!.onBack as () => void)());
        if (stop === "unmount") rendered.unmount();
        if (stop === "supersede") {
          ensureSubscriptionSocket.mockResolvedValue({ serverInfo: { mode: "Full" } });
          harness.deckText = "Deck\n1 Island (BBB) 002";
          await submitHostSetup(URL_B);
        }
        const sent = startHosting.mock.calls.length + startP2PHostingSession.mock.calls.length;
        const navigated = harness.navigate.mock.calls.length;
        await act(async () => { held.resolve({ scryfall_oracle_id: "island" }); await pending; });
        expect(startHosting.mock.calls.length + startP2PHostingSession.mock.calls.length).toBe(sent);
        expect(harness.navigate).toHaveBeenCalledTimes(navigated);
        cleanup();
        ensureSubscriptionSocket.mockResolvedValue({ serverInfo: { mode: "Full" } });
        renderPage("/multiplayer?view=host-setup");
        await submitHostSetup(URL_B);
        expect(startHosting.mock.calls[startHosting.mock.calls.length - 1][1].printings).toEqual(stop === "supersede"
          ? [{ section: "main_deck", name: "Island", count: 1, printing: { set_code: "bbb", collector_number: "002" } }]
          : expected);
        cleanup();
      }
    });
  });
});
