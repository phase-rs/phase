import { cleanup, render, screen, waitFor } from "@testing-library/react";
import { useEffect } from "react";
import { MemoryRouter } from "react-router";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { LobbyGame } from "../../adapter/types";

/**
 * The join/spectate origin the lobby produced must ride all the way onto the
 * route. These cases drive the real page with a stubbed `LobbyView` that
 * invokes the callback under test, and read the navigation the page performed.
 * The real `multiplayerStore` module is used (everything but the one export
 * below is `importOriginal`) so the origin constructors under test are the
 * production ones.
 */
const harness = vi.hoisted(() => ({
  navigate: vi.fn(),
  lobbyAction: null as null | ((props: Record<string, unknown>) => void),
  connectionMode: undefined as string | undefined,
}));

/**
 * `findLobbyGameByCode` reads the store module's private per-source channel
 * snapshots, which nothing this file renders can populate. Replacing that one
 * export is what makes a cross-source `game_code` COLLISION expressible: the
 * spectate context lookup must consult only the authority being watched.
 */
const storeMocks = vi.hoisted(() => ({ findLobbyGameByCode: vi.fn() }));
vi.mock("../../stores/multiplayerStore", async (importOriginal) => ({
  ...(await importOriginal<typeof import("../../stores/multiplayerStore")>()),
  findLobbyGameByCode: storeMocks.findLobbyGameByCode,
}));

vi.mock("react-router", async (importOriginal) => ({
  ...(await importOriginal<typeof import("react-router")>()),
  useNavigate: () => harness.navigate,
}));

vi.mock("../../components/lobby/LobbyView", () => ({
  LobbyView: (props: Record<string, unknown>) => {
    harness.connectionMode = props.connectionMode as string | undefined;
    useEffect(() => {
      harness.lobbyAction?.(props);
      // eslint-disable-next-line react-hooks/exhaustive-deps
    }, []);
    return <div data-testid="lobby" />;
  },
}));

vi.mock("../../components/menu/MyDecks", () => ({
  MyDecks: ({
    mode,
    onSelectDeck,
  }: {
    mode?: string;
    onSelectDeck: (name: string) => void;
  }) => {
    useEffect(() => {
      if (mode === "select") onSelectDeck("Test Deck");
      // eslint-disable-next-line react-hooks/exhaustive-deps
    }, [mode]);
    return <div data-testid="my-decks" />;
  },
}));

vi.mock("../../components/lobby/HostSetup", () => ({ HostSetup: () => null }));
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
import {
  adHocLobbySource,
  useMultiplayerStore,
  type LobbySource,
} from "../../stores/multiplayerStore";

const HOSTING_URL = "wss://hosting.example/ws";
const ORIGIN_URL = "wss://play.example.com/ws";
const origin = adHocLobbySource(ORIGIN_URL) as LobbySource;

const lookupJoinTarget = vi.fn();

function draftGame(): LobbyGame {
  return {
    game_code: "ABC123",
    host_name: "Alice",
    created_at: 1_700_000_000,
    has_password: false,
    draft_metadata: { setCode: "TST", draftKind: "Premier" },
  } as LobbyGame;
}

function renderPage(
  entry: string | { pathname: string; search: string; state: unknown } = "/multiplayer",
) {
  return render(
    <MemoryRouter initialEntries={[entry as never]}>
      <MultiplayerPage />
    </MemoryRouter>,
  );
}

/** The path the page navigated to, as parsed search params. */
function navigatedParams(): { path: string; params: URLSearchParams } {
  const call = harness.navigate.mock.calls.find(
    ([target]) => typeof target === "string" && target !== "/multiplayer",
  );
  const target = call?.[0] as string;
  const [path, search = ""] = target.split("?");
  return { path, params: new URLSearchParams(search) };
}

