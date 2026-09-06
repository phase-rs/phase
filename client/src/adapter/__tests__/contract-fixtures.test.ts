import { readFileSync } from "node:fs";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";

import { beforeEach, describe, expect, it, vi } from "vitest";

import type { InteractionSubmission } from "../generated/interaction";
import type { GameAction, GameObject, GameState, WaitingFor } from "../types";
import { PROTOCOL_VERSION, WebSocketAdapter } from "../ws-adapter";

class MockWebSocket extends EventTarget {
  static OPEN = 1;
  static last: MockWebSocket | null = null;
  readyState = MockWebSocket.OPEN;
  onopen: (() => void) | null = null;
  onmessage: ((event: { data: string }) => void) | null = null;
  onerror: (() => void) | null = null;
  onclose: (() => void) | null = null;
  send = vi.fn();
  close = vi.fn();
  constructor(public url: string) {
    super();
    MockWebSocket.last = this;
  }
  dispatchSynthetic(type: "message" | "close", data?: string) {
    if (type === "message" && data !== undefined) {
      this.onmessage?.({ data });
      this.dispatchEvent(new MessageEvent("message", { data }));
    } else if (type === "close") {
      this.onclose?.();
      this.dispatchEvent(new Event("close"));
    }
  }
}

vi.stubGlobal("WebSocket", MockWebSocket);

const SERVER_HELLO = JSON.stringify({
  type: "ServerHello",
  data: {
    server_version: "0.0.0-test",
    build_commit: "testhash",
    protocol_version: PROTOCOL_VERSION,
    mode: "Full",
  },
});

/** Settle the async handshake so `this.ws` is bound before tests fire
 *  game-level frames. Mirrors the helper in `ws-adapter.test.ts`. */
async function completeHandshake(adapter: WebSocketAdapter): Promise<MockWebSocket> {
  await Promise.resolve();
  const ws = MockWebSocket.last!;
  ws.dispatchSynthetic("message", SERVER_HELLO);
  await Promise.resolve();
  await Promise.resolve();
  return (adapter as unknown as { ws: MockWebSocket }).ws;
}
vi.stubGlobal("localStorage", {
  getItem: vi.fn(() => null),
  setItem: vi.fn(),
  removeItem: vi.fn(),
});

function readFixture<T>(name: string): T {
  const fixturesDir = resolve(
    dirname(fileURLToPath(import.meta.url)),
    "../../../../fixtures/adapter-contract",
  );
  return JSON.parse(readFileSync(resolve(fixturesDir, name), "utf8")) as T;
}

describe("shared adapter contract fixtures", () => {
  beforeEach(() => {
    vi.clearAllMocks();
  });

  it("drives GameStarted through the websocket adapter", async () => {
    const fixture = readFixture<{ type: "GameStarted"; data: { state: GameState } }>("game_started.json");
    const adapter = new WebSocketAdapter(
      "ws://localhost:9374/ws",
      "host",
      { main_deck: [], sideboard: [] },
    );
    const listener = vi.fn();
    adapter.onEvent(listener);

    MockWebSocket.last = null;
    const initPromise = adapter.initialize();
    const ws = await completeHandshake(adapter);
    ws.dispatchSynthetic("message", JSON.stringify(fixture));
    await initPromise;

    expect(listener).toHaveBeenCalledWith({
      type: "playerIdentity",
      playerId: 0,
      opponentName: "Opponent",
      playerNames: { 0: "Host", 1: "Opponent" },
    });
  });

  it("drives StateUpdate through the websocket adapter", async () => {
    const gameStartedFixture = readFixture<{ type: "GameStarted"; data: { state: GameState } }>("game_started.json");
    const stateUpdateFixture = readFixture<{
      type: "StateUpdate";
      data: { state: GameState; events: unknown[] };
    }>("state_update.json");

    const adapter = new WebSocketAdapter(
      "ws://localhost:9374/ws",
      "host",
      { main_deck: [], sideboard: [] },
    );
    const listener = vi.fn();
    adapter.onEvent(listener);

    MockWebSocket.last = null;
    const initPromise = adapter.initialize();
    const ws = await completeHandshake(adapter);
    ws.dispatchSynthetic("message", JSON.stringify(gameStartedFixture));
    await initPromise;

    ws.dispatchSynthetic("message", JSON.stringify(stateUpdateFixture));

    // The engine pair now travels as one seq-stamped `EngineSnapshot`, so the
    // state is asserted through `snapshot.state` rather than a sibling field.
    expect(listener).toHaveBeenCalledWith(
      expect.objectContaining({
        type: "stateChanged",
        snapshot: expect.objectContaining({
          state: expect.objectContaining(stateUpdateFixture.data.state),
          seq: expect.any(Number),
        }),
        events: stateUpdateFixture.data.events,
      }),
    );
  });

  it("loads the curated action, waiting state, and object fixtures", () => {
    const gameAction = readFixture<GameAction>("game_action.json");
    const waitingFor = readFixture<WaitingFor>("waiting_for.json");
    const categoryChoice = readFixture<WaitingFor>("waiting_for_category_choice.json");
    const gameObject = readFixture<GameObject>("game_object.json");

    expect(gameAction.type).toBe("ChooseLegend");
    expect(waitingFor.type).toBe("EffectZoneChoice");
    expect(categoryChoice.type).toBe("CategoryChoice");
    if (categoryChoice.type === "CategoryChoice") {
      expect(categoryChoice.data.chooser_scope).toBe("ControllerForAll");
      expect(categoryChoice.data.source_controller).toBe(0);
      expect(categoryChoice.data.choose_filter?.type).toBe("Typed");
      expect(categoryChoice.data.sacrifice_filter?.type).toBe("Typed");
      expect(JSON.stringify(categoryChoice.data.choose_filter)).toContain('"Non":"Land"');
      expect(JSON.stringify(categoryChoice.data.sacrifice_filter)).toContain('"Non":"Land"');
      expect(categoryChoice.data.eligible_per_category[0]).toEqual([10]);
      expect(categoryChoice.data.all_kept).toEqual([]);
    }
    expect(gameObject.name).toBe("Fixture Bear");
    expect(gameObject.id).toBe(1);
  });

  it("loads the curated shortcut allocation submission", () => {
    // `readFixture`'s `as T` is an unchecked cast, so the type annotation links
    // nothing on its own — the values are what tie this end to the Rust row.
    const submission = readFixture<InteractionSubmission>("shortcut_allocation_submission.json");

    expect(submission.interactionId).toBe("i-1.0.1");
    expect(submission.response.type).toBe("shortcut");
    if (submission.response.type !== "shortcut") return;
    expect(submission.response.data.decision).toEqual({
      type: "fixed",
      data: { iterations: 18 },
    });
    // Every answerable point the offer publishes, not just the allocation: the
    // Rust row drives this same payload through production submission, which
    // refuses a partial pin set.
    expect(submission.response.data.pins).toEqual([
      { group: 0, choiceIds: ["i-1.0.1.k0"], amounts: [] },
      { group: 1, choiceIds: ["i-1.0.1.k2"], amounts: [] },
      {
        group: 2,
        choiceIds: ["i-1.0.1.k4", "i-1.0.1.k5", "i-1.0.1.k6"],
        amounts: [
          { choiceId: "i-1.0.1.k4", amount: 6 },
          { choiceId: "i-1.0.1.k5", amount: 6 },
          { choiceId: "i-1.0.1.k6", amount: 6 },
        ],
      },
    ]);
  });
});
