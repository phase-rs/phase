import "fake-indexeddb/auto";
import { act, cleanup, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { MemoryRouter, Route, Routes } from "react-router";
import { MotionGlobalConfig } from "framer-motion";
import { createStore, get } from "idb-keyval";
import { GamePage } from "../GamePage";
import { gameStateFactory } from "../../test/factories/gameStateFactory";
import { P2PHostAdapter } from "../../adapter/p2p-adapter";
import type { EngineSnapshot, GameState, PlayerId } from "../../adapter/types";
import { useGameStore } from "../../stores/gameStore";
import { useMultiplayerStore } from "../../stores/multiplayerStore";
import {
  clearGameStrict,
  loadCheckpoints,
  loadFullTerminalCleanupStrict,
  loadGameStrict,
  loadP2PHostSession,
  saveActiveGame,
  saveCheckpoints,
  saveFullTerminalCleanupStrict,
  saveP2PHostSession,
  saveResumableGameStrict,
  type FullTerminalCleanupObligation,
  type PersistedP2PHostSession,
} from "../../services/gamePersistence";
import {
  commitP2PTerminalResult,
  clearP2PTerminalResult,
  loadP2PTerminalResult,
  type P2PTerminalResult,
} from "../../services/p2pTerminalResult";

// Keep GamePage, GameProvider, both stores and persistence real. Only external
// engine/network boundaries and unrelated expensive rendering are stubbed.
vi.mock("idb-keyval", async (original) => {
  const actual = await original<typeof import("idb-keyval")>();
  return { ...actual, set: vi.fn(actual.set), del: vi.fn(actual.del) };
});
const boundary = vi.hoisted(() => ({
  key: vi.fn<() => { release: { version: string } } | null>(() => null),
  start: vi.fn(async () => ({ port: 9374 })),
  hostRoom: vi.fn(),
}));
vi.mock("../../services/nativeEngine", () => ({
  canAttemptNativeEngine: () => true,
  nativeEngineKeyForCurrentOrigin: boundary.key,
  ensureNativeEngine: boundary.start,
}));
vi.mock("../../network/connection", () => ({ hostRoom: boundary.hostRoom, joinRoom: vi.fn() }));
vi.mock("../../constants/storage", async (original) => ({
  ...await original<typeof import("../../constants/storage")>(),
  loadActiveDeck: () => ({ main: [{ name: "Island", count: 1 }], sideboard: [] }),
  loadSavedDeckBracket: () => null,
}));
vi.mock("../../adapter/wasm-adapter", () => ({
  WasmAdapter: class {},
  getSharedAdapter: vi.fn(),
  getHostAdapter: () => ({ releaseHostSession: vi.fn(async () => {}) }),
}));
vi.mock("../../game/dispatch", () => ({
  dispatchAction: vi.fn(), dispatchResolveAll: vi.fn(), processRemoteUpdate: vi.fn(),
  restoreGameState: vi.fn(), abandonPendingDispatches: vi.fn(), currentSnapshot: new Map(),
}));
vi.mock("../../game/controllers/gameLoopController", () => ({
  createGameLoopController: () => ({ start: vi.fn(), stop: vi.fn(), dispose: vi.fn() }),
}));
vi.mock("../../hooks/useGameplayPreferencesSync", () => ({ useGameplayPreferencesSync: vi.fn() }));
vi.mock("../../audio/useAudioContext", () => ({ useAudioContext: vi.fn() }));
vi.mock("../../audio/AudioManager", () => ({ audioManager: { setContext: vi.fn() } }));
vi.mock("../../components/board/GameBoard", () => ({ GameBoard: () => null }));
vi.mock("../../components/board/BattlefieldBackground", () => ({ BattlefieldBackground: () => null }));
vi.mock("../../components/hud/HUD", () => ({ HUD: () => null }));
vi.mock("../../components/stack/StackDisplay", () => ({ StackDisplay: () => null }));
vi.mock("../../components/hand/PlayerHand", () => ({ PlayerHand: () => null }));
vi.mock("../../components/hand/MobileHandDrawer", () => ({ MobileHandDrawer: () => null }));
vi.mock("../../components/debug/DebugPanel", () => ({ DebugPanel: () => null }));
vi.mock("../../hooks/useCardDataMeta", () => ({ useCardDataMeta: () => null, formatRelativeDate: () => "" }));
vi.mock("../../services/playerAvatars", () => ({ assignRandomAvatars: () => [] }));

let sequence = 0;
let gameId: string;
let session: PersistedP2PHostSession;
let terminal: P2PTerminalResult;
let snapshot: ReturnType<typeof gameStateFactory.build>;

function mountPage() {
  return render(
    <MemoryRouter initialEntries={[`/game/${gameId}?mode=p2p-host`]}>
      <Routes><Route path="/game/:id" element={<GamePage />} /></Routes>
    </MemoryRouter>,
  );
}

beforeEach(async () => {
  MotionGlobalConfig.skipAnimations = true;
  vi.clearAllMocks();
  vi.mocked((await import("idb-keyval")).set).mockImplementation(
    (await vi.importActual<typeof import("idb-keyval")>("idb-keyval")).set,
  );
  vi.mocked((await import("idb-keyval")).del).mockImplementation(
    (await vi.importActual<typeof import("idb-keyval")>("idb-keyval")).del,
  );
  boundary.key.mockReturnValue(null);
  boundary.start.mockResolvedValue({ port: 9374 });
  localStorage.clear();
  useGameStore.getState().reset();
  useMultiplayerStore.setState({ activePlayerId: null });
  gameId = `native-terminal-consumer-${++sequence}`;
  snapshot = gameStateFactory.build();
  session = {
    gameId, roomCode: "ABCDE", sessionKey: `${gameId}-session`, useBroker: false,
    playerTokens: { 0: "host-token", 1: "guest-token" }, guestDecks: {},
    kickedTokens: [], eliminatedSeats: [], playerCount: 2,
    hostDeckData: { player: { main_deck: ["Island"], sideboard: [] } }, gameStarted: true,
    nativeSession: {
      gameCode: "NATIVE", fullKey: { game_code: "NATIVE", generation: 1 },
      playerTokens: { 0: "native-host-token", 1: "native-guest-token" },
    },
  };
  terminal = {
    key: session.sessionKey,
    lease: { sessionKey: session.sessionKey, hostIncarnation: "previous-host" },
    recipient: 0, revision: 7, terminalId: `${gameId}-terminal`,
    finalStateCommitment: "sha256:retained",
    display: { winner: 0, reason: "Retained native result" },
  };
  await saveResumableGameStrict(gameId, snapshot);
  await saveCheckpoints(gameId, [snapshot]);
  await saveP2PHostSession(gameId, session);
  expect(await commitP2PTerminalResult(terminal)).toBe(true);
  saveActiveGame({ id: gameId, mode: "p2p-host", difficulty: "" });
  // Reach controls: the production readers recover each record BEFORE rendering.
  expect(await loadP2PHostSession(gameId)).toEqual(session);
  expect(await loadGameStrict(gameId)).toEqual(snapshot);
  expect(await loadCheckpoints(gameId)).toEqual([snapshot]);
  expect(await loadFullTerminalCleanupStrict(gameId)).toBeNull();
});

afterEach(() => { cleanup(); MotionGlobalConfig.skipAnimations = false; vi.restoreAllMocks(); });

describe("native terminal production consumer before cleanup ownership", () => {
  it("control: retains restart authority without a terminal consumer", async () => {
    expect(await loadP2PTerminalResult(session.sessionKey)).toEqual(terminal);
    expect(await loadP2PHostSession(gameId)).toEqual(session);
    expect(await loadGameStrict(gameId)).toEqual(snapshot);
  });

  it("control: the non-native host still cleans up resume records after terminal display", async () => {
    await clearP2PTerminalResult(session.sessionKey);
    const wasmSession = { ...session, nativeSession: undefined };
    await saveP2PHostSession(gameId, wasmSession);
    const peer = { on: vi.fn(), off: vi.fn(), connect: vi.fn(), destroy: vi.fn() };
    const adapter = new P2PHostAdapter(
      session.hostDeckData, peer as unknown as ConstructorParameters<typeof P2PHostAdapter>[1],
      () => () => {}, 2, undefined, undefined, 5000, undefined, true, undefined,
      { gameId, roomCode: session.roomCode, resumeData: { session: wasmSession } },
    );
    expect(adapter.usesNativeEngine()).toBe(false);
    vi.spyOn(useMultiplayerStore.getState(), "takeActiveP2PHost").mockReturnValueOnce(adapter);
    vi.spyOn(useGameStore.getState(), "resumeP2PHost").mockResolvedValue(undefined);
    const subscription = vi.spyOn(adapter, "onEvent");
    const clears = vi.spyOn(await import("../../services/gamePersistence"), "clearGame");
    mountPage();
    await waitFor(() => expect(subscription).toHaveBeenCalledOnce());
    const host = adapter as unknown as {
      commitTerminalIfComplete: (snapshot: EngineSnapshot, revision: number) => Promise<boolean>;
    };
    await act(async () => {
      await host.commitTerminalIfComplete({
        state: gameStateFactory.build({ waiting_for: { type: "GameOver", data: { winner: 0 } } }),
        legalResult: { actions: [], autoPassRecommended: false }, seq: 1,
      }, terminal.revision);
    });
    expect(screen.getByRole("heading", { name: "VICTORY" })).toBeInTheDocument();
    expect(clears).toHaveBeenCalledWith(gameId);
    await act(async () => { await clears.mock.results[0].value; });
    expect(await loadP2PHostSession(gameId)).toBeNull();
    expect(await loadGameStrict(gameId)).toBeNull();
    expect(await loadCheckpoints(gameId)).toEqual([]);
  });

  it.each(["missing key", "startup rejection"])(
    "retains restart authority after the actual page displays a result with %s",
    async (failure) => {
      if (failure === "startup rejection") {
        boundary.key.mockReturnValue({ release: { version: "test-version" } });
        boundary.start.mockRejectedValueOnce(new Error("native start rejected"));
      }
      const writes = vi.spyOn(await import("../../services/gamePersistence"), "clearGame");
      mountPage();
      await waitFor(() => expect(useGameStore.getState().waitingFor?.type).toBe("GameOver"));
      expect(screen.getByRole("heading", { name: "VICTORY" })).toBeInTheDocument();
      expect(writes).not.toHaveBeenCalled();
      expect(useGameStore.getState().waitingFor?.type).toBe("GameOver");
      expect(boundary.hostRoom).not.toHaveBeenCalled();
      expect(boundary.start).toHaveBeenCalledTimes(failure === "startup rejection" ? 1 : 0);
      expect(await loadFullTerminalCleanupStrict(gameId)).toBeNull();
      expect(await loadP2PTerminalResult(session.sessionKey)).toEqual(terminal);
      const remaining = {
        snapshot: await loadGameStrict(gameId),
        checkpoints: await loadCheckpoints(gameId),
        host: await loadP2PHostSession(gameId),
      };
      console.info("native pre-owner persistence after page clear", failure, remaining);
      // RED on public b3: all three records disappear despite the absent owner.
      expect(remaining).toEqual({ snapshot, checkpoints: [snapshot], host: session });
    },
  );

  it.each(["disconnected without receipt", "terminal send refused", "state ACK without receipt", "owner save failure", "ACK success", "ACK rejection"])(
    "native cleanup ownership through the actual adapter/Provider/page: %s",
    async (incomplete) => {
      await clearP2PTerminalResult(session.sessionKey);
      const peer = { on: vi.fn(), off: vi.fn(), connect: vi.fn(), destroy: vi.fn() };
      const adapter = new P2PHostAdapter(
        session.hostDeckData,
        peer as unknown as ConstructorParameters<typeof P2PHostAdapter>[1],
        () => () => {}, 2, undefined, undefined, 5000, undefined, true, undefined,
        { gameId, roomCode: session.roomCode, resumeData: { session } }, {},
      );
      const finalState = gameStateFactory.build({ waiting_for: { type: "GameOver", data: { winner: 0 } } });
      const cleanupAuthority = vi.fn(async () => ({
        fullKey: session.nativeSession!.fullKey, terminalRevision: terminal.revision,
        deliveries: [{ recipient: 0, deliveryId: "native-host-delivery", credential: "native-host-credential" }],
      }));
      const acknowledge = vi.fn(async () => {
        // ACK must run only after durable owner write and resume deletion.
        expect(await loadFullTerminalCleanupStrict(gameId)).not.toBeNull();
        expect(await loadGameStrict(gameId)).toBeNull();
        expect(await loadCheckpoints(gameId)).toEqual([]);
        expect(await loadP2PHostSession(gameId)).toBeNull();
        if (incomplete === "ACK rejection") throw new Error("native terminal ACK rejected");
      });
      if (incomplete === "owner save failure") {
        const idb = await import("idb-keyval");
        const write = (await vi.importActual<typeof import("idb-keyval")>("idb-keyval")).set;
        vi.spyOn(idb, "set").mockImplementation(async (key, value, store) => {
          if (key === `phase-full-terminal-cleanup:${gameId}`) throw new Error("owner write rejected");
          await write(key, value, store);
        });
      }
      // Use the same typed private-state seam as the existing multiplayer
      // adapter tests. The real readiness gate, emit and persistence run below.
      const host = adapter as unknown as {
        nativeBridge: unknown;
        nativeTerminalAckReady: boolean;
        disconnectedSeats: Map<PlayerId, { disconnectedAt: number; timer: null }>;
        guestSessions: Map<PlayerId, { send: (message: { type: string }) => Promise<boolean>; close: () => void }>;
        guestAckedRevisions: Map<PlayerId, number>;
        issuedTerminalResults: Map<PlayerId, P2PTerminalResult>;
        terminalResultReceipts: Set<PlayerId>;
        recordGuestAck: (pid: PlayerId, revision: number) => void;
        recordTerminalReceipt: (pid: PlayerId, source: never, result: P2PTerminalResult) => void;
        tryAcknowledgeNativeTerminal: () => Promise<void>;
        commitTerminalIfComplete: (snapshot: EngineSnapshot, revision: number) => Promise<boolean>;
      };
      host.nativeBridge = {
        playerIds: () => [0, 1],
        terminalFinalViews: async () => new Map<PlayerId, GameState>([[0, finalState], [1, finalState]]),
        terminalDisplay: () => terminal.display,
        retainSuspendedTerminalViews: vi.fn(),
        terminalCleanupAuthority: cleanupAuthority,
        acknowledgeTerminalCleanup: acknowledge,
        dispose: vi.fn(),
      };
      host.nativeTerminalAckReady = true;
      if (incomplete === "disconnected without receipt") {
        host.disconnectedSeats.set(1, { disconnectedAt: Date.now(), timer: null });
      } else {
        host.disconnectedSeats.clear();
        host.guestAckedRevisions.set(1, terminal.revision - 1);
        host.guestSessions.set(1, {
          send: async () => incomplete !== "terminal send refused", close: vi.fn(),
        });
      }
      // Handoff comes from the real Provider's takeActiveP2PHost path. Native
      // startup/hydration is outside this gate test, so only that action is stubbed.
      vi.spyOn(useMultiplayerStore.getState(), "takeActiveP2PHost").mockReturnValueOnce(adapter);
      vi.spyOn(useGameStore.getState(), "resumeP2PHost").mockResolvedValue(undefined);
      const subscription = vi.spyOn(adapter, "onEvent");
      const clears = vi.spyOn(await import("../../services/gamePersistence"), "clearGame");
      mountPage();
      await waitFor(() => expect(subscription).toHaveBeenCalledOnce());
      await act(async () => {
        expect(await host.commitTerminalIfComplete({
          state: finalState, legalResult: { actions: [], autoPassRecommended: false }, seq: 1,
        }, terminal.revision)).toBe(true);
      });
      if (incomplete === "state ACK without receipt") {
        host.recordGuestAck(1, terminal.revision);
        await act(async () => host.tryAcknowledgeNativeTerminal());
      }
      const receiptRequired = incomplete === "owner save failure"
        || incomplete === "ACK success" || incomplete === "ACK rejection";
      if (receiptRequired) {
        const issued = host.issuedTerminalResults.get(1)!;
        const source = host.guestSessions.get(1)!;
        host.recordTerminalReceipt(1, source as never, issued);
        await act(async () => host.tryAcknowledgeNativeTerminal());
      }
      expect(host.disconnectedSeats.has(1)).toBe(incomplete === "disconnected without receipt");
      expect(host.terminalResultReceipts.has(1)).toBe(receiptRequired);
      const cleanupReady = receiptRequired;
      expect(cleanupAuthority).toHaveBeenCalledTimes(cleanupReady ? 1 : 0);
      expect(acknowledge).toHaveBeenCalledTimes(incomplete === "ACK success" || incomplete === "ACK rejection" ? 1 : 0);
      expect(screen.getByRole("heading", { name: "VICTORY" })).toBeInTheDocument();
      expect(clears).not.toHaveBeenCalled();
      const pendingOwner = await loadFullTerminalCleanupStrict(gameId);
      if (incomplete === "ACK rejection") expect(pendingOwner).not.toBeNull();
      else expect(pendingOwner).toBeNull();
      expect(await loadP2PTerminalResult(session.sessionKey)).not.toBeNull();
      const remaining = {
        snapshot: await loadGameStrict(gameId), checkpoints: await loadCheckpoints(gameId),
        host: await loadP2PHostSession(gameId),
      };
      console.info("native guest gate persistence", incomplete, remaining);
      expect(remaining).toEqual(incomplete === "ACK success" || incomplete === "ACK rejection"
        ? { snapshot: null, checkpoints: [], host: null }
        : { snapshot, checkpoints: [snapshot], host: session });
      if (incomplete === "ACK rejection") {
        // Restart must discover only the durable owner and show its saved
        // result even though the sidecar key is unavailable in this process.
        cleanup();
        useGameStore.getState().reset();
        mountPage();
        await waitFor(() => expect(useGameStore.getState().waitingFor?.type).toBe("GameOver"));
        expect(screen.getByRole("heading", { name: "VICTORY" })).toBeInTheDocument();
        expect(await loadFullTerminalCleanupStrict(gameId)).toEqual(pendingOwner);
        expect(boundary.hostRoom).not.toHaveBeenCalled();
        expect(boundary.start).not.toHaveBeenCalled();
      }
    },
  );

  it.each(["immediately after the owner write", "after a partial resume deletion"])(
    "a cleanup-only restart %s finishes deleting the resume records and keeps the owner",
    async (interruption) => {
      const owner: FullTerminalCleanupObligation = {
        gameId, p2pSessionKey: terminal.key, p2pTerminalId: terminal.terminalId,
        p2pResult: terminal, fullKey: session.nativeSession!.fullKey,
        terminalRevision: terminal.revision,
        deliveries: [{ recipient: 0, deliveryId: "host-delivery", credential: "host-credential" }],
      };
      await saveFullTerminalCleanupStrict(owner);
      if (interruption === "after a partial resume deletion") {
        // The game-record delete fails after the checkpoint and host-session
        // deletes have landed, as a crash mid-`clearGameStrict` would leave it.
        const idb = await import("idb-keyval");
        const actualDel = (await vi.importActual<typeof import("idb-keyval")>("idb-keyval")).del;
        vi.mocked(idb.del).mockImplementation(async (key, store) => {
          if (key === `phase-game:${gameId}`) throw new Error("interrupted");
          await actualDel(key, store);
        });
        await expect(clearGameStrict(gameId)).rejects.toThrow("interrupted");
        vi.mocked(idb.del).mockImplementation(actualDel);
        expect(await loadCheckpoints(gameId)).toEqual([]);
        expect(await loadP2PHostSession(gameId)).toBeNull();
        expect(await loadGameStrict(gameId)).toEqual(snapshot);
      } else {
        // Reach control: the resume records are still on disk at restart.
        expect(await loadGameStrict(gameId)).toEqual(snapshot);
        expect(await loadP2PHostSession(gameId)).toEqual(session);
      }

      mountPage();
      await waitFor(() => expect(useGameStore.getState().waitingFor?.type).toBe("GameOver"));
      expect(screen.getByRole("heading", { name: "VICTORY" })).toBeInTheDocument();
      await waitFor(async () => expect(await loadGameStrict(gameId)).toBeNull());
      expect(await loadCheckpoints(gameId)).toEqual([]);
      expect(await loadP2PHostSession(gameId)).toBeNull();
      // The native engine key is unavailable in this process, so no ACK ran
      // and the owner stays for the next attempt.
      expect(await loadFullTerminalCleanupStrict(gameId)).toEqual(owner);
      expect(boundary.hostRoom).not.toHaveBeenCalled();
    },
  );

  it("control: owner remains discoverable after the lifecycle deletes resume records", async () => {
    const owner: FullTerminalCleanupObligation = {
      gameId, p2pSessionKey: terminal.key, p2pTerminalId: terminal.terminalId,
      p2pResult: terminal, fullKey: session.nativeSession!.fullKey,
      terminalRevision: terminal.revision,
      deliveries: [{ recipient: 0, deliveryId: "host-delivery", credential: "host-credential" }],
    };
    await saveFullTerminalCleanupStrict(owner);
    await clearGameStrict(gameId);
    expect(await loadP2PHostSession(gameId)).toBeNull();
    expect(await loadGameStrict(gameId)).toBeNull();
    expect(await loadFullTerminalCleanupStrict(gameId)).toEqual(owner);
    // Verify the same underlying IndexedDB record, independent of a return mock.
    const store = createStore("phase-game-state", "phase-game-state");
    expect(await get(`phase-full-terminal-cleanup:${gameId}`, store)).toEqual(owner);
  });
});
