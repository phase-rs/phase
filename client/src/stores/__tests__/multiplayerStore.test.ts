import { beforeEach, describe, expect, it } from "vitest";

import { useMultiplayerStore } from "../multiplayerStore";

describe("multiplayerStore", () => {
  beforeEach(() => {
    useMultiplayerStore.setState({
      displayName: "",
      connectionStatus: "disconnected",
      activePlayerId: null,
      opponentDisplayName: null,
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
});
