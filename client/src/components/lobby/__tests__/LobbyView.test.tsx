import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act, cleanup, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";

import type { LobbyGame } from "../../../adapter/types";
import { LobbyView } from "../LobbyView";
import { SERVER_PRESETS } from "../../../services/serverDetection";
import type { DirectorySource } from "../../../services/serverDirectory";
import {
  useMultiplayerStore,
  type AmbientLobbyFrame,
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
/**
 * This is the only test in the suite that mounts the real `LobbyView`, and the
 * real `LobbyView` calls `refreshServerDirectory()` on mount. Without this mock
 * the suite would issue a live `GET` to the official directory — a defect even
 * on a run that passes.
 */
const directoryMocks = vi.hoisted(() => ({
  refreshServerDirectory: vi.fn(() => Promise.resolve()),
}));
/**
 * Only `refreshServerDirectory` is replaced. Everything else — `healthHint`
 * above all — stays REAL, so the U19 row measures the production
 * classification and not a stub of it.
 */
vi.mock("../../../services/serverDirectory", async (importOriginal) => ({
  ...(await importOriginal<typeof import("../../../services/serverDirectory")>()),
  refreshServerDirectory: directoryMocks.refreshServerDirectory,
}));
/**
 * The real `LobbyView` can dial a directory source, and the store's factory
 * wrapper reports the outcome. Left unmocked, this suite would build a queue
 * against the REAL official host — `vitest.config.ts` defines
 * `__OFFICIAL_MULTIPLAYER_SERVER_URL__` as the production URL. A recording
 * stub, plus the never-called transport assertion in V-U12ra.
 */
const metricsMocks = vi.hoisted(() => ({
  reportConnectOutcome: vi.fn(),
  flushMetricsNow: vi.fn(),
  installServerMetricsLifecycle: vi.fn(),
  metricsUrl: vi.fn(() => "https://metrics.test/servers/metrics"),
}));
vi.mock("../../../services/serverMetrics", () => metricsMocks);
vi.mock("../../../stores/multiplayerStore", async (importOriginal) => ({
  ...(await importOriginal<typeof import("../../../stores/multiplayerStore")>()),
  findLobbyGameByCode: storeMocks.findLobbyGameByCode,
}));

/**
 * LobbyView holds no socket of its own: listings arrive through the store's
 * `subscribeLobby` fan-out, ambient frames through `subscribeAmbientLobby`,
 * and per-source player counts through `sourceStatus`. Tests stub those two
 * store actions, so every case drives the component through exactly the
 * surface production drives it through — including across a reconnect,
 * which replaces the socket underneath the store but not the subscription.
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
 * Stand-in for the store's ambient fan-out. The store owns each source's
 * socket and re-attaches its listener to whatever socket a reconnect
 * produces, so what the view actually consumes is a `(frame, source)` pair —
 * which is what a test delivers here.
 */
function ambientFanOut() {
  const subscribers = new Set<
    (frame: AmbientLobbyFrame, source: LobbySource) => void
  >();
  return {
    subscribe: vi.fn((onFrame: (frame: AmbientLobbyFrame, source: LobbySource) => void) => {
      subscribers.add(onFrame);
      return () => {
        subscribers.delete(onFrame);
      };
    }),
    subscriberCount: () => subscribers.size,
    emit: (frame: AmbientLobbyFrame, source: LobbySource) => {
      for (const fn of [...subscribers]) fn(frame, source);
    },
  };
}

/** Per-source status rows as the store writes them: one row per channel,
 * carrying that channel's state and the count it last reported ON ITS
 * CURRENT SOCKET (`null` = has reported none since it last opened). */
function statusRows(
  ...rows: [url: string, state: LobbySourceStatus["state"], playerCount: number | null][]
): Map<string, LobbySourceStatus> {
  return new Map(
    rows.map(([url, state, playerCount]) => [
      url,
      { state, serverInfo: null, playerCount } satisfies LobbySourceStatus,
    ]),
  );
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
  const originalSubscribeAmbient =
    useMultiplayerStore.getState().subscribeAmbientLobby;

  beforeEach(() => {
    // Egress guards — defence-in-depth; the real mitigation is the module
    // mock above. These only make the absence of a send observable.
    vi.stubGlobal("navigator", { sendBeacon: vi.fn(() => true) });
    vi.stubGlobal("fetch", vi.fn(async () => ({ ok: false, status: 599 })));
    useMultiplayerStore.setState({
      hostingServer: HOSTING_URL,
      userLobbySources: [],
      sourceStatus: new Map(),
      toasts: new Map(),
      directorySources: [],
      directoryFetchedAtMs: null,
      disabledDirectorySources: [],
    });
  });

  /** A minimal projected listing. Built by hand rather than through
   * `projectDirectoryBody`, whose module this file mocks away. */
  function directoryEntry(
    url: string,
    score: number | undefined,
    rowScore: DirectorySource["row"]["score"] = null,
  ): DirectorySource {
    return {
      source: { url, name: url, origin: "directory", kind: "LobbyOnly", score },
      row: {
        url,
        name: url,
        mode: "LobbyOnly",
        server_version: "0.71.0",
        protocol_version: 55,
        lobby_protocol_version: 4,
        current_players: 0,
        first_seen_ms: 1,
        last_seen_ms: 2,
        score: rowScore,
      },
      rejection: null,
      fullRejection: null,
    };
  }

  afterEach(() => {
    cleanup();
    vi.unstubAllGlobals();
    vi.clearAllMocks();
    // `clearAllMocks` drops calls but keeps queued return values.
    storeMocks.findLobbyGameByCode.mockReset();
    useMultiplayerStore.setState({
      subscribeLobby: originalSubscribeLobby,
      subscribeAmbientLobby: originalSubscribeAmbient,
    });
  });

  it("calls onServerOffline when the shared subscription socket is unreachable", async () => {
    // Store returns `null` when the socket can't be opened (withReconnect
    // exhausted, invalid URL, etc.). LobbyView's offline fallback fires.
    useMultiplayerStore.setState({
      subscribeLobby: vi.fn().mockResolvedValue(null),
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
    useMultiplayerStore.setState({ subscribeLobby });
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

  /** Both sources browsed, handshaken and each reporting a count on its
   * current socket — the store's status rows are where a live count lives,
   * so this is the whole per-source surface the view reads. */
  function renderTwoSources(onJoinGame?: (...args: unknown[]) => void) {
    const ambient = ambientFanOut();
    useMultiplayerStore.setState({
      userLobbySources: [SOURCE_A, SOURCE_B],
      sourceStatus: statusRows([SOURCE_A.url, "open", 3], [SOURCE_B.url, "open", 5]),
      subscribeLobby: vi.fn(async () => () => {}),
      subscribeAmbientLobby: ambient.subscribe,
    });
    renderLobby(onJoinGame ? { onJoinGame } : {});
    return ambient;
  }

  it("counts only the sources still being browsed", async () => {
    renderTwoSources();

    // Reach-guard: both counts really are in the total before the removal, so
    // the assertion after it measures the drop and not an empty chip.
    expect(await screen.findByText("8 online")).toBeInTheDocument();

    act(() => {
      useMultiplayerStore.setState({ userLobbySources: [SOURCE_A] });
    });

    // B's status row (and its count) survives until its channel closes; the
    // chip must not keep counting a server nobody browses.
    expect(await screen.findByText("3 online")).toBeInTheDocument();
    expect(screen.queryByText("8 online")).not.toBeInTheDocument();
  });

  it("drops a source's count from the total once that source goes offline", async () => {
    renderTwoSources();

    // Reach-guard: both counts really are in the total while both sources are
    // open, so the assertion after the flap measures the drop.
    expect(await screen.findByText("8 online")).toBeInTheDocument();

    act(() => {
      useMultiplayerStore.setState({
        // Leaving `"open"` rewrites the row without a count — exactly what
        // the store does on the `"offline"` transition.
        sourceStatus: statusRows([SOURCE_A.url, "open", 3], [SOURCE_B.url, "offline", null]),
      });
    });

    // B is still a browsed source (the picker shows it, marked down), but it
    // is delivering nothing — counting it would advertise players on a
    // server shown as offline.
    expect(await screen.findByText("3 online")).toBeInTheDocument();
    expect(screen.queryByText("8 online")).not.toBeInTheDocument();
  });

  it("does not re-admit a source's old count when it reconnects without reporting", async () => {
    // The state a flapped source spends the rest of the session in: back to
    // `"open"`, on a NEW socket that has sent no `PlayerCount` yet. The old
    // number must not come back — nothing live is backing it.
    renderTwoSources();

    // Reach-guard: the pre-flap total is really 8, so the assertions below
    // measure the count staying out and not an empty chip.
    expect(await screen.findByText("8 online")).toBeInTheDocument();

    act(() => {
      useMultiplayerStore.setState({
        sourceStatus: statusRows([SOURCE_A.url, "open", 3], [SOURCE_B.url, "offline", null]),
      });
    });
    expect(await screen.findByText("3 online")).toBeInTheDocument();

    act(() => {
      useMultiplayerStore.setState({
        sourceStatus: statusRows([SOURCE_A.url, "open", 3], [SOURCE_B.url, "open", null]),
      });
    });

    expect(await screen.findByText("3 online")).toBeInTheDocument();
    expect(screen.queryByText("8 online")).not.toBeInTheDocument();
  });

  it("keeps its ambient subscription across a source's connection flap", async () => {
    // The subscribe effect deliberately excludes `sourceStatus` from its
    // deps, so it must not need to re-run: the store re-attaches its own
    // listener to the post-reconnect socket and keeps fanning out to the
    // subscriber registered here.
    const onJoinGame = vi.fn();
    const ambient = renderTwoSources(onJoinGame);
    await waitFor(() => {
      expect(ambient.subscriberCount()).toBe(1);
    });

    act(() => {
      useMultiplayerStore.setState({
        sourceStatus: statusRows([SOURCE_A.url, "open", 3], [SOURCE_B.url, "reconnecting", null]),
      });
    });
    act(() => {
      useMultiplayerStore.setState({
        sourceStatus: statusRows([SOURCE_A.url, "open", 3], [SOURCE_B.url, "open", null]),
      });
    });

    // Subscribed exactly once across the flap — no churn, no second listener
    // (which would open the modal twice on one frame).
    expect(ambient.subscribe).toHaveBeenCalledTimes(1);
    expect(ambient.subscriberCount()).toBe(1);

    act(() => {
      ambient.emit({ kind: "passwordRequired", gameCode: "ABC123" }, SOURCE_B);
    });

    const user = userEvent.setup();
    await user.type(await screen.findByPlaceholderText("Enter password"), "hunter2{Enter}");
    expect(onJoinGame).toHaveBeenCalledWith(
      "ABC123",
      SOURCE_B,
      "hunter2",
      undefined,
      undefined,
    );
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
    const ambient = renderTwoSources(onJoinGame);
    await waitFor(() => {
      expect(ambient.subscriberCount()).toBe(1);
    });

    act(() => {
      ambient.emit({ kind: "passwordRequired", gameCode: "ABC123" }, SOURCE_B);
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

  // V-U11j
  it("refreshes the server directory on mount, in server mode only", async () => {
    useMultiplayerStore.setState({
      subscribeLobby: vi.fn().mockResolvedValue(() => {}),
      subscribeAmbientLobby: vi.fn(() => () => {}),
    });
    renderLobby({});
    expect(directoryMocks.refreshServerDirectory).toHaveBeenCalledTimes(1);

    cleanup();
    directoryMocks.refreshServerDirectory.mockClear();
    // Paired negative: P2P has no lobby to browse and no directory to read.
    render(
      <LobbyView
        onHostGame={vi.fn()}
        onHostP2P={vi.fn()}
        onJoinGame={vi.fn()}
        connectionMode="p2p"
      />,
    );
    expect(directoryMocks.refreshServerDirectory).toHaveBeenCalledTimes(0);
  });

  // V-U11k
  it("re-subscribes on membership change, not on a score-only refresh", async () => {
    const subscribeLobby = vi.fn().mockResolvedValue(() => {});
    useMultiplayerStore.setState({
      subscribeLobby,
      subscribeAmbientLobby: vi.fn(() => () => {}),
      directorySources: [directoryEntry("wss://d.example/ws", 40)],
    });
    renderLobby({});
    await waitFor(() => {
      expect(subscribeLobby).toHaveBeenCalledTimes(1);
    });

    // A refresh that only moves a score must not tear down and re-attach every
    // channel's listener.
    act(() => {
      useMultiplayerStore.setState({
        directorySources: [directoryEntry("wss://d.example/ws", 90)],
      });
    });
    expect(subscribeLobby).toHaveBeenCalledTimes(1);

    // Paired positive: a source at a NEW url does re-subscribe — otherwise the
    // assertion above would only prove that nothing ever re-subscribes.
    act(() => {
      useMultiplayerStore.setState({
        directorySources: [directoryEntry("wss://e.example/ws", 90)],
      });
    });
    await waitFor(() => {
      expect(subscribeLobby).toHaveBeenCalledTimes(2);
    });
  });

  // ── U12r + U19 ──────────────────────────────────────────────────────────

  // V-U12ra
  it("refreshes the directory when the tab becomes visible again", () => {
    const visibility = vi.spyOn(document, "visibilityState", "get");
    try {
      renderLobby({});
      // Reach-guard: the mount effect already fired once, so the increment
      // below is attributable to the event and not to the mount.
      expect(directoryMocks.refreshServerDirectory).toHaveBeenCalledTimes(1);

      visibility.mockReturnValue("visible");
      document.dispatchEvent(new Event("visibilitychange"));
      expect(directoryMocks.refreshServerDirectory).toHaveBeenCalledTimes(2);

      // Paired negative: going HIDDEN is not a refresh trigger.
      visibility.mockReturnValue("hidden");
      document.dispatchEvent(new Event("visibilitychange"));
      expect(directoryMocks.refreshServerDirectory).toHaveBeenCalledTimes(2);

      // Egress check — defence-in-depth; the module mock is the mitigation.
      expect(navigator.sendBeacon).not.toHaveBeenCalled();
      expect(globalThis.fetch).not.toHaveBeenCalled();
    } finally {
      visibility.mockRestore();
    }
  });

  // V-U12rb
  it("installs no visibility refresh in p2p mode and removes it on unmount", () => {
    const visibility = vi.spyOn(document, "visibilityState", "get");
    try {
      visibility.mockReturnValue("visible");

      // (i) P2P has no lobby to browse and no directory to read.
      render(
        <LobbyView
          onHostGame={vi.fn()}
          onHostP2P={vi.fn()}
          onJoinGame={vi.fn()}
          onSpectate={vi.fn()}
          onServerOffline={vi.fn()}
          connectionMode="p2p"
        />,
      );
      // Reach-guard: a p2p-ONLY element, so the zero below is the effect's
      // guard and not a render that never happened.
      expect(screen.getByText(/Dedicated server unavailable/)).toBeInTheDocument();
      document.dispatchEvent(new Event("visibilitychange"));
      expect(directoryMocks.refreshServerDirectory).toHaveBeenCalledTimes(0);

      // (ii) Server mode: the listener works, and is gone after unmount.
      cleanup();
      const { unmount } = renderLobby({});
      document.dispatchEvent(new Event("visibilitychange"));
      const beforeUnmount = directoryMocks.refreshServerDirectory.mock.calls.length;
      // Its own reach-guard: the listener really was installed, so the
      // unchanged count below is the CLEANUP and not a listener that never ran.
      expect(beforeUnmount).toBeGreaterThan(1);

      unmount();
      document.dispatchEvent(new Event("visibilitychange"));
      expect(directoryMocks.refreshServerDirectory).toHaveBeenCalledTimes(beforeUnmount);
    } finally {
      visibility.mockRestore();
    }
  });

  // V-U19g
  it("hints a row from its own listing's raw components, and only that row", async () => {
    const LISTED = "wss://listed.example/ws";
    const listedSource: LobbySource = {
      url: LISTED,
      name: "listed.example",
      origin: "directory",
      kind: "Full",
      score: 60,
    };
    const presetSource: LobbySource = {
      url: SERVER_PRESETS[0].url,
      name: "lobby.phase-rs.dev",
      origin: "official",
    };
    useMultiplayerStore.setState({
      // Ranked (`value` non-null, samples above Rust's floor) and slow.
      directorySources: [
        directoryEntry(LISTED, 60, {
          value: 60,
          samples: 40,
          success_rate: 1,
          completion_rate: 1,
          median_rtt_ms: 800,
        }),
      ],
      subscribeLobby: vi.fn(async (onUpdate) => {
        onUpdate([lobbyGame("LST01", "Table Listed", 100)], listedSource);
        onUpdate([lobbyGame("PRE01", "Table Preset", 200)], presetSource);
        return () => {};
      }),
      subscribeAmbientLobby: vi.fn(() => () => {}),
    });

    renderLobby({});

    const listedRow = await screen.findByRole("button", { name: /Table Listed/ });
    expect(listedRow.textContent).toContain("SLOW");
    // Paired negative: a row from a source with NO listing carries no hint, so
    // the join is by source URL and not applied to every row.
    const presetRow = screen.getByRole("button", { name: /Table Preset/ });
    expect(presetRow.textContent).not.toContain("SLOW");
  });
});
