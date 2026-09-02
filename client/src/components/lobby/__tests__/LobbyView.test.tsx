import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act, cleanup, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";

import type { LobbyGame } from "../../../adapter/types";
import type { PhaseSocket } from "../../../services/openPhaseSocket";
import { LobbyView } from "../LobbyView";
import { SERVER_PRESETS } from "../../../services/serverDetection";
import {
  useMultiplayerStore,
  type LobbySource,
} from "../../../stores/multiplayerStore";

/**
 * `findLobbyGameByCode` reads the store module's private per-source channel
 * snapshots, which no store action these tests stub can populate. Replacing
 * that one export (everything else stays actual, so `useMultiplayerStore` is
 * the same singleton the component uses) is what makes a cross-source code
 * COLLISION expressible: the rescan can be made to name a different authority
 * than the socket a frame arrived on. Default `undefined` matches the real
 * function against empty channels, so the other cases are unaffected.
 */
const storeMocks = vi.hoisted(() => ({ findLobbyGameByCode: vi.fn() }));
vi.mock("../../../stores/multiplayerStore", async (importOriginal) => ({
  ...(await importOriginal<typeof import("../../../stores/multiplayerStore")>()),
  findLobbyGameByCode: storeMocks.findLobbyGameByCode,
}));

/**
 * LobbyView now delegates to the shared subscription socket via the
 * multiplayer store's `subscribeLobby` / `ensureSubscriptionSocket`
 * actions, rather than opening its own `WebSocket` directly. Tests stub
 * those store actions with promise-returning mocks so the component's
 * cleanup paths (offline fallback, unmount-before-subscribe) stay
 * observable without a real socket.
 */
const HOSTING_URL = "wss://us.phase-rs.dev/ws";

function lobbyGame(code: string, roomName: string, createdAt: number): LobbyGame {
  return {
    game_code: code,
    host_name: "Alice",
    room_name: roomName,
    created_at: createdAt,
    has_password: false,
    host_build_commit: "testhash",
  };
}

/**
 * Stand-in for a source's subscription socket: `LobbyView` attaches its
 * ambient (`PlayerCount` / `PasswordRequired`) listener to `ws`, so the test
 * can push server frames onto one specific source's socket.
 */
function ambientSocket() {
  const listeners = new Set<(event: MessageEvent) => void>();
  return {
    socket: {
      ws: {
        addEventListener: (_type: string, fn: (event: MessageEvent) => void) => {
          listeners.add(fn);
        },
        removeEventListener: (_type: string, fn: (event: MessageEvent) => void) => {
          listeners.delete(fn);
        },
      },
    } as unknown as PhaseSocket,
    listenerCount: () => listeners.size,
    emit: (type: string, data: unknown) => {
      const event = { data: JSON.stringify({ type, data }) } as MessageEvent;
      for (const fn of [...listeners]) fn(event);
    },
  };
}

function renderLobby(props: {
  onJoinGame?: (...args: unknown[]) => void;
  onSpectate?: (...args: unknown[]) => void;
}) {
  return render(
    <LobbyView
      onHostGame={vi.fn()}
      onHostP2P={vi.fn()}
      onJoinGame={props.onJoinGame ?? vi.fn()}
      onSpectate={props.onSpectate ?? vi.fn()}
      onServerOffline={vi.fn()}
    />,
  );
}

