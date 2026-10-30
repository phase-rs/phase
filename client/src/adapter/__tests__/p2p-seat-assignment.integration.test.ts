import { readFile } from "node:fs/promises";
import { resolve } from "node:path";
import React from "react";
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { MemoryRouter } from "react-router";
import { afterEach, beforeAll, beforeEach, describe, expect, it, vi } from "vitest";
import initEngine, { apply_seat_mutation } from "@wasm/engine";
import type * as EngineModule from "@wasm/engine";
import type * as WasmAdapterModule from "../wasm-adapter";
import type * as ConnectionModule from "../../network/connection";
import type * as PersistenceModule from "../../services/gamePersistence";
import type { DataConnection } from "peerjs";
import type { SeatMutation, SeatMutationResult, SeatState, SeatView } from "../../multiplayer/seatTypes";
import { FakeDataConnection } from "../../network/__tests__/fakeDataConnection";
import { WIRE_PROTOCOL_VERSION } from "../../network/protocol";
import { P2PHostAdapter } from "../p2p-adapter";
import { HostControlTile } from "../../components/chrome/HostControlTile";
import { FORMAT_DEFAULTS, useMultiplayerStore } from "../../stores/multiplayerStore";
import type { HostingSettings } from "../../stores/multiplayerStore";

const lifecycle = vi.hoisted(() => ({
  initialize: vi.fn<() => Promise<void>>(async () => {}),
  inputs: [] as string[],
  connect: null as null | ((connection: DataConnection) => void),
}));

// Only lifecycle/transport services are stubbed. Both seat exports below are
// the real compiled Rust functions; no JS transition table participates.
vi.mock("../wasm-adapter", async (original) => {
  const actual = await original<typeof WasmAdapterModule>();
  // The hoisted mock factory runs before static bindings are initialized.
  const engine = await vi.importActual<typeof EngineModule>("@wasm/engine");
  return {
    ...actual,
    getHostAdapter: () => ({
      initialize: lifecycle.initialize,
      projectSeatView: async (state: string) => engine.project_seat_view(state),
      applySeatMutation: async (state: string, mutation: string) => {
        lifecycle.inputs.push(state);
        return engine.apply_seat_mutation(state, mutation);
      },
      evaluateDeckFormatGate: async () => ({ compatible: true, reasons: [] }),
      releaseHostSession: async () => {},
    }),
  };
});
vi.mock("../../network/connection", async (original) => ({
  ...await original<typeof ConnectionModule>(),
  hostRoom: async () => ({
    peer: { id: "seat-assignment-host", destroy: () => {} },
    destroy: () => {},
    roomCode: "QUEUE1",
    onGuestConnected: (callback: (connection: DataConnection) => void) => {
      lifecycle.connect = callback;
      return () => { lifecycle.connect = null; };
    },
  }),
}));
vi.mock("../../services/gamePersistence", async (original) => ({
  ...await original<typeof PersistenceModule>(),
  saveP2PHostSession: async () => {},
}));
vi.mock("../../services/aiDeckCatalog", () => ({
  useAiDeckCatalog: () => ({
    candidates: [{
      id: "queue-deck", name: "Queue deck", source: "bundled",
      deck: { main: [{ count: 1, name: "Forest" }], sideboard: [] },
      coveragePct: null, archetype: null, bracket: null,
    }],
    allCandidates: [], loading: false, error: null,
  }),
}));

