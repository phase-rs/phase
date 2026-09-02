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
  type LobbySourceStatus,
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
      // A browsed source lists the same code; the typed address names an
      // ad-hoc host nobody browses. Modelled as the real function behaves:
      // an UNSCOPED scan finds the colliding row, a scan scoped to the
      // ad-hoc host finds nothing (there is no snapshot for it).
      const collidingRow = lobbyGame("ABC123", "Someone else's table", 100);
      storeMocks.findLobbyGameByCode.mockImplementation(
        (_code: string, sourceUrl?: string) =>
          sourceUrl === undefined
            ? { game: collidingRow, source: SOURCE_A }
            : undefined,
      );
      const onSpectate = vi.fn();
      renderLobby({ onSpectate });
      const user = await typeCode("ABC123@play.example.com");

      await user.click(screen.getByRole("button", { name: "Watch" }));

      // The context lookup is scoped to the authority being watched...
      expect(storeMocks.findLobbyGameByCode).toHaveBeenCalledWith(
        "ABC123",
        "wss://play.example.com/ws",
      );
      // ...so the colliding row on another source is never handed to the
      // spectate handler as the row that picks the draft-vs-game route.
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

  /** Both sources browsed and handshaken, each with its own ambient socket.
   * `sourceStatus` mirrors production: a source only ever delivers ambient
   * frames after `ensureSubscriptionSocket` has recorded it `"open"`. */
  function renderTwoSources(onJoinGame?: (...args: unknown[]) => void) {
    const sockets = new Map([
      [SOURCE_A.url, ambientSocket()],
      [SOURCE_B.url, ambientSocket()],
    ]);
    useMultiplayerStore.setState({
      userLobbySources: [SOURCE_A, SOURCE_B],
      sourceStatus: new Map([
        [SOURCE_A.url, { state: "open", serverInfo: null } satisfies LobbySourceStatus],
        [SOURCE_B.url, { state: "open", serverInfo: null } satisfies LobbySourceStatus],
      ]),
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

  it("drops a source's count from the total once that source goes offline", async () => {
    const sockets = renderTwoSources();
    await waitFor(() => {
      expect(sockets.get(SOURCE_B.url)!.listenerCount()).toBe(1);
    });

    act(() => {
      sockets.get(SOURCE_A.url)!.emit("PlayerCount", { count: 3 });
      sockets.get(SOURCE_B.url)!.emit("PlayerCount", { count: 5 });
    });

    // Reach-guard: both counts really are in the total while both sources are
    // open, so the assertion after the flap measures the drop.
    expect(await screen.findByText("8 online")).toBeInTheDocument();

    act(() => {
      useMultiplayerStore.setState({
        sourceStatus: new Map([
          [SOURCE_A.url, { state: "open", serverInfo: null } satisfies LobbySourceStatus],
          [SOURCE_B.url, { state: "offline", serverInfo: null } satisfies LobbySourceStatus],
        ]),
      });
    });

    // B is still a browsed source (the picker shows it, marked down) and its
    // last count is still in `playerCounts`, but it is delivering nothing —
    // counting it would advertise players on a server shown as offline.
    expect(await screen.findByText("3 online")).toBeInTheDocument();
    expect(screen.queryByText("8 online")).not.toBeInTheDocument();
  });

  it("sends a reactive password retry to the source the frame arrived on", async () => {
    // Codes are unique per authority, not across the merged list: the same
    // code is listed on BOTH sources while B is the server actually demanding
    // a password. An unscoped rescan resolves to A (first in derived order),
    // so every field the modal carries must be looked up scoped to B.
    const listedOnA: LobbyGame = { ...lobbyGame("ABC123", "Table A", 100), format: "Commander" };
    const listedOnB: LobbyGame = { ...lobbyGame("ABC123", "Table B", 200), format: "Modern" };
    storeMocks.findLobbyGameByCode.mockImplementation(
      (_code: string, sourceUrl?: string) =>
        sourceUrl === SOURCE_B.url
          ? { game: listedOnB, source: SOURCE_B }
          : { game: listedOnA, source: SOURCE_A },
    );
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

    // The lookup is scoped to the arriving authority...
    expect(storeMocks.findLobbyGameByCode).toHaveBeenCalledWith("ABC123", SOURCE_B.url);
    // ...so origin, format and context all come from B. `context` is what
    // routes the join downstream (`MultiplayerPage` branches on
    // `context.draft_metadata`), so A's colliding row must not be carried.
    expect(onJoinGame).toHaveBeenCalledWith(
      "ABC123",
      SOURCE_B,
      "hunter2",
      listedOnB.format,
      listedOnB,
    );
    expect(onJoinGame).not.toHaveBeenCalledWith(
      "ABC123",
      expect.anything(),
      "hunter2",
      listedOnA.format,
      listedOnA,
    );
  });
});
