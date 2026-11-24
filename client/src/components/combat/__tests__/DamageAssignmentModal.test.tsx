import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { CombatDamageSplit, GameObject, WaitingFor } from "../../../adapter/types.ts";
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

type DamagePrompt = Extract<WaitingFor, { type: "AssignCombatDamage" }>["data"];

const playerTarget = { type: "Player", data: 1 } as const;
const planeswalkerTarget = { type: "Planeswalker", data: 30 } as const;
const battleTarget = { type: "Battle", data: 31 } as const;

const lethalFirstTitle = "Assign lethal damage to each blocker in order, then the rest";
const standardSuggestion: CombatDamageSplit = {
  assignments: [[20, 2], [21, 3]],
  trample_damage: 5,
  controller_damage: 0,
};

function renderDamagePrompt(
  overrides: Partial<DamagePrompt> = {},
  suggestion?: CombatDamageSplit,
) {
  const data: DamagePrompt = {
    player: 0,
    attacker_id: 10,
    total_damage: 10,
    blockers: [
      { blocker_id: 20, lethal_minimum: 2 },
      { blocker_id: 21, lethal_minimum: 3 },
    ],
    trample: "Standard",
    defending_player: 1,
    attack_target: playerTarget,
    ...overrides,
  };
  useGameStore.setState({
    gameState: buildGameState({
      objects: buildObjectMap(
        creature(10, "Rampager", data.total_damage, 4),
        creature(20, "Guard A", 3, 3),
        creature(21, "Guard B", 3, 3),
        buildGameObjectWithCoreTypes(["Planeswalker"], { id: 30, name: "Jace Beleren" }),
        buildGameObjectWithCoreTypes(["Battle"], { id: 31, name: "Invasion of Zendikar" }),
      ),
      derived: {
        attack_targets: { [data.attacker_id]: data.attack_target },
        combat_damage_suggestion: suggestion,
      },
    }),
  });
  render(<DamageAssignmentModal data={data} />);
}

function button(name: string) {
  return screen.getByRole("button", { name });
}