function deferred() {
  // The client targets ES2020, which does not expose Promise.withResolvers.
  let resolve!: () => void;
  const promise = new Promise<void>((done) => { resolve = done; });
  return { promise, resolve };
}
const deck = { main_deck: ["Forest"], sideboard: [], commander: [] };
const ai = { type: "Ai" as const, data: { difficulty: "Medium", deck: { type: "DeckList" as const, data: deck } } };
function fill(seatIndex: number): SeatMutation {
  return { type: "SetKind", data: { seatIndex, kind: ai, expectedKind: { type: "WaitingHuman" } } };
}
const hosts: P2PHostAdapter[] = [];
function privateState(host: P2PHostAdapter) {
  return host as unknown as {
    pregameSeatState: SeatState;
    playerTokens: Map<number, string>;
    guestSessions: Map<number, unknown>;
    guestDecks: Map<number, unknown>;
    aiDecks: Map<number, unknown>;
    pregameOpQueue: Promise<void>;
  };
}
function makeHost() {
  const host = new P2PHostAdapter(
    { player: deck, opponent: deck, ai_decks: [] },
    { id: "queue-host", destroy: () => {} } as never,
    (callback) => { lifecycle.connect = callback; return () => { lifecycle.connect = null; }; },
    4, FORMAT_DEFAULTS.Commander,
  );
  hosts.push(host);
  return host;
}
async function join(name = "Guest") {
  const connection = new FakeDataConnection();
  expect(lifecycle.connect).not.toBeNull();
  lifecycle.connect!(connection as unknown as DataConnection);
  await connection.simulateData({
    type: "guest_deck", displayName: name,
    deckData: { player: deck }, wireProtocolVersion: WIRE_PROTOCOL_VERSION,
  });
  return connection;
}
async function settled(host: P2PHostAdapter) {
  await privateState(host).pregameOpQueue;
}
async function publishedMutation(connection: FakeDataConnection) {
  await vi.waitFor(async () => {
    const sent = await connection.getSentMessages() as Array<{ type: string; view?: SeatView }>;
    const mutationIndex = sent.findIndex((message) => message.type === "seat_mutate");
    expect(mutationIndex).toBeGreaterThanOrEqual(0);
    expect(sent.slice(mutationIndex + 1)).toContainEqual(expect.objectContaining({
      type: "seat_snapshot", view: expect.objectContaining({ seats: expect.arrayContaining([{ type: "JoinedHuman" }]) }),
    }));
  });
}
function retainGuest(host: P2PHostAdapter, connection: FakeDataConnection, seat = 1) {
  expect(host.getPlayerSlots()[seat]).toMatchObject({ name: "Guest", kind: { type: "JoinedHuman" } });
  expect(privateState(host).pregameSeatState.tokens[seat]).toBe(privateState(host).playerTokens.get(seat));
  expect(privateState(host).playerTokens.get(seat)).toEqual(expect.any(String));
  expect(privateState(host).playerTokens.get(seat)).not.toBe("");
  expect(privateState(host).guestSessions.has(seat)).toBe(true);
  expect(privateState(host).guestDecks.get(seat)).toEqual(deck);
  expect(privateState(host).aiDecks.has(seat)).toBe(false);
  expect(connection.open).toBe(true);
}
function renderControls(host: P2PHostAdapter, mutate: (mutation: SeatMutation) => void, mutateAsync: (mutation: SeatMutation) => Promise<void>) {
  useMultiplayerStore.setState({
    hostGameCode: "QUEUE1", hostingStatus: "waiting", serverInfo: null,
    hostSession: { formatConfig: FORMAT_DEFAULTS.Commander, timerSeconds: null, matchType: "Bo1" },
    playerSlots: host.getPlayerSlots(), seatMutate: mutate, seatMutateAsync: mutateAsync,
  });
  render(React.createElement(MemoryRouter, { initialEntries: ["/multiplayer"] }, React.createElement(HostControlTile)));
}

beforeAll(async () => {
  const bytes = await readFile(resolve(__dirname, "../../wasm/engine_wasm_bg.wasm"));
  await initEngine({ module_or_path: await WebAssembly.compile(bytes) });
}, 300_000);
beforeEach(() => {
  lifecycle.initialize.mockReset().mockResolvedValue(undefined);
  lifecycle.inputs.length = 0;
});
afterEach(() => {
  cleanup();
  for (const host of hosts.splice(0)) host.dispose();
  vi.restoreAllMocks();
  useMultiplayerStore.setState({ hostGameCode: null, hostingStatus: "idle", hostSession: null, playerSlots: [] });
});

