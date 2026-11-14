import "fake-indexeddb/auto";
import { act, cleanup, render, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { GameProvider } from "../GameProvider";
import { PROTOCOL_VERSION } from "../../adapter/ws-adapter";
import type { WsAdapterEvent } from "../../adapter/ws-adapter";
import { gameStateFactory } from "../../test/factories/gameStateFactory";
import { useGameStore } from "../../stores/gameStore";
import {
  clearGame,
  loadActiveGame,
  saveActiveGame,
  type ActiveGameMeta,
  type NativeSoloSession,
} from "../../services/gamePersistence";
import { loadFullTerminalDelivery, type FullTerminalDelivery } from "../../services/fullTerminalResult";

// Real Provider, ordinary WebSocketAdapter over the native-engine factory,
// real terminal helpers, localStorage pointer and IndexedDB result ledger. The
// fake bridge only controls wire replies; the page consumer below clears the
// game on display exactly as GamePage does.
vi.mock("idb-keyval", async (original) => {
  const actual = await original<typeof import("idb-keyval")>();
  return { ...actual, set: vi.fn(actual.set) };
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
  canAttemptNativeEngine: () => true,
  ensureNativeEngine: async () => ({ port: 1 }),
  nativeEngineKeyForCurrentOrigin: () => ({ release: { version: "test" } }),
}));
vi.mock("../../services/nativeEngineSocket", () => ({
  NativeEngineSocket: function NativeEngineSocket() { return new BridgeSocket(); },
}));
vi.mock("../../hooks/useGameplayPreferencesSync", () => ({ useGameplayPreferencesSync: vi.fn() }));
vi.mock("../../audio/AudioManager", () => ({ audioManager: { setContext: vi.fn() } }));
vi.mock("../../services/playerAvatars", () => ({ assignRandomAvatars: () => [] }));

type Request = { type: string; data?: unknown };
type AckOutcome = "reject" | "pending" | "success";
const GAME_ID = "native-terminal-recovery";
let ackOutcome: AckOutcome;
let bootstrap: FullTerminalDelivery | null;
let delivery: FullTerminalDelivery;
let session: NativeSoloSession;
let sockets: BridgeSocket[];
let requests: Request[];
let pendingAcks: BridgeSocket[];

// One shell bridge connection. Every native socket, playable or terminal-only,
// is built by the provider's factory, so all requests are seen here.
class BridgeSocket {
  static OPEN = 1;
  readonly url = "native-engine";
  readyState = 1;
  onopen: (() => void) | null = null;
  onmessage: ((event: { data: string }) => void) | null = null;
  onerror: (() => void) | null = null;
  onclose: (() => void) | null = null;
  constructor() {
    sockets.push(this);
    queueMicrotask(() => this.receive({
      type: "ServerHello",
      data: { mode: "Full", protocol_version: PROTOCOL_VERSION, server_version: "test", build_commit: "test" },
    }));
  }
  addEventListener() {}
  removeEventListener() {}
  receive(message: Request) { this.onmessage?.({ data: JSON.stringify(message) }); }
  close() {
    if (this.readyState === 3) return;
    this.readyState = 3;
    queueMicrotask(() => this.onclose?.());
  }
  send(data: string) {
    const message = JSON.parse(data) as Request;
    requests.push(message);
    queueMicrotask(() => {
      switch (message.type) {
        case "BootstrapTerminalDelivery":
          this.receive({ type: "TerminalBootstrapResult", data: { delivery: bootstrap } });
          break;
        case "ReadTerminalResult":
          this.receive({ type: "TerminalResult", data: { delivery } });
          break;
        case "AckTerminalDelivery":
          if (ackOutcome === "reject") this.onerror?.();
          else if (ackOutcome === "pending") pendingAcks.push(this);
          else this.receive({ type: "TerminalDeliveryAcknowledged", data: { delivery_id: delivery.deliveryId } });
          break;
        case "Reconnect":
          this.receive({ type: "GameStarted", data: {
            state: gameStateFactory.build(), your_player: 0, full_key: session.fullKey,
            game_code: session.gameCode, player_token: session.playerToken,
          } });
          break;
      }
    });
  }
}

let sequence = 0;
beforeEach(async () => {
  localStorage.clear();
  useGameStore.getState().reset();
  vi.clearAllMocks();
  const idb = await import("idb-keyval");
  vi.mocked(idb.set).mockImplementation(
    (await vi.importActual<typeof import("idb-keyval")>("idb-keyval")).set,
  );
  sockets = [];
  requests = [];
  pendingAcks = [];
  ackOutcome = "success";
  const key = { game_code: `NATIVE${++sequence}`, generation: 2 };
  session = { gameCode: key.game_code, playerId: 0, playerToken: "pre-terminal-token", fullKey: key };
  delivery = {
    key, terminalRevision: 7, deliveryId: "native-delivery", credential: "native-credential",
    display: { winner: 0, reason: "Commander victory" },
    finalView: gameStateFactory.build({ waiting_for: { type: "GameOver", data: { winner: 0 } } }),
  };
  bootstrap = null;
  saveActiveGame({ id: GAME_ID, mode: "ai", difficulty: "Medium", nativeSession: session });
});
afterEach(async () => {
  cleanup();
  await act(async () => { for (const socket of sockets) socket.close(); });
  vi.restoreAllMocks();
});

function pointer(): ActiveGameMeta | null {
  return loadActiveGame();
}

/** Mounts the provider with GamePage's display consumer: it clears the game. */
function mountProvider() {
  const displayCleared: Promise<void>[] = [];
  const events = vi.fn((event: WsAdapterEvent) => {
    if (event.type === "terminalDelivery") displayCleared.push(clearGame(GAME_ID));
  });
  const mounted = render(
    <GameProvider gameId={GAME_ID} mode="ai" onWsEvent={events}><div /></GameProvider>,
  );
  return { ...mounted, events, displayCleared: () => Promise.all(displayCleared) };
}

