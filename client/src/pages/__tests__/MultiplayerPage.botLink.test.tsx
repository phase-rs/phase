import { StrictMode, useEffect, useState } from "react";
import { act, cleanup, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { createMemoryRouter, RouterProvider, useLocation } from "react-router";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

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

vi.mock("../../components/lobby/HostSetup", () => ({
  HostSetup: (props: Record<string, unknown>) => {
    harness.hostSetup = props;
    const [firstProps] = useState(props);
    useEffect(() => {
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
import { hostLinkSearch, type HostSeed } from "../multiplayerPageState";
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
  beforeEach(() => {
    vi.clearAllMocks();
    harness.hostSetup = null;
    harness.lobby = null;
    harness.myDecks = null;
    harness.hostMounts = [];
    harness.deckBuilderSearches = [];
    localStorage.clear();
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
      ensureSubscriptionSocket: vi.fn(async () => null),
    });
  });

  afterEach(() => {
    cleanup();
    vi.unstubAllGlobals();
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
    });
  });

  describe("guest arrival", () => {
    it("waits for a host that has not opened the game, and retries on the link's origin", async () => {
      const user = userEvent.setup();
      lookupJoinTarget.mockResolvedValueOnce(joinTargetNotFound);
      renderRouted(JOIN_LINK);

      expect(await screen.findByText(multiplayerEn.page.waitingForHostTitle)).toBeInTheDocument();
      expect(screen.getByText(multiplayerEn.page.waitingForHostMessage)).toBeInTheDocument();

      await user.click(screen.getByRole("button", { name: "Retry" }));

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
});