describe("conditional waiting-seat assignments through the real WASM reducer (local integration)", () => {
  it("queues assignment behind admission, publishes the no-op, and still permits explicit replacement", async () => {
    const ready = deferred();
    lifecycle.initialize.mockImplementationOnce(() => ready.promise);
    const host = makeHost();
    const initializing = host.initialize();
    const guest = await join();
    expect(privateState(host).pregameSeatState.seats[1].type).toBe("WaitingHuman");
    const assignment = host.applySeatMutation(fill(1));
    ready.resolve();
    await Promise.all([initializing, assignment]);
    await settled(host);
    expect(JSON.parse(lifecycle.inputs[0]).seats[1].type).toBe("JoinedHuman");
    retainGuest(host, guest);
    await publishedMutation(guest);
    const token = privateState(host).playerTokens.get(1);
    const other = await join("Other");
    await settled(host);
    const otherToken = privateState(host).playerTokens.get(2);
    await host.applySeatMutation(fill(3));
    expect(host.getPlayerSlots()[3].kind.type).toBe("Ai");
    expect(privateState(host).playerTokens.get(1)).toBe(token);
    await host.applySeatMutation({ type: "SetKind", data: { seatIndex: 1, kind: ai } });
    expect(host.getPlayerSlots()[1].kind.type).toBe("Ai");
    expect(privateState(host).playerTokens.has(1)).toBe(false);
    await vi.waitFor(() => expect(guest.open).toBe(false));
    expect(privateState(host).playerTokens.get(2)).toBe(otherToken);
    expect(other.open).toBe(true);
  });

  it("assignment before admission leaves its AI in place and admits the guest to another open seat", async () => {
    const host = makeHost();
    await host.initialize();
    await host.applySeatMutation(fill(1));
    const guest = await join();
    await settled(host);
    expect(host.getPlayerSlots()[1].kind).toMatchObject(ai);
    retainGuest(host, guest, 2);
  });

  it("Add AI sends its captured waiting intent through queued admission", async () => {
    const ready = deferred();
    lifecycle.initialize.mockImplementationOnce(() => ready.promise);
    const host = makeHost();
    const initializing = host.initialize();
    const guest = await join();
    let submitted: Promise<void> | undefined;
    renderControls(host, (mutation) => { submitted = host.applySeatMutation(mutation); }, (mutation) => host.applySeatMutation(mutation));
    fireEvent.click(screen.getAllByRole("button", { name: "Add AI" })[0]);
    expect(submitted).toBeDefined();
    ready.resolve();
    await Promise.all([initializing, submitted]);
    await settled(host);
    retainGuest(host, guest);
    await publishedMutation(guest);
  });

  it("Fill With AI retains a later captured seat claimed during its first await, then dispatches Start", async () => {
    const host = makeHost();
    await host.initialize();
    const paused = deferred();
    const firstApplied = deferred();
    let startState: SeatState | undefined;
    const submitted: Promise<void>[] = [];
    renderControls(host, (mutation) => { submitted.push(host.applySeatMutation(mutation)); }, async (mutation) => {
      if (mutation.type === "Start") {
        // This fixture stops at the existing Start dispatcher boundary; start
        // the real reducer without loading a game/card DB or mocking its rules.
        const result = apply_seat_mutation(JSON.stringify(privateState(host).pregameSeatState), JSON.stringify(mutation)) as SeatMutationResult;
        startState = result.state;
        return;
      }
      await host.applySeatMutation(mutation);
      if (mutation.data.seatIndex === 1) { firstApplied.resolve(); await paused.promise; }
    });
    fireEvent.click(screen.getByRole("button", { name: "Fill With AI" }));
    await firstApplied.promise;
    const guest = await join();
    await settled(host);
    paused.resolve();
    await vi.waitFor(() => expect(startState?.gameStarted).toBe(true));
    await Promise.all(submitted);
    retainGuest(host, guest, 2);
    expect(startState?.seats.map((seat) => seat.type)).toEqual(["HostHuman", "Ai", "JoinedHuman", "Ai"]);
  });

  it("ordinary setup succeeds, skips the AI at an early guest's seat, and assigns the remaining AI", async () => {
    let guest: FakeDataConnection | undefined;
    const initialize = P2PHostAdapter.prototype.initialize;
    vi.spyOn(P2PHostAdapter.prototype, "initialize").mockImplementation(async function (this: P2PHostAdapter) {
      hosts.push(this);
      await initialize.call(this);
      guest = await join();
      await settled(this);
    });
    const settings: HostingSettings = {
      displayName: "Host", public: false, password: "", timerSeconds: null,
      formatConfig: FORMAT_DEFAULTS.Commander, matchType: "Bo1", loopDetection: { type: "Off" },
      aiSeats: [1, 3].map((seatIndex) => ({ seatIndex, difficulty: "Medium", deckName: null, deck: { type: "DeckList", data: deck } })),
      startWhenFull: false, ranked: false, roomName: "Queue regression",
    };
    const ok = await useMultiplayerStore.getState().startP2PHostingSession(settings, deck, { brokerUrl: null });
    expect(ok).toBe(true);
    const active = hosts[hosts.length - 1];
    expect(active).toBeDefined();
    expect(guest).toBeDefined();
    retainGuest(active!, guest!);
    expect(active!.getPlayerSlots()[3].kind).toMatchObject(ai);
    expect(active!.getPlayerSlots()[2].kind.type).toBe("WaitingHuman");
    await publishedMutation(guest!);
  });
});
