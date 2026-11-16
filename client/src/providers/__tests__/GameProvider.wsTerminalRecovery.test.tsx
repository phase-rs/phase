import "fake-indexeddb/auto";
import { act, cleanup, render, renderHook, waitFor } from "@testing-library/react";
import { MemoryRouter, useLocation } from "react-router";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { GameProvider } from "../GameProvider";
import { useResumables } from "../../hooks/useResumables";
import { PROTOCOL_VERSION, WebSocketAdapter } from "../../adapter/ws-adapter";
import { gameStateFactory } from "../../test/factories/gameStateFactory";
import { useGameStore } from "../../stores/gameStore";
import { useMultiplayerStore } from "../../stores/multiplayerStore";
import { saveWsSession, loadWsSession, WS_SESSION_TTL_MS, type WsSessionData } from "../../services/multiplayerSession";
import {
  commitFullTerminalDelivery, loadFullTerminalDelivery, type FullTerminalDelivery,
} from "../../services/fullTerminalResult";
import {
  clearGame,
  loadActiveGame,
  saveActiveGame,
} from "../../services/gamePersistence";

// Real Provider, ordinary WebSocketAdapter, raw terminal helpers, localStorage
// and IndexedDB. The fake server only controls wire replies/transport failures.
vi.mock("idb-keyval", async (original) => {
  const actual = await original<typeof import("idb-keyval")>();
  return { ...actual, get: vi.fn(actual.get), set: vi.fn(actual.set) };
});
const controller = vi.hoisted(() => vi.fn(() => ({ start: vi.fn(), stop: vi.fn(), dispose: vi.fn() })));
vi.mock("../../game/controllers/gameLoopController", () => ({ createGameLoopController: controller }));
vi.mock("../../game/dispatch", () => ({
  dispatchAction: vi.fn(), processRemoteUpdate: vi.fn(async () => {}),
  abandonPendingDispatches: vi.fn(),
}));
vi.mock("../../adapter/wasm-adapter", () => ({
  WasmAdapter: class {},
  getSharedAdapter: vi.fn(() => ({ openStateCodecSession: async () => null })),
}));
vi.mock("../../services/nativeEngine", () => ({
  canAttemptNativeEngine: () => false, nativeEngineKeyForCurrentOrigin: () => null,
}));
vi.mock("../../constants/storage", async (original) => ({
  ...await original<typeof import("../../constants/storage")>(),
  loadActiveDeck: () => ({ main: [{ name: "Island", count: 1 }], sideboard: [] }),
  loadSavedDeckBracket: () => null,
}));
vi.mock("../../hooks/useGameplayPreferencesSync", () => ({ useGameplayPreferencesSync: vi.fn() }));
vi.mock("../../audio/AudioManager", () => ({ audioManager: { setContext: vi.fn() } }));
vi.mock("../../services/playerAvatars", () => ({ assignRandomAvatars: () => [] }));
vi.mock("../../services/serverDetection", async (original) => ({
  ...await original<typeof import("../../services/serverDetection")>(),
  detectServerUrl: async () => "wss://fresh-game-fallback.example/ws",
}));

type Request = { type: string; data?: unknown };
type AckOutcome = "false" | "reject" | "pending" | "success";
let ackOutcome: AckOutcome;
let bootstrap: FullTerminalDelivery | null;
let delivery: FullTerminalDelivery;
let session: WsSessionData;
let sockets: ServerSocket[];
let requests: { url: string; message: Request }[];
let pendingAcks: ServerSocket[];
let holdRead: boolean;
let holdBootstrap: boolean;
let pendingBootstraps: ServerSocket[];
let successfulAckResponses: number;

