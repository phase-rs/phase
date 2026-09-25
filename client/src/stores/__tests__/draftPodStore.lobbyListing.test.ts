/**
 * Creating a listed pod through the real chain: `draftPodStore.createPod`
 * resolves a broker and hands the open client to the real
 * `DraftPodHostAdapter` / `P2PDraftHost`, which registers, updates and
 * withdraws the listing.
 *
 * Real: `draftPodStore`, `multiplayerDraftStore`, `multiplayerStore` (its
 * `ensureSubscriptionSocket` stubbed below), `DraftPodHostAdapter`,
 * `P2PDraftHost`, `brokerClient`.
 * Mocked: `adapter/draft-adapter`'s `DraftAdapter` class, `network/connection`'s
 * `hostRoom`, `network/draftPeerSession`'s `createDraftPeerSession`,
 * `services/draftPersistence`'s IndexedDB session functions,
 * `services/openPhaseSocket`'s `openPhaseSocket`, and global `fetch`.
 */
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

const socketState = vi.hoisted(() => {
  class FakeLobbySocket extends EventTarget {
    readyState = 1;
    onopen: unknown = null;
    onmessage: unknown = null;
    onerror: unknown = null;
    onclose: unknown = null;
    sent: string[] = [];
    send = vi.fn((data: string) => {
      this.sent.push(data);
    });
    close = vi.fn(() => {
      this.readyState = 3;
      this.dispatchEvent(new Event("close"));
    });
    /** Deliver a wire frame to whatever `registerHost` listener is attached. */
    deliver(frame: unknown): void {
      this.dispatchEvent(new MessageEvent("message", { data: JSON.stringify(frame) }));
    }
  }
  return {
    FakeLobbySocket,
    sockets: [] as InstanceType<typeof FakeLobbySocket>[],
    urls: [] as string[],
  };
});

/** Captures `P2PDraftHost`'s subscription so a test can simulate a guest connecting. */
const hostRoomState = vi.hoisted(() => ({
  onGuestConnected: null as ((conn: unknown) => void) | null,
}));

/** Captures the fake guest session's first-contact handler. Single-slot: every
 * row here drives at most one guest connection. */
const sessionState = vi.hoisted(() => ({
  onMessage: null as ((message: unknown) => void | Promise<void>) | null,
  send: vi.fn(async () => {}),
  close: vi.fn(),
}));

const mocks = vi.hoisted(() => ({
  openPhaseSocket: vi.fn(),
}));

vi.mock("../../adapter/draft-adapter", async (importOriginal) => {
  const original = await importOriginal<typeof import("../../adapter/draft-adapter")>();
  return {
    ...original,
    // `function`, not an arrow: production calls `new DraftAdapter()`.
    DraftAdapter: vi.fn().mockImplementation(function () {
      return {
        loadCardDatabase: vi.fn(async () => 0),
        draftProcedure: vi.fn(async () => draftProcedureFixture({
          pod_size: 6,
          human_seats: 1,
          min_pod_size: 2,
          max_pod_size: 8,
          allowed_pod_sizes: [2, 3, 4, 5, 6, 7, 8],
          packs_per_player: 3,
          cards_per_pick: 1,
          distribution: "PickAndPass",
          min_deck_size: 40,
          post_draft_play: "TournamentPairings",
        })),
        createMultiplayerDraft: vi.fn(async () => {}),
        getViewForSeat: vi.fn(async (seat: number) => ({
          status: "Deckbuilding",
          kind: "Premier",
          launch_capability: "None",
          commanders_required: 0,
          seat_index: seat,
          current_round: 1,
          pairings: [],
          pool_groups: {
            color_groups: [], type_groups: [], cmc_groups: [], rarity_groups: [],
            type_filter_options: [], color_filter_options: [],
            color_counts: { white: 0, blue: 0, black: 0, red: 0, green: 0 },
            workspace_capabilities: { rarity_group_order: null },
            workspace_row_classification: { creature_instance_ids: [], noncreature_instance_ids: [] },
          },
          pool: [],
          standings: [],
          next_pairing_round: 1,
          timer_remaining_ms: null,
          seats: [],
          match_config: { match_type: "Bo1" },
        })),
        exportSession: vi.fn(async () => null),
      };
    }),
  };
});

