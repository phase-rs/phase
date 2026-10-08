import { afterEach, describe, expect, it } from "vitest";

import { useMultiplayerStore } from "../multiplayerStore.ts";

const initialAvatars = useMultiplayerStore.getState().playerAvatars;

afterEach(() => {
  useMultiplayerStore.setState({ playerAvatars: initialAvatars });
  localStorage.clear();
});

describe("multiplayerStore player avatar identities", () => {
  it("keeps exact card and external identities independent by player id", () => {
    const playerAvatars = new Map([
      [0, { kind: "card", cardName: "Jace, the Mind Sculptor" } as const],
      [1, { kind: "external", url: "https://provider.example/avatar.png" } as const],
    ]);

    useMultiplayerStore.setState({ playerAvatars });

    expect(useMultiplayerStore.getState().playerAvatars).toEqual(playerAvatars);
    expect(useMultiplayerStore.getState().playerAvatars.get(0)).toEqual({
      kind: "card",
      cardName: "Jace, the Mind Sculptor",
    });
    expect(useMultiplayerStore.getState().playerAvatars.get(1)).toEqual({
      kind: "external",
      url: "https://provider.example/avatar.png",
    });
  });

  it("starts empty and is excluded from persisted multiplayer state", () => {
    expect(initialAvatars).toEqual(new Map());

    useMultiplayerStore.setState({
      playerAvatars: new Map([[7, { kind: "card", cardName: "Private Resolver Input" }]]),
      displayName: "Persisted Player",
    });

    const persisted = localStorage.getItem("phase-multiplayer");
    expect(persisted).toContain("Persisted Player");
    expect(persisted).not.toContain("playerAvatars");
    expect(persisted).not.toContain("Private Resolver Input");
  });

  it("can be cleared atomically at a session boundary", () => {
    useMultiplayerStore.setState({
      playerAvatars: new Map([[3, { kind: "external", url: "https://one.example/a.png" }]]),
    });

    useMultiplayerStore.setState({ playerAvatars: new Map() });

    expect(useMultiplayerStore.getState().playerAvatars).toEqual(new Map());
  });
});
