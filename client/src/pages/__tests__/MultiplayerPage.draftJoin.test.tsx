import { cleanup, render, screen, waitFor } from "@testing-library/react";
import { useEffect } from "react";
import { MemoryRouter } from "react-router";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { LobbyGame } from "../../adapter/types";
import { refuseRealWebSockets } from "../../test/helpers/refusingWebSocket";

/**
 * Drives the real page with a stubbed `LobbyView` that invokes the callback
 * under test, mirroring `MultiplayerPage.joinOrigin.test.tsx`'s harness. The
 * real `multiplayerStore` and `multiplayerDraftStore` modules run: this suite
 * measures the production resolve-then-dial loop and the production
 * `joinDraft`, not a stand-in for either.
 */
const harness = vi.hoisted(() => ({
  navigate: vi.fn(),
  lobbyAction: null as null | ((props: Record<string, unknown>) => void),
  /** Empty means the deck picker was never mounted with a mode — "no deck
   * selection" for the tests below. */
  myDecksModes: [] as (string | undefined)[],
}));

const storeMocks = vi.hoisted(() => ({ findLobbyGameByCode: vi.fn() }));
vi.mock("../../stores/multiplayerStore", async (importOriginal) => ({
  ...(await importOriginal<typeof import("../../stores/multiplayerStore")>()),
  findLobbyGameByCode: storeMocks.findLobbyGameByCode,
}));

/**
 * `network/connection` stays real except for `joinRoom`, which is the one
 * seam that would otherwise open a live PeerJS signaling connection. A
 * rejecting stub ends the guest's "new"-seat dial after exactly one call.
 */
const connectionMocks = vi.hoisted(() => ({
  joinRoom: vi.fn(async (_code: string, _signal?: AbortSignal, _timeoutMs?: number) => {
    throw new Error("test stub: no live PeerJS connection");
  }),
}));
vi.mock("../../network/connection", async (importOriginal) => ({
  ...(await importOriginal<typeof import("../../network/connection")>()),
  joinRoom: connectionMocks.joinRoom,
}));

vi.mock("react-router", async (importOriginal) => ({
  ...(await importOriginal<typeof import("react-router")>()),
  useNavigate: () => harness.navigate,
}));

