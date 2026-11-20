import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { GameObject } from "../../../adapter/types.ts";
import { dispatchAction } from "../../../game/dispatch.ts";
import { useGameStore } from "../../../stores/gameStore.ts";
import { buildGameObjectWithCoreTypes, buildObjectMap } from "../../../test/factories/gameObjectFactory.ts";
import { buildGameState } from "../../../test/factories/gameStateFactory.ts";
import { BlockerDamageAssignmentModal, DamageAssignmentModal } from "../DamageAssignmentModal.tsx";

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

function setObjects(...objects: GameObject[]) {
  useGameStore.setState({
    gameState: buildGameState({ objects: buildObjectMap(...objects) }),
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

  describe("assign the remaining damage", () => {
    // Colossal Dreadmaw (6/6): "Trample (This creature can deal excess combat damage to the
    // player or planeswalker it's attacking.)" blocked by Grizzly Bears (2/2).
    function renderDreadmawBlockedByBears() {
      setObjects(creature(10, "Colossal Dreadmaw", 6, 6), creature(20, "Grizzly Bears", 2, 2));
      render(
        <DamageAssignmentModal
          data={{
            player: 0,
            attacker_id: 10,
            total_damage: 6,
            blockers: [{ blocker_id: 20, lethal_minimum: 2 }],
            trample: "Standard",
            defending_player: 1,
            attack_target: { type: "Player", data: 1 },
          }}
        />,
      );
    }

    it("dumps the excess on the defending player once every blocker has lethal", () => {
      renderDreadmawBlockedByBears();

      // Reach guard: the shortcut's name tracks the live remaining amount.
      expect(
        screen.getByRole("button", { name: "Assign the remaining 6 damage to Defending Player (Trample)" }),
      ).toBeEnabled();

      const bearsIncrement = screen.getAllByRole("button", { name: "+" })[0];
      fireEvent.click(bearsIncrement);
      fireEvent.click(bearsIncrement);
      fireEvent.click(
        screen.getByRole("button", { name: "Assign the remaining 4 damage to Defending Player (Trample)" }),
      );

      // With nothing left to divide, every shortcut is disabled.
      const shortcuts = screen.getAllByRole("button", { name: /^Assign the remaining/ });
      expect(shortcuts).toHaveLength(2);
      for (const shortcut of shortcuts) expect(shortcut).toBeDisabled();

      const assignButton = screen.getByRole("button", { name: "Assign Damage" });
      expect(assignButton).toBeEnabled();
      fireEvent.click(assignButton);

      expect(dispatchAction).toHaveBeenCalledWith({
        type: "AssignCombatDamage",
        data: { assignments: [[20, 2]], trample_damage: 4, controller_damage: 0 },
      });
    });

    it("only moves points and leaves CR 702.19b lethal ordering to the gate", () => {
      renderDreadmawBlockedByBears();

      fireEvent.click(
        screen.getByRole("button", { name: "Assign the remaining 6 damage to Defending Player (Trample)" }),
      );

      const assignButton = screen.getByRole("button", { name: "Assign Damage" });
      expect(assignButton).toBeDisabled();
      fireEvent.click(assignButton);
      expect(dispatchAction).not.toHaveBeenCalled();
    });

    it("fills the planeswalker controller row under trample over planeswalkers", () => {
      // Thrasta, Tempest's Roar (7/7, "Trample, haste" / "Trample over planeswalkers")
      // attacking Jace Beleren (loyalty 3), blocked by Grizzly Bears.
      setObjects(
        creature(10, "Thrasta, Tempest's Roar", 7, 7),
        creature(20, "Grizzly Bears", 2, 2),
      );
      render(
        <DamageAssignmentModal
          data={{
            player: 0,
            attacker_id: 10,
            total_damage: 7,
            blockers: [{ blocker_id: 20, lethal_minimum: 2 }],
            trample: "OverPlaneswalkers",
            defending_player: 1,
            attack_target: { type: "Planeswalker", data: 30 },
            pw_loyalty: 3,
            pw_controller: 1,
          }}
        />,
      );

      const [bearsIncrement, planeswalkerIncrement] = screen.getAllByRole("button", { name: "+" });
      fireEvent.click(bearsIncrement);
      fireEvent.click(bearsIncrement);
      fireEvent.click(planeswalkerIncrement);
      fireEvent.click(planeswalkerIncrement);
      fireEvent.click(planeswalkerIncrement);
      fireEvent.click(
        screen.getByRole("button", { name: "Assign the remaining 2 damage to PW Controller (Trample Over PW)" }),
      );
      fireEvent.click(screen.getByRole("button", { name: "Assign Damage" }));

      expect(dispatchAction).toHaveBeenCalledWith({
        type: "AssignCombatDamage",
        data: { assignments: [[20, 2]], trample_damage: 3, controller_damage: 2 },
      });
    });

    it("is shared by the banding blocker division modal", () => {
      // CR 510.1d + CR 702.22k: Hill Giant (3/3) blocking a band of Benalish Hero
      // ("Banding (...)") and Grizzly Bears.
      setObjects(
        creature(40, "Hill Giant", 3, 3),
        creature(50, "Benalish Hero", 1, 1),
        creature(51, "Grizzly Bears", 2, 2),
      );
      render(
        <BlockerDamageAssignmentModal
          data={{ player: 0, blocker_id: 40, total_damage: 3, attackers: [50, 51] }}
        />,
      );

      fireEvent.click(
        screen.getByRole("button", { name: "Assign the remaining 3 damage to Benalish Hero" }),
      );
      fireEvent.click(screen.getByRole("button", { name: "Assign Damage" }));

      expect(dispatchAction).toHaveBeenCalledWith({
        type: "AssignBlockerDamage",
        data: {
          assignments: [
            [50, 3],
            [51, 0],
          ],
        },
      });
    });
  });
});
