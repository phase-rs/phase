import "fake-indexeddb/auto";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { DraftPlayerView } from "../../adapter/draft-adapter";
import type { CommanderSeatDecks, DraftCommanderLaunch, DraftP2PMessage } from "../../network/draftProtocol";
import type { SeatKind, SeatMutation } from "../../multiplayer/seatTypes";

const probe = vi.hoisted(() => ({
  host: null as unknown,
  view: null as unknown,
  initializeGate: null as Promise<void> | null,
  initializeEntered: vi.fn(),
  installGate: null as Promise<void> | null,
  installEntered: vi.fn(),
  kinds: new Map<number, SeatKind>(),
  join: null as ((seat: number) => void) | null,
  start: vi.fn(async () => ({ log_entries: [] })),
  terminate: vi.fn(async () => {}),
  controller: { start: vi.fn(), stop: vi.fn(), dispose: vi.fn() },
}));

vi.mock("../../network/connection", async (importOriginal) => ({
  ...(await importOriginal<typeof import("../../network/connection")>()),
  hostRoom: vi.fn(async (_signal?: AbortSignal, options?: { preferredRoomCode?: string }) => ({
    roomCode: options?.preferredRoomCode ?? "ROSTR",
    peerId: "host-peer",
    peer: { id: "host-peer", destroy: vi.fn() },
    onGuestConnected: vi.fn(() => () => {}),
    destroy: vi.fn(),
  })),
}));

vi.mock("../../adapter/p2p-draft-host", async (importOriginal) => {
  const original = await importOriginal<typeof import("../../adapter/p2p-draft-host")>();
  return {
    ...original,
    P2PDraftHost: class extends original.P2PDraftHost {
      constructor(...args: ConstructorParameters<typeof original.P2PDraftHost>) {
        super(...args);
        probe.host = this;
      }
    },
  };
});

vi.mock("../../adapter/draft-adapter", async (importOriginal) => ({
  ...(await importOriginal<typeof import("../../adapter/draft-adapter")>()),
  DraftAdapter: class {
    async loadCardDatabase() {}
    async draftProcedure() {
      return {
        pod_size: 4, human_seats: 3, min_pod_size: 4, max_pod_size: 8,
        allowed_pod_sizes: [4, 6, 8], packs_per_player: 3, cards_per_pick: 2,
        pick_selection_mode: "Direct", distribution: "PickAndPass",
        allowed_set_layouts: ["Uniform"], min_deck_size: 60, cube_min_deck_size: 60,
        commanders_required: 1, post_draft_play: "None",
        launch_capability: "CommanderMultiplayer", match_config: { match_type: "Bo1" },
      };
    }
    async exportSession() {
      return JSON.stringify({
        pools: Array.from({ length: 4 }, (_, seat) => [
          { name: `Legend ${seat}` }, { name: `Spare ${seat}` },
        ]),
        submitted_decks: Object.fromEntries(Array.from({ length: 4 }, (_, seat) => [seat, {
          seat, main_deck: [`Legend ${seat}`, `Spell ${seat}`], commanders: [`Legend ${seat}`],
        }])),
      });
    }
    async getBotDeck(seat: number) {
      return { main_deck: [`Bot ${seat}`], lands: {}, commander: [`Bot ${seat}`] };
    }
    async boosterPackPoolForGame() { return ["Host-only booster"]; }
    async setSeatConnected() {}
    async getViewForSeat() { return probe.view; }
    dispose() {}
  },
}));