describe("LobbyView", () => {
  const originalSubscribeLobby = useMultiplayerStore.getState().subscribeLobby;
  const originalEnsureSubscription =
    useMultiplayerStore.getState().ensureSubscriptionSocket;

  beforeEach(() => {
    useMultiplayerStore.setState({
      hostingServer: HOSTING_URL,
      userLobbySources: [],
      sourceStatus: new Map(),
      toasts: new Map(),
    });
  });

  afterEach(() => {
    cleanup();
    vi.clearAllMocks();
    // `clearAllMocks` drops calls but keeps queued return values.
    storeMocks.findLobbyGameByCode.mockReset();
    useMultiplayerStore.setState({
      subscribeLobby: originalSubscribeLobby,
      ensureSubscriptionSocket: originalEnsureSubscription,
    });
  });

  it("calls onServerOffline when the shared subscription socket is unreachable", async () => {
    // Store returns `null` when the socket can't be opened (withReconnect
    // exhausted, invalid URL, etc.). LobbyView's offline fallback fires.
    useMultiplayerStore.setState({
      subscribeLobby: vi.fn().mockResolvedValue(null),
      ensureSubscriptionSocket: vi.fn().mockResolvedValue(null),
    });
    const onServerOffline = vi.fn();
    render(
      <LobbyView
        onHostGame={vi.fn()}
        onHostP2P={vi.fn()}
        onJoinGame={vi.fn()}
        onServerOffline={onServerOffline}
      />,
    );

    // Flush the microtask from `await subscribeLobby()`.
    await Promise.resolve();
    await Promise.resolve();

    expect(onServerOffline).toHaveBeenCalledTimes(1);
  });

  it("does not call onServerOffline when component unmounts before subscribe resolves", async () => {
    // Mount, unmount, THEN resolve the pending subscribe — the effect's
    // `cancelled` guard must suppress the offline callback.
    let resolveSubscribe!: (v: null) => void;
    useMultiplayerStore.setState({
      subscribeLobby: vi
        .fn()
        .mockReturnValue(
          new Promise<null>((r) => {
            resolveSubscribe = r;
          }),
        ),
      ensureSubscriptionSocket: vi.fn().mockResolvedValue(null),
    });
    const onServerOffline = vi.fn();
    const { unmount } = render(
      <LobbyView
        onHostGame={vi.fn()}
        onHostP2P={vi.fn()}
        onJoinGame={vi.fn()}
        onServerOffline={onServerOffline}
      />,
    );

    unmount();
    resolveSubscribe(null);
    await Promise.resolve();

    expect(onServerOffline).not.toHaveBeenCalled();
  });

  it("does not subscribe in p2p mode", () => {
    const subscribeLobby = vi.fn();
    useMultiplayerStore.setState({
      subscribeLobby,
      ensureSubscriptionSocket: vi.fn(),
    });
    render(
      <LobbyView
        onHostGame={vi.fn()}
        onHostP2P={vi.fn()}
        onJoinGame={vi.fn()}
        connectionMode="p2p"
        onServerOffline={vi.fn()}
      />,
    );

    expect(subscribeLobby).not.toHaveBeenCalled();
  });

  // Renamed from "fires offline fallback when the stored server address is
  // invalid": with per-source channels the component never inspects the URL —
  // what it measures is the `subscribeLobby() === null` -> `onServerOffline`
  // wiring, which the mock is what actually produces.
  it("fires the offline fallback when every source failed to open", async () => {
    useMultiplayerStore.setState({
      hostingServer: "wss:",
      // The real store's `subscribeLobby` resolves `null` only when EVERY
      // source's first open settled null; mirror that contract here.
      subscribeLobby: vi.fn().mockResolvedValue(null),
      ensureSubscriptionSocket: vi.fn().mockResolvedValue(null),
    });
    const onServerOffline = vi.fn();

    render(
      <LobbyView
        onHostGame={vi.fn()}
        onHostP2P={vi.fn()}
        onJoinGame={vi.fn()}
        onServerOffline={onServerOffline}
      />,
    );

    await Promise.resolve();
    await Promise.resolve();

    expect(onServerOffline).toHaveBeenCalledTimes(1);
  });

  describe("typed join codes", () => {
    async function typeCode(code: string) {
      const user = userEvent.setup();
      await user.type(screen.getByRole("textbox"), code);
      return user;
    }

    it("joins a CODE@host code on that host without touching the store", async () => {
      const onJoinGame = vi.fn();
      renderLobby({ onJoinGame });
      const user = await typeCode("ABC123@play.example.com");

      await user.click(screen.getByRole("button", { name: "Join" }));

      expect(onJoinGame).toHaveBeenCalledWith("ABC123", {
        url: "wss://play.example.com/ws",
        name: "play.example.com",
        origin: "user",
      });
      // The join origin is carried, not stored: browsing stays where it was.
      expect(useMultiplayerStore.getState().hostingServer).toBe(HOSTING_URL);
      expect(useMultiplayerStore.getState().userLobbySources).toEqual([]);
    });

    // Uppercasing the whole input (the old behaviour) destroys both formats
    // below: `WS://` is no longer a recognised scheme and `LOCALHOST` no
    // longer matches the loopback rule.
    it("preserves an explicit ws:// scheme in the typed address", async () => {
      const onJoinGame = vi.fn();
      renderLobby({ onJoinGame });
      const user = await typeCode("abc123@ws://192.168.1.5:9374");

      await user.click(screen.getByRole("button", { name: "Join" }));

      expect(onJoinGame).toHaveBeenCalledWith(
        "ABC123",
        expect.objectContaining({ url: "ws://192.168.1.5:9374/ws" }),
      );
    });

    it("keeps the loopback rule for a lowercase localhost address", async () => {
      const onJoinGame = vi.fn();
      renderLobby({ onJoinGame });
      const user = await typeCode("abc123@localhost:9374");

      await user.click(screen.getByRole("button", { name: "Join" }));

      expect(onJoinGame).toHaveBeenCalledWith(
        "ABC123",
        expect.objectContaining({ url: "ws://localhost:9374/ws" }),
      );
    });

    it("refuses a malformed server address with a toast", async () => {
      const onJoinGame = vi.fn();
      renderLobby({ onJoinGame });
      const user = await typeCode("ABC123@play example.com");

      await user.click(screen.getByRole("button", { name: "Join" }));

      expect(onJoinGame).not.toHaveBeenCalled();
      // Paired positive: the refusal is observable, so the negative above is
      // not just "the click did nothing".
      expect(
        [...useMultiplayerStore.getState().toasts.values()].map((toast) => toast.message),
      ).toContain("That join code's server address isn't valid.");
    });

    it("falls back to the hosting server for a bare code nobody listed", async () => {
      const onJoinGame = vi.fn();
      renderLobby({ onJoinGame });
      const user = await typeCode("ABC123");

      await user.click(screen.getByRole("button", { name: "Join" }));

      expect(onJoinGame).toHaveBeenCalledWith(
        "ABC123",
        expect.objectContaining({ url: HOSTING_URL }),
      );
    });

    it("watches a CODE@host code on that host", async () => {
      const onSpectate = vi.fn();
      renderLobby({ onSpectate });
      const user = await typeCode("ABC123@play.example.com");

      await user.click(screen.getByRole("button", { name: "Watch" }));

      expect(onSpectate).toHaveBeenCalledWith(
        "ABC123",
        expect.objectContaining({ url: "wss://play.example.com/ws", origin: "user" }),
        undefined,
      );
      expect(useMultiplayerStore.getState().hostingServer).toBe(HOSTING_URL);
    });
  });

  it("orders official first, then by score, then by wait time", async () => {
    const official: LobbySource = {
      url: SERVER_PRESETS[0].url,
      name: "lobby.phase-rs.dev",
      origin: "official",
    };
    const scored: LobbySource = {
      url: "wss://scored.example/ws",
      name: "scored.example",
      origin: "user",
      score: 70,
    };
    const unscored: LobbySource = {
      url: "wss://unscored.example/ws",
      name: "unscored.example",
      origin: "user",
    };
    useMultiplayerStore.setState({
      userLobbySources: [scored, unscored],
      // Fed in an order that matches NEITHER the expected output nor a plain
      // `created_at` sort, so the comparator is what produces the assertion.
      subscribeLobby: vi.fn(async (onUpdate) => {
        onUpdate([lobbyGame("UNSC1", "Table Unscored", 100)], unscored);
        onUpdate([lobbyGame("SCOR1", "Table Scored", 300)], scored);
        onUpdate([lobbyGame("OFFI1", "Table Official", 400)], official);
        return () => {};
      }),
      ensureSubscriptionSocket: vi.fn().mockResolvedValue(null),
    });

    renderLobby({});

    // Row-scoped: the join-by-code button's accessible name is exactly
    // "Join", so this regex cannot pick it up.
    const rows = await screen.findAllByRole("button", {
      name: /Table (Official|Scored|Unscored)/,
    });
    expect(
      rows.map((row) =>
        /Table (Official|Scored|Unscored)/.exec(row.textContent ?? "")?.[0],
      ),
    ).toEqual(["Table Official", "Table Scored", "Table Unscored"]);
  });

  const SOURCE_A: LobbySource = { url: "wss://a.example/ws", name: "a.example", origin: "user" };
  const SOURCE_B: LobbySource = { url: "wss://b.example/ws", name: "b.example", origin: "user" };

  /** Both sources browsed, each with its own ambient socket. */
  function renderTwoSources(onJoinGame?: (...args: unknown[]) => void) {
    const sockets = new Map([
      [SOURCE_A.url, ambientSocket()],
      [SOURCE_B.url, ambientSocket()],
    ]);
    useMultiplayerStore.setState({
      userLobbySources: [SOURCE_A, SOURCE_B],
      subscribeLobby: vi.fn(async () => () => {}),
      ensureSubscriptionSocket: vi.fn(
        async (url: string) => sockets.get(url)?.socket ?? null,
      ),
    });
    renderLobby(onJoinGame ? { onJoinGame } : {});
    return sockets;
  }

  it("counts only the sources still being browsed", async () => {
    const sockets = renderTwoSources();
    await waitFor(() => {
      expect(sockets.get(SOURCE_B.url)!.listenerCount()).toBe(1);
    });

    act(() => {
      sockets.get(SOURCE_A.url)!.emit("PlayerCount", { count: 3 });
      sockets.get(SOURCE_B.url)!.emit("PlayerCount", { count: 5 });
    });

    // Reach-guard: both counts really are in the total before the removal, so
    // the assertion after it measures the drop and not an empty chip.
    expect(await screen.findByText("8 online")).toBeInTheDocument();

    act(() => {
      useMultiplayerStore.setState({ userLobbySources: [SOURCE_A] });
    });

    // B's last reported count is still in `playerCounts` (never pruned); the
    // chip must not keep counting a server nobody browses.
    expect(await screen.findByText("3 online")).toBeInTheDocument();
    expect(screen.queryByText("8 online")).not.toBeInTheDocument();
  });

  it("sends a reactive password retry to the source the frame arrived on", async () => {
    // Codes are unique per authority, not across the merged list: the same
    // code is listed on A while B is the server actually demanding a password.
    const listedOnA = lobbyGame("ABC123", "Table A", 100);
    storeMocks.findLobbyGameByCode.mockReturnValue({ game: listedOnA, source: SOURCE_A });
    const onJoinGame = vi.fn();
    const sockets = renderTwoSources(onJoinGame);
    await waitFor(() => {
      expect(sockets.get(SOURCE_B.url)!.listenerCount()).toBe(1);
    });

    act(() => {
      sockets.get(SOURCE_B.url)!.emit("PasswordRequired", { game_code: "ABC123" });
    });

    const user = userEvent.setup();
    await user.type(await screen.findByPlaceholderText("Enter password"), "hunter2{Enter}");

    // The retry goes to B — the socket the demand arrived on — while the
    // rescan still supplies the display context.
    expect(onJoinGame).toHaveBeenCalledWith(
      "ABC123",
      SOURCE_B,
      "hunter2",
      listedOnA.format,
      listedOnA,
    );
  });
});