vi.mock("../../network/connection", async (importOriginal) => ({
  ...(await importOriginal<typeof import("../../network/connection")>()),
  hostRoom: vi.fn(async () => ({
    roomCode: "PDABC",
    peerId: "phase2-PDABC",
    peer: { destroy: vi.fn(), on: vi.fn() },
    onGuestConnected: (handler: (conn: unknown) => void) => {
      hostRoomState.onGuestConnected = handler;
      return () => {
        hostRoomState.onGuestConnected = null;
      };
    },
    destroy: vi.fn(),
  })),
}));

vi.mock("../../network/draftPeerSession", () => ({
  createDraftPeerSession: vi.fn(() => ({
    onMessage: vi.fn((handler: (message: unknown) => void | Promise<void>) => {
      sessionState.onMessage = handler;
      return vi.fn();
    }),
    onDisconnect: vi.fn(() => vi.fn()),
    send: sessionState.send,
    close: sessionState.close,
  })),
}));

vi.mock("../../services/draftPersistence", async (importOriginal) => ({
  ...(await importOriginal<typeof import("../../services/draftPersistence")>()),
  loadDraftHostSession: vi.fn(async () => null),
  saveDraftHostSession: vi.fn(async () => {}),
  clearDraftHostSession: vi.fn(async () => {}),
}));

vi.mock("../../services/openPhaseSocket", async (importOriginal) => ({
  ...(await importOriginal<typeof import("../../services/openPhaseSocket")>()),
  openPhaseSocket: mocks.openPhaseSocket,
}));

import { draftProcedureFixture } from "../../adapter/__tests__/draftProcedureFixture";
import { DraftPodHostAdapter } from "../../adapter/draftPodHostAdapter";
import { DRAFT_PROTOCOL_VERSION } from "../../network/draftProtocol";
import { PROTOCOL_VERSION, LOBBY_PROTOCOL_VERSION } from "../../adapter/ws-adapter";
import { OFFICIAL_MULTIPLAYER_SERVER_URL } from "../../config/multiplayerServer";
import { useDraftPodStore } from "../draftPodStore";
import { DRAFT_OFFLINE_ERROR, useMultiplayerDraftStore } from "../multiplayerDraftStore";
import { useMultiplayerStore } from "../multiplayerStore";
import { useConnectivityStore } from "../connectivityStore";

const ANCHOR = "wss://anchor.example/ws";

/** Parsed wire frames a fake socket's `send` calls carried, in order, minus
 * the client's own keepalive `Ping` frames — so a row's frame list does not
 * depend on how long it runs. */
function frames(socket: { sent: string[] }): { type: string; data?: unknown }[] {
  return socket.sent
    .map((raw) => JSON.parse(raw) as { type: string; data?: unknown })
    .filter((frame) => frame.type !== "Ping");
}

function configureListedPod(overrides: { podSize?: number } = {}): void {
  useDraftPodStore.setState((prev) => ({
    config: {
      ...prev.config,
      packs: [
        { code: "TST", name: "Test Set" },
        { code: "TST", name: "Test Set" },
        { code: "XYZ", name: "XYZ Set" },
      ],
      setCode: "TST+XYZ",
      podSize: overrides.podSize ?? 6,
    },
    hostDisplayName: "Host",
  }));
  useDraftPodStore.getState().setListing({ isPublic: true });
}

const realHostDraft = useMultiplayerDraftStore.getState().hostDraft;

/** A `LobbyOnly` probe answer for `ensureSubscriptionSocket`. Declared with a
 * bare `vi.fn()` (mirroring `MultiplayerPage.hostServer.test.tsx`) rather than
 * typed against the store's `PhaseSocket`-returning signature, since this
 * probe result is read only for its `serverInfo.mode`. */
const ensureSubscriptionSocketMock = vi.fn();

