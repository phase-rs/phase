import { act, cleanup, renderHook, waitFor } from "@testing-library/react";
import { MemoryRouter, useLocation } from "react-router";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { useResumables } from "../useResumables";
import {
  clearOnlineSession,
  loadActiveGame,
  saveActiveGame,
  type ActiveGameMeta,
} from "../../services/gamePersistence";
import { saveWsSession, type WsSessionData } from "../../services/multiplayerSession";
import type { P2PSessionData } from "../../services/p2pSession";

const { loadP2PSession } = vi.hoisted(() => ({ loadP2PSession: vi.fn() }));

vi.mock("../../services/p2pSession", () => ({ loadP2PSession }));
vi.mock("../../services/nativeEngine", () => ({
  canAttemptNativeEngine: () => true,
  nativeEngineKeyForCurrentOrigin: () => ({ release: { version: "test" } }),
}));

const NATIVE_KEY = { game_code: "NATIV1", generation: 1 };

function nativeSession(terminalOnly = false): NonNullable<ActiveGameMeta["nativeSession"]> {
  return {
    gameCode: NATIVE_KEY.game_code,
    playerId: 0,
    playerToken: "native-token",
    fullKey: NATIVE_KEY,
    ...(terminalOnly ? { terminalOnly: true as const } : {}),
  };
}

function renderResumables() {
  return renderHook(
    () => ({ resumables: useResumables(), location: useLocation() }),
    { wrapper: ({ children }) => <MemoryRouter>{children}</MemoryRouter> },
  );
}

beforeEach(() => {
  localStorage.clear();
  loadP2PSession.mockReset();
});

afterEach(() => cleanup());

describe("useResumables — retained results", () => {
  it("offers a retained online result as a reopen action, never as a match", async () => {
    const session: WsSessionData = {
      gameCode: "ONLIN1",
      playerId: 1,
      playerToken: "token",
      fullKey: { game_code: "ONLIN1", generation: 2 },
      serverUrl: "wss://example.test/ws",
      timestamp: Date.now(),
      terminalOnly: true,
    };
    saveWsSession(session);
    saveActiveGame({ id: "online-finished", mode: "online", difficulty: "Medium" });

    const { result } = renderResumables();
    await waitFor(() => expect(result.current.resumables.retainedResult?.id).toBe("online-finished"));
    expect(result.current.resumables.match).toBeNull();

    act(() => result.current.resumables.viewRetainedResult());
    expect(result.current.location.pathname).toBe("/game/online-finished");
    expect(result.current.location.search).toBe("?mode=host");
  });

  it("removes the retained action once the acknowledgement clears the pointer", async () => {
    const session: WsSessionData = {
      gameCode: "ONLIN2",
      playerId: 1,
      playerToken: "token",
      fullKey: { game_code: "ONLIN2", generation: 2 },
      serverUrl: "wss://example.test/ws",
      timestamp: Date.now(),
      terminalOnly: true,
    };
    saveWsSession(session);
    saveActiveGame({ id: "online-acknowledged", mode: "online", difficulty: "Medium" });

    const { result } = renderResumables();
    await waitFor(() => expect(result.current.resumables.retainedResult?.id).toBe("online-acknowledged"));

    // The successful ACK releases the pointer through the production session owner.
    act(() => clearOnlineSession("online-acknowledged", session));
    await waitFor(() => expect(result.current.resumables.retainedResult).toBeNull());

    act(() => result.current.resumables.viewRetainedResult());
    expect(result.current.location.pathname).toBe("/");
  });

  it("offers a retained native result and routes back to its game", async () => {
    saveActiveGame({
      id: "native-finished",
      mode: "ai",
      difficulty: "Hard",
      aiSeats: [{ difficulty: "Hard" }, { difficulty: "Hard" }],
      nativeSession: nativeSession(true),
    });

    const { result } = renderResumables();
    await waitFor(() => expect(result.current.resumables.retainedResult?.id).toBe("native-finished"));
    expect(result.current.resumables.match).toBeNull();

    act(() => result.current.resumables.viewRetainedResult());
    expect(result.current.location.pathname).toBe("/game/native-finished");
    expect(result.current.location.search).toBe("?mode=ai&difficulty=Hard&players=3");
  });
});

describe("useResumables — stale validation lookups", () => {
  async function startStaleLookup() {
    let resolveLookup!: (value: P2PSessionData | null) => void;
    loadP2PSession.mockImplementation(
      () => new Promise<P2PSessionData | null>((resolve) => { resolveLookup = resolve; }),
    );
    saveActiveGame({ id: "p2p-game", mode: "p2p-join", difficulty: "Medium", p2pRoomCode: "ABCDE" });
    const rendered = renderResumables();
    await waitFor(() => expect(loadP2PSession).toHaveBeenCalledOnce());

    const replacement: ActiveGameMeta = {
      id: "replacement",
      mode: "ai",
      difficulty: "Medium",
      nativeSession: nativeSession(),
    };
    // The storage notification starts a second validation, which finds this
    // replacement synchronously and publishes it.
    saveActiveGame(replacement);
    await waitFor(() => expect(rendered.result.current.resumables.match?.id).toBe("replacement"));
    return { ...rendered, replacement, resolveLookup: () => resolveLookup };
  }

  it("a missing lookup that settles late cannot clear the replacement pointer", async () => {
    const { result, replacement, resolveLookup } = await startStaleLookup();

    await act(async () => { resolveLookup()(null); });

    expect(loadActiveGame()).toEqual(replacement);
    expect(result.current.resumables.match?.id).toBe("replacement");
    expect(result.current.resumables.retainedResult).toBeNull();
  });

  it("a successful lookup that settles late cannot replace the newer match", async () => {
    const { result, replacement, resolveLookup } = await startStaleLookup();

    await act(async () => { resolveLookup()({} as P2PSessionData); });

    expect(loadActiveGame()).toEqual(replacement);
    expect(result.current.resumables.match?.id).toBe("replacement");
  });
});
