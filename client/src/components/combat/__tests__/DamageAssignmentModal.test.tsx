import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { GameObject } from "../../../adapter/types.ts";
import { dispatchAction } from "../../../game/dispatch.ts";
import { useGameStore } from "../../../stores/gameStore.ts";
import { buildGameObjectWithCoreTypes, buildObjectMap } from "../../../test/factories/gameObjectFactory.ts";
import { buildGameState } from "../../../test/factories/gameStateFactory.ts";
import { DamageAssignmentModal } from "../DamageAssignmentModal.tsx";

vi.mock("../../../game/dispatch.ts", () => ({
  dispatchAction: vi.fn(),
}));

function creature(id: number, name: string, power: number, toughness: number): GameObject {
  return buildGameObjectWithCoreTypes(["Creature"], {
    id,
    card_id: id,
    owner: id === 10 ? 0 : 1,
    controller: id === 10 ? 0 : 1,
    zone: "Battlefield",
    name,
    power,
    toughness,
    mana_cost: { type: "Cost", shards: [], generic: 0 },
    base_power: power,
    base_toughness: toughness,
    timestamp: 1,
    entered_battlefield_turn: 1,
  });
}

function setCombatObjects() {
  const objects = [
    creature(10, "Rampager", 4, 4),
    creature(20, "Guard A", 3, 3),
    creature(21, "Guard B", 3, 3),
  ];
  useGameStore.setState({
    gameState: buildGameState({ objects: buildObjectMap(...objects) }),
  });
}

describe("DamageAssignmentModal", () => {
  beforeEach(() => {
    vi.mocked(dispatchAction).mockReset();
    vi.mocked(dispatchAction).mockResolvedValue(undefined);
    setCombatObjects();
  });

  afterEach(() => {
    cleanup();
    useGameStore.setState({ gameState: null });
  });

  it("allows a trampler with insufficient damage for all blockers to assign no excess", () => {
    render(
      <DamageAssignmentModal
        data={{
          player: 0,
          attacker_id: 10,
          total_damage: 4,
          blockers: [
            { blocker_id: 20, lethal_minimum: 3 },
            { blocker_id: 21, lethal_minimum: 3 },
          ],
          trample: "Standard",
          defending_player: 1,
          attack_target: { type: "Player", data: 1 },
        }}
      />,
    );

    const assignButton = screen.getByRole("button", { name: "Assign Damage" });
    const incrementButtons = screen.getAllByRole("button", { name: "+" });

    expect(assignButton).toBeDisabled();

    fireEvent.click(incrementButtons[0]);
    fireEvent.click(incrementButtons[0]);
    fireEvent.click(incrementButtons[1]);
    fireEvent.click(incrementButtons[1]);

    expect(assignButton).toBeEnabled();

    fireEvent.click(assignButton);

    expect(dispatchAction).toHaveBeenCalledWith({
      type: "AssignCombatDamage",
      data: {
        assignments: [
          [20, 2],
          [21, 2],
        ],
        trample_damage: 0,
        controller_damage: 0,
      },
    });
  });
});