// Message/close dispatch follows the existing ws-adapter MockWebSocket: one
// IDL message delivery, with close occurring asynchronously like a browser.
class ServerSocket extends EventTarget {
  static OPEN = 1;
  readyState = 1;
  onopen: (() => void) | null = null;
  onmessage: ((event: { data: string }) => void) | null = null;
  onerror: (() => void) | null = null;
  onclose: (() => void) | null = null;
  constructor(readonly url: string) {
    super();
    sockets.push(this);
    queueMicrotask(() => this.receive({
      type: "ServerHello",
      data: { mode: "Full", protocol_version: PROTOCOL_VERSION, server_version: "test", build_commit: "test" },
    }));
  }
  receive(message: Request) { this.onmessage?.({ data: JSON.stringify(message) }); }
  close() {
    if (this.readyState === 3) return;
    this.readyState = 3;
    queueMicrotask(() => { this.onclose?.(); this.dispatchEvent(new Event("close")); });
  }
  send(data: string) {
    const message = JSON.parse(data) as Request;
    requests.push({ url: this.url, message });
    queueMicrotask(() => {
      switch (message.type) {
        case "ReadTerminalResult":
          if (!holdRead) this.receive({ type: "TerminalResult", data: { delivery } });
          break;
        case "BootstrapTerminalDelivery":
          if (holdBootstrap) pendingBootstraps.push(this);
          else this.receive({ type: "TerminalBootstrapResult", data: bootstrap ? { delivery: bootstrap } : {} });
          break;
        case "AckTerminalDelivery":
          if (ackOutcome === "reject") this.onerror?.();
          else if (ackOutcome === "pending") pendingAcks.push(this);
          else {
            if (ackOutcome === "success") successfulAckResponses += 1;
            this.receive({
              type: ackOutcome === "success" ? "TerminalDeliveryAcknowledged" : "TerminalDeliveryRejected",
              data: { delivery_id: delivery.deliveryId },
            });
          }
          break;
        case "Reconnect":
          this.receive({ type: "GameStarted", data: {
            state: gameStateFactory.build(), your_player: 0, full_key: session.fullKey,
            game_code: session.gameCode, player_token: session.playerToken,
          } });
          break;
        // Fresh gameplay requests are deliberately recorded without replies.
        // A terminal restart must never reach this path.
      }
    });
  }
}

let sequence = 0;
beforeEach(async () => {
  localStorage.clear();
  useGameStore.getState().reset();
  vi.clearAllMocks();
  const actualIdb = await vi.importActual<typeof import("idb-keyval")>("idb-keyval");
  vi.mocked((await import("idb-keyval")).get).mockImplementation(actualIdb.get);
  vi.mocked((await import("idb-keyval")).set).mockImplementation(actualIdb.set);
  sockets = [];
  requests = [];
  pendingAcks = [];
  holdRead = false;
  holdBootstrap = false;
  pendingBootstraps = [];
  successfulAckResponses = 0;
  ackOutcome = "success";
  const key = { game_code: `WS${++sequence}`, generation: 3 };
  session = {
    gameCode: key.game_code, fullKey: key, playerToken: "pre-terminal-token",
    serverUrl: "wss://retained-terminal.example/ws", timestamp: Date.now(),
  };
  delivery = {
    key, terminalRevision: 7, deliveryId: "recipient-delivery", credential: "recipient-credential",
    display: { winner: 0, reason: "Retained WebSocket result" },
    finalView: gameStateFactory.build({ waiting_for: { type: "GameOver", data: { winner: 0 } } }),
  };
  bootstrap = null;
  vi.stubGlobal("WebSocket", ServerSocket);
});
afterEach(async () => {
  cleanup();
  await act(async () => { for (const socket of sockets) socket.close(); });
  vi.unstubAllGlobals();
  vi.restoreAllMocks();
});

function mountProvider() {
  const events = vi.fn();
  return {
    ...render(<GameProvider gameId="ws-terminal-recovery" mode="online" onWsEvent={events}><div /></GameProvider>),
    events,
  };
}

