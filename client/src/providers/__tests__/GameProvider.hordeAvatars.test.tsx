import { act } from "react";
import { cleanup, render } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

vi.mock("../../services/serverDetection.ts", () => ({
  detectServerUrl: vi.fn(() => new Promise<string>(() => {})),
}));

import type { HordeRuleset } from "../../adapter/types.ts";
import { assignRandomAvatars } from "../../services/playerAvatars.ts";
import { useGameStore } from "../../stores/gameStore.ts";
import { useMultiplayerDraftStore } from "../../stores/multiplayerDraftStore.ts";
import { useMultiplayerStore } from "../../stores/multiplayerStore.ts";
import {
  buildFormatConfig,
  buildGameState,
  buildPlayers,
} from "../../test/factories/gameStateFactory.ts";
import { GameProvider } from "../GameProvider.tsx";

const TURTLE_TEAMUP_RULESET: HordeRuleset = {
  challenge_deck: "TurtleTeamUpHorde",
  wave: { type: "FixedCount", data: 2 },
  survivor_setup_turns: 0,
  combined_base_life: 10,
  per_extra_survivor_life_delta: 10,
  horde_creatures_forced_attackers: true,
  horde_creatures_haste: false,
};

const BOSS_PRINTING_ID = "9a2d23c4-d381-4a92-b4b1-0cf71948f941";
const BOSS_AVATAR_URL =
  `https://api.scryfall.com/cards/${BOSS_PRINTING_ID}?format=image&version=art_crop`;

function expectedRandom(playerCount: number, seed: string) {
  return new Map(
    assignRandomAvatars(playerCount, seed).map((avatar, playerId) => [
      playerId,
      { kind: "card" as const, cardName: avatar.cardName },
    ]),
  );
}

describe("GameProvider Horde seat identities", () => {
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

  it("installs Turtle and Boss identities for a Turtle Team-Up Horde game", () => {
    const gameId = "horde-avatars-tmnt";
    render(
      <GameProvider gameId={gameId} mode="ai" playerCount={2}>
        <div />
      </GameProvider>,
    );

    act(() => {
      useGameStore.setState({
        gameId,
        gameState: {
          ...buildGameState({ players: buildPlayers([{ id: 0 }, { id: 1 }]) }),
          format_config: buildFormatConfig({
            format: "Horde",
            horde_ruleset: TURTLE_TEAMUP_RULESET,
          }),
          horde_seats: [1],
          command_zone: [100, 101],
          objects: {
            100: {
              name: "Shredder, Foot Clan Overlord",
              owner: 1,
              is_horde_deck_token: true,
              token_image_ref: {
                preset_id: "shredder-preset",
                scryfall_id: BOSS_PRINTING_ID,
                scryfall_oracle_id: "shredder-oracle",
              },
            },
            101: { name: "Horde Emblem", owner: 1, is_emblem: true },
          },
        } as never,
      });
    });

    expect(useMultiplayerStore.getState().playerNames).toEqual(
      new Map([[0, "Leonardo"], [1, "Shredder"]]),
    );
    expect(useMultiplayerStore.getState().playerAvatars).toEqual(new Map([
      [0, { kind: "card", cardName: "Leonardo, Sewer Samurai" }],
      [1, { kind: "external", url: BOSS_AVATAR_URL }],
    ]));
  });

  it("assigns Turtles in seat order across allied survivors", () => {
    const gameId = "horde-avatars-allies";
    render(
      <GameProvider gameId={gameId} mode="ai" playerCount={3}>
        <div />
      </GameProvider>,
    );

    act(() => {
      useGameStore.setState({
        gameId,
        gameState: {
          ...buildGameState({ players: buildPlayers([{ id: 0 }, { id: 1 }, { id: 2 }]) }),
          format_config: buildFormatConfig({
            format: "Horde",
            horde_ruleset: TURTLE_TEAMUP_RULESET,
          }),
          horde_seats: [1],
          command_zone: [100],
          objects: {
            100: {
              name: "Krang, Dimension X Overlord",
              owner: 1,
              is_horde_deck_token: true,
              token_image_ref: {
                preset_id: "krang-preset",
                scryfall_id: BOSS_PRINTING_ID,
                scryfall_oracle_id: "krang-oracle",
              },
            },
          },
        } as never,
      });
    });

    expect(useMultiplayerStore.getState().playerNames).toEqual(
      new Map([[0, "Leonardo"], [1, "Krang"], [2, "Raphael"]]),
    );
    expect(useMultiplayerStore.getState().playerAvatars.get(2)).toEqual({
      kind: "card",
      cardName: "Raphael, Ninja Destroyer",
    });
  });

  it("keeps the seeded identities for other challenge decks", () => {
    const gameId = "horde-avatars-other-deck";
    render(
      <GameProvider gameId={gameId} mode="ai" playerCount={2}>
        <div />
      </GameProvider>,
    );

    act(() => {
      useGameStore.setState({
        gameId,
        gameState: {
          ...buildGameState({ players: buildPlayers([{ id: 0 }, { id: 1 }]) }),
          format_config: buildFormatConfig({
            format: "Horde",
            horde_ruleset: {
              ...TURTLE_TEAMUP_RULESET,
              challenge_deck: "CybermanHorde",
            },
          }),
          horde_seats: [1],
          command_zone: [],
          objects: { 50: { name: "Cyberman", owner: 1, zone: "Library" } },
        } as never,
      });
    });

    // No themed seats outside Turtle Team-Up: the random fallback survives.
    expect(useMultiplayerStore.getState().playerAvatars).toEqual(expectedRandom(2, gameId));
    expect(useMultiplayerStore.getState().playerNames.get(0)).toBe("You");
  });

  it("waits for the Boss when none is deployed yet, then applies", () => {
    const gameId = "horde-avatars-late-boss";
    render(
      <GameProvider gameId={gameId} mode="ai" playerCount={2}>
        <div />
      </GameProvider>,
    );

    const hordeState = (objects: object, commandZone: number[]) => ({
      ...buildGameState({ players: buildPlayers([{ id: 0 }, { id: 1 }]) }),
      format_config: buildFormatConfig({
        format: "Horde",
        horde_ruleset: TURTLE_TEAMUP_RULESET,
      }),
      horde_seats: [1],
      command_zone: commandZone,
      objects,
    });

    act(() => {
      useGameStore.setState({ gameId, gameState: hordeState({}, []) as never });
    });
    expect(useMultiplayerStore.getState().playerAvatars).toEqual(expectedRandom(2, gameId));

    act(() => {
      useGameStore.setState({
        gameState: hordeState(
          {
            100: {
              name: "Slash, the Dark Mirror",
              owner: 1,
              is_horde_deck_token: true,
              token_image_ref: {
                preset_id: "slash-preset",
                scryfall_id: BOSS_PRINTING_ID,
                scryfall_oracle_id: "slash-oracle",
              },
            },
          },
          [100],
        ) as never,
      });
    });
    expect(useMultiplayerStore.getState().playerNames).toEqual(
      new Map([[0, "Leonardo"], [1, "Slash"]]),
    );
    expect(useMultiplayerStore.getState().playerAvatars.get(1)).toEqual({
      kind: "external",
      url: BOSS_AVATAR_URL,
    });
  });
});
