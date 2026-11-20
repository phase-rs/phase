import { readFile } from "node:fs/promises";
import { resolve } from "node:path";
import { afterEach, beforeAll, expect, it, vi } from "vitest";
import init, { clear_game_state } from "@wasm/engine";
import { FakeDataConnection } from "../../network/__tests__/fakeDataConnection";
import { WIRE_PROTOCOL_VERSION, type P2PMessage } from "../../network/protocol";
import type { TransportConnection, TransportPeer } from "../../network/transport";
import { p2pFinalStateCommitment } from "../../services/p2pTerminalResult";
import { FORMAT_DEFAULTS } from "../../stores/multiplayerStore";
import { P2PGuestAdapter, P2PHostAdapter } from "../p2p-adapter";
import type { GameAction, GameEvent, GameState } from "../types";
import { WasmAdapter } from "../wasm-adapter";

const forestOracle = "b34bb2dc-c1af-4d77-b0b3-a0fb342a5fc6";
const forest = {
  name: "Forest", scryfall_oracle_id: forestOracle, mana_cost: { type: "NoCost" },
  card_type: { supertypes: ["Basic"], core_types: ["Land"], subtypes: ["Forest"] },
  keywords: [], abilities: [], triggers: [], static_abilities: [], replacements: [],
};
const control = {
  ...forest, name: "Control", scryfall_oracle_id: "uncovered-control",
  card_type: { supertypes: [], core_types: ["Land"], subtypes: [] },
};
const deck = { main_deck: ["Forest", "Forest", "Forest", "Control"], sideboard: [], commander: [],
  printings: [{ section: "main_deck", name: "Forest", count: 1, printing: { set_code: "m21", collector_number: "272" } }],
};
const opponent = { main_deck: Array<string>(12).fill("Control"), sideboard: [], commander: [] };
const stamp = { set_code: "m21", collector_number: "272", automatic_art_catalog_version: 1 };

class LinkedConnection extends FakeDataConnection {
  readonly peer = "phase-art-test";
  other?: LinkedConnection;

  override send(bytes: unknown) {
    super.send(bytes);
    void this.other?.simulateData(bytes);
  }

  protected override onDecodedSend(_message: P2PMessage) {
    // The other production PeerSession answers keep-alive frames.
  }
}

function connectPair(accept: (connection: TransportConnection) => void) {
  const host = new LinkedConnection();
  const guest = new LinkedConnection();
  host.other = guest;
  guest.other = host;
  accept(host as unknown as TransportConnection);
  return { host, guest: guest as unknown as TransportConnection };
}

beforeAll(async () => {
  const bytes = await readFile(resolve(__dirname, "../../wasm/engine_wasm_bg.wasm"));
  const module = await WebAssembly.compile(bytes);
  expect((await init({ module_or_path: module })).memory).toBeInstanceOf(WebAssembly.Memory);
}, 60_000);

afterEach(() => {
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
  clear_game_state();
});