async function startTerminalPath(path: "saved" | "bootstrap" | "live") {
  saveWsSession(session);
  expect(loadWsSession()).toEqual(session);
  if (path === "saved") expect(await commitFullTerminalDelivery(delivery)).toBe(true);
  if (path === "bootstrap") bootstrap = delivery;
  const mounted = mountProvider();
  if (path === "live") {
    await waitFor(() => expect(requests.some(({ message }) => message.type === "Reconnect")).toBe(true));
    await waitFor(() => expect(mounted.events).toHaveBeenCalledWith(expect.objectContaining({ type: "reconnected" })));
    // JSON frame through the REAL ordinary adapter (callerManaged is absent).
    await act(async () => { sockets[sockets.length - 1]!.receive({ type: "TerminalResult", data: { delivery } }); });
  }
  await waitFor(() => expect(mounted.events).toHaveBeenCalledWith({ type: "terminalDelivery", delivery }));
  await waitFor(() => expect(requests.some(({ message }) => message.type === "AckTerminalDelivery")).toBe(true));
  expect(await loadFullTerminalDelivery(delivery.key)).toEqual(delivery);
  return mounted;
}

describe("ordinary WebSocket terminal routing across real persistence and remount", () => {
  it("delayed bootstrap cannot overwrite a replacement session with the old terminal route", async () => {
    saveWsSession(session);
    holdBootstrap = true;
    const mounted = mountProvider();
    await waitFor(() => expect(pendingBootstraps).toHaveLength(1));
    const replacement = {
      ...session, gameCode: "NEW", fullKey: { game_code: "NEW", generation: 4 },
      serverUrl: "wss://new-game.example/ws", playerToken: "new-token",
    };
    saveWsSession(replacement);
    await act(async () => {
      pendingBootstraps[0].receive({ type: "TerminalBootstrapResult", data: { delivery } });
    });
    await waitFor(() => expect(mounted.events).toHaveBeenCalledWith({ type: "terminalDelivery", delivery }));
    expect(successfulAckResponses).toBe(1);
    expect(loadWsSession()).toEqual(replacement);
    expect(controller).not.toHaveBeenCalled();
  });

  it("ordinary adapter disposal does not emit route removal while terminal ACK is pending", async () => {
    ackOutcome = "pending";
    await startTerminalPath("live");
    const adapter = useGameStore.getState().adapter;
    expect(adapter).toBeInstanceOf(WebSocketAdapter);
    const events = vi.fn();
    (adapter as WebSocketAdapter).onEvent(events);
    await act(async () => { adapter!.dispose(); });
    expect(events).not.toHaveBeenCalledWith({ type: "sessionChanged", session: null });
    expect(loadWsSession()).toMatchObject({ fullKey: session.fullKey, terminalOnly: true });
  });

  it.each(["saved", "bootstrap", "live"] as const)("%s delayed ACK cannot erase another saved session", async (path) => {
    ackOutcome = "pending";
    await startTerminalPath(path);
    const replacement: WsSessionData = {
      ...session, serverUrl: "wss://another-game.example/ws",
      fullKey: { game_code: "NEW", generation: 4 }, gameCode: "NEW", playerToken: "new-player-token",
    };
    saveWsSession(replacement);
    expect(pendingAcks).toHaveLength(1);
    await act(async () => {
      pendingAcks[0].receive({ type: "TerminalDeliveryAcknowledged", data: { delivery_id: delivery.deliveryId } });
    });
    expect(loadWsSession()).toEqual(replacement);
  });

  it("shows a saved result while the terminal-only read is pending", async () => {
    saveWsSession(session);
    await commitFullTerminalDelivery(delivery);
    holdRead = true;
    const mounted = mountProvider();
    await waitFor(() => expect(mounted.events).toHaveBeenCalledWith({
      type: "terminalDelivery", delivery: { ...delivery, finalView: undefined },
    }));
    expect(loadWsSession()).toMatchObject({ serverUrl: session.serverUrl, terminalOnly: true });
    expect(requests.some(({ message }) => message.type === "AckTerminalDelivery")).toBe(false);
    expect(controller).not.toHaveBeenCalled();
  });

  it("fails closed without gameplay when the terminal ledger cannot be read", async () => {
    saveWsSession({ ...session, terminalOnly: true });
    const idb = await import("idb-keyval");
    vi.spyOn(idb, "get").mockRejectedValueOnce(new Error("terminal storage unavailable"));
    const mounted = mountProvider();
    await waitFor(() => expect(mounted.events).toHaveBeenCalledWith(expect.objectContaining({ type: "terminalUnavailable" })));
    expect(loadWsSession()).toMatchObject({ serverUrl: session.serverUrl, terminalOnly: true });
    expect(requests).toEqual([]);
    expect(controller).not.toHaveBeenCalled();
  });

  it.each(["saved", "bootstrap", "live"] as const)("%s retains route beyond playable TTL and clears only after a delayed successful ACK", async (path) => {
    ackOutcome = "pending";
    await startTerminalPath(path);
    const retained = loadWsSession()!;
    saveWsSession({ ...retained, timestamp: Date.now() - WS_SESSION_TTL_MS - 1 });
    expect(loadWsSession()).toMatchObject({ serverUrl: session.serverUrl, fullKey: session.fullKey, terminalOnly: true });
    expect(pendingAcks).toHaveLength(1);
    await act(async () => {
      pendingAcks[0].receive({ type: "TerminalDeliveryAcknowledged", data: { delivery_id: delivery.deliveryId } });
    });
    await waitFor(() => expect(loadWsSession()).toBeNull());
    expect(await loadFullTerminalDelivery(delivery.key)).toEqual(delivery);
  });

  it.each(["saved", "bootstrap", "live"] as const)("control: %s remount retries terminal ACK when the route is retained", async (path) => {
    ackOutcome = "false";
    const mounted = await startTerminalPath(path);
    mounted.unmount();
    await act(async () => { for (const socket of sockets) socket.close(); });
    // Counterfactual storage control: restore only the lost routing record via
    // the production writer. Leave the delivery ledger and Provider unchanged.
    saveWsSession(session);
    ackOutcome = "success";
    const requestCount = requests.length;
    const controllerCount = controller.mock.calls.length;
    mountProvider();
    await waitFor(() => expect(requests.slice(requestCount)).toContainEqual({
      url: session.serverUrl, message: {
        type: "AckTerminalDelivery", data: { delivery_id: delivery.deliveryId, credential: delivery.credential },
      },
    }));
    expect(controller.mock.calls.length).toBe(controllerCount);
    expect(requests.slice(requestCount).some(({ message }) => message.type === "CreateGame")).toBe(false);
  });

  it.each(["saved", "bootstrap", "live"] as const)("control: %s ACK succeeds on the retained endpoint and tuple", async (path) => {
    saveActiveGame({ id: "ws-terminal-recovery", mode: "online", difficulty: "" });
    await startTerminalPath(path);
    expect(requests).toContainEqual({ url: session.serverUrl, message: {
      type: "AckTerminalDelivery", data: { delivery_id: delivery.deliveryId, credential: delivery.credential },
    } });
    await waitFor(() => expect(loadWsSession()).toBeNull());
    await waitFor(() => expect(loadActiveGame()).toBeNull());
    expect(successfulAckResponses).toBe(1);
  });

  it.each([
    ["the completed game's pointer", null],
    ["a replacement pointer for another game", { id: "rematch", mode: "online" as const, difficulty: "" }],
  ])("releases %s on matching acknowledgement after navigation and keeps replacement", async (_desc, replacement) => {
    ackOutcome = "pending";
    saveActiveGame({ id: "ws-terminal-recovery", mode: "online", difficulty: "" });
    const mounted = await startTerminalPath("live");
    expect(loadActiveGame()).toEqual({ id: "ws-terminal-recovery", mode: "online", difficulty: "" });
    expect(loadWsSession()).toMatchObject({ fullKey: session.fullKey, terminalOnly: true });

    // Unmount before ACK arrives to model navigation away from the game page.
    mounted.unmount();
    expect(pendingAcks).toHaveLength(1);

    if (replacement) {
      saveActiveGame(replacement);
    }

    await act(async () => {
      pendingAcks[0].receive({ type: "TerminalDeliveryAcknowledged", data: { delivery_id: delivery.deliveryId } });
    });

    if (replacement) {
      expect(loadActiveGame()).toEqual(replacement);
    } else {
      await waitFor(() => expect(loadActiveGame()).toBeNull());
      expect(loadWsSession()).toBeNull();
    }
  });

  it("keeps the online pointer across display cleanup and failed ACK, and reopening retries", async () => {
    ackOutcome = "pending";
    saveActiveGame({ id: "ws-terminal-recovery", mode: "online", difficulty: "" });
    const mounted = await startTerminalPath("live");
    // Display cleanup (clearGame) must not delete the online pointer.
    await act(async () => { await clearGame("ws-terminal-recovery"); });
    expect(loadActiveGame()).toEqual({ id: "ws-terminal-recovery", mode: "online", difficulty: "" });
    expect(loadWsSession()).toMatchObject({ fullKey: session.fullKey, terminalOnly: true });

    // Unmount before ACK lands to model navigation to Home.
    mounted.unmount();

    // Reject the pending ACK (failed ACK).
    await act(async () => {
      pendingAcks[0].onerror?.();
    });

    // Pointer is preserved on failed ACK.
    expect(loadActiveGame()).toEqual({ id: "ws-terminal-recovery", mode: "online", difficulty: "" });
    expect(loadWsSession()).toMatchObject({ fullKey: session.fullKey, terminalOnly: true });

    // Reopen without gameplay connection retries ACK.
    ackOutcome = "success";
    const reopened = mountProvider();
    await waitFor(() => expect(reopened.events).toHaveBeenCalledWith({
      type: "terminalDelivery", delivery: { ...delivery, finalView: undefined },
    }));
    await waitFor(() => expect(loadActiveGame()).toBeNull());
    expect(loadWsSession()).toBeNull();
  });

  it.each([
    ["the completed game's pointer", false],
    ["a replacement session for another game", true],
  ])("a mounted Home learns of a result retained after navigation, and a reopened matching ACK releases %s", async (_case, replace) => {
    const finished = { id: "ws-terminal-recovery", mode: "online" as const, difficulty: "" };
    saveActiveGame(finished);
    saveWsSession(session);
    // Hold the result's IndexedDB commit so retention finishes after Back to Home.
    const idb = await import("idb-keyval");
    const actualSet = (await vi.importActual<typeof import("idb-keyval")>("idb-keyval")).set;
    let releaseRetention!: () => void;
    const retentionHeld = new Promise<void>((resolve) => { releaseRetention = resolve; });
    vi.mocked(idb.set).mockImplementation(async (key, value, store) => {
      if ((value as Partial<FullTerminalDelivery> | undefined)?.deliveryId === delivery.deliveryId) {
        await retentionHeld;
      }
      return actualSet(key, value, store);
    });
    ackOutcome = "reject";

    // Home stays mounted for the whole sequence: it is never remounted.
    const home = renderHook(
      () => ({ resumables: useResumables(), location: useLocation() }),
      { wrapper: ({ children }) => <MemoryRouter>{children}</MemoryRouter> },
    );
    await waitFor(() => expect(home.result.current.resumables.match?.id).toBe(finished.id));

    const game = mountProvider();
    await waitFor(() => expect(game.events).toHaveBeenCalledWith(expect.objectContaining({ type: "reconnected" })));
    await act(async () => { sockets[sockets.length - 1]!.receive({ type: "TerminalResult", data: { delivery } }); });
    expect(vi.mocked(idb.set)).toHaveBeenCalledWith(expect.anything(), delivery, expect.anything());

    // Back to Home before retention lands: the session still looks playable.
    game.unmount();
    expect(loadWsSession()).toMatchObject({ fullKey: session.fullKey, playerToken: session.playerToken });
    expect(loadWsSession()?.terminalOnly).toBeUndefined();
    expect(home.result.current.resumables.match?.id).toBe(finished.id);
    expect(home.result.current.resumables.retainedResult).toBeNull();

    // Retention lands and the ACK fails.
    await act(async () => { releaseRetention(); });
    await waitFor(() => expect(requests.some(({ message }) => message.type === "AckTerminalDelivery")).toBe(true));
    expect(await loadFullTerminalDelivery(delivery.key)).toEqual(delivery);
    expect(loadWsSession()).toMatchObject({ fullKey: session.fullKey, terminalOnly: true });
    expect(loadActiveGame()).toEqual(finished);
    expect(successfulAckResponses).toBe(0);

    // The mounted Home swaps Resume for View Result.
    await waitFor(() => expect(home.result.current.resumables.retainedResult?.id).toBe(finished.id));
    expect(home.result.current.resumables.match).toBeNull();
    act(() => home.result.current.resumables.viewRetainedResult());
    expect(home.result.current.location.pathname).toBe(`/game/${finished.id}`);
    expect(home.result.current.location.search).toBe("?mode=host");

    // Reopening re-displays the result without gameplay and retries the ACK.
    ackOutcome = "pending";
    const controllerCount = controller.mock.calls.length;
    const reopened = mountProvider();
    await waitFor(() => expect(reopened.events).toHaveBeenCalledWith({ type: "terminalDelivery", delivery }));
    await waitFor(() => expect(pendingAcks).toHaveLength(1));
    expect(controller.mock.calls.length).toBe(controllerCount);

    const rematch = { id: "rematch", mode: "online" as const, difficulty: "" };
    const rematchSession: WsSessionData = {
      ...session, gameCode: "NEXT", fullKey: { game_code: "NEXT", generation: 1 }, playerToken: "next-token",
    };
    if (replace) {
      saveActiveGame(rematch);
      saveWsSession(rematchSession);
    }
    await act(async () => {
      pendingAcks[0]!.receive({ type: "TerminalDeliveryAcknowledged", data: { delivery_id: delivery.deliveryId } });
    });

    if (replace) {
      expect(loadActiveGame()).toEqual(rematch);
      expect(loadWsSession()).toEqual(rematchSession);
      await waitFor(() => expect(home.result.current.resumables.match?.id).toBe(rematch.id));
    } else {
      await waitFor(() => expect(loadActiveGame()).toBeNull());
      expect(loadWsSession()).toBeNull();
      await waitFor(() => expect(home.result.current.resumables.match).toBeNull());
    }
    expect(home.result.current.resumables.retainedResult).toBeNull();
  });

  it.each([
    ["saved", "false"], ["saved", "reject"], ["saved", "pending"],
    ["bootstrap", "false"], ["bootstrap", "reject"], ["bootstrap", "pending"],
    ["live", "false"], ["live", "reject"], ["live", "pending"],
  ] as const)("retains terminal-only routing for %s delivery after ACK %s and remount", async (path, failure) => {
    ackOutcome = failure;
    const mounted = await startTerminalPath(path);
    const retainedRoute = loadWsSession();
    mounted.unmount();
    // Interrupt the pending request to model the old browser process ending.
    await act(async () => { for (const socket of sockets) socket.close(); });
    ackOutcome = "success";
    bootstrap = delivery;
    const requestCount = requests.length;
    const controllerCount = controller.mock.calls.length;
    mountProvider();
    await waitFor(() => expect(requests.slice(requestCount).some(({ message }) => message.type !== "ClientHello")).toBe(true));
    const restartRequests = requests.slice(requestCount).map(({ message }) => message.type).filter((type) => type !== "ClientHello");
    console.info("WS failed-ACK restart", { path, failure, retainedRoute, restartRequests });
    // Both assertions run: the first identifies the lost persisted endpoint,
    // the second proves the actual restart falls into gameplay setup.
    expect.soft(retainedRoute).toMatchObject({
      serverUrl: session.serverUrl, fullKey: session.fullKey,
      playerToken: session.playerToken, terminalOnly: true,
    });
    await waitFor(() => {
      expect(requests.slice(requestCount).some(({ message }) => message.type === "AckTerminalDelivery")).toBe(true);
    });
    expect.soft(controller.mock.calls.length).toBe(controllerCount);
    expect(await loadFullTerminalDelivery(delivery.key)).toEqual(delivery);
  });

  describe("terminal response identity binding", () => {
    const sentTypes = () => requests.map(({ message }) => message.type).filter((type) => type !== "ClientHello");

    it("reports a foreign bootstrap key without committing, acknowledging, or reconnecting", async () => {
      saveWsSession(session);
      bootstrap = { ...delivery, key: { game_code: "FOREIGN", generation: 1 } };
      const { events } = mountProvider();

      await waitFor(() => expect(events).toHaveBeenCalledWith({
        type: "terminalUnavailable", message: "Mismatched terminal bootstrap key",
      }));
      expect(sentTypes()).toEqual(["BootstrapTerminalDelivery"]);
      expect(events).not.toHaveBeenCalledWith(expect.objectContaining({ type: "terminalDelivery" }));
      expect(await loadFullTerminalDelivery(bootstrap.key)).toBeNull();
      expect(await loadFullTerminalDelivery(delivery.key)).toBeNull();
      expect(loadWsSession()).toEqual(session);
    });

    it.each([
      ["another credential", { credential: "foreign-credential" }],
      ["another delivery", { deliveryId: "foreign-delivery" }],
    ])("keeps the retained result and route when the read returns %s", async (_case, changed) => {
      const retained = delivery;
      await commitFullTerminalDelivery(retained);
      saveWsSession({ ...session, terminalOnly: true });
      delivery = { ...retained, ...changed, display: { winner: 1, reason: "Another game's result" } };
      const { events } = mountProvider();

      await waitFor(() => expect(events).toHaveBeenCalledWith({
        type: "terminalUnavailable", message: "Mismatched terminal delivery read",
      }));
      // No acknowledgement was sent for the foreign reply.
      expect(sentTypes()).toEqual(["ReadTerminalResult"]);
      const published = events.mock.calls
        .map(([event]) => event as { type: string })
        .filter((event) => event.type === "terminalDelivery");
      expect(published).toEqual([{ type: "terminalDelivery", delivery: { ...retained, finalView: undefined } }]);
      expect(await loadFullTerminalDelivery(session.fullKey)).toEqual(retained);
      expect(loadWsSession()).toEqual({ ...session, terminalOnly: true });
    });

    it("keeps the recovery route when the server acknowledges a different delivery", async () => {
      ackOutcome = "pending";
      saveWsSession({ ...session, terminalOnly: true });
      await commitFullTerminalDelivery(delivery);
      const { events } = mountProvider();

      await waitFor(() => expect(pendingAcks).toHaveLength(1));
      await act(async () => {
        pendingAcks[0].receive({
          type: "TerminalDeliveryAcknowledged",
          data: { delivery_id: "other-delivery-id" },
        });
      });

      expect(sentTypes()).toEqual(["ReadTerminalResult", "AckTerminalDelivery"]);
      expect(events).toHaveBeenCalledWith({ type: "terminalDelivery", delivery });
      expect(loadWsSession()).toEqual({ ...session, terminalOnly: true });
    });

    it("restores authoritative guest seat 1 identity upon terminal recovery", async () => {
      const guestSession: WsSessionData = {
        ...session,
        playerId: 1,
        terminalOnly: true,
      };
      saveWsSession(guestSession);
      const guestDelivery: FullTerminalDelivery = {
        ...delivery,
        display: { winner: 1, reason: "Life reduction victory (CR 704.5a)" },
      };
      await commitFullTerminalDelivery(guestDelivery);

      useMultiplayerStore.getState().setActivePlayerId(null as never);
      expect(useMultiplayerStore.getState().activePlayerId).toBeNull();

      const { events } = mountProvider();
      await waitFor(() => expect(events).toHaveBeenCalledWith({
        type: "terminalDelivery",
        delivery: { ...guestDelivery, finalView: undefined },
      }));

      // The authoritative recipient seat 1 is restored before identity event
      expect(useMultiplayerStore.getState().activePlayerId).toBe(1);
    });

    it("leaves the seat unknown when a legacy save has no player id", async () => {
      // Legacy online saves recorded no `playerId`; the terminal-only recovery
      // never runs `playerIdentity`, so no seat may be assumed for the labels.
      saveWsSession({ ...session, terminalOnly: true });
      await commitFullTerminalDelivery(delivery);
      useMultiplayerStore.getState().setActivePlayerId(1 as never);

      const { events } = mountProvider();
      await waitFor(() => expect(events).toHaveBeenCalledWith({
        type: "terminalDelivery",
        delivery: { ...delivery, finalView: undefined },
      }));

      // A previous game's seat must not label this recovered result.
      expect(useMultiplayerStore.getState().activePlayerId).toBeNull();
    });
  });
});