vi.mock("../../adapter/p2p-adapter", () => ({
  P2PHostAdapter: vi.fn().mockImplementation(function (...args: unknown[]) {
    const waiting = new Set(Array.from({ length: (args[3] as number) - 1 }, (_, i) => i + 1));
    const listeners: Array<(event: unknown) => void> = [];
    const fill = (seat: number) => {
      waiting.delete(seat);
      if (waiting.size === 0) listeners.forEach((listener) => listener({ type: "roomFull" }));
    };
    probe.join = fill;
    for (const seat of waiting) probe.kinds.set(seat, { type: "WaitingHuman" } as SeatKind);
    return {
      onEvent: (listener: (event: unknown) => void) => { listeners.push(listener); return () => {}; },
      initialize: async () => { probe.initializeEntered(); await probe.initializeGate; },
      applySeatMutation: async (mutation: SeatMutation) => {
        if (mutation.type !== "SetKind") throw new Error("Unexpected mutation");
        probe.installEntered(mutation);
        await probe.installGate;
        probe.kinds.set(mutation.data.seatIndex, mutation.data.kind);
        fill(mutation.data.seatIndex);
      },
      startPregameGame: probe.start,
      getSnapshot: async () => ({
        seq: 1, state: { waiting_for: { type: "Priority", data: {} } },
        legalResult: { actions: [], autoPassRecommended: false, endContinuousEffectOffers: [],
          manaPaymentShortcutActions: [], spellCosts: {}, legalActionsByObject: {} },
      }),
      terminateGame: probe.terminate,
      dispose: vi.fn(),
    };
  }),
}));
vi.mock("../../game/controllers/gameLoopController", () => ({
  createGameLoopController: () => probe.controller,
}));

import { P2PDraftHost } from "../../adapter/p2p-draft-host";
import { useMultiplayerDraftStore } from "../multiplayerDraftStore";
import { useConnectivityStore } from "../connectivityStore";

function gate() {
  let release!: () => void;
  const promise = new Promise<void>((resolve) => { release = resolve; });
  return { promise, release };
}
type Session = { send: ReturnType<typeof vi.fn>; close: ReturnType<typeof vi.fn> };
type PrivateHost = {
  draftStarted: boolean;
  seatTokens: Map<number, string>;
  guestSessions: Map<number, Session>;
  disconnectedSeats: Map<number, { timer: ReturnType<typeof setTimeout> }>;
  handleGuestDisconnect: (seat: number) => void;
};
function asPrivate() { return probe.host as PrivateHost; }
function connect(seat: number) {
  const disconnected = asPrivate().disconnectedSeats.get(seat);
  if (disconnected) clearTimeout(disconnected.timer);
  asPrivate().disconnectedSeats.delete(seat);
  const session = { send: vi.fn(async (_message: DraftP2PMessage) => {}), close: vi.fn() };
  asPrivate().guestSessions.set(seat, session);
  asPrivate().seatTokens.set(seat, `token-${seat}`);
  return session;
}
function launches(session: Session): DraftCommanderLaunch[] {
  return session.send.mock.calls.flatMap(([message]) =>
    message.type === "draft_commander_launch" ? [message.launch] : []);
}
function expectedDeck(seat: number) {
  return { main_deck: [`Legend ${seat}`, `Spell ${seat}`], sideboard: [`Spare ${seat}`],
    commander: [`Legend ${seat}`] };
}
async function openPod(options: { allRemainingBots?: boolean; targetDisconnected?: boolean } = {}) {
  await expect(useMultiplayerDraftStore.getState().hostDraft({
    poolInput: { type: "Set", data: { pools: [{ code: "TST" }], sequence: ["TST"] } },
    kind: "CommanderDraft", podSize: 4, hostDisplayName: "Host",
    tournamentFormat: "Swiss", podPolicy: "Competitive",
  } as never)).resolves.toEqual({ status: "opened" });
  const view = {
    status: "Complete", kind: "CommanderDraft", launch_capability: "CommanderMultiplayer",
    match_config: { match_type: "Bo1" }, draft_set_codes: ["CMR"], pool: [],
    seats: Array.from({ length: 4 }, (_, seat_index) => ({
      seat_index, display_name: `Seat ${seat_index}`, has_submitted_deck: true,
      is_bot: seat_index === 3 || (options.allRemainingBots === true && seat_index === 2),
      connected: seat_index !== 1 || !options.targetDisconnected,
    })),
  } as unknown as DraftPlayerView;
  probe.view = view;
  asPrivate().draftStarted = true;
  const target = options.targetDisconnected ? null : connect(1);
  const healthy = options.allRemainingBots ? null : connect(2);
  useMultiplayerDraftStore.setState({ view, phase: "complete", seatIndex: 0, error: null });
  const prepared = vi.spyOn(probe.host as P2PDraftHost, "commanderSeatDecks");
  return { view, target, healthy, prepared };
}
async function finish(launch: Promise<void>, navigate: ReturnType<typeof vi.fn>, healthy: Session | null) {
  if (healthy) {
    await vi.waitFor(() => expect(launches(healthy)).toHaveLength(1));
    expect(launches(healthy)[0].localDeck).toEqual(expectedDeck(2));
    expect(launches(healthy)[0]).not.toHaveProperty("booster_pack_pool");
    probe.join!(2);
  }
  await launch;
  expect(probe.start).toHaveBeenCalledTimes(1);
  expect(probe.controller.start).toHaveBeenCalledTimes(1);
  expect(navigate).toHaveBeenCalledWith(expect.stringMatching(/^\/game\/.*\?mode=draft-match$/));
}