it("delivers assigned art through fresh setup, live updates and reconnect", async () => {
  vi.stubGlobal("Worker", class {
    constructor() { throw new Error("force the production main-thread fallback"); }
  });
  const fetcher = vi.fn(async (input: RequestInfo | URL) =>
    String(input) === "/card-data.json"
      ? new Response(JSON.stringify({ forest, control }))
      : new Response(null, { status: 404 }),
  );
  vi.stubGlobal("fetch", fetcher);
  const initHost = vi.spyOn(WasmAdapter.prototype, "initializeMultiplayerHostGame");
  const transition = vi.spyOn(WasmAdapter.prototype, "getViewerTransitionSnapshot");
  const snapshot = vi.spyOn(WasmAdapter.prototype, "getViewerSnapshot");
  const peer = { id: "phase-art-host", destroyed: false, disconnected: false,
    on() { return this; }, off() { return this; }, destroy() {},
  } as unknown as TransportPeer;
  let accept!: (connection: TransportConnection) => void;
  const host = new P2PHostAdapter({ player: deck, opponent }, peer, (handler) => {
    accept = handler;
    return () => {};
  }, 2, FORMAT_DEFAULTS.Freeform);
  const projection = vi.spyOn(host as unknown as {
    projectTransitionForViewer(viewer: number, events: GameEvent[], generation: number): Promise<{ fresh: boolean }>;
  }, "projectTransitionForViewer");
  let guest: P2PGuestAdapter | undefined;
  let reconnect: P2PGuestAdapter | undefined;
  try {
    const joined = vi.fn();
    host.onEvent((event) => {
      if (event.type === "roomFull") joined();
    });
    await host.initialize();
    const initial = connectPair(accept);
    guest = new P2PGuestAdapter({ player: opponent }, peer, peer.id, initial.guest);
    await guest.initialize();
    await vi.waitFor(() => expect(joined).toHaveBeenCalled());
    const random = vi.spyOn(Math, "random").mockReturnValue(4910 / Number.MAX_SAFE_INTEGER);
    try {
      await host.initializeGame();
    } finally {
      random.mockRestore();
    }
    await guest.initializeGame();
    expect(initHost).toHaveBeenCalledOnce();
    const setupResult = await initHost.mock.results[0].value;
    expect(setupResult.events.length).toBeGreaterThan(0);
    expect(transition).toHaveBeenCalledWith(1, setupResult.events);
    expect(projection).toHaveBeenCalledWith(1, setupResult.events, expect.any(Number));
    expect((await projection.mock.results[0].value).fresh).toBe(true);
    const setup = (await initial.host.getSentMessages() as P2PMessage[])
      .find((frame) => frame.type === "game_setup");
    expect(setup?.wireProtocolVersion).toBe(WIRE_PROTOCOL_VERSION);
    expect(setup?.assignedPlayerId).toBe(1);
    expect(setup?.state.players).toHaveLength(2);
    expect((await guest.getState()).waiting_for.type).toBe("ChooseStartingPlayer");
    expect(fetcher).toHaveBeenCalledWith("/card-data.json");

    const submit = (seat: number, action: GameAction) =>
      seat === 0 ? host.submitAction(action, 0) : guest!.submitAction(action, 1);
    const waiting = (await host.getState()).waiting_for;
    if (waiting.type !== "ChooseStartingPlayer") throw new Error("expected a starting-player choice");
    await submit(waiting.data.player, { type: "ChooseStartingPlayer", data: { player: 0 } });

    const wasm = initHost.mock.contexts[0] as WasmAdapter;
    const owner = await wasm.getViewerSnapshot(0);
    expect(owner.actions.some((action) => action.type === "MulliganDecision")).toBe(true);
    expect(owner.state.players[0].hand).toContain(1);
    const card = owner.state.objects[1];
    expect(card.name).toBe("Forest");
    expect(card.card_id).toBe(1);
    expect(card.printing).toEqual(stamp);
    expect(card).toHaveProperty("base_printing", stamp);
    await vi.waitFor(async () => {
      const hidden = (await guest!.getState()).objects[1];
      expect(hidden).toBeDefined();
      expect(hidden.name).toBe("Hidden Card");
      expect(hidden.printing).toBeUndefined();
      expect(hidden).not.toHaveProperty("base_printing");
    });

    for (const seat of [0, 1]) {
      await submit(seat, { type: "MulliganDecision", data: { choice: { type: "Keep" } } });
    }
    let play: GameAction | undefined;
    for (let i = 0; i < 8; i++) {
      const legal = await host.getLegalActions();
      play = legal.actions.find((action) => action.type === "PlayLand" && action.data.object_id === 1);
      if (play) break;
      const priority = (await host.getState()).waiting_for;
      if (priority.type !== "Priority") throw new Error("expected priority on the way to main phase");
      await submit(priority.data.player, { type: "PassPriority" });
    }
    if (!play) throw new Error("missing legal PlayLand for the stamped Forest");
    transition.mockClear();
    projection.mockClear();
    const live = await host.submitAction(play, 0);
    expect(live.events.length).toBeGreaterThan(0);
    expect(transition).toHaveBeenCalledWith(1, live.events);
    expect(projection).toHaveBeenCalledWith(1, live.events, expect.any(Number));
    expect((await projection.mock.results[0].value).fresh).toBe(true);
    function assertPublic(state: GameState) {
      const object = state.objects[1];
      expect(object).toMatchObject({
        card_id: card.card_id, name: "Forest", zone: "Battlefield",
        printing: stamp, base_printing: stamp,
      });
    }
    const update = (await initial.host.getSentMessages() as P2PMessage[])
      .find((frame) => frame.type === "state_update" && frame.state.objects[1]?.zone === "Battlefield");
    if (update?.type !== "state_update") throw new Error("missing public state update");
    assertPublic(update.state);
    await vi.waitFor(async () => assertPublic(await guest!.getState()));

    guest.dispose();
    initial.host.simulateClose();
    snapshot.mockClear();
    const resumed = connectPair(accept);
    if (!setup?.playerToken) throw new Error("game setup did not admit a reconnect token");
    reconnect = new P2PGuestAdapter({ player: opponent }, peer, peer.id, resumed.guest, setup.playerToken);
    await reconnect.initialize();
    await reconnect.initializeGame();
    expect(snapshot).toHaveBeenCalledWith(1);
    const ack = (await resumed.host.getSentMessages() as P2PMessage[]).find((frame) => frame.type === "reconnect_ack");
    if (ack?.type !== "reconnect_ack") throw new Error("missing reconnect acknowledgement");
    expect(ack.wireProtocolVersion).toBe(WIRE_PROTOCOL_VERSION);
    assertPublic(ack.state);
    const received = await reconnect.getState();
    assertPublic(received);
    const mutated = structuredClone(received);
    delete mutated.objects[1].printing!.automatic_art_catalog_version;
    expect(await p2pFinalStateCommitment(mutated)).not.toBe(await p2pFinalStateCommitment(received));
  } finally {
    reconnect?.dispose();
    guest?.dispose();
    host.dispose();
  }
}, 30_000);