function confirmDamage(assignments: [number, number][], trample = 0, controller = 0) {
  const confirm = button("Assign Damage");
  expect(confirm).toBeEnabled();
  fireEvent.click(confirm);
  expect(dispatchAction).toHaveBeenCalledExactlyOnceWith({
    type: "AssignCombatDamage",
    data: { assignments, trample_damage: trample, controller_damage: controller },
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

  describe("engine-suggested damage", () => {
    it("applies the Standard trample suggestion exactly", () => {
      renderDamagePrompt({}, standardSuggestion);
      fireEvent.click(button(lethalFirstTitle));
      confirmDamage(standardSuggestion.assignments, standardSuggestion.trample_damage, standardSuggestion.controller_damage);
    });

    it("applies the trample-over-planeswalkers suggestion exactly", () => {
      const suggestion: CombatDamageSplit = {
        assignments: [[20, 2], [21, 3]],
        trample_damage: 3,
        controller_damage: 2,
      };
      renderDamagePrompt({
        attack_target: planeswalkerTarget,
        trample: "OverPlaneswalkers",
        pw_loyalty: 3,
        pw_controller: 1,
      }, suggestion);
      fireEvent.click(button(lethalFirstTitle));
      confirmDamage(suggestion.assignments, suggestion.trample_damage, suggestion.controller_damage);
    });

    it("replaces a conflicting draft and is idempotent", () => {
      renderDamagePrompt({
        attack_target: planeswalkerTarget,
        trample: "OverPlaneswalkers",
        pw_loyalty: 8,
        pw_controller: 1,
      }, standardSuggestion);
      const [guardA, , planeswalker, controller] = screen.getAllByRole("button", { name: "+" });
      for (const increment of [guardA, guardA, guardA, planeswalker, controller]) {
        fireEvent.click(increment);
      }

      const fill = button(lethalFirstTitle);
      const clearController = button("Clear damage assigned to PW Controller (Trample Over PW)");
      expect(clearController).toBeEnabled();
      expect(button("Assign Damage")).toBeDisabled();
      fireEvent.click(fill);
      expect(button("Assign Damage")).toBeEnabled();
      expect(clearController).toBeDisabled();
      fireEvent.click(fill);
      confirmDamage(standardSuggestion.assignments, standardSuggestion.trample_damage, standardSuggestion.controller_damage);
    });

    it("matches suggestion entries to blocker IDs regardless of entry order", () => {
      renderDamagePrompt({}, {
        ...standardSuggestion,
        assignments: [[21, 3], [20, 2]],
      });
      fireEvent.click(button(lethalFirstTitle));
      confirmDamage([[20, 2], [21, 3]], 5);
    });

    it("offers a touch-sized shortcut with a destination-neutral label and title", () => {
      renderDamagePrompt({}, standardSuggestion);
      const fill = button(lethalFirstTitle);
      expect(fill).toHaveTextContent("Lethal first");
      expect(fill).toHaveAttribute("title", lethalFirstTitle);
      expect(fill).toHaveClass("!min-h-11", "min-w-11");
    });

    it("hides an empty suggestion for a zero-blocker prompt without trample", () => {
      renderDamagePrompt({ blockers: [], trample: null, total_damage: 3 }, {
        assignments: [],
        trample_damage: 0,
        controller_damage: 0,
      });
      expect(screen.queryByRole("button", { name: lethalFirstTitle })).not.toBeInTheDocument();
    });

    it("hides controller spillover when the prompt still requires planeswalker loyalty", () => {
      renderDamagePrompt({
        attack_target: planeswalkerTarget,
        trample: "OverPlaneswalkers",
        pw_loyalty: 3,
        pw_controller: 1,
      }, {
        assignments: [[20, 2], [21, 3]],
        trample_damage: 0,
        controller_damage: 5,
      });
      expect(screen.queryByRole("button", { name: lethalFirstTitle })).not.toBeInTheDocument();
    });

    it("hides the shortcut when the engine sends no suggestion", () => {
      renderDamagePrompt();
      expect(screen.queryByText("Lethal first")).not.toBeInTheDocument();
      expect(screen.queryByRole("button", { name: lethalFirstTitle })).not.toBeInTheDocument();
    });
  });

  describe("row shortcuts", () => {
    it.each([
      { target: planeswalkerTarget, name: "Jace Beleren" },
      { target: battleTarget, name: "Invasion of Zendikar" },
      { target: playerTarget, name: "Defending Player" },
    ])("names the live $target.type in the Standard trample row and shortcuts", ({ target, name }) => {
      renderDamagePrompt({ attack_target: target });
      const label = `${name} (Trample)`;

      expect(screen.getByText(label)).toBeInTheDocument();
      expect(button(`Clear damage assigned to ${label}`)).toBeInTheDocument();
      expect(button(`Assign the remaining 10 damage to ${label}`)).toBeInTheDocument();
    });

    it("uses the lethal minimum, caps at remaining damage and disables lethal when satisfied or exhausted", () => {
      renderDamagePrompt({
        total_damage: 4,
        blockers: [
          { blocker_id: 20, lethal_minimum: 2 },
          { blocker_id: 21, lethal_minimum: 3 },
          { blocker_id: 22, lethal_minimum: 1 },
          { blocker_id: 23, lethal_minimum: 0 },
        ],
      });
      const lethalA = button("Assign lethal damage to Guard A");
      const lethalB = button("Assign lethal damage to Guard B");
      const lethalC = button("Assign lethal damage to Object 22");
      expect(button("Assign lethal damage to Object 23")).toBeDisabled();
      fireEvent.click(lethalA);
      expect(lethalA).toBeDisabled();
      fireEvent.click(lethalB);
      expect(lethalB).toBeDisabled();
      expect(lethalC).toBeDisabled();
      confirmDamage([[20, 2], [21, 2], [22, 0], [23, 0]]);
    });

    it("keeps an above-lethal value and clears another blocker row", () => {
      renderDamagePrompt({ total_damage: 5 });
      const [guardA, guardB] = screen.getAllByRole("button", { name: "+" });
      const clearB = button("Clear damage assigned to Guard B");
      expect(clearB).toBeDisabled();
      fireEvent.click(guardB);
      for (let i = 0; i < 3; i++) fireEvent.click(guardA);
      const lethalA = button("Assign lethal damage to Guard A");
      expect(lethalA).toBeDisabled();
      fireEvent.click(lethalA);
      fireEvent.click(clearB);
      expect(clearB).toBeDisabled();
      fireEvent.click(button("Assign the remaining 2 damage to Guard A"));
      confirmDamage([[20, 5], [21, 0]]);
    });

    it("clears the trample and controller rows", () => {
      renderDamagePrompt({
        attack_target: planeswalkerTarget,
        trample: "OverPlaneswalkers",
        pw_loyalty: 3,
        pw_controller: 1,
      });
      fireEvent.click(button("Assign lethal damage to Guard A"));
      fireEvent.click(button("Assign lethal damage to Guard B"));
      const [, , planeswalker] = screen.getAllByRole("button", { name: "+" });
      for (let i = 0; i < 3; i++) fireEvent.click(planeswalker);
      fireEvent.click(button("Assign the remaining 2 damage to PW Controller (Trample Over PW)"));
      const clearTrample = button("Clear damage assigned to Planeswalker (loyalty: 3)");
      const clearController = button("Clear damage assigned to PW Controller (Trample Over PW)");
      fireEvent.click(clearTrample);
      expect(clearTrample).toBeDisabled();
      fireEvent.click(clearController);
      expect(clearController).toBeDisabled();
      fireEvent.click(button("Assign the remaining 5 damage to Guard B"));
      confirmDamage([[20, 2], [21, 8]]);
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

    it("clears a banding assignment before moving the damage to another attacker", () => {
      render(
        <BlockerDamageAssignmentModal
          data={{ player: 0, blocker_id: 10, total_damage: 3, attackers: [20, 21] }}
        />,
      );
      fireEvent.click(button("Assign the remaining 3 damage to Guard A"));
      const clear = button("Clear damage assigned to Guard A");
      fireEvent.click(clear);
      expect(clear).toBeDisabled();
      fireEvent.click(button("Assign the remaining 3 damage to Guard B"));
      fireEvent.click(button("Assign Damage"));
      expect(dispatchAction).toHaveBeenCalledExactlyOnceWith({
        type: "AssignBlockerDamage",
        data: { assignments: [[20, 0], [21, 3]] },
      });
    });
  });
});