describe("createPod lobby listing — production entry", () => {
  let initializeSpy: ReturnType<typeof vi.spyOn>;

  beforeEach(() => {
    socketState.sockets = [];
    socketState.urls = [];
    hostRoomState.onGuestConnected = null;
    sessionState.onMessage = null;
    sessionState.send.mockClear();
    sessionState.close.mockClear();

    mocks.openPhaseSocket.mockReset().mockImplementation(async (url: string) => {
      socketState.urls.push(url);
      const ws = new socketState.FakeLobbySocket();
      socketState.sockets.push(ws);
      return {
        ws,
        serverInfo: {
          version: "test",
          buildCommit: "test",
          mode: "LobbyOnly",
          protocolVersion: PROTOCOL_VERSION,
          lobbyProtocolVersion: LOBBY_PROTOCOL_VERSION,
        },
        close: () => ws.close(),
      };
    });

    vi.stubGlobal("__DRAFT_POOLS_URL__", "/draft-pools.json");
    vi.stubGlobal("__CARD_DATA_URL__", "/card-data.json");
    vi.stubGlobal("fetch", vi.fn(async (input: unknown) => {
      const url = String(input);
      if (url.includes("draft-pools")) {
        return { ok: true, status: 200, json: async () => ({ tst: { code: "TST" }, xyz: { code: "XYZ" } }) };
      }
      return { ok: true, status: 200, text: async () => "{}", json: async () => ({}) };
    }));

    useConnectivityStore.setState({ forcedOffline: false, browserOnline: true });
    useDraftPodStore.getState().reset();
    useMultiplayerDraftStore.setState({ hostDraft: realHostDraft });
    ensureSubscriptionSocketMock.mockReset().mockImplementation(async () => ({
      serverInfo: {
        version: "test",
        buildCommit: "test",
        mode: "LobbyOnly",
        protocolVersion: PROTOCOL_VERSION,
        lobbyProtocolVersion: LOBBY_PROTOCOL_VERSION,
      },
    }));
    useMultiplayerStore.setState({
      hostingServer: ANCHOR,
      sourceStatus: new Map(),
      ensureSubscriptionSocket: ensureSubscriptionSocketMock,
    });

    initializeSpy = vi.spyOn(DraftPodHostAdapter.prototype, "initialize");
  });

  afterEach(async () => {
    await useMultiplayerDraftStore.getState().leave();
    useMultiplayerDraftStore.setState({ hostDraft: realHostDraft });
    initializeSpy.mockRestore();
    vi.unstubAllGlobals();
  });

  it("lists a pod, tracks its seats and withdraws it when the host leaves", async () => {
    configureListedPod();

    // Not awaited yet: `registerHost`'s promise settles only once the fake
    // socket delivers a reply below, and `createPod` awaits it.
    const creating = useDraftPodStore.getState().createPod();
    await vi.waitFor(() => expect(socketState.sockets).toHaveLength(1));
    const socket = socketState.sockets[0]!;
    await vi.waitFor(() => expect(frames(socket).some((f) => f.type === "CreateGameWithSettings")).toBe(true));
    socket.deliver({ type: "GameCreated", data: { game_code: "AB12CD", player_token: "tok" } });

    await creating;
    expect(useMultiplayerDraftStore.getState().roomCode).toBe("PDABC");
    expect(mocks.openPhaseSocket).toHaveBeenCalledWith(ANCHOR, expect.anything());

    const createFrame = frames(socket).find((f) => f.type === "CreateGameWithSettings");
    expect(createFrame?.data).toMatchObject({
      host_peer_id: "phase2-PDABC",
      public: true,
      player_count: 6,
      room_name: "Host's table",
      password: null,
      display_name: "Host",
      draft_metadata: { setCode: "TST+XYZ", draftKind: "Premier" },
    });

    await vi.waitFor(() => {
      const updates = frames(socket).filter((f) => f.type === "UpdateLobbyMetadata");
      expect(updates).toHaveLength(1);
      expect((updates[0]!.data as { current_players: number; max_players: number })).toMatchObject({
        current_players: 1,
        max_players: 6,
      });
    });

    expect(hostRoomState.onGuestConnected).toBeTypeOf("function");
    hostRoomState.onGuestConnected!({} as never);
    await vi.waitFor(() => expect(sessionState.onMessage).toBeTypeOf("function"));
    void sessionState.onMessage!({
      type: "draft_join",
      displayName: "Alice",
      draftProtocolVersion: DRAFT_PROTOCOL_VERSION,
    });

    await vi.waitFor(() => {
      const updates = frames(socket).filter((f) => f.type === "UpdateLobbyMetadata");
      expect(updates).toHaveLength(2);
      expect((updates[1]!.data as { current_players: number; max_players: number })).toMatchObject({
        current_players: 2,
        max_players: 6,
      });
    });

    await useMultiplayerDraftStore.getState().leave();

    const finalFrames = frames(socket);
    expect(finalFrames[finalFrames.length - 1]?.type).toBe("UnregisterLobby");
    expect(socket.close).toHaveBeenCalledOnce();
  });

  it("withdraws the listing when the draft starts", async () => {
    configureListedPod();

    const creating = useDraftPodStore.getState().createPod();
    await vi.waitFor(() => expect(socketState.sockets).toHaveLength(1));
    const socket = socketState.sockets[0]!;
    await vi.waitFor(() => expect(frames(socket).some((f) => f.type === "CreateGameWithSettings")).toBe(true));
    socket.deliver({ type: "GameCreated", data: { game_code: "AB12CD", player_token: "tok" } });
    await creating;
    expect(useMultiplayerDraftStore.getState().roomCode).toBe("PDABC");

    await useMultiplayerDraftStore.getState().startDraft(true);

    await vi.waitFor(() => {
      expect(frames(socket).map((f) => f.type)).toEqual([
        "CreateGameWithSettings",
        "UpdateLobbyMetadata",
        "UnregisterLobby",
      ]);
    });
    expect(socket.close).toHaveBeenCalledOnce();

    const framesBeforeLeave = frames(socket).length;
    await useMultiplayerDraftStore.getState().leave();
    expect(frames(socket)).toHaveLength(framesBeforeLeave);
  });

  it("lists on the official lobby when the anchor is a dedicated server", async () => {
    useMultiplayerStore.setState({
      sourceStatus: new Map([[ANCHOR, {
        state: "open",
        playerCount: 0,
        serverInfo: {
          version: "test", buildCommit: "test", mode: "Full",
          protocolVersion: PROTOCOL_VERSION, lobbyProtocolVersion: LOBBY_PROTOCOL_VERSION,
        },
      }]]),
    });
    configureListedPod();

    const creating = useDraftPodStore.getState().createPod();

    await vi.waitFor(() => expect(mocks.openPhaseSocket).toHaveBeenCalled());
    expect(mocks.openPhaseSocket).toHaveBeenCalledWith(OFFICIAL_MULTIPLAYER_SERVER_URL, expect.anything());
    expect(mocks.openPhaseSocket).not.toHaveBeenCalledWith(ANCHOR, expect.anything());

    const socket = socketState.sockets[0]!;
    await vi.waitFor(() => expect(frames(socket).some((f) => f.type === "CreateGameWithSettings")).toBe(true));
    socket.deliver({ type: "GameCreated", data: { game_code: "AB12CD", player_token: "tok" } });
    await creating;
  });

  it("fails pod setup with the lobby's reason when it refuses the listing", async () => {
    configureListedPod();

    const creating = useDraftPodStore.getState().createPod();
    await vi.waitFor(() => expect(socketState.sockets).toHaveLength(1));
    const socket = socketState.sockets[0]!;
    await vi.waitFor(() => expect(frames(socket).some((f) => f.type === "CreateGameWithSettings")).toBe(true));

    socket.deliver({ type: "Error", data: { message: "Room name is not allowed on the public lobby." } });
    await creating;

    expect(useDraftPodStore.getState().configError).toBe("Room name is not allowed on the public lobby.");
    expect(useMultiplayerDraftStore.getState().phase).toBe("error");
    expect(socket.close).toHaveBeenCalledOnce();
  });

  it("closes the lobby connection when a newer pod replaces this one before it starts", async () => {
    let firstResolved: boolean | undefined;
    useMultiplayerDraftStore.setState({
      hostDraft: (config) => {
        const first = realHostDraft(config);
        void first.then((result) => {
          firstResolved = result;
        });
        void realHostDraft({
          ...config,
          persistenceId: "superseding-pod",
          listing: undefined,
        });
        return first;
      },
    });
    configureListedPod();

    await useDraftPodStore.getState().createPod();
    await vi.waitFor(() => expect(socketState.sockets.length).toBeGreaterThanOrEqual(1));

    await vi.waitFor(() => expect(initializeSpy).toHaveBeenCalledTimes(1));
    expect(firstResolved).toBe(false);

    const firstSocket = socketState.sockets[0]!;
    expect(frames(firstSocket).some((f) => f.type === "CreateGameWithSettings")).toBe(false);
    expect(firstSocket.close).toHaveBeenCalledOnce();
  });

  it("closes the lobby connection when the app goes offline before the pod starts", async () => {
    useMultiplayerDraftStore.setState({
      hostDraft: (config) => {
        const result = realHostDraft(config);
        useConnectivityStore.setState({ forcedOffline: true });
        return result;
      },
    });
    configureListedPod();

    await useDraftPodStore.getState().createPod();

    await vi.waitFor(() => expect(socketState.sockets).toHaveLength(1));
    const socket = socketState.sockets[0]!;
    await vi.waitFor(() => expect(socket.close).toHaveBeenCalledOnce());
    expect(initializeSpy).not.toHaveBeenCalled();
    expect(useDraftPodStore.getState().configError).toBe(DRAFT_OFFLINE_ERROR);
  });
});
