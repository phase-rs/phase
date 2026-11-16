import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { ACTIVE_GAME_CHANGED_EVENT } from "../activeGameChange";

import {
  clearWsSession,
  loadWsSession,
  saveWsSession,
  WS_SESSION_STORAGE_KEY,
} from "../multiplayerSession";

const originalLocalStorageDescriptor = Object.getOwnPropertyDescriptor(
  globalThis,
  "localStorage",
);

function setLocalStorage(storage: Partial<Storage>): void {
  Object.defineProperty(globalThis, "localStorage", {
    configurable: true,
    value: storage,
  });
}

function setMemoryLocalStorage(): void {
  const items = new Map<string, string>();
  setLocalStorage({
    getItem: (key) => items.get(key) ?? null,
    setItem: (key, value) => {
      items.set(key, value);
    },
    removeItem: (key) => {
      items.delete(key);
    },
    clear: () => {
      items.clear();
    },
  });
}

beforeEach(() => {
  setMemoryLocalStorage();
});

afterEach(() => {
  if (originalLocalStorageDescriptor) {
    Object.defineProperty(globalThis, "localStorage", originalLocalStorageDescriptor);
  }
});

describe("multiplayer WebSocket session persistence", () => {
  it.each(["endpoint", "game code", "generation", "player token"])(
    "does not clear a replacement session with a different %s", (changed) => {
      const expected = {
        gameCode: "ABCDE", playerToken: "old-token",
        fullKey: { game_code: "ABCDE", generation: 1 },
        serverUrl: "wss://play.example/ws", timestamp: Date.now(),
      };
      const replacement = {
        ...expected,
        ...(changed === "endpoint" ? { serverUrl: "wss://new.example/ws" } : {}),
        ...(changed === "game code" ? { gameCode: "NEW", fullKey: { game_code: "NEW", generation: 1 } } : {}),
        ...(changed === "generation" ? { fullKey: { game_code: "ABCDE", generation: 2 } } : {}),
        ...(changed === "player token" ? { playerToken: "another-seat-token" } : {}),
      };
      saveWsSession(replacement);
      clearWsSession(expected);
      expect(loadWsSession()).toEqual(replacement);
      saveWsSession({ ...expected, terminalOnly: true }, expected);
      expect(loadWsSession()).toEqual(replacement);
    },
  );

  it("loads a valid reconnect session", () => {
    const session = {
      gameCode: "ABCDE",
      playerToken: "token",
      fullKey: { game_code: "ABCDE", generation: 1 },
      serverUrl: "wss://play.example/ws",
      timestamp: Date.now(),
    };

    saveWsSession(session);

    expect(loadWsSession()).toEqual(session);
  });

  it("removes expired reconnect sessions", () => {
    localStorage.setItem(
      WS_SESSION_STORAGE_KEY,
      JSON.stringify({
        gameCode: "ABCDE",
        playerToken: "token",
        fullKey: { game_code: "ABCDE", generation: 1 },
        serverUrl: "wss://play.example/ws",
        timestamp: 0,
      }),
    );

    expect(loadWsSession()).toBeNull();
    expect(localStorage.getItem(WS_SESSION_STORAGE_KEY)).toBeNull();
  });

  it("migrates legacy reconnect records by invalidating records without a Full session key", () => {
    localStorage.setItem(
      WS_SESSION_STORAGE_KEY,
      JSON.stringify({
        gameCode: "ABCDE",
        playerToken: "legacy-token",
        serverUrl: "wss://play.example/ws",
        timestamp: Date.now(),
      }),
    );

    expect(loadWsSession()).toBeNull();
    expect(localStorage.getItem(WS_SESSION_STORAGE_KEY)).toBeNull();
  });

  it("does not crash app startup when localStorage reads are blocked", () => {
    setLocalStorage({
      getItem: () => {
        throw new Error("storage blocked");
      },
      removeItem: () => undefined,
    });

    expect(loadWsSession()).toBeNull();
  });

  it("does not crash hosting when localStorage writes are blocked", () => {
    setLocalStorage({
      setItem: () => {
        throw new Error("quota exceeded");
      },
    });

    expect(() => {
      saveWsSession({
        gameCode: "ABCDE",
        playerToken: "token",
        fullKey: { game_code: "ABCDE", generation: 1 },
        serverUrl: "wss://play.example/ws",
        timestamp: Date.now(),
      });
    }).not.toThrow();
  });

  it("does not crash cleanup when localStorage removal is blocked", () => {
    setLocalStorage({
      removeItem: () => {
        throw new Error("storage blocked");
      },
    });

    expect(() => clearWsSession()).not.toThrow();
  });
});

describe("multiplayer WebSocket session change notification", () => {
  const session = {
    gameCode: "ABCDE",
    playerToken: "token",
    fullKey: { game_code: "ABCDE", generation: 1 },
    serverUrl: "wss://play.example/ws",
    timestamp: Date.now(),
  };

  function countNotifications() {
    const seen = vi.fn();
    window.addEventListener(ACTIVE_GAME_CHANGED_EVENT, seen);
    return { seen, stop: () => window.removeEventListener(ACTIVE_GAME_CHANGED_EVENT, seen) };
  }

  it("notifies the resume list for every landed save and removal, and only those", () => {
    const { seen, stop } = countNotifications();
    saveWsSession(session);
    expect(seen).toHaveBeenCalledTimes(1);

    // A terminal-only transition for the stored identity lands and notifies.
    saveWsSession({ ...session, terminalOnly: true }, session);
    expect(seen).toHaveBeenCalledTimes(2);

    // A write or removal bound to another identity changes nothing.
    const other = { ...session, playerToken: "another-seat-token" };
    saveWsSession({ ...other, terminalOnly: true }, other);
    clearWsSession(other);
    expect(seen).toHaveBeenCalledTimes(2);

    clearWsSession(session);
    expect(loadWsSession()).toBeNull();
    expect(seen).toHaveBeenCalledTimes(3);

    // Clearing an empty store has nothing to report.
    clearWsSession();
    expect(seen).toHaveBeenCalledTimes(3);
    stop();
  });

  it("notifies for a page-only session when storage writes are blocked", () => {
    setLocalStorage({
      getItem: () => null,
      setItem: () => {
        throw new Error("quota exceeded");
      },
      removeItem: () => undefined,
    });
    const { seen, stop } = countNotifications();
    saveWsSession({ ...session, terminalOnly: true });
    expect(loadWsSession()).toEqual({ ...session, terminalOnly: true });
    expect(seen).toHaveBeenCalledTimes(1);

    clearWsSession();
    expect(loadWsSession()).toBeNull();
    expect(seen).toHaveBeenCalledTimes(2);
    stop();
  });
});
