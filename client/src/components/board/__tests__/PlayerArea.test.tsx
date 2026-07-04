import { cleanup, render } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { useGameStore } from "../../../stores/gameStore.ts";
import {
  buildCommanderGameObject,
  buildObjectMap,
} from "../../../test/factories/gameObjectFactory.ts";
import {
  buildCommanderFormatConfig,
  buildFormatConfig,
  buildGameState,
  buildPlayer,
} from "../../../test/factories/gameStateFactory.ts";
import { PlayerArea } from "../PlayerArea.tsx";

vi.mock("../../../hooks/useCardImage", () => ({
  useCardImage: () => ({ src: null, isLoading: false }),
}));

describe("PlayerArea", () => {
  beforeEach(() => {
    const commander = buildCommanderGameObject({
      owner: 1,
      controller: 1,
      name: "Opponent Commander",
    });

    useGameStore.setState({
      gameState: buildGameState({
        players: [
          buildPlayer({ id: 0, life: 40 }),
          buildPlayer({ id: 1, life: 40 }),
        ],
        objects: buildObjectMap(commander),
        next_object_id: 102,
        next_timestamp: 2,
        format_config: buildCommanderFormatConfig(),
        command_zone: [commander.id],
        commander_damage: [],
      }),
      legalActions: [],
      spellCosts: {},
    });
  });

  afterEach(() => {
    cleanup();
  });

  it("renders an opponent commander as a command-zone card", () => {
    const { container } = render(<PlayerArea playerId={1} mode="focused" />);

    expect(
      container.querySelector('button[title="Commander: Opponent Commander"]'),
    ).toBeInTheDocument();
  });

  it("renders command-zone commanders without commander damage", () => {
    const commander = buildCommanderGameObject({
      owner: 1,
      controller: 1,
      name: "Opponent Commander",
    });

    useGameStore.setState({
      gameState: buildGameState({
        players: [
          buildPlayer({ id: 0, life: 40 }),
          buildPlayer({ id: 1, life: 40 }),
        ],
        objects: buildObjectMap(commander),
        next_object_id: 102,
        next_timestamp: 2,
        format_config: buildFormatConfig({
          format: "TinyLeaders",
          starting_life: 20,
          min_players: 2,
          max_players: 2,
          deck_size: 50,
          singleton: true,
          command_zone: true,
          commander_damage_threshold: null,
          range_of_influence: null,
          team_based: false,
          uses_commander: false,
          allow_debug_actions: false,
        }),
        command_zone: [commander.id],
        commander_damage: [],
      }),
      legalActions: [],
      spellCosts: {},
    });

    const { container } = render(<PlayerArea playerId={1} mode="focused" />);

    expect(
      container.querySelector('button[title="Commander: Opponent Commander"]'),
    ).toBeInTheDocument();
    expect(container.querySelector('[data-testid="commander-damage-1"]')).toBeNull();
  });
});
