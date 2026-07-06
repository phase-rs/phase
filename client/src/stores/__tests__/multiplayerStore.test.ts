import { waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";

import type { PlayerSlot } from "../../multiplayer/seatTypes";
import { formatMetadata } from "../../data/formatRegistry";
import {
  FORMAT_DEFAULTS,
  migrateOfficialServerAddress,
  type HostingSettings,
  useMultiplayerStore,
} from "../multiplayerStore";

const p2pMocks = vi.hoisted(() => ({
  hostDestroy: vi.fn(),
  initialize: vi.fn(async () => undefined),
  applySeatMutation: vi.fn(async () => undefined),
  startNow: vi.fn(),
  startPregameGame: vi.fn(async () => undefined),
  getPlayerSlots: vi.fn(() => []),
  dispose: vi.fn(),
}));

const socketMocks = vi.hoisted(() => ({
  send: vi.fn(),
}));

vi.mock("../../network/connection", () => ({
  hostRoom: vi.fn(async () => ({
    peer: { id: "peer-id", destroy: p2pMocks.hostDestroy },
    destroy: p2pMocks.hostDestroy,
    roomCode: "ABCDE",
    onGuestConnected: vi.fn(),
  })),
}));

vi.mock("../../adapter/p2p-adapter", () => ({
  P2PHostAdapter: vi.fn().mockImplementation(function () {
    return {
      onEvent: vi.fn(),
      initialize: p2pMocks.initialize,
      applySeatMutation: p2pMocks.applySeatMutation,
      startNow: p2pMocks.startNow,
      startPregameGame: p2pMocks.startPregameGame,
      getPlayerSlots: p2pMocks.getPlayerSlots,
      dispose: p2pMocks.dispose,
    };
  }),
}));

vi.mock("../../services/openPhaseSocket", () => ({
  HandshakeError: class HandshakeError extends Error {
    kind: string;

    constructor(message: string, kind: string) {
      super(message);
      this.kind = kind;
    }
  },
  openPhaseSocket: vi.fn(async () => ({
    serverInfo: { mode: "Full", protocolVersion: 1 },
    ws: {
      send: socketMocks.send,
      close: vi.fn(),
      onmessage: null,
      onerror: null,
      onclose: null,
    },
  })),
  withReconnect: vi.fn(),
}));

function hostingSettings(
  overrides: Partial<HostingSettings> = {},
): HostingSettings {
  return {
    displayName: "Host",
    public: true,
    password: "",
    timerSeconds: null,
    formatConfig: FORMAT_DEFAULTS.Commander,
    matchType: "Bo1",
    loopDetection: { type: "Off" },
    aiSeats: [],
    startWhenFull: false,
    ranked: false,
    roomName: "Test room",
    ...overrides,
  };
}

describe("multiplayerStore", () => {
  beforeEach(() => {
    useMultiplayerStore.getState().cancelHosting();
    vi.clearAllMocks();
    useMultiplayerStore.setState({
      displayName: "",
      connectionStatus: "disconnected",
      activePlayerId: null,
      opponentDisplayName: null,
      serverAddress: "ws://localhost:8787",
    });
  });

  it("initializes with a stable UUID playerId", () => {
    const id1 = useMultiplayerStore.getState().playerId;
    expect(id1).toMatch(/^[0-9a-f]{8}-/);
    const id2 = useMultiplayerStore.getState().playerId;
    expect(id2).toBe(id1);
  });

  it("persists displayName across store resets", () => {
    useMultiplayerStore.getState().setDisplayName("TestPlayer");
    expect(useMultiplayerStore.getState().displayName).toBe("TestPlayer");
  });

  it("does not persist connectionStatus or activePlayerId", () => {
    useMultiplayerStore.getState().setConnectionStatus("connected");
    expect(useMultiplayerStore.getState().connectionStatus).toBe("connected");
    useMultiplayerStore.getState().setActivePlayerId(1);
    expect(useMultiplayerStore.getState().activePlayerId).toBe(1);
  });

  it("setActivePlayerId updates activePlayerId", () => {
    useMultiplayerStore.getState().setActivePlayerId(1);
    expect(useMultiplayerStore.getState().activePlayerId).toBe(1);
    useMultiplayerStore.getState().setActivePlayerId(null);
    expect(useMultiplayerStore.getState().activePlayerId).toBeNull();
  });

  it("derives Two-Headed Giant defaults from the registry metadata", () => {
    expect(FORMAT_DEFAULTS.TwoHeadedGiant).toBe(
      formatMetadata("TwoHeadedGiant")?.default_config,
    );
    for (const metadata of Object.values(FORMAT_DEFAULTS)) {
      expect(FORMAT_DEFAULTS[metadata.format]).toBe(
        formatMetadata(metadata.format)?.default_config,
      );
    }
  });

  it("migrates official persisted server addresses to the configured deployment default", () => {
    expect(
      migrateOfficialServerAddress(
        "wss://lobby.phase-rs.dev/ws",
        "wss://selfhost.example/ws",
      ),
    ).toBe("wss://selfhost.example/ws");
    expect(
      migrateOfficialServerAddress(
        "wss://us.phase-rs.dev/ws",
        "wss://selfhost.example/ws",
      ),
    ).toBe("wss://selfhost.example/ws");
  });

  it("does not migrate custom self-hosted server addresses", () => {
    expect(
      migrateOfficialServerAddress(
        "wss://play.example.com/ws",
        "wss://selfhost.example/ws",
      ),
    ).toBe("wss://play.example.com/ws");
  });

  it("strips AI seats from team-based server host settings", async () => {
    useMultiplayerStore.getState().startHosting(
      hostingSettings({
        formatConfig: FORMAT_DEFAULTS.TwoHeadedGiant,
        aiSeats: [{ seatIndex: 1, difficulty: "Hard", deckName: null }],
      }),
      {
        main_deck: ["Forest"],
        sideboard: [],
        commander: [],
      },
    );

    await waitFor(() => expect(socketMocks.send).toHaveBeenCalled());
    const frame = JSON.parse(socketMocks.send.mock.calls[0][0] as string) as {
      data: { ai_seats: unknown[] };
    };
    expect(frame.data.ai_seats).toEqual([]);
  });

  it("passes AI seats through for non-team server host settings", async () => {
    const aiSeats = [{ seatIndex: 1, difficulty: "Hard", deckName: null }];
    useMultiplayerStore.getState().startHosting(
      hostingSettings({ aiSeats }),
      {
        main_deck: ["Forest"],
        sideboard: [],
        commander: ["Goreclaw, Terror of Qal Sisma"],
      },
    );

    await waitFor(() => expect(socketMocks.send).toHaveBeenCalled());
    const frame = JSON.parse(socketMocks.send.mock.calls[0][0] as string) as {
      data: { ai_seats: unknown[] };
    };
    expect(frame.data.ai_seats).toEqual(aiSeats);
  });

  it("applies setup-time AI seats when starting a P2P host session", async () => {
    const ok = await useMultiplayerStore.getState().startP2PHostingSession(
      hostingSettings({
        aiSeats: [
          { seatIndex: 1, difficulty: "Hard", deckName: null },
          { seatIndex: 3, difficulty: "Easy", deckName: "My Deck" },
        ],
      }),
      {
        main_deck: ["Forest"],
        sideboard: [],
        commander: ["Goreclaw, Terror of Qal Sisma"],
      },
      { useBroker: false },
    );

    expect(ok).toBe(true);
    expect(p2pMocks.applySeatMutation).toHaveBeenNthCalledWith(1, {
      type: "SetKind",
      data: {
        seatIndex: 1,
        kind: {
          type: "Ai",
          data: { difficulty: "Hard", deck: { type: "Random" } },
        },
      },
    });
    expect(p2pMocks.applySeatMutation).toHaveBeenNthCalledWith(2, {
      type: "SetKind",
      data: {
        seatIndex: 3,
        kind: {
          type: "Ai",
          data: { difficulty: "Easy", deck: { type: "Named", data: "My Deck" } },
        },
      },
    });
  });

  it("does not apply setup-time AI seats when starting a team-based P2P host session", async () => {
    const ok = await useMultiplayerStore.getState().startP2PHostingSession(
      hostingSettings({
        formatConfig: FORMAT_DEFAULTS.TwoHeadedGiant,
        aiSeats: [{ seatIndex: 1, difficulty: "Hard", deckName: null }],
      }),
      {
        main_deck: ["Forest"],
        sideboard: [],
        commander: [],
      },
      { useBroker: false },
    );

    expect(ok).toBe(true);
    expect(p2pMocks.applySeatMutation).not.toHaveBeenCalled();
  });

  it("removes open P2P seats in order before starting with current players", async () => {
    const ok = await useMultiplayerStore.getState().startP2PHostingSession(
      hostingSettings(),
      {
        main_deck: ["Forest"],
        sideboard: [],
        commander: ["Goreclaw, Terror of Qal Sisma"],
      },
      { useBroker: false },
    );
    expect(ok).toBe(true);

    const slots: PlayerSlot[] = [
      { playerId: 0, name: "Host", kind: { type: "HostHuman" } },
      { playerId: 1, name: "", kind: { type: "WaitingHuman" } },
      { playerId: 2, name: "Guest", kind: { type: "JoinedHuman" } },
      { playerId: 3, name: "", kind: { type: "WaitingHuman" } },
    ];
    useMultiplayerStore.setState({ playerSlots: slots });

    await useMultiplayerStore.getState().startLobbyWithCurrentPlayers();

    expect(p2pMocks.applySeatMutation).toHaveBeenNthCalledWith(1, {
      type: "Remove",
      data: { seatIndex: 3 },
    });
    expect(p2pMocks.applySeatMutation).toHaveBeenNthCalledWith(2, {
      type: "Remove",
      data: { seatIndex: 1 },
    });
    expect(p2pMocks.startNow).toHaveBeenCalledOnce();
    expect(p2pMocks.startPregameGame).toHaveBeenCalledOnce();
  });

  it("reports a server host connection error instead of falling through to P2P", async () => {
    useMultiplayerStore.setState({
      hostingStatus: "waiting",
      hostGameCode: "ABCDE",
    });

    await expect(
      useMultiplayerStore.getState().seatMutateAsync({ type: "Start" }),
    ).rejects.toThrow("Host connection is not active.");
  });
});