describe("MultiplayerPage join origin", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    // `clearAllMocks` drops calls but keeps queued implementations.
    storeMocks.findLobbyGameByCode.mockReset();
    harness.lobbyAction = null;
    harness.connectionMode = undefined;
    localStorage.setItem("active-deck", "Test Deck");
    lookupJoinTarget.mockResolvedValue({
      ok: true,
      info: { is_p2p: false, format_config: null },
    });
    // zustand actions are plain state fields, so `setState` swaps them.
    useMultiplayerStore.setState({
      hostingServer: HOSTING_URL,
      userLobbySources: [],
      sourceStatus: new Map(),
      displayName: "Tester",
      toasts: new Map(),
      lookupJoinTarget,
      resolveGuest: vi.fn(),
    });
  });

  afterEach(cleanup);

  function toastMessages(): string[] {
    return [...useMultiplayerStore.getState().toasts.values()].map((t) => t.message);
  }

  it("carries the join origin on the /game route", async () => {
    harness.lobbyAction = (props) => {
      (props.onJoinGame as (code: string, origin: LobbySource) => void)("ABC123", origin);
    };

    renderPage();

    await waitFor(() => {
      expect(harness.navigate).toHaveBeenCalled();
    });
    const { path, params } = navigatedParams();
    expect(path).toMatch(/^\/game\//);
    expect(params.get("mode")).toBe("join");
    expect(params.get("code")).toBe("ABC123");
    expect(params.get("server")).toBe(ORIGIN_URL);
    // The lookup went to the same authority, not to the hosting server.
    expect(lookupJoinTarget).toHaveBeenCalledWith("ABC123", origin, undefined);
  });

  it("carries the spectate origin on the /game route", async () => {
    harness.lobbyAction = (props) => {
      (props.onSpectate as (code: string, origin: LobbySource) => void)("ABC123", origin);
    };

    renderPage();

    await waitFor(() => {
      expect(harness.navigate).toHaveBeenCalled();
    });
    const { path, params } = navigatedParams();
    expect(path).toMatch(/^\/game\//);
    expect(params.get("mode")).toBe("spectate");
    expect(params.get("server")).toBe(ORIGIN_URL);
  });

  it("carries the spectate origin on the /draft-spectator route", async () => {
    harness.lobbyAction = (props) => {
      (props.onSpectate as (code: string, origin: LobbySource, context?: LobbyGame) => void)(
        "ABC123",
        origin,
        draftGame(),
      );
    };

    renderPage();

    await waitFor(() => {
      expect(harness.navigate).toHaveBeenCalled();
    });
    const { path, params } = navigatedParams();
    expect(path).toBe("/draft-spectator");
    expect(params.get("code")).toBe("ABC123");
    expect(params.get("server")).toBe(ORIGIN_URL);
  });

  /** The same `game_code` listed by two authorities: unscoped resolves to the
   * one that is NOT the origin, exactly as the real derived-order scan would. */
  const LISTING_URL = "wss://other.example/ws";
  const listingSource = adHocLobbySource(LISTING_URL) as LobbySource;
  function collidingLookup() {
    storeMocks.findLobbyGameByCode.mockImplementation(
      (_code: string, sourceUrl?: string) =>
        sourceUrl === undefined || sourceUrl === LISTING_URL
          ? { game: draftGame(), source: listingSource }
          : undefined,
    );
  }

  it("routes to the draft spectator when the origin itself lists the draft", async () => {
    collidingLookup();
    harness.lobbyAction = (props) => {
      (props.onSpectate as (code: string, origin: LobbySource) => void)(
        "ABC123",
        listingSource,
      );
    };

    renderPage();

    await waitFor(() => {
      expect(harness.navigate).toHaveBeenCalled();
    });
    // Paired positive for the case below: this row DOES route to the draft
    // spectator when it is looked up on the authority being watched.
    const { path, params } = navigatedParams();
    expect(path).toBe("/draft-spectator");
    expect(params.get("server")).toBe(LISTING_URL);
    expect(storeMocks.findLobbyGameByCode).toHaveBeenCalledWith("ABC123", LISTING_URL);
  });

  it("ignores a draft row listed by a source other than the spectate origin", async () => {
    collidingLookup();
    harness.lobbyAction = (props) => {
      (props.onSpectate as (code: string, origin: LobbySource) => void)("ABC123", origin);
    };

    renderPage();

    await waitFor(() => {
      expect(harness.navigate).toHaveBeenCalled();
    });
    // The collision is invisible from this origin, so the code stays a game:
    // the reach-guard is that it navigated at all, to /game with the origin.
    const { path, params } = navigatedParams();
    expect(path).toMatch(/^\/game\//);
    expect(params.get("mode")).toBe("spectate");
    expect(params.get("server")).toBe(ORIGIN_URL);
    expect(storeMocks.findLobbyGameByCode).toHaveBeenCalledWith("ABC123", ORIGIN_URL);
  });

  it("routes a typed code the server does not know to the draft spectator with its origin", async () => {
    lookupJoinTarget.mockResolvedValue({
      ok: false,
      reason: "not_found",
      message: "No such game",
    });
    harness.lobbyAction = (props) => {
      (props.onSpectate as (code: string, origin: LobbySource) => void)("ABC123", origin);
    };

    renderPage();

    await waitFor(() => {
      expect(harness.navigate).toHaveBeenCalled();
    });
    const { path, params } = navigatedParams();
    expect(path).toBe("/draft-spectator");
    expect(params.get("server")).toBe(ORIGIN_URL);
  });

  it("does not navigate when the spectate lookup fails for another reason", async () => {
    lookupJoinTarget.mockResolvedValue({
      ok: false,
      reason: "room_full",
      message: "That table is full",
    });
    harness.lobbyAction = (props) => {
      (props.onSpectate as (code: string, origin: LobbySource) => void)("ABC123", origin);
    };

    renderPage();

    // Paired positive: the failure is reported, so the "no navigation"
    // assertion is not just "nothing happened".
    await waitFor(() => {
      expect(toastMessages()).toContain("That table is full");
    });
    expect(harness.navigate).not.toHaveBeenCalled();
  });

  it("re-joins on the origin carried by the deck-rejected state", async () => {
    renderPage({
      pathname: "/multiplayer",
      search: "",
      state: { deckRejected: true, reason: "bad deck", joinCode: "ABC123", server: ORIGIN_URL },
    });

    await waitFor(() => {
      expect(navigatedParams().params.get("server")).toBe(ORIGIN_URL);
    });
  });

  it("re-joins on the hosting server when the rejection carried no origin", async () => {
    renderPage({
      pathname: "/multiplayer",
      search: "",
      state: { deckRejected: true, reason: "bad deck", joinCode: "ABC123" },
    });

    await waitFor(() => {
      expect(navigatedParams().params.get("server")).toBe(HOSTING_URL);
    });
  });

  it("refuses a re-join with no authority left to join through", async () => {
    useMultiplayerStore.setState({ hostingServer: null });

    renderPage({
      pathname: "/multiplayer",
      search: "",
      state: { deckRejected: true, reason: "bad deck", joinCode: "ABC123" },
    });

    await waitFor(() => {
      expect(toastMessages()).toContain("Pick a server before joining a game.");
    });
    expect(
      harness.navigate.mock.calls.filter(([target]) => target !== "/multiplayer"),
    ).toHaveLength(0);
  });

  it("mounts in P2P mode when the hosting server is None", async () => {
    useMultiplayerStore.setState({ hostingServer: null });

    renderPage();

    await screen.findByTestId("lobby");
    expect(harness.connectionMode).toBe("p2p");
  });

  it("navigates a direct P2P code with no origin and no server param", async () => {
    harness.lobbyAction = (props) => {
      (props.onJoinGame as (code: string, origin: LobbySource | null) => void)("ABCDE", null);
    };

    renderPage();

    await waitFor(() => {
      expect(harness.navigate).toHaveBeenCalled();
    });
    const { params } = navigatedParams();
    expect(params.get("mode")).toBe("p2p-join");
    expect(params.get("server")).toBeNull();
    // A direct code never consults a lobby authority.
    expect(lookupJoinTarget).not.toHaveBeenCalled();
  });
});