vi.mock("../../components/lobby/LobbyView", () => ({
  LobbyView: (props: Record<string, unknown>) => {
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
      harness.myDecksModes.push(mode);
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

vi.mock("../../stores/gameStore", () => ({
  useGameStore: { setState: vi.fn() },
  saveActiveGame: vi.fn(),
}));

// `importOriginal` rather than a bespoke stub: `multiplayerDraftStore` runs
// real (unlike `joinOrigin.test.tsx`, which stubs it).
vi.mock("../../constants/storage", async (importOriginal) => ({
  ...(await importOriginal<typeof import("../../constants/storage")>()),
  loadActiveDeck: () => ({ main: ["Island"], sideboard: [] }),
  touchDeckPlayed: vi.fn(),
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
import {
  adHocLobbySource,
  useMultiplayerStore,
  type LobbySource,
} from "../../stores/multiplayerStore";
import { useMultiplayerDraftStore } from "../../stores/multiplayerDraftStore";
import { ACTIVE_DECK_KEY } from "../../constants/storage";

const HOSTING_URL = "wss://hosting.example/ws";
const ORIGIN_URL = "wss://play.example.com/ws";
const ORIGIN = adHocLobbySource(ORIGIN_URL) as LobbySource;

const lookupJoinTarget = vi.fn();
const resolveGuest = vi.fn();

const p2pDraftRow: LobbyGame = {
  game_code: "ABC123",
  host_name: "Alice",
  created_at: 1_700_000_000,
  has_password: false,
  is_p2p: true,
  draft_metadata: { setCode: "MKM", draftKind: "Premier" },
};

function renderPage() {
  return render(
    <MemoryRouter initialEntries={["/multiplayer"]}>
      <MultiplayerPage />
    </MemoryRouter>,
  );
}

function toastMessages(): string[] {
  return [...useMultiplayerStore.getState().toasts.values()].map((t) => t.message);
}

/** The path/params the page navigated to. */
function navigatedParams(): { path: string; params: URLSearchParams } {
  const call = harness.navigate.mock.calls.find(
    ([target]) => typeof target === "string" && target !== "/multiplayer" && target !== "/",
  );
  const target = call?.[0] as string;
  const [path, search = ""] = target.split("?");
  return { path, params: new URLSearchParams(search) };
}

describe("MultiplayerPage draft join routing", () => {
  let socketUrls: string[] = [];

  beforeEach(() => {
    vi.clearAllMocks();
    // `clearAllMocks` drops calls but keeps queued `mockResolvedValueOnce`
    // implementations (e.g. a queued password_required→ok sequence).
    storeMocks.findLobbyGameByCode.mockReset();
    harness.myDecksModes = [];
    socketUrls = refuseRealWebSockets();
    connectionMocks.joinRoom.mockReset();
    connectionMocks.joinRoom.mockImplementation(async () => {
      throw new Error("test stub: no live PeerJS connection");
    });
    lookupJoinTarget.mockReset();
    resolveGuest.mockReset();
    harness.lobbyAction = null;
    localStorage.setItem(ACTIVE_DECK_KEY, "Test Deck");
    lookupJoinTarget.mockResolvedValue({
      ok: true,
      info: { is_p2p: false, format_config: null },
    });
    resolveGuest.mockResolvedValue({
      ok: true,
      peerInfo: {
        game_code: "ABC123",
        host_peer_id: "phase2-ABCDE",
        match_config: { match_type: "Bo1" },
        player_count: 8,
        filled_seats: 1,
      },
    });
    useMultiplayerStore.setState({
      hostingServer: HOSTING_URL,
      userLobbySources: [],
      sourceStatus: new Map(),
      displayName: "Tester",
      toasts: new Map(),
      lookupJoinTarget,
      resolveGuest,
    });
  });

  afterEach(async () => {
    const opened = [...socketUrls];
    await useMultiplayerDraftStore.getState().leave(true);
    cleanup();
    vi.unstubAllGlobals();
    expect(opened).toEqual([]);
  });

  it("resolves through the broker and dials the host peer, never the listing code", async () => {
    harness.lobbyAction = (props) => {
      (
        props.onJoinGame as (
          code: string,
          origin: LobbySource,
          password: string | undefined,
          format: undefined,
          context: LobbyGame,
        ) => void
      )("ABC123", ORIGIN, undefined, undefined, p2pDraftRow);
    };

    renderPage();

    await waitFor(() => {
      expect(connectionMocks.joinRoom).toHaveBeenCalled();
    });

    expect(resolveGuest).toHaveBeenCalledWith("ABC123", ORIGIN, undefined);
    expect(connectionMocks.joinRoom).toHaveBeenCalledTimes(1);
    expect(connectionMocks.joinRoom.mock.calls[0][0]).toBe("ABCDE");
    expect(connectionMocks.joinRoom.mock.calls[0][0]).not.toBe("ABC123");
  });

  it("forwards the row's password into the first resolve", async () => {
    harness.lobbyAction = (props) => {
      (
        props.onJoinGame as (
          code: string,
          origin: LobbySource,
          password: string | undefined,
          format: undefined,
          context: LobbyGame,
        ) => void
      )("ABC123", ORIGIN, "pw", undefined, p2pDraftRow);
    };

    renderPage();

    await waitFor(() => {
      expect(resolveGuest).toHaveBeenCalled();
    });
    expect(resolveGuest).toHaveBeenCalledWith("ABC123", ORIGIN, "pw");
  });

  it("re-prompts on password_required and retries on the same socket", async () => {
    resolveGuest
      .mockResolvedValueOnce({ ok: false, reason: "password_required", message: "Password required" })
      .mockResolvedValueOnce({
        ok: true,
        peerInfo: {
          game_code: "ABC123",
          host_peer_id: "phase2-ABCDE",
          match_config: { match_type: "Bo1" },
          player_count: 8,
          filled_seats: 1,
        },
      });
    // happy-dom does not implement `window.prompt`, so `vi.spyOn` has nothing
    // to wrap — `vi.stubGlobal` is the idiom this codebase uses elsewhere
    // (`MyDecks.test.tsx`).
    vi.stubGlobal("prompt", vi.fn(() => "pw2"));
    harness.lobbyAction = (props) => {
      (
        props.onJoinGame as (
          code: string,
          origin: LobbySource,
          password: string | undefined,
          format: undefined,
          context: LobbyGame,
        ) => void
      )("ABC123", ORIGIN, undefined, undefined, p2pDraftRow);
    };

    renderPage();

    await waitFor(() => {
      expect(connectionMocks.joinRoom).toHaveBeenCalled();
    });

    expect(resolveGuest).toHaveBeenNthCalledWith(1, "ABC123", ORIGIN, undefined);
    expect(resolveGuest).toHaveBeenNthCalledWith(2, "ABC123", ORIGIN, "pw2");
    expect(connectionMocks.joinRoom.mock.calls[0][0]).toBe("ABCDE");
  });

  it("shows the 'Can't join this room' dialog on room_full, without dialing", async () => {
    resolveGuest.mockResolvedValue({
      ok: false,
      reason: "room_full",
      message: "Game ABC123 is full",
    });
    harness.lobbyAction = (props) => {
      (
        props.onJoinGame as (
          code: string,
          origin: LobbySource,
          password: string | undefined,
          format: undefined,
          context: LobbyGame,
        ) => void
      )("ABC123", ORIGIN, undefined, undefined, p2pDraftRow);
    };

    renderPage();

    await screen.findByText("Game ABC123 is full");
    expect(screen.getByText("Can't join this room")).toBeInTheDocument();
    expect(connectionMocks.joinRoom).not.toHaveBeenCalled();
  });

  it("classifies a typed code from the join origin's own listing", async () => {
    storeMocks.findLobbyGameByCode.mockImplementation(
      (code: string, sourceUrl?: string) =>
        code === "ABC123" && sourceUrl === ORIGIN_URL
          ? { game: p2pDraftRow, source: ORIGIN }
          : undefined,
    );
    harness.lobbyAction = (props) => {
      (props.onJoinGame as (code: string, origin: LobbySource) => void)("ABC123", ORIGIN);
    };

    renderPage();

    await waitFor(() => {
      expect(connectionMocks.joinRoom).toHaveBeenCalled();
    });

    expect(connectionMocks.joinRoom.mock.calls[0][0]).toBe("ABCDE");
    expect(lookupJoinTarget).not.toHaveBeenCalled();
    expect(storeMocks.findLobbyGameByCode).toHaveBeenCalledWith("ABC123", ORIGIN_URL);
  });

  it("refuses to watch a P2P draft row", async () => {
    harness.lobbyAction = (props) => {
      (
        props.onSpectate as (code: string, origin: LobbySource, context: LobbyGame) => void
      )("ABC123", ORIGIN, p2pDraftRow);
    };

    renderPage();

    await waitFor(() => {
      expect(toastMessages()).toContain("Player-hosted draft pods can't be watched.");
    });
    expect(harness.navigate).not.toHaveBeenCalledWith(
      expect.stringContaining("/draft-spectator"),
    );
  });

  it("refuses a non-P2P draft row without contacting the broker", async () => {
    harness.lobbyAction = (props) => {
      (
        props.onJoinGame as (
          code: string,
          origin: LobbySource,
          password: string | undefined,
          format: undefined,
          context: LobbyGame,
        ) => void
      )("ABC123", ORIGIN, undefined, undefined, { ...p2pDraftRow, is_p2p: undefined });
    };

    renderPage();

    await waitFor(() => {
      expect(toastMessages()).toContain("Server-hosted draft pods can't be joined from the lobby.");
    });
    expect(resolveGuest).not.toHaveBeenCalled();
    expect(connectionMocks.joinRoom).not.toHaveBeenCalled();
  });

  it("refuses to join while already in a live draft pod", async () => {
    useMultiplayerDraftStore.setState({ role: "host", phase: "lobby" });
    harness.lobbyAction = (props) => {
      (
        props.onJoinGame as (
          code: string,
          origin: LobbySource,
          password: string | undefined,
          format: undefined,
          context: LobbyGame,
        ) => void
      )("ABC123", ORIGIN, undefined, undefined, p2pDraftRow);
    };

    renderPage();

    await waitFor(() => {
      expect(toastMessages()).toContain(
        "You're already in a draft pod. Leave it before joining another.",
      );
    });
    expect(resolveGuest).not.toHaveBeenCalled();
    expect(connectionMocks.joinRoom).not.toHaveBeenCalled();
  });

  it("a non-live phase with a role set does not block the join", async () => {
    useMultiplayerDraftStore.setState({ role: "guest", phase: "complete" });
    harness.lobbyAction = (props) => {
      (
        props.onJoinGame as (
          code: string,
          origin: LobbySource,
          password: string | undefined,
          format: undefined,
          context: LobbyGame,
        ) => void
      )("ABC123", ORIGIN, undefined, undefined, p2pDraftRow);
    };

    renderPage();

    await waitFor(() => {
      expect(connectionMocks.joinRoom).toHaveBeenCalled();
    });
    expect(connectionMocks.joinRoom.mock.calls[0][0]).toBe("ABCDE");
  });

  it("a constructed P2P row still resolves and navigates to /game", async () => {
    lookupJoinTarget.mockResolvedValue({
      ok: true,
      info: { is_p2p: true, format_config: null },
    });
    const constructedRow: LobbyGame = {
      game_code: "ABC123",
      host_name: "Alice",
      created_at: 1_700_000_000,
      has_password: false,
      is_p2p: true,
      format: "Standard",
    };
    harness.lobbyAction = (props) => {
      (
        props.onJoinGame as (
          code: string,
          origin: LobbySource,
          password: string | undefined,
          format: string,
          context: LobbyGame,
        ) => void
      )("ABC123", ORIGIN, undefined, "Standard", constructedRow);
    };

    renderPage();

    await waitFor(() => {
      expect(harness.navigate).toHaveBeenCalled();
    });
    const { path, params } = navigatedParams();
    expect(path).toMatch(/^\/game\//);
    expect(params.get("mode")).toBe("p2p-join");
    expect(params.get("code")).toBe("ABCDE");
    expect(connectionMocks.joinRoom).not.toHaveBeenCalled();
    expect(harness.myDecksModes).toContain("select");
  });

  it("resolves a typed code of a player-hosted draft with no local lobby row", async () => {
    lookupJoinTarget.mockResolvedValue({
      ok: true,
      info: {
        is_p2p: true,
        format_config: null,
        draft_metadata: { setCode: "MKM", draftKind: "Premier" },
      },
    });
    harness.lobbyAction = (props) => {
      (props.onJoinGame as (code: string, origin: LobbySource) => void)("ABC123", ORIGIN);
    };

    renderPage();

    await waitFor(() => {
      expect(connectionMocks.joinRoom).toHaveBeenCalled();
    });

    expect(lookupJoinTarget).toHaveBeenCalledWith("ABC123", ORIGIN, undefined);
    expect(resolveGuest).toHaveBeenCalledWith("ABC123", ORIGIN, undefined);
    expect(connectionMocks.joinRoom).toHaveBeenCalledTimes(1);
    expect(connectionMocks.joinRoom.mock.calls[0][0]).toBe("ABCDE");
    expect(harness.myDecksModes).toEqual([]);
    expect(harness.navigate).not.toHaveBeenCalledWith(expect.stringContaining("/game/"));
  });

  it("refuses a typed code of a server-hosted draft with no local lobby row", async () => {
    lookupJoinTarget.mockResolvedValue({
      ok: true,
      info: {
        is_p2p: false,
        format_config: null,
        draft_metadata: { setCode: "MKM", draftKind: "Premier" },
      },
    });
    harness.lobbyAction = (props) => {
      (props.onJoinGame as (code: string, origin: LobbySource) => void)("ABC123", ORIGIN);
    };

    renderPage();

    await waitFor(() => {
      expect(toastMessages()).toContain("Server-hosted draft pods can't be joined from the lobby.");
    });
    expect(resolveGuest).not.toHaveBeenCalled();
    expect(connectionMocks.joinRoom).not.toHaveBeenCalled();
    expect(harness.myDecksModes).toEqual([]);
  });

  it("forwards the entered password when resolving a typed draft code's password_required retry", async () => {
    lookupJoinTarget
      .mockResolvedValueOnce({ ok: false, reason: "password_required", message: "Password required" })
      .mockResolvedValueOnce({
        ok: true,
        info: {
          is_p2p: true,
          format_config: null,
          draft_metadata: { setCode: "MKM", draftKind: "Premier" },
        },
      });
    vi.stubGlobal("prompt", vi.fn(() => "pw2"));
    harness.lobbyAction = (props) => {
      (props.onJoinGame as (code: string, origin: LobbySource) => void)("ABC123", ORIGIN);
    };

    renderPage();

    await waitFor(() => {
      expect(connectionMocks.joinRoom).toHaveBeenCalled();
    });

    expect(lookupJoinTarget).toHaveBeenCalledTimes(2);
    expect(lookupJoinTarget).toHaveBeenNthCalledWith(2, "ABC123", ORIGIN, "pw2");
    expect(resolveGuest).toHaveBeenCalledWith("ABC123", ORIGIN, "pw2");
    expect(connectionMocks.joinRoom.mock.calls[0][0]).toBe("ABCDE");
    expect(harness.myDecksModes).toEqual([]);
  });

  it("refuses to watch a player-hosted draft pod with no local lobby row", async () => {
    lookupJoinTarget.mockResolvedValue({
      ok: true,
      info: {
        is_p2p: true,
        format_config: null,
        draft_metadata: { setCode: "MKM", draftKind: "Premier" },
      },
    });
    harness.lobbyAction = (props) => {
      (props.onSpectate as (code: string, origin: LobbySource) => void)("ABC123", ORIGIN);
    };

    renderPage();

    await waitFor(() => {
      expect(toastMessages()).toContain("Player-hosted draft pods can't be watched.");
    });
    expect(harness.navigate).not.toHaveBeenCalledWith(
      expect.stringContaining("/draft-spectator"),
    );
  });

  it("routes a typed code of a server-hosted draft to the draft spectator", async () => {
    lookupJoinTarget.mockResolvedValue({
      ok: true,
      info: {
        is_p2p: false,
        format_config: null,
        draft_metadata: { setCode: "MKM", draftKind: "Premier" },
      },
    });
    harness.lobbyAction = (props) => {
      (props.onSpectate as (code: string, origin: LobbySource) => void)("ABC123", ORIGIN);
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
});
