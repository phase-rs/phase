import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { GameObject, GameState } from "../../../adapter/types.ts";
import { dispatchAction } from "../../../game/dispatch.ts";
import { useGameStore } from "../../../stores/gameStore.ts";
import { DamageAssignmentModal } from "../DamageAssignmentModal.tsx";

vi.mock("../../../game/dispatch.ts", () => ({
  dispatchAction: vi.fn(),
}));

function creature(id: number, name: string, power: number, toughness: number): GameObject {
  return {
    id,
    card_id: id,
    owner: id === 10 ? 0 : 1,
    controller: id === 10 ? 0 : 1,
    zone: "Battlefield",
    tapped: false,
    face_down: false,
    flipped: false,
    transformed: false,
    damage_marked: 0,
    dealt_deathtouch_damage: false,
    attached_to: null,
    attachments: [],
    counters: {},
    name,
    power,
    toughness,
    loyalty: null,
    card_types: { supertypes: [], core_types: ["Creature"], subtypes: [] },
    mana_cost: { type: "Cost", shards: [], generic: 0 },
    keywords: [],
    abilities: [],
    trigger_definitions: [],
    replacement_definitions: [],
    static_definitions: [],
    color: [],
    base_power: power,
    base_toughness: toughness,
    base_keywords: [],
    base_color: [],
    timestamp: 1,
    entered_battlefield_turn: 1,
  };
}

function setCombatObjects() {
  useGameStore.setState({
    gameState: {
      objects: {
        10: creature(10, "Rampager", 4, 4),
        20: creature(20, "Guard A", 3, 3),
        21: creature(21, "Guard B", 3, 3),
      },
    } as unknown as GameState,
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