/** Resumes the pointer into a live game and returns its playable socket. */
async function resumeLiveGame() {
  const mounted = mountProvider();
  await waitFor(() => expect(controller).toHaveBeenCalledOnce());
  // A live seat is asked for its terminal result first, then reconnects.
  const types = requests.map(({ type }) => type).filter((type) => type !== "ClientHello");
  expect(types).toEqual(["BootstrapTerminalDelivery", "Reconnect"]);
  return { ...mounted, live: sockets[sockets.length - 1]! };
}

async function reopen() {
  const before = requests.length;
  const mounted = mountProvider();
  const after = () => requests.slice(before).map(({ type }) => type).filter((type) => type !== "ClientHello");
  return { ...mounted, after };
}

describe("native solo terminal recovery across real persistence and reopen", () => {
  it("recovers the server's result on reopen after the local commit failed", async () => {
    const { live, events, unmount } = await resumeLiveGame();
    const idb = await import("idb-keyval");
    vi.mocked(idb.set).mockRejectedValueOnce(new Error("quota exceeded"));
    await act(async () => { live.receive({ type: "TerminalResult", data: { delivery } }); });
    await waitFor(() => expect(events).toHaveBeenCalledWith({
      type: "terminalUnavailable", message: "Failed to retain terminal delivery",
    }));
    expect(events).not.toHaveBeenCalledWith(expect.objectContaining({ type: "terminalDelivery" }));
    expect(requests.map(({ type }) => type)).not.toContain("AckTerminalDelivery");
    expect(pointer()).toEqual({ id: GAME_ID, mode: "ai", difficulty: "Medium", nativeSession: session });
    unmount();
    await act(async () => { for (const socket of sockets) socket.close(); });

    // The server prepared the result and retired the playable key.
    bootstrap = delivery;
    const controllers = controller.mock.calls.length;
    const reopened = await reopen();
    await waitFor(() => expect(reopened.events).toHaveBeenCalledWith({ type: "terminalDelivery", delivery }));
    await waitFor(() => expect(pointer()).toBeNull());
    expect(reopened.after()).toEqual(["BootstrapTerminalDelivery", "AckTerminalDelivery"]);
    expect(requests).toContainEqual({
      type: "BootstrapTerminalDelivery",
      data: { request: { key: session.fullKey, playerToken: session.playerToken, requestId: expect.any(String) } },
    });
    expect(await loadFullTerminalDelivery(delivery.key)).toEqual(delivery);
    expect(controller.mock.calls.length).toBe(controllers);
  });

  it("re-displays the same result and retries the ACK on reopen after the bridge ACK failed", async () => {
    ackOutcome = "reject";
    const { live, events, unmount, displayCleared } = await resumeLiveGame();
    await act(async () => { live.receive({ type: "TerminalResult", data: { delivery } }); });
    await waitFor(() => expect(events).toHaveBeenCalledWith({ type: "terminalDelivery", delivery }));
    await waitFor(() => expect(requests.map(({ type }) => type)).toContain("AckTerminalDelivery"));
    await displayCleared();
    // Displaying the result cleared the game, but not its recovery pointer.
    expect(pointer()).toEqual({
      id: GAME_ID, mode: "ai", difficulty: "Medium", nativeSession: { ...session, terminalOnly: true },
    });
    unmount();
    await act(async () => { for (const socket of sockets) socket.close(); });

    ackOutcome = "success";
    const controllers = controller.mock.calls.length;
    const reopened = await reopen();
    await waitFor(() => expect(reopened.events).toHaveBeenCalledWith({ type: "terminalDelivery", delivery }));
    expect(reopened.events).toHaveBeenCalledWith({
      type: "terminalDelivery", delivery: { ...delivery, finalView: undefined },
    });
    await waitFor(() => expect(pointer()).toBeNull());
    expect(reopened.after()).toEqual(["ReadTerminalResult", "AckTerminalDelivery"]);
    expect(requests.at(-1)).toEqual({
      type: "AckTerminalDelivery",
      data: { delivery_id: delivery.deliveryId, credential: delivery.credential },
    });
    expect(controller.mock.calls.length).toBe(controllers);
  });

  it.each([
    ["the completed game's pointer", null],
    ["a replacement pointer for another game", { id: "rematch", mode: "ai", difficulty: "Medium" }],
    ["a replacement pointer for another native session", {
      id: GAME_ID, mode: "ai", difficulty: "Medium",
      nativeSession: { gameCode: "NEXT", playerId: 0, playerToken: "next-token", fullKey: { game_code: "NEXT", generation: 1 } },
    }],
  ] as const)("a successful ACK on the live game clears %s only if it is this game's", async (_case, replacement) => {
    ackOutcome = "pending";
    const { live, events, displayCleared } = await resumeLiveGame();
    await act(async () => { live.receive({ type: "TerminalResult", data: { delivery } }); });
    await waitFor(() => expect(events).toHaveBeenCalledWith({ type: "terminalDelivery", delivery }));
    await waitFor(() => expect(pendingAcks).toHaveLength(1));
    await displayCleared();
    expect(pointer()?.nativeSession).toEqual({ ...session, terminalOnly: true });
    if (replacement) saveActiveGame(replacement);

    await act(async () => {
      pendingAcks[0]!.receive({ type: "TerminalDeliveryAcknowledged", data: { delivery_id: delivery.deliveryId } });
    });
    await waitFor(() => expect(pointer()).toEqual(replacement));
  });
});
