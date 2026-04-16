/**
 * Focused tests for the `P2PHostAdapter` ↔ `BrokerClient` integration —
 * specifically the unregister-timing invariants (reviewer G4):
 *   - Fires exactly once, after `initializeGame` succeeds.
 *   - Does NOT fire when `initializeGame` throws.
 * Plus the `startNow()` escape hatch that resolves the guest-deck gate.
 *
 * The WASM engine is mocked end-to-end so a push on `initializeGame`'s
 * resolver is the only way the gate opens.
 */
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type Peer from "peerjs";
import type { DataConnection } from "peerjs";

import { P2PHostAdapter } from "../p2p-adapter";
import type { BrokerClient } from "../../services/brokerClient";
import { FakeDataConnection } from "../../network/__tests__/fakeDataConnection";

const mocks = vi.hoisted(() => ({
  initializeGame: vi.fn(async () => ({ events: [] })),
  getLegalActions: vi.fn(async () => ({
    actions: [],
    autoPassRecommended: false,
  })),
  getFilteredState: vi.fn(async (pid: number) => ({ filteredFor: pid })),
  setMultiplayerMode: vi.fn(async (_enabled: boolean) => undefined),
}));

vi.mock("../wasm-adapter", () => ({
  WasmAdapter: vi.fn().mockImplementation(() => ({
    initialize: vi.fn(async () => undefined),
    initializeGame: mocks.initializeGame,
    submitAction: vi.fn(async () => ({ events: [] })),
    getState: vi.fn(async () => ({})),
    getLegalActions: mocks.getLegalActions,
    getFilteredState: mocks.getFilteredState,
    setMultiplayerMode: mocks.setMultiplayerMode,
    dispose: vi.fn(),
  })),
}));

interface FakePeer {
  on(event: string, handler: (conn: DataConnection) => void): void;
  off(event: string, handler: (conn: DataConnection) => void): void;
  connect(): never;
  destroy(): void;
}

function createFakePeer(): {
  peer: FakePeer;
  onGuestConnected: (handler: (conn: DataConnection) => void) => () => void;
  emitConnection: (conn: DataConnection) => void;
} {
  const handlers = new Set<(conn: DataConnection) => void>();
  return {
    peer: {
      on() {},
      off() {},
      connect() {
        throw new Error("not used in tests");
      },
      destroy() {},
    },
    onGuestConnected(handler) {
      handlers.add(handler);
      return () => handlers.delete(handler);
    },
    emitConnection(conn) {
      for (const h of handlers) h(conn);
    },
  };
}

class FakeOpenableConnection extends FakeDataConnection {
  private openHandlers = new Set<() => void>();
  override on(event: string, handler: (...args: unknown[]) => void): this {
    if (event === "open") {
      this.openHandlers.add(handler as () => void);
      return this;
    }
    return super.on(event, handler);
  }
  fireOpen() {
    for (const h of this.openHandlers) h();
  }
}

function makeBrokerMock(): BrokerClient & {
  unregister: ReturnType<typeof vi.fn>;
} {
  const unregister = vi.fn(async (_code: string) => undefined);
  return {
    serverInfo: {
      version: "",
      buildCommit: "",
      protocolVersion: 1,
      mode: "LobbyOnly",
    },
    registerHost: vi.fn(async () => ({
      gameCode: "GAME01",
      playerToken: "tok",
    })),
    updateMetadata: vi.fn(),
    unregister,
    close: vi.fn(),
  };
}

function makeHost(
  broker?: BrokerClient,
  brokerGameCode?: string,
  playerCount = 2,
) {
  const { peer, onGuestConnected, emitConnection } = createFakePeer();
  const hostDeck = {
    player: { main_deck: ["Mountain"], sideboard: [] },
    opponent: { main_deck: ["Forest"], sideboard: [] },
    ai_decks: [],
  };
  const adapter = new P2PHostAdapter(
    hostDeck,
    peer as unknown as Peer,
    onGuestConnected,
    playerCount,
    undefined,
    undefined,
    5_000,
    broker,
    true,
    brokerGameCode,
  );
  return { adapter, emitConnection };
}

beforeEach(() => {
  mocks.initializeGame.mockClear();
  mocks.initializeGame.mockImplementation(async () => ({ events: [] }));
  mocks.getLegalActions.mockClear();
  mocks.getFilteredState.mockClear();
  mocks.setMultiplayerMode.mockClear();
});

