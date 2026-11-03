import { StrictMode, useEffect, useLayoutEffect, useState } from "react";
import { act, cleanup, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { createMemoryRouter, RouterProvider, useLocation } from "react-router";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { refuseRealWebSockets } from "../../test/helpers/refusingWebSocket";

/**
 * Discord bot-link arrival on the real page and store, through a DATA router
 * so the strip and every later navigation are real history entries. The
 * page's children are stubs that record their props, so the TEST decides when
 * a child's callback fires.
 */
const harness = vi.hoisted(() => ({
  /** The live props of each stubbed child. */
  hostSetup: null as Record<string, unknown> | null,
  lobby: null as Record<string, unknown> | null,
  myDecks: null as Record<string, unknown> | null,
  /** The props each `HostSetup` mount received on its FIRST render. */
  hostMounts: [] as Record<string, unknown>[],
  /** The search of each `/deck-builder` entry the page navigated to. */
  deckBuilderSearches: [] as string[],
}));

const metricsMocks = vi.hoisted(() => ({
  reportConnectOutcome: vi.fn(),
  flushMetricsNow: vi.fn(),
  installServerMetricsLifecycle: vi.fn(),
  metricsUrl: vi.fn(() => "https://metrics.test/servers/metrics"),
}));
vi.mock("../../services/serverMetrics", () => metricsMocks);

const buildMocks = vi.hoisted(() => ({
  checkDeployedBuild: vi.fn(),
  updateToLatestBuild: vi.fn(),
  isBuildUpdateInFlight: vi.fn(),
  reloadIfNoLiveGame: vi.fn(),
}));
vi.mock("../../pwa/registerServiceWorker", () => buildMocks);

vi.mock("../../components/lobby/HostSetup", () => ({
  HostSetup: (props: Record<string, unknown>) => {
    harness.hostSetup = props;
    const [firstProps] = useState(props);
    // Layout, not passive: it runs in the commit that inserts `host-setup`, so
    // a `findByTestId("host-setup")` can never resolve before the mount is
    // recorded. A passive effect can still be pending then under CI load.
    useLayoutEffect(() => {
      harness.hostMounts.push(firstProps);
    }, [firstProps]);
    return <div data-testid="host-setup" />;
  },
}));

vi.mock("../../components/lobby/LobbyView", () => ({
  LobbyView: (props: Record<string, unknown>) => {
    harness.lobby = props;
    return <div data-testid="lobby" />;
  },
}));

vi.mock("../../components/menu/MyDecks", () => ({
  MyDecks: (props: Record<string, unknown>) => {
    harness.myDecks = props;
    return <div data-testid="my-decks" />;
  },
}));

vi.mock("../../components/chrome/ScreenChrome", () => ({ ScreenChrome: () => null }));
vi.mock("../../components/chrome/ShellContext", () => ({ useInShell: () => false }));
vi.mock("../../components/menu/MenuParticles", () => ({ MenuParticles: () => null }));
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

import { MultiplayerPage } from "../MultiplayerPage";
import { __resetBotLinkStashForTests, hostLinkSearch, parseBotLink, type HostSeed } from "../multiplayerPageState";
import { useMultiplayerStore } from "../../stores/multiplayerStore";
import multiplayerEn from "../../i18n/locales/en/multiplayer.json";

const LINK_SERVER = "wss://lobby.example/ws";
const JOIN_LINK = `/multiplayer?join=AB12CD@${LINK_SERVER}`;
const SEED: HostSeed = {
  code: "AB12CD",
  format: "Commander",
  playerCount: 4,
  roomName: "Friday",
  serverUrl: null,
};
const HOST_LINK = `/multiplayer?${hostLinkSearch(SEED)}`;

const lookupJoinTarget = vi.fn();
const resolveGuest = vi.fn();
const STASH_KEY = "phase:bot-link";

function deferred<T>(): { promise: Promise<T>; resolve: (value: T) => void } {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((done) => { resolve = done; });
  return { promise, resolve };
}

/** The kind of the link the stash holds, or null when there is none. */
function stashedKind(): string | null {
  const raw = sessionStorage.getItem(STASH_KEY);
  if (raw === null) return null;
  return parseBotLink((JSON.parse(raw) as { search: string }).search)?.kind ?? null;
}

function DeckBuilderStub() {
  const location = useLocation();
  useEffect(() => {
    harness.deckBuilderSearches.push(location.search);
  }, [location.search]);
  return <div data-testid="deck-builder" />;
}

function renderRouted(entry: string, { strict = false } = {}) {
  const router = createMemoryRouter(
    [
      { path: "/multiplayer", element: <MultiplayerPage /> },
      { path: "/deck-builder", element: <DeckBuilderStub /> },
    ],
    { initialEntries: [entry] },
  );
  const tree = <RouterProvider router={router} />;
  render(strict ? <StrictMode>{tree}</StrictMode> : tree);
  return router;
}

type TestRouter = ReturnType<typeof renderRouted>;

async function go(router: TestRouter, to: string): Promise<void> {
  await act(async () => {
    await router.navigate(to);
  });
}

function lastReturnTo(): string | null {
  const search = harness.deckBuilderSearches[harness.deckBuilderSearches.length - 1];
  return search === undefined ? null : new URLSearchParams(search).get("returnTo");
}

/** The active-deck banner's Change, beside its Edit (the identity banner has
 * a "Change" of its own). */
function activeDeckChangeButton(): HTMLElement {
  const edit = screen.getByRole("button", { name: multiplayerEn.page.edit });
  return within(edit.parentElement!).getByRole("button", { name: multiplayerEn.page.change });
}

function seedOf(props: Record<string, unknown> | null | undefined): HostSeed | undefined {
  return props?.seed as HostSeed | undefined;
}

const joinTargetOk = {
  ok: true,
  info: {
    game_code: "AB12CD",
    is_p2p: true,
    player_count: 4,
    filled_seats: 1,
    match_config: { match_type: "Bo1" },
    format_config: null,
  },
};
const joinTargetNotFound = {
  ok: false,
  reason: "not_found",
  message: "Game not found in lobby: AB12CD",
};

describe("MultiplayerPage Discord bot links", () => {
  let reload: ReturnType<typeof vi.fn>;
  let socketUrls: string[] = [];

  beforeEach(() => {
    vi.clearAllMocks();
    socketUrls = refuseRealWebSockets();
    harness.hostSetup = null;
    harness.lobby = null;
    harness.myDecks = null;
    harness.hostMounts = [];
    harness.deckBuilderSearches = [];
    localStorage.clear();
    sessionStorage.clear();
    __resetBotLinkStashForTests();
    // Reset, not just clear: a test's unconsumed `…Once` value must not leak.
    for (const mock of Object.values(buildMocks)) mock.mockReset();
    buildMocks.checkDeployedBuild.mockResolvedValue("current");
    buildMocks.updateToLatestBuild.mockResolvedValue("manual");
    buildMocks.isBuildUpdateInFlight.mockReturnValue(false);
    buildMocks.reloadIfNoLiveGame.mockReturnValue(true);
    reload = vi.fn();
    Object.defineProperty(window.location, "reload", { configurable: true, value: reload });
    vi.stubGlobal("navigator", { sendBeacon: vi.fn(() => true) });
    vi.stubGlobal("fetch", vi.fn(async () => ({ ok: false, status: 599 })));
    lookupJoinTarget.mockResolvedValue(joinTargetOk);
    useMultiplayerStore.setState({
      hostingServer: "wss://hosting.example/ws",
      connectionMode: "p2p",
      userLobbySources: [],
      sourceStatus: new Map(),
      directorySources: [],
      disabledDirectorySources: [],
      displayName: "Tester",
      toasts: new Map(),
      lookupJoinTarget,
      resolveGuest,
      ensureSubscriptionSocket: vi.fn(async () => null),
    });
  });

  afterEach(() => {
    const opened = [...socketUrls];
    cleanup();
    vi.unstubAllGlobals();
    expect(opened).toEqual([]);
  });

  describe("host arrival", () => {
    it("seeds HostSetup's first mount and strips the link", async () => {
      const router = renderRouted(HOST_LINK);
      await screen.findByTestId("host-setup");

      expect(harness.hostMounts).toHaveLength(1);
      expect(seedOf(harness.hostMounts[0])).toEqual(SEED);
      expect(router.state.location.search).toBe("");
    });

    it("mounts HostSetup unseeded for a plain host-setup view", async () => {
      renderRouted("/multiplayer?view=host-setup");
      await screen.findByTestId("host-setup");

      expect(seedOf(harness.hostMounts[0])).toBeUndefined();
    });

    it("remounts a host-setup view with the seed when the link also carries view", async () => {
      renderRouted(`/multiplayer?view=host-setup&${hostLinkSearch(SEED)}`);
      await screen.findByTestId("host-setup");

      await waitFor(() => expect(seedOf(harness.hostMounts[harness.hostMounts.length - 1])).toEqual(SEED));
    });

    it("leaves the seed alone and looks nothing up on the strip entry", async () => {
      const router = renderRouted(HOST_LINK);
      await screen.findByTestId("host-setup");

      expect(router.state.location.search).toBe("");
      expect(seedOf(harness.hostSetup)).toEqual(SEED);
      expect(harness.hostMounts).toHaveLength(1);
      expect(lookupJoinTarget).not.toHaveBeenCalled();
    });

    it("drops the seed when the lobby's own Host Game is used", async () => {
      renderRouted(HOST_LINK);
      await screen.findByTestId("host-setup");

      act(() => (harness.hostSetup!.onBack as () => void)());
      await screen.findByTestId("lobby");
      act(() => (harness.lobby!.onHostGame as () => void)());
      await screen.findByTestId("host-setup");

      expect(seedOf(harness.hostSetup)).toBeUndefined();
    });
  });

  describe("invalid link", () => {
    it("toasts and strips a malformed link", async () => {
      const router = renderRouted("/multiplayer?code=ab");

      await waitFor(() => expect(router.state.location.search).toBe(""));
      expect(useMultiplayerStore.getState().toasts.get("generic")?.message).toBe(
        multiplayerEn.page.invalidGameLink,
      );
      // Neither stashed nor gated.
      expect(buildMocks.checkDeployedBuild).not.toHaveBeenCalled();
      expect(sessionStorage.getItem(STASH_KEY)).toBeNull();
    });
  });

  describe("guest arrival", () => {
    it("waits for a host that has not opened the game, and retries on the link's origin", async () => {
      const user = userEvent.setup();
      lookupJoinTarget.mockResolvedValueOnce(joinTargetNotFound);
      renderRouted(JOIN_LINK);

      expect(await screen.findByText(multiplayerEn.page.waitingForHostTitle)).toBeInTheDocument();
      expect(screen.getByText(multiplayerEn.page.waitingForHostMessage)).toBeInTheDocument();

      await user.click(screen.getByRole("button", { name: multiplayerEn.connectionToast.retry }));

      await screen.findByTestId("my-decks");
      expect(lookupJoinTarget).toHaveBeenCalledTimes(2);
      for (const [code, origin] of lookupJoinTarget.mock.calls) {
        expect(code).toBe("AB12CD");
        expect((origin as { url: string }).url).toBe(LINK_SERVER);
      }
      expect(screen.queryByText(multiplayerEn.page.waitingForHostTitle)).not.toBeInTheDocument();
    });

    it("looks the code up on the link's server, not the hosting anchor", async () => {
      renderRouted(JOIN_LINK);

      await waitFor(() => expect(lookupJoinTarget).toHaveBeenCalled());
      const [, origin] = lookupJoinTarget.mock.calls[0];
      expect((origin as { url: string }).url).toBe(LINK_SERVER);
      expect(useMultiplayerStore.getState().hostingServer).toBe("wss://hosting.example/ws");
    });

    it("handles a link that arrives while the page is already mounted", async () => {
      const router = renderRouted("/multiplayer");
      await screen.findByTestId("lobby");
      expect(lookupJoinTarget).not.toHaveBeenCalled();

      await go(router, JOIN_LINK);

      await waitFor(() => expect(lookupJoinTarget).toHaveBeenCalledOnce());
    });

    it("handles an arrival once under StrictMode", async () => {
      renderRouted(JOIN_LINK, { strict: true });

      await screen.findByTestId("my-decks");
      expect(lookupJoinTarget).toHaveBeenCalledOnce();
    });

    it("handles an arrival once without StrictMode", async () => {
      renderRouted(JOIN_LINK);

      await screen.findByTestId("my-decks");
      expect(lookupJoinTarget).toHaveBeenCalledOnce();
    });

    it("handles the same link again when it is re-opened as a new entry", async () => {
      const router = renderRouted("/multiplayer");
      await screen.findByTestId("lobby");

      await go(router, JOIN_LINK);
      await waitFor(() => expect(router.state.location.search).toBe(""));
      await go(router, JOIN_LINK);

      await waitFor(() => expect(lookupJoinTarget).toHaveBeenCalledTimes(2));
    });
  });

  describe("deck-builder round trip", () => {
    async function returnFromDeckBuilder(router: TestRouter): Promise<void> {
      const returnTo = lastReturnTo();
      expect(returnTo).not.toBeNull();
      harness.hostMounts = [];
      await go(router, returnTo!);
      await screen.findByTestId("host-setup");
    }

    it("keeps the seed across Edit from the seeded host-setup", async () => {
      const user = userEvent.setup();
      localStorage.setItem("active-deck", "Test Deck");
      const router = renderRouted(HOST_LINK);
      await screen.findByTestId("host-setup");

      await user.click(screen.getByRole("button", { name: multiplayerEn.page.edit }));
      await screen.findByTestId("deck-builder");
      expect(lastReturnTo()).toBe(`/multiplayer?${hostLinkSearch(SEED)}`);

      await returnFromDeckBuilder(router);
      expect(seedOf(harness.hostMounts[0])).toEqual(SEED);
      expect(router.state.location.search).toBe("");
    });

    it("keeps the existing return for an unseeded host-setup Edit", async () => {
      const user = userEvent.setup();
      localStorage.setItem("active-deck", "Test Deck");
      renderRouted("/multiplayer");
      await screen.findByTestId("lobby");
      act(() => (harness.lobby!.onHostGame as () => void)());
      await screen.findByTestId("host-setup");

      await user.click(screen.getByRole("button", { name: multiplayerEn.page.edit }));
      await screen.findByTestId("deck-builder");

      expect(lastReturnTo()).toBe("/multiplayer?view=host-setup");
    });

    it("keeps the seed across Edit from deck-select with the pending seeded host", async () => {
      const router = renderRouted(HOST_LINK);
      await screen.findByTestId("host-setup");

      // No active deck, so submitting parks the host action on deck-select.
      await act(async () => {
        await (harness.hostSetup!.onHost as (s: unknown, url: string | null) => Promise<boolean>)(
          {
            displayName: "Tester",
            public: false,
            password: "",
            timerSeconds: null,
            formatConfig: { format: "Commander", max_players: 4 },
            matchType: "Bo1",
            loopDetection: { type: "Off" },
            aiSeats: [],
            startWhenFull: false,
            ranked: false,
            roomName: "Friday",
            requestedCode: "AB12CD",
          },
          null,
        );
      });
      await screen.findByTestId("my-decks");
      act(() => (harness.myDecks!.onEditDeck as (name: string) => void)("X"));
      await screen.findByTestId("deck-builder");
      expect(lastReturnTo()).toBe(`/multiplayer?${hostLinkSearch(SEED)}`);

      await returnFromDeckBuilder(router);
      expect(seedOf(harness.hostMounts[0])).toEqual(SEED);
    });

    it("keeps the seed across Edit after Pick Deck on the seeded host-setup", async () => {
      const user = userEvent.setup();
      const router = renderRouted(HOST_LINK);
      await screen.findByTestId("host-setup");

      await user.click(screen.getByRole("button", { name: multiplayerEn.page.pickDeck }));
      await screen.findByTestId("my-decks");
      act(() => (harness.myDecks!.onEditDeck as (name: string) => void)("X"));
      await screen.findByTestId("deck-builder");
      expect(lastReturnTo()).toBe(`/multiplayer?${hostLinkSearch(SEED)}`);

      await returnFromDeckBuilder(router);
      expect(seedOf(harness.hostMounts[0])).toEqual(SEED);
    });

    it("keeps the seed across Edit after Change on the seeded host-setup", async () => {
      const user = userEvent.setup();
      localStorage.setItem("active-deck", "Test Deck");
      renderRouted(HOST_LINK);
      await screen.findByTestId("host-setup");

      await user.click(activeDeckChangeButton());
      await screen.findByTestId("my-decks");
      act(() => (harness.myDecks!.onEditDeck as (name: string) => void)("X"));
      await screen.findByTestId("deck-builder");

      expect(lastReturnTo()).toBe(`/multiplayer?${hostLinkSearch(SEED)}`);
    });

    it("does not let a leftover seed capture a later join's Edit", async () => {
      renderRouted(HOST_LINK);
      await screen.findByTestId("host-setup");

      act(() => (harness.hostSetup!.onBack as () => void)());
      await screen.findByTestId("lobby");
      await act(async () => {
        await (harness.lobby!.onJoinGame as (code: string, origin: unknown) => Promise<void>)(
          "QQ11QQ",
          { url: LINK_SERVER, name: "lobby.example", origin: "user" },
        );
      });
      await screen.findByTestId("my-decks");
      act(() => (harness.myDecks!.onEditDeck as (name: string) => void)("X"));
      await screen.findByTestId("deck-builder");

      expect(lastReturnTo()).toBe("/multiplayer?view=deck-select");
    });

    it("does not let a leftover seed and deck-select return capture a later join's Edit", async () => {
      const user = userEvent.setup();
      localStorage.setItem("active-deck", "Test Deck");
      renderRouted(HOST_LINK);
      await screen.findByTestId("host-setup");

      await user.click(activeDeckChangeButton());
      await screen.findByTestId("my-decks");
      // No pending action, so choosing a deck returns to host-setup.
      act(() => (harness.myDecks!.onSelectDeck as (name: string) => void)("X"));
      await screen.findByTestId("host-setup");
      act(() => (harness.hostSetup!.onBack as () => void)());
      await screen.findByTestId("lobby");
      await act(async () => {
        await (harness.lobby!.onJoinGame as (code: string, origin: unknown) => Promise<void>)(
          "QQ11QQ",
          { url: LINK_SERVER, name: "lobby.example", origin: "user" },
        );
      });
      await screen.findByTestId("my-decks");
      act(() => (harness.myDecks!.onEditDeck as (name: string) => void)("X"));
      await screen.findByTestId("deck-builder");

      expect(lastReturnTo()).toBe("/multiplayer?view=deck-select");
    });
  });

  describe("version gate", () => {
    const toastMessage = () => useMultiplayerStore.getState().toasts.get("generic")?.message;

    it("proceeds on a current build and clears the stash", async () => {
      renderRouted(JOIN_LINK);

      await waitFor(() => expect(lookupJoinTarget).toHaveBeenCalledOnce());
      expect(buildMocks.checkDeployedBuild).toHaveBeenCalledOnce();
      expect(buildMocks.updateToLatestBuild).not.toHaveBeenCalled();
      expect(sessionStorage.getItem(STASH_KEY)).toBeNull();
    });

    it("proceeds when the deployed build cannot be read", async () => {
      buildMocks.checkDeployedBuild.mockResolvedValue("unknown");
      renderRouted(JOIN_LINK);

      await waitFor(() => expect(lookupJoinTarget).toHaveBeenCalledOnce());
      expect(buildMocks.updateToLatestBuild).not.toHaveBeenCalled();
      expect(screen.queryByText(multiplayerEn.page.updatingTitle)).not.toBeInTheDocument();
      expect(sessionStorage.getItem(STASH_KEY)).toBeNull();
    });

    it("holds a link on a stale build while the update runs", async () => {
      buildMocks.checkDeployedBuild.mockResolvedValue("stale");
      buildMocks.updateToLatestBuild.mockReturnValue(new Promise(() => {}));
      const router = renderRouted(JOIN_LINK);

      expect(await screen.findByText(multiplayerEn.page.updatingTitle)).toBeInTheDocument();
      expect(screen.getByText(multiplayerEn.page.updatingMessage)).toBeInTheDocument();
      expect(screen.queryByRole("button", { name: multiplayerEn.joinErrorDialog.dismiss })).not.toBeInTheDocument();
      expect(buildMocks.updateToLatestBuild).toHaveBeenCalledWith({ deadlineMs: 15_000 });
      expect(lookupJoinTarget).not.toHaveBeenCalled();
      expect(stashedKind()).toBe("join");
      expect(router.state.location.search).toBe("");
    });

    it("keeps the updating dialog when the update reloads", async () => {
      buildMocks.checkDeployedBuild.mockResolvedValue("stale");
      buildMocks.updateToLatestBuild.mockResolvedValue("reloading");
      renderRouted(JOIN_LINK);

      expect(await screen.findByText(multiplayerEn.page.updatingTitle)).toBeInTheDocument();
      await act(async () => {});
      expect(screen.getByText(multiplayerEn.page.updatingTitle)).toBeInTheDocument();
      expect(screen.queryByText(multiplayerEn.page.updateFailedTitle)).not.toBeInTheDocument();
      expect(lookupJoinTarget).not.toHaveBeenCalled();
    });

    describe("manual fallback", () => {
      beforeEach(() => {
        buildMocks.checkDeployedBuild.mockResolvedValue("stale");
      });

      it("continues on this build and drops the stash when no update is in flight", async () => {
        const user = userEvent.setup();
        renderRouted(JOIN_LINK);

        await screen.findByText(multiplayerEn.page.updateFailedTitle);
        expect(screen.getByText(multiplayerEn.page.updateFailedMessage)).toBeInTheDocument();
        await user.click(screen.getByRole("button", { name: multiplayerEn.page.continueAnyway }));

        await waitFor(() => expect(lookupJoinTarget).toHaveBeenCalledOnce());
        const [code, origin] = lookupJoinTarget.mock.calls[0];
        expect(code).toBe("AB12CD");
        expect((origin as { url: string }).url).toBe(LINK_SERVER);
        expect(screen.queryByText(multiplayerEn.page.updateFailedTitle)).not.toBeInTheDocument();
        expect(sessionStorage.getItem(STASH_KEY)).toBeNull();
      });

      it("ignores a backdrop click: Continue anyway takes the button", async () => {
        const user = userEvent.setup();
        renderRouted(JOIN_LINK);

        await screen.findByText(multiplayerEn.page.updateFailedTitle);
        await user.click(screen.getByTestId("join-error-dialog-backdrop"));
        await act(async () => {});

        expect(screen.getByText(multiplayerEn.page.updateFailedTitle)).toBeInTheDocument();
        expect(lookupJoinTarget).not.toHaveBeenCalled();
        expect(stashedKind()).toBe("join");
      });

      it("refreshes through the live-game guard and keeps the stash", async () => {
        const user = userEvent.setup();
        renderRouted(JOIN_LINK);

        await screen.findByText(multiplayerEn.page.updateFailedTitle);
        await user.click(screen.getByRole("button", { name: multiplayerEn.page.joinErrorRefresh }));

        expect(buildMocks.reloadIfNoLiveGame).toHaveBeenCalledOnce();
        expect(toastMessage()).toBeUndefined();
        expect(stashedKind()).toBe("join");
      });

      it("never reloads a live game from the manual Refresh", async () => {
        const user = userEvent.setup();
        buildMocks.reloadIfNoLiveGame.mockReturnValue(false);
        renderRouted(JOIN_LINK);

        await screen.findByText(multiplayerEn.page.updateFailedTitle);
        await user.click(screen.getByRole("button", { name: multiplayerEn.page.joinErrorRefresh }));

        expect(buildMocks.reloadIfNoLiveGame).toHaveBeenCalledOnce();
        expect(toastMessage()).toBe(multiplayerEn.page.refreshAfterGame);
        expect(screen.getByText(multiplayerEn.page.updateFailedTitle)).toBeInTheDocument();
        expect(reload).not.toHaveBeenCalled();
        expect(stashedKind()).toBe("join");
        expect(lookupJoinTarget).not.toHaveBeenCalled();
      });

      it("keeps the stash on Continue anyway while an update is in flight, and the reload re-applies it", async () => {
        const user = userEvent.setup();
        buildMocks.isBuildUpdateInFlight.mockReturnValue(true);
        renderRouted(JOIN_LINK);

        await screen.findByText(multiplayerEn.page.updateFailedTitle);
        await user.click(screen.getByRole("button", { name: multiplayerEn.page.continueAnyway }));

        await waitFor(() => expect(lookupJoinTarget).toHaveBeenCalledOnce());
        expect(screen.queryByText(multiplayerEn.page.updateFailedTitle)).not.toBeInTheDocument();
        expect(stashedKind()).toBe("join");

        // The in-flight update reloads: a new document on the current build.
        cleanup();
        __resetBotLinkStashForTests();
        buildMocks.checkDeployedBuild.mockResolvedValue("current");
        renderRouted("/multiplayer");

        await waitFor(() => expect(lookupJoinTarget).toHaveBeenCalledTimes(2));
        const [code, origin] = lookupJoinTarget.mock.calls[1];
        expect(code).toBe("AB12CD");
        expect((origin as { url: string }).url).toBe(LINK_SERVER);
        expect(sessionStorage.getItem(STASH_KEY)).toBeNull();
      });

      it("does not re-gate a seeded host's Edit return after Continue anyway", async () => {
        const user = userEvent.setup();
        localStorage.setItem("active-deck", "Test Deck");
        const router = renderRouted(HOST_LINK);

        await screen.findByText(multiplayerEn.page.updateFailedTitle);
        await user.click(screen.getByRole("button", { name: multiplayerEn.page.continueAnyway }));
        await screen.findByTestId("host-setup");
        expect(seedOf(harness.hostMounts[0])).toEqual(SEED);

        await user.click(screen.getByRole("button", { name: multiplayerEn.page.edit }));
        await screen.findByTestId("deck-builder");
        const returnTo = lastReturnTo();
        expect(returnTo).toBe(`/multiplayer?${hostLinkSearch(SEED)}`);
        harness.hostMounts = [];
        await go(router, returnTo!);

        // Reach guard: the return is a new arrival and its build is checked.
        await waitFor(() => expect(buildMocks.checkDeployedBuild).toHaveBeenCalledTimes(2));
        await act(async () => {});
        // Soft, so a regression reports both symptoms of a re-gate.
        expect.soft(buildMocks.updateToLatestBuild).toHaveBeenCalledOnce();
        expect.soft(screen.queryByText(multiplayerEn.page.updateFailedTitle)).not.toBeInTheDocument();
        expect(screen.queryByText(multiplayerEn.page.updatingTitle)).not.toBeInTheDocument();
        await screen.findByTestId("host-setup");
        expect(seedOf(harness.hostMounts[0])?.code).toBe("AB12CD");
        expect(sessionStorage.getItem(STASH_KEY)).toBeNull();
      });
    });

    it.each([
      ["a join link", JOIN_LINK],
      ["a host link", HOST_LINK],
    ])("re-applies %s from the stash after the update reloads", async (_label, entry) => {
      buildMocks.checkDeployedBuild.mockResolvedValue("stale");
      buildMocks.updateToLatestBuild.mockResolvedValue("reloading");
      renderRouted(entry);
      await screen.findByText(multiplayerEn.page.updatingTitle);

      cleanup();
      __resetBotLinkStashForTests();
      buildMocks.checkDeployedBuild.mockResolvedValue("current");
      renderRouted("/multiplayer");

      if (entry === JOIN_LINK) {
        await waitFor(() => expect(lookupJoinTarget).toHaveBeenCalledOnce());
        const [code, origin] = lookupJoinTarget.mock.calls[0];
        expect(code).toBe("AB12CD");
        expect((origin as { url: string }).url).toBe(LINK_SERVER);
      } else {
        await screen.findByTestId("host-setup");
        expect(seedOf(harness.hostMounts[0])).toEqual(SEED);
      }
      expect(sessionStorage.getItem(STASH_KEY)).toBeNull();
    });

    it.each([false, true])("gates the strip entry's arrival once (StrictMode %s)", async (strict) => {
      buildMocks.checkDeployedBuild.mockResolvedValue("stale");
      buildMocks.updateToLatestBuild.mockReturnValue(new Promise(() => {}));
      const router = renderRouted(JOIN_LINK, { strict });

      await screen.findByText(multiplayerEn.page.updatingTitle);
      expect(router.state.location.search).toBe("");
      await act(async () => {});
      expect(buildMocks.checkDeployedBuild).toHaveBeenCalledOnce();
      expect(buildMocks.updateToLatestBuild).toHaveBeenCalledOnce();
    });

    it("reads the stash once per document across a remount", async () => {
      buildMocks.checkDeployedBuild.mockResolvedValue("stale");
      buildMocks.updateToLatestBuild.mockReturnValue(new Promise(() => {}));
      const router = renderRouted(HOST_LINK);
      await screen.findByText(multiplayerEn.page.updatingTitle);

      await go(router, "/deck-builder");
      await screen.findByTestId("deck-builder");
      await go(router, "/multiplayer");
      await screen.findByTestId("lobby");
      await act(async () => {});

      expect(buildMocks.checkDeployedBuild).toHaveBeenCalledOnce();
    });

    it("prefers the URL's link over an earlier document's stash", async () => {
      sessionStorage.setItem(STASH_KEY, JSON.stringify({ search: JOIN_LINK.slice("/multiplayer".length), at: Date.now() }));
      renderRouted(HOST_LINK);

      await screen.findByTestId("host-setup");
      expect(seedOf(harness.hostMounts[0])).toEqual(SEED);
      await act(async () => {});
      expect(lookupJoinTarget).not.toHaveBeenCalled();
      expect(sessionStorage.getItem(STASH_KEY)).toBeNull();
    });

    it.each([
      ["an expired stash is ignored and removed", 11 * 60 * 1000, false],
      ["a fresh stash is applied", 0, true],
    ])("%s", async (_label, age, applied) => {
      sessionStorage.setItem(
        STASH_KEY,
        JSON.stringify({ search: JOIN_LINK.slice("/multiplayer".length), at: Date.now() - age }),
      );
      renderRouted("/multiplayer");
      await screen.findByTestId("lobby");

      if (applied) {
        await waitFor(() => expect(lookupJoinTarget).toHaveBeenCalledOnce());
      } else {
        await act(async () => {});
        expect(lookupJoinTarget).not.toHaveBeenCalled();
        expect(buildMocks.checkDeployedBuild).not.toHaveBeenCalled();
      }
      expect(sessionStorage.getItem(STASH_KEY)).toBeNull();
    });

    it("drops a stash read by an invalid link's run", async () => {
      sessionStorage.setItem(STASH_KEY, JSON.stringify({ search: JOIN_LINK.slice("/multiplayer".length), at: Date.now() }));
      const router = renderRouted("/multiplayer?code=ab");

      await waitFor(() => expect(router.state.location.search).toBe(""));
      expect(toastMessage()).toBe(multiplayerEn.page.invalidGameLink);
      expect(sessionStorage.getItem(STASH_KEY)).toBeNull();
      expect(lookupJoinTarget).not.toHaveBeenCalled();
      expect(buildMocks.checkDeployedBuild).not.toHaveBeenCalled();
    });

    it("leaves a pending gate's stash alone when a later invalid link arrives", async () => {
      buildMocks.checkDeployedBuild.mockResolvedValue("stale");
      buildMocks.updateToLatestBuild.mockReturnValue(new Promise(() => {}));
      const router = renderRouted(JOIN_LINK);
      await screen.findByText(multiplayerEn.page.updatingTitle);

      await go(router, "/multiplayer?code=ab");
      await waitFor(() => expect(toastMessage()).toBe(multiplayerEn.page.invalidGameLink));
      expect(stashedKind()).toBe("join");
    });

    it("replaces a join error dialog with the updating dialog", async () => {
      lookupJoinTarget.mockResolvedValueOnce(joinTargetNotFound);
      const router = renderRouted(JOIN_LINK);
      await screen.findByText(multiplayerEn.page.waitingForHostTitle);

      buildMocks.checkDeployedBuild.mockResolvedValue("stale");
      buildMocks.updateToLatestBuild.mockReturnValue(new Promise(() => {}));
      await go(router, HOST_LINK);

      await screen.findByText(multiplayerEn.page.updatingTitle);
      await waitFor(() => expect(screen.queryByText(multiplayerEn.page.waitingForHostTitle)).not.toBeInTheDocument());
    });

    it("clears the guest's waiting dialog on a host arrival", async () => {
      lookupJoinTarget.mockResolvedValueOnce(joinTargetNotFound);
      const router = renderRouted(JOIN_LINK);
      await screen.findByText(multiplayerEn.page.waitingForHostTitle);

      await go(router, HOST_LINK);

      await screen.findByTestId("host-setup");
      expect(seedOf(harness.hostMounts[harness.hostMounts.length - 1])).toEqual(SEED);
      expect(screen.queryByText(multiplayerEn.page.waitingForHostTitle)).not.toBeInTheDocument();
    });

    describe("a gate overtaken by a newer arrival", () => {
      it("does not apply its link after the newer one proceeded", async () => {
        const first = deferred<string>();
        buildMocks.checkDeployedBuild.mockReturnValueOnce(first.promise);
        const router = renderRouted(HOST_LINK);
        await screen.findByTestId("lobby");

        await go(router, JOIN_LINK);
        await waitFor(() => expect(lookupJoinTarget).toHaveBeenCalledOnce());
        await act(async () => first.resolve("current"));

        expect(screen.queryByTestId("host-setup")).not.toBeInTheDocument();
        expect(harness.hostMounts).toHaveLength(0);
        expect(lookupJoinTarget).toHaveBeenCalledOnce();
      });

      it("leaves the newer arrival's stash and dialog alone", async () => {
        const first = deferred<string>();
        buildMocks.checkDeployedBuild.mockReturnValueOnce(first.promise).mockResolvedValue("stale");
        buildMocks.updateToLatestBuild.mockReturnValue(new Promise(() => {}));
        const router = renderRouted(HOST_LINK);
        await screen.findByTestId("lobby");

        await go(router, JOIN_LINK);
        await screen.findByText(multiplayerEn.page.updatingTitle);
        expect(stashedKind()).toBe("join");
        await act(async () => first.resolve("current"));

        expect(stashedKind()).toBe("join");
        expect(screen.getByText(multiplayerEn.page.updatingTitle)).toBeInTheDocument();
        expect(screen.queryByTestId("host-setup")).not.toBeInTheDocument();
      });

      it("does not open its manual dialog after the newer one proceeded", async () => {
        const update = deferred<string>();
        buildMocks.checkDeployedBuild.mockResolvedValueOnce("stale").mockResolvedValue("current");
        buildMocks.updateToLatestBuild.mockReturnValueOnce(update.promise);
        const router = renderRouted(HOST_LINK);
        await screen.findByText(multiplayerEn.page.updatingTitle);

        await go(router, JOIN_LINK);
        await waitFor(() => expect(lookupJoinTarget).toHaveBeenCalledOnce());
        await act(async () => update.resolve("manual"));

        expect(screen.queryByText(multiplayerEn.page.updateFailedTitle)).not.toBeInTheDocument();
        expect(harness.hostMounts).toHaveLength(0);
      });

      it("makes its manual dialog inert while the newer arrival's check is pending", async () => {
        const user = userEvent.setup();
        const second = deferred<string>();
        buildMocks.checkDeployedBuild.mockResolvedValueOnce("stale").mockReturnValueOnce(second.promise);
        const router = renderRouted(HOST_LINK);
        await screen.findByText(multiplayerEn.page.updateFailedTitle);

        await go(router, JOIN_LINK);
        expect(stashedKind()).toBe("join");
        await user.click(screen.getByRole("button", { name: multiplayerEn.page.continueAnyway }));
        await act(async () => {});

        expect(harness.hostMounts).toHaveLength(0);
        expect(stashedKind()).toBe("join");

        await act(async () => second.resolve("current"));
        await waitFor(() => expect(lookupJoinTarget).toHaveBeenCalledOnce());
        expect(harness.hostMounts).toHaveLength(0);
        expect(screen.queryByText(multiplayerEn.page.updateFailedTitle)).not.toBeInTheDocument();
        expect(sessionStorage.getItem(STASH_KEY)).toBeNull();
      });
    });

    describe("\"Client out of date\" Refresh", () => {
      async function openOutOfDateDialog(): Promise<TestRouter> {
        resolveGuest.mockResolvedValue({ ok: false, reason: "build_mismatch", message: "m" });
        const router = renderRouted(JOIN_LINK);
        await screen.findByTestId("my-decks");
        act(() => (harness.myDecks!.onSelectDeck as (name: string) => void)("Deck"));
        await screen.findByText(multiplayerEn.page.joinErrorOutOfDateTitle);
        return router;
      }

      function refreshButton(): HTMLElement {
        return screen.getByRole("button", { name: multiplayerEn.page.joinErrorRefresh });
      }

      it("goes through the updater on a stale build", async () => {
        const user = userEvent.setup();
        await openOutOfDateDialog();
        const update = deferred<string>();
        buildMocks.checkDeployedBuild.mockResolvedValueOnce("stale");
        buildMocks.updateToLatestBuild.mockReturnValueOnce(update.promise);

        await user.click(refreshButton());

        expect(await screen.findByText(multiplayerEn.page.updatingTitle)).toBeInTheDocument();
        expect(screen.getByText(multiplayerEn.page.updatingMessage)).toBeInTheDocument();
        expect(screen.queryByText(multiplayerEn.page.joinErrorOutOfDateTitle)).not.toBeInTheDocument();
        expect(buildMocks.updateToLatestBuild).toHaveBeenCalledWith({ deadlineMs: 15_000 });

        await act(async () => update.resolve("manual"));
        await waitFor(() => expect(screen.queryByText(multiplayerEn.page.updatingTitle)).not.toBeInTheDocument());
        expect(buildMocks.reloadIfNoLiveGame).toHaveBeenCalledOnce();
      });

      it("leaves the updating dialog up while the updater reloads", async () => {
        const user = userEvent.setup();
        await openOutOfDateDialog();
        buildMocks.checkDeployedBuild.mockResolvedValueOnce("stale");
        buildMocks.updateToLatestBuild.mockResolvedValueOnce("reloading");

        await user.click(refreshButton());

        expect(await screen.findByText(multiplayerEn.page.updatingTitle)).toBeInTheDocument();
        await act(async () => {});
        expect(screen.getByText(multiplayerEn.page.updatingTitle)).toBeInTheDocument();
        expect(buildMocks.reloadIfNoLiveGame).not.toHaveBeenCalled();
      });

      it("neither reloads nor clears a bot-link gate's updating dialog when its own update fails", async () => {
        const user = userEvent.setup();
        const router = await openOutOfDateDialog();
        const refreshUpdate = deferred<string>();
        buildMocks.checkDeployedBuild.mockResolvedValue("stale");
        buildMocks.updateToLatestBuild.mockReturnValueOnce(refreshUpdate.promise).mockReturnValue(new Promise(() => {}));
        await user.click(refreshButton());
        await screen.findByText(multiplayerEn.page.updatingTitle);

        await go(router, HOST_LINK);
        await waitFor(() => expect(buildMocks.updateToLatestBuild).toHaveBeenCalledTimes(2));
        await act(async () => refreshUpdate.resolve("manual"));

        expect(buildMocks.reloadIfNoLiveGame).not.toHaveBeenCalled();
        expect(screen.getByText(multiplayerEn.page.updatingTitle)).toBeInTheDocument();
        expect(stashedKind()).toBe("host");
      });

      it("does not reload over a bot-link gate that proceeded while its update ran", async () => {
        const user = userEvent.setup();
        const router = await openOutOfDateDialog();
        const refreshUpdate = deferred<string>();
        buildMocks.checkDeployedBuild.mockResolvedValueOnce("stale").mockResolvedValueOnce("unknown");
        buildMocks.updateToLatestBuild.mockReturnValueOnce(refreshUpdate.promise);
        await user.click(refreshButton());
        await screen.findByText(multiplayerEn.page.updatingTitle);

        await go(router, HOST_LINK);
        await screen.findByTestId("host-setup");
        await act(async () => refreshUpdate.resolve("manual"));

        expect(buildMocks.reloadIfNoLiveGame).not.toHaveBeenCalled();
        expect(seedOf(harness.hostSetup)).toEqual(SEED);
        expect(sessionStorage.getItem(STASH_KEY)).toBeNull();
        expect(screen.queryByText(multiplayerEn.page.updatingTitle)).not.toBeInTheDocument();
      });

      it.each(["current", "unknown"])("reloads at once on a %s build", async (build) => {
        const user = userEvent.setup();
        await openOutOfDateDialog();
        buildMocks.checkDeployedBuild.mockResolvedValueOnce(build);

        await user.click(refreshButton());

        await waitFor(() => expect(buildMocks.reloadIfNoLiveGame).toHaveBeenCalledOnce());
        expect(buildMocks.updateToLatestBuild).not.toHaveBeenCalled();
        expect(screen.queryByText(multiplayerEn.page.updatingTitle)).not.toBeInTheDocument();
        expect(toastMessage()).toBeUndefined();
      });

      it.each(["current", "stale"])("never reloads a live game (%s build)", async (build) => {
        const user = userEvent.setup();
        await openOutOfDateDialog();
        buildMocks.checkDeployedBuild.mockResolvedValueOnce(build);
        buildMocks.reloadIfNoLiveGame.mockReturnValue(false);

        await user.click(refreshButton());

        await waitFor(() => expect(toastMessage()).toBe(multiplayerEn.page.refreshAfterGame));
        expect(reload).not.toHaveBeenCalled();
        expect(screen.queryByText(multiplayerEn.page.joinErrorOutOfDateTitle)).not.toBeInTheDocument();
        expect(screen.queryByText(multiplayerEn.page.updatingTitle)).not.toBeInTheDocument();
        expect(screen.queryByText(multiplayerEn.page.updateFailedTitle)).not.toBeInTheDocument();
      });
    });
  });
});
