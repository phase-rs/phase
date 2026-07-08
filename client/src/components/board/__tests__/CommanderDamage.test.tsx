import { cleanup, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it } from "vitest";

import { useGameStore } from "../../../stores/gameStore.ts";
import { useMultiplayerStore } from "../../../stores/multiplayerStore.ts";
import {
  buildCommanderGameObject,
  buildObjectMap,
} from "../../../test/factories/gameObjectFactory.ts";
import {
  buildCommanderFormatConfig,
  buildGameState,
  buildPlayer,
} from "../../../test/factories/gameStateFactory.ts";
import { CommanderDamage } from "../CommanderDamage.tsx";

/**
 * CR 903.10a: Commander damage tallies are public game state. Every viewer
 * MUST be able to see how much commander damage every player has taken from
 * every commander — including opponents. The engine emits
 * `derived.commander_damage_by_attacker` keyed by the attacking commander's
 * controller, with `victim`, `commander`, and `damage` on each entry, and
 * filters nothing per viewer (`crates/engine/src/game/derived_views.rs`).
 */

describe("CommanderDamage", () => {
  afterEach(() => {
    cleanup();
  });

  beforeEach(() => {
    useGameStore.setState({ gameState: undefined, legalActions: [], spellCosts: {} });
    useMultiplayerStore.setState({ playerNames: new Map() });
  });

  /**
   * Scenario: local P0 has dealt 7 commander damage to opponent P1. The
   * derived view keys the entry by the attacker's controller (P0). When
   * rendering opponent P1's panel, CommanderDamage must surface the badge
   * — this is the bug-report scenario.
   */
  it("renders opponent's commander-damage tally taken from the local player", () => {
    const myCmd = buildCommanderGameObject({
      id: 101,
      owner: 0,
      controller: 0,
      name: "My Commander",
      power: 5,
      toughness: 5,
      base_power: 5,
      base_toughness: 5,
    });
    useGameStore.setState({
      gameState: buildGameState({
        players: [
          buildPlayer({ id: 0, life: 40 }),
          buildPlayer({ id: 1, life: 40 }),
        ],
        objects: buildObjectMap(myCmd),
        next_object_id: 1000,
        next_timestamp: 2,
        format_config: buildCommanderFormatConfig(),
        command_zone: [myCmd.id],
        commander_damage: [],
        derived: {
          commander_damage_by_attacker: {
            // Attacker = local P0; victim = opponent P1.
            "0": [{ victim: 1, commander: myCmd.id, damage: 7 }],
          },
        },
      }),
    });

    render(<CommanderDamage playerId={1} />);

    const root = screen.getByTestId("commander-damage-1");
    expect(root).toBeInTheDocument();
    expect(root.textContent).toContain("My Commander");
    expect(root.textContent).toContain("7");
  });

  /**
   * Mirror case: opponent's commander dealt damage to me. The component
   * already exercises this when rendering the local player's panel; pin
   * the behavior so future refactors can't silently regress it.
   */
  it("renders local player's commander-damage tally taken from an opponent", () => {
    const oppCmd = buildCommanderGameObject({
      id: 202,
      owner: 1,
      controller: 1,
      name: "Opp Commander",
    });
    useGameStore.setState({
      gameState: buildGameState({
        players: [
          buildPlayer({ id: 0, life: 40 }),
          buildPlayer({ id: 1, life: 40 }),
        ],
        objects: buildObjectMap(oppCmd),
        next_object_id: 1000,
        next_timestamp: 2,
        format_config: buildCommanderFormatConfig(),
        command_zone: [oppCmd.id],
        commander_damage: [],
        derived: {
          commander_damage_by_attacker: {
            "1": [{ victim: 0, commander: oppCmd.id, damage: 11 }],
          },
        },
      }),
    });

    render(<CommanderDamage playerId={0} />);

    const root = screen.getByTestId("commander-damage-0");
    expect(root.textContent).toContain("Opp Commander");
    expect(root.textContent).toContain("11");
  });

  it("uses shortened commander names in compact mode", () => {
    const myCmd = buildCommanderGameObject({
      id: 101,
      owner: 0,
      controller: 0,
      name: "Otrimi, the Ever-Playful",
    });
    useGameStore.setState({
      gameState: buildGameState({
        players: [
          buildPlayer({ id: 0, life: 40 }),
          buildPlayer({ id: 1, life: 40 }),
        ],
        objects: buildObjectMap(myCmd),
        next_object_id: 1000,
        next_timestamp: 2,
        format_config: buildCommanderFormatConfig(),
        command_zone: [myCmd.id],
        commander_damage: [],
        derived: {
          commander_damage_by_attacker: {
            "0": [{ victim: 1, commander: myCmd.id, damage: 7 }],
          },
        },
      }),
    });

    render(<CommanderDamage playerId={1} compact />);

    const root = screen.getByTestId("commander-damage-1");
    expect(root.textContent).toContain("Otrimi");
    expect(root.textContent).not.toContain("the Ever-Playful");
    expect(screen.getByTitle("Commander damage from Otrimi, the Ever-Playful: 7/21")).toBeInTheDocument();
  });

  it("keeps opposing player identity in the tooltip and seat-color rail", () => {
    const oppCmd = buildCommanderGameObject({
      id: 202,
      owner: 1,
      controller: 1,
      name: "Opp Commander",
    });
    useMultiplayerStore.setState({ playerNames: new Map([[1, "Atraxa"]]) });
    useGameStore.setState({
      gameState: buildGameState({
        seat_order: [0, 1],
        players: [
          buildPlayer({ id: 0, life: 40 }),
          buildPlayer({ id: 1, life: 40 }),
        ],
        objects: buildObjectMap(oppCmd),
        next_object_id: 1000,
        next_timestamp: 2,
        format_config: buildCommanderFormatConfig(),
        command_zone: [oppCmd.id],
        commander_damage: [],
        derived: {
          commander_damage_by_attacker: {
            "1": [{ victim: 0, commander: oppCmd.id, damage: 11 }],
          },
        },
      }),
    });

    render(<CommanderDamage playerId={0} />);

    const attackerRail = screen.getByTitle("Commander damage from Atraxa: 11/21");
    expect(attackerRail).toHaveStyle({ borderLeftColor: "#F43F5E" });
    expect(screen.queryByText("Atraxa")).not.toBeInTheDocument();
    expect(screen.queryByText("Opp 1")).not.toBeInTheDocument();
  });

  it("shows the player label once when multiple commanders from that player dealt damage", () => {
    const firstCmd = buildCommanderGameObject({
      id: 202,
      owner: 1,
      controller: 1,
      name: "First Partner",
    });
    const secondCmd = buildCommanderGameObject({
      id: 303,
      owner: 1,
      controller: 1,
      name: "Second Partner",
    });
    useMultiplayerStore.setState({ playerNames: new Map([[1, "Partner Player"]]) });
    useGameStore.setState({
      gameState: buildGameState({
        seat_order: [0, 1],
        players: [
          buildPlayer({ id: 0, life: 40 }),
          buildPlayer({ id: 1, life: 40 }),
        ],
        objects: buildObjectMap(firstCmd, secondCmd),
        next_object_id: 1000,
        next_timestamp: 2,
        format_config: buildCommanderFormatConfig(),
        command_zone: [firstCmd.id, secondCmd.id],
        commander_damage: [],
        derived: {
          commander_damage_by_attacker: {
            "1": [
              { victim: 0, commander: firstCmd.id, damage: 4 },
              { victim: 0, commander: secondCmd.id, damage: 6 },
            ],
          },
        },
      }),
    });

    render(<CommanderDamage playerId={0} />);

    expect(screen.getByText("Partner Player")).toBeInTheDocument();
    expect(screen.getByText("First Partner")).toBeInTheDocument();
    expect(screen.getByText("Second Partner")).toBeInTheDocument();
  });

  /**
   * No damage to this victim → no badge. Component must not render an
   * empty container that takes up layout space on opponent panels.
   */
  it("renders nothing when no commander damage targets this victim", () => {
    const myCmd = buildCommanderGameObject({ id: 101, owner: 0, controller: 0 });
    useGameStore.setState({
      gameState: buildGameState({
        players: [
          buildPlayer({ id: 0, life: 40 }),
          buildPlayer({ id: 1, life: 40 }),
        ],
        objects: buildObjectMap(myCmd),
        next_object_id: 1000,
        next_timestamp: 2,
        format_config: buildCommanderFormatConfig(),
        command_zone: [myCmd.id],
        commander_damage: [],
        derived: {
          commander_damage_by_attacker: {
            // Damage exists, but it's against P0 — should be invisible on P1's panel.
            "1": [{ victim: 0, commander: myCmd.id, damage: 4 }],
          },
        },
      }),
    });

    render(<CommanderDamage playerId={1} />);

    expect(screen.queryByTestId("commander-damage-1")).not.toBeInTheDocument();
  });
});
