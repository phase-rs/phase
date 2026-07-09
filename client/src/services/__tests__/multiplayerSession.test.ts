import { afterEach, beforeEach, describe, expect, it } from "vitest";

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
  it("loads a valid reconnect session", () => {
    const session = {
      gameCode: "ABCDE",
      playerToken: "token",
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
        serverUrl: "wss://play.example/ws",
        timestamp: 0,
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
