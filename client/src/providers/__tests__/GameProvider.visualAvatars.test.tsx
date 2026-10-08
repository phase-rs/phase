import { act } from "react";
import { cleanup, render } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

vi.mock("../../services/serverDetection.ts", () => ({
  detectServerUrl: vi.fn(() => new Promise<string>(() => {})),
}));

import { assignRandomAvatars } from "../../services/playerAvatars.ts";
import { useGameStore } from "../../stores/gameStore.ts";
import { useMultiplayerDraftStore } from "../../stores/multiplayerDraftStore.ts";
import { useMultiplayerStore } from "../../stores/multiplayerStore.ts";
import {
  buildCommanderFormatConfig,
  buildGameState,
  buildPlayers,
} from "../../test/factories/gameStateFactory.ts";
import { GameProvider } from "../GameProvider.tsx";

function expectedRandom(playerCount: number, seed: string) {
  return new Map(
    assignRandomAvatars(playerCount, seed).map((avatar, playerId) => [
      playerId,
      { kind: "card" as const, cardName: avatar.cardName },
    ]),
  );
}

describe("GameProvider semantic visual avatars", () => {
  beforeEach(() => {
    localStorage.clear();
    useMultiplayerStore.setState({
      playerNames: new Map(),
      playerAvatars: new Map(),
      activePlayerId: 0,
    });
    useMultiplayerDraftStore.setState({ matchPairing: null });
    useGameStore.setState({
      adapter: null,
      gameId: null,
      gameState: null,
      gameMode: null,
      waitingFor: null,
    });
  });

  afterEach(() => {
    cleanup();
    vi.restoreAllMocks();
  });

  it("installs the complete seeded random identity map synchronously", () => {
    const fetchSpy = vi.spyOn(globalThis, "fetch");

    render(
      <GameProvider gameId="avatar-random" mode="ai" playerCount={3}>
        <div />
      </GameProvider>,
    );

    expect(useMultiplayerStore.getState().playerAvatars).toEqual(
      expectedRandom(3, "avatar-random"),
    );
    expect(useMultiplayerStore.getState().playerNames.get(0)).toBe("You");
    expect(fetchSpy).not.toHaveBeenCalled();
  });

  it("replaces random identities with the first commander for each owner", () => {
    render(
      <GameProvider gameId="avatar-commanders" mode="ai" playerCount={2}>
        <div />
      </GameProvider>,
    );

    act(() => {
      useGameStore.setState({
        gameId: "avatar-commanders",
        gameState: {
          ...buildGameState({ players: buildPlayers([{ id: 0 }, { id: 1 }]) }),
          command_zone: [10, 11, 12],
          objects: {
            10: { name: "Atraxa, Praetors' Voice", owner: 0, is_commander: true },
            11: { name: "Ignored Partner", owner: 0, is_commander: true },
            12: { name: "Niv-Mizzet Reborn", owner: 1, is_commander: true },
          },
        } as never,
      });
    });

    expect(useMultiplayerStore.getState().playerAvatars).toEqual(new Map([
      [0, { kind: "card", cardName: "Atraxa, Praetors' Voice" }],
      [1, { kind: "card", cardName: "Niv-Mizzet Reborn" }],
    ]));
  });

  it.each([
    [{ type: "Bot", botName: "Jace" }, 0, "Jace, the Mind Sculptor"],
    [{ type: "HumanHost", opponentName: "Liliana" }, 0, "Liliana of the Veil"],
    [{ type: "HumanGuest", opponentName: "Chandra" }, 1, "Chandra, Torch of Defiance"],
  ] as const)(
    "installs draft %s identities with the exact local seat",
    (pairing, localPlayerId, opponentCardName) => {
      const gameId = `avatar-draft-${pairing.type}`;
      useMultiplayerDraftStore.setState({ matchPairing: pairing as never });
      useGameStore.setState({
        adapter: {} as never,
        gameId,
        gameState: buildGameState({ players: buildPlayers([{ id: 0 }, { id: 1 }]) }),
      });

      render(
        <GameProvider gameId={gameId} mode="draft-match">
          <div />
        </GameProvider>,
      );

      const state = useMultiplayerStore.getState();
      const opponentPlayerId = localPlayerId === 0 ? 1 : 0;
      expect(state.activePlayerId).toBe(localPlayerId);
      expect(state.playerNames.get(localPlayerId)).toBe("You");
      expect(state.playerAvatars.get(opponentPlayerId)).toEqual({
        kind: "card",
        cardName: opponentCardName,
      });
    },
  );

  it("preserves lobby names while replacing online identities and clears on a local setup", () => {
    useMultiplayerStore.setState({
      playerNames: new Map([[0, "Lobby Host"], [1, "Lobby Guest"]]),
    });
    const online = render(
      <GameProvider gameId="avatar-online" mode="online" playerCount={2}>
        <div />
      </GameProvider>,
    );

    expect(useMultiplayerStore.getState().playerNames).toEqual(
      new Map([[0, "Lobby Host"], [1, "Lobby Guest"]]),
    );
    expect(useMultiplayerStore.getState().playerAvatars).toEqual(
      expectedRandom(2, "avatar-online"),
    );

    act(() => {
      useGameStore.setState({
        gameState: {
          ...buildGameState({ players: buildPlayers([{ id: 0 }, { id: 1 }]) }),
          format_config: buildCommanderFormatConfig(),
          command_zone: [21, 22],
          objects: {
            21: { name: "Kinnan, Bonder Prodigy", owner: 0, is_commander: true },
            22: { name: "Muldrotha, the Gravetide", owner: 1, is_commander: true },
          },
        } as never,
      });
    });
    expect(useMultiplayerStore.getState().playerNames).toEqual(
      new Map([[0, "Lobby Host"], [1, "Lobby Guest"]]),
    );
    expect(useMultiplayerStore.getState().playerAvatars).toEqual(new Map([
      [0, { kind: "card", cardName: "Kinnan, Bonder Prodigy" }],
      [1, { kind: "card", cardName: "Muldrotha, the Gravetide" }],
    ]));

    online.unmount();
    render(
      <GameProvider gameId="avatar-local" mode="local">
        <div />
      </GameProvider>,
    );
    expect(useMultiplayerStore.getState().playerAvatars).toEqual(new Map());
    expect(useMultiplayerStore.getState().playerNames).toEqual(new Map());
  });
});