afterEach(() => {
  vi.clearAllMocks();
});

describe("P2PHostAdapter — broker integration", () => {
  it("rejects construction when broker is set without a brokerGameCode", () => {
    const broker = makeBrokerMock();
    const { peer, onGuestConnected } = createFakePeer();
    const hostDeck = {
      player: { main_deck: [], sideboard: [] },
      opponent: { main_deck: [], sideboard: [] },
      ai_decks: [],
    };
    expect(
      () =>
        new P2PHostAdapter(
          hostDeck,
          peer as unknown as Peer,
          onGuestConnected,
          2,
          undefined,
          undefined,
          5_000,
          broker,
          true,
        ),
    ).toThrow("brokerGameCode is required");
  });

  it("fires broker.unregister exactly once after initializeGame succeeds", async () => {
    const broker = makeBrokerMock();
    const { adapter, emitConnection } = makeHost(broker, "GAME01");
    await adapter.initialize();

    // Connect one guest and send its deck so the guest-deck gate resolves.
    const conn = new FakeOpenableConnection();
    emitConnection(conn as unknown as DataConnection);
    conn.fireOpen();
    conn.simulateData({
      type: "guest_deck",
      deckData: {
        player: { main_deck: ["Forest"], sideboard: [] },
      },
    });

    await adapter.initializeGame();
    // Allow the fire-and-forget `.unregister(...)` chain to settle.
    await new Promise((r) => setTimeout(r, 0));

    expect(broker.unregister).toHaveBeenCalledTimes(1);
    expect(broker.unregister).toHaveBeenCalledWith("GAME01");
  });

  it("does NOT call broker.unregister when initializeGame throws", async () => {
    const broker = makeBrokerMock();
    mocks.initializeGame.mockRejectedValueOnce(new Error("WASM panic"));
    const { adapter, emitConnection } = makeHost(broker, "GAME01");
    await adapter.initialize();

    const conn = new FakeOpenableConnection();
    emitConnection(conn as unknown as DataConnection);
    conn.fireOpen();
    conn.simulateData({
      type: "guest_deck",
      deckData: {
        player: { main_deck: ["Forest"], sideboard: [] },
      },
    });

    await expect(adapter.initializeGame()).rejects.toThrow("WASM panic");
    await new Promise((r) => setTimeout(r, 0));

    // Critical: when engine-init fails, leaving the lobby entry alive
    // means the 5-minute `check_expired` backstop eventually reaps it
    // — but a clobbered unregister here would orphan the broker with
    // a "lobby gone, engine failed to start" stuck state.
    expect(broker.unregister).not.toHaveBeenCalled();
  });

  it("works without a broker (pure-PeerJS room) — no unregister attempted", async () => {
    const { adapter, emitConnection } = makeHost();
    await adapter.initialize();

    const conn = new FakeOpenableConnection();
    emitConnection(conn as unknown as DataConnection);
    conn.fireOpen();
    conn.simulateData({
      type: "guest_deck",
      deckData: {
        player: { main_deck: ["Forest"], sideboard: [] },
      },
    });

    await expect(adapter.initializeGame()).resolves.toBeDefined();
  });

  it("startNow() resolves the guest-deck gate so initializeGame runs with partial seats", async () => {
    // 3-seat room: one guest has joined and submitted a deck (opens
    // `guestDecks.size = 1`), but seat 2's guest hasn't dialed in yet.
    // Without `startNow()`, `initializeGame` would wait indefinitely for
    // seat 2. `startNow()` unblocks it so the host can launch with the
    // seat count that actually showed up.
    const { adapter, emitConnection } = makeHost(undefined, undefined, 3);
    await adapter.initialize();

    const conn = new FakeOpenableConnection();
    emitConnection(conn as unknown as DataConnection);
    conn.fireOpen();
    conn.simulateData({
      type: "guest_deck",
      deckData: {
        player: { main_deck: ["Forest"], sideboard: [] },
      },
    });

    const initPromise = adapter.initializeGame();
    // Without `startNow()`, initializeGame would hang on the
    // `guestDecks.size < playerCount - 1` gate (1 < 2).
    adapter.startNow();

    await expect(initPromise).resolves.toBeDefined();
  });
});