describe("Commander roster finalization through the real store and host", () => {
  const releases: Array<() => void> = [];
  beforeEach(() => {
    vi.clearAllMocks();
    probe.initializeGate = null;
    probe.installGate = null;
    probe.kinds.clear();
    useConnectivityStore.setState({ forcedOffline: false, browserOnline: true });
    vi.stubGlobal("fetch", vi.fn(async () => ({ ok: true, text: async () => "{}" })));
  });
  afterEach(async () => {
    releases.splice(0).forEach((release) => release());
    await useMultiplayerDraftStore.getState().cancelCommanderLaunch();
    await useMultiplayerDraftStore.getState().leave(true);
    vi.unstubAllGlobals();
    vi.restoreAllMocks();
  });
  function hold() { const held = gate(); releases.push(held.release); return held; }

  it.each([false, true])("fills a late dropped human with its prepared deck (remaining bots: %s)", async (allRemainingBots) => {
    const { view, target, healthy, prepared } = await openPod({ allRemainingBots });
    const held = hold();
    probe.initializeGate = held.promise;
    const navigate = vi.fn();
    const launch = useMultiplayerDraftStore.getState().launchCommanderGame(navigate);
    try {
      await vi.waitFor(() => expect(probe.initializeEntered).toHaveBeenCalledOnce());
      const decks: CommanderSeatDecks = await prepared.mock.results[0].value;
      expect(decks.liveSeatDecks.find(({ seat }) => seat === 1)?.deck).toEqual(expectedDeck(1));
      asPrivate().handleGuestDisconnect(1);
      expect(view.seats[1].connected).toBe(true);
      held.release();
      await vi.waitFor(() => expect(probe.kinds.get(3)?.type).toBe("Ai"));
      // Discriminating assertion: base installs the bot but strands seat 1 WaitingHuman.
      await vi.waitFor(() => expect(probe.kinds.get(1)).toEqual({ type: "Ai", data: {
        difficulty: expect.any(String), deck: { type: "DeckList", data: expectedDeck(1) },
      } }));
      expect(launches(target!)).toEqual([]);
      await finish(launch, navigate, healthy);
    } finally {
      held.release();
      await useMultiplayerDraftStore.getState().cancelCommanderLaunch();
      await launch;
    }
  });

  it("removes a human dropped during another seat's AI installation before inviting anyone", async () => {
    const { target, healthy } = await openPod();
    const held = hold();
    probe.installGate = held.promise;
    const navigate = vi.fn();
    const launch = useMultiplayerDraftStore.getState().launchCommanderGame(navigate);
    try {
      await vi.waitFor(() => expect(probe.installEntered).toHaveBeenCalledOnce());
      expect(launches(healthy!)).toEqual([]);
      asPrivate().handleGuestDisconnect(1);
      held.release();
      await vi.waitFor(() => expect(probe.kinds.get(3)?.type).toBe("Ai"));
      await vi.waitFor(() => expect(probe.kinds.get(1)).toMatchObject({ type: "Ai", data: {
        deck: { type: "DeckList", data: expectedDeck(1) },
      } }));
      expect(probe.installEntered.mock.calls.filter(([m]) => m.data.seatIndex === 1)).toHaveLength(1);
      expect(launches(target!)).toEqual([]);
      await finish(launch, navigate, healthy);
    } finally {
      held.release();
      await useMultiplayerDraftStore.getState().cancelCommanderLaunch();
      await launch;
    }
  });

  it("invites a reconnect before final classification despite the stale disconnected view", async () => {
    const { healthy } = await openPod({ targetDisconnected: true });
    const held = hold();
    probe.initializeGate = held.promise;
    const navigate = vi.fn();
    const launch = useMultiplayerDraftStore.getState().launchCommanderGame(navigate);
    await vi.waitFor(() => expect(probe.initializeEntered).toHaveBeenCalledOnce());
    const current = connect(1);
    held.release();
    await vi.waitFor(() => expect(launches(current)).toHaveLength(1));
    expect(launches(current)[0].localDeck).toEqual(expectedDeck(1));
    expect(probe.kinds.get(1)?.type).toBe("WaitingHuman");
    probe.join!(1);
    await finish(launch, navigate, healthy);
  });

  it("keeps an engine commitment when that human reconnects during installation", async () => {
    const { healthy } = await openPod({ targetDisconnected: true });
    const held = hold();
    probe.installGate = held.promise;
    const navigate = vi.fn();
    const launch = useMultiplayerDraftStore.getState().launchCommanderGame(navigate);
    await vi.waitFor(() => expect(probe.installEntered).toHaveBeenCalled());
    const current = connect(1);
    held.release();
    await finish(launch, navigate, healthy);
    expect(probe.kinds.get(1)).toMatchObject({ type: "Ai", data: {
      deck: { type: "DeckList", data: expectedDeck(1) },
    } });
    expect(probe.installEntered.mock.calls.filter(([m]) => m.data.seatIndex === 1)).toHaveLength(1);
    expect(launches(current)).toEqual([]);
  });

  it("cancels an entered installation without a late invitation or runtime", async () => {
    const { target, healthy } = await openPod();
    const held = hold();
    probe.installGate = held.promise;
    const navigate = vi.fn();
    const launch = useMultiplayerDraftStore.getState().launchCommanderGame(navigate);
    await vi.waitFor(() => expect(probe.installEntered).toHaveBeenCalledOnce());
    await useMultiplayerDraftStore.getState().cancelCommanderLaunch();
    held.release();
    await launch;
    expect(probe.terminate).toHaveBeenCalledOnce();
    expect(launches(target!)).toEqual([]);
    expect(launches(healthy!)).toEqual([]);
    expect(useMultiplayerDraftStore.getState().commanderLaunch).toBeNull();
    expect(navigate).not.toHaveBeenCalled();
  });

  it("surfaces a selected send rejection and tears down instead of waiting for a full room", async () => {
    const { target } = await openPod();
    const hostLaunch = vi.fn();
    (probe.host as P2PDraftHost).onEvent((event) => {
      if (event.type === "commanderLaunch") hostLaunch(event.launch);
    });
    target!.send.mockImplementation(async (message) => {
      if (message.type === "draft_commander_launch") throw new Error("launch transport closed");
    });
    const navigate = vi.fn();
    await useMultiplayerDraftStore.getState().launchCommanderGame(navigate);
    expect(launches(target!)).toHaveLength(1);
    expect(probe.terminate).toHaveBeenCalledOnce();
    expect(useMultiplayerDraftStore.getState().error).toContain("launch transport closed");
    expect(hostLaunch).toHaveBeenCalledWith(expect.objectContaining({ localDeck: expectedDeck(0) }));
    expect(useMultiplayerDraftStore.getState().commanderLaunch).toBeNull();
    expect(probe.start).not.toHaveBeenCalled();
    expect(navigate).not.toHaveBeenCalled();
  });
});
