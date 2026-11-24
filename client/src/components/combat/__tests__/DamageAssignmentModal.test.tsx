import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { GameObject, WaitingFor } from "../../../adapter/types.ts";
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

function renderDamagePrompt(overrides: Partial<DamagePrompt> = {}, live = true) {
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
      ),
      derived: { attack_targets: live ? { [data.attacker_id]: data.attack_target } : {} },
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

  describe("lethal preset", () => {
    it.each([
      { target: playerTarget, trample: "Standard" },
      { target: playerTarget, trample: "OverPlaneswalkers" },
      { target: planeswalkerTarget, trample: "Standard" },
      { target: battleTarget, trample: "Standard" },
      { target: battleTarget, trample: "OverPlaneswalkers" },
    ] as const)("assigns 2 and 3 to blockers and 5 over a live $target.type ($trample)", ({ target, trample }) => {
      renderDamagePrompt({ attack_target: target, trample });
      fireEvent.click(button("Lethal to blockers, rest tramples over"));
      confirmDamage([[20, 2], [21, 3]], 5);
    });

    it.each([
      { title: "insufficient damage", total: 4, minimums: [3, 3], shares: [3, 1], excess: 0 },
      { title: "zero lethal", total: 10, minimums: [0, 3], shares: [0, 3], excess: 7 },
      { title: "deathtouch-style lethal", total: 10, minimums: [1, 3], shares: [1, 3], excess: 6 },
    ])("uses the prompt's lethal minima in order for $title", ({ total, minimums, shares, excess }) => {
      renderDamagePrompt({
        total_damage: total,
        blockers: [
          { blocker_id: 20, lethal_minimum: minimums[0] },
          { blocker_id: 21, lethal_minimum: minimums[1] },
        ],
      });
      fireEvent.click(button(excess > 0 ? "Lethal to blockers, rest tramples over" : "Lethal to blockers"));
      confirmDamage([[20, shares[0]], [21, shares[1]]], excess);
    });

    it.each([
      { loyalty: 3, trampleDamage: 3, controllerDamage: 2 },
      { loyalty: 0, trampleDamage: 0, controllerDamage: 5 },
      { loyalty: 8, trampleDamage: 5, controllerDamage: 0 },
    ])("splits excess with planeswalker loyalty $loyalty", ({ loyalty, trampleDamage, controllerDamage }) => {
      renderDamagePrompt({
        attack_target: planeswalkerTarget,
        trample: "OverPlaneswalkers",
        pw_loyalty: loyalty,
        pw_controller: 1,
      });
      fireEvent.click(button("Lethal to blockers, rest tramples over"));
      confirmDamage([[20, 2], [21, 3]], trampleDamage, controllerDamage);
    });

    it("replaces a conflicting draft, clears stale controller damage and is idempotent", () => {
      renderDamagePrompt({
        attack_target: planeswalkerTarget,
        trample: "OverPlaneswalkers",
        pw_loyalty: 8,
        pw_controller: 1,
      });
      const [guardA, , planeswalker, controller] = screen.getAllByRole("button", { name: "+" });
      for (const increment of [guardA, planeswalker, controller]) fireEvent.click(increment);
      const preset = button("Lethal to blockers, rest tramples over");
      fireEvent.click(preset);
      expect(button("Assign Damage")).toBeEnabled();
      expect(button("Clear damage assigned to PW Controller (Trample Over PW)")).toBeDisabled();
      fireEvent.click(preset);
      confirmDamage([[20, 2], [21, 3]], 5);
    });

    it.each([
      { title: "a nontrampler", attackTarget: playerTarget, trample: null, live: true },
      { title: "a departed planeswalker", attackTarget: planeswalkerTarget, trample: "Standard", live: false },
      { title: "a departed battle", attackTarget: battleTarget, trample: "Standard", live: false },
    ] as const)("leaves excess unassigned for $title", ({ attackTarget, trample, live }) => {
      renderDamagePrompt({ attack_target: attackTarget, trample }, live);
      fireEvent.click(button("Lethal to blockers"));
      expect(button("Assign Damage")).toBeDisabled();
      if (trample) expect(button("Clear damage assigned to Defending Player (Trample)")).toBeDisabled();

      fireEvent.click(button("Assign the remaining 5 damage to Guard A"));
      confirmDamage([[20, 7], [21, 3]]);
    });

    it("leaves excess unassigned once a trample-over planeswalker has left combat", () => {
      // CR 702.19e: the engine then sends no loyalty, only the original defender as controller.
      renderDamagePrompt(
        { attack_target: planeswalkerTarget, trample: "OverPlaneswalkers", pw_controller: 1 },
        false,
      );
      fireEvent.click(button("Lethal to blockers"));
      expect(button("Assign Damage")).toBeDisabled();
      expect(button("Clear damage assigned to Planeswalker (loyalty: 0)")).toBeDisabled();
      expect(button("Clear damage assigned to PW Controller (Trample Over PW)")).toBeDisabled();

      fireEvent.click(button("Assign the remaining 5 damage to Guard A"));
      confirmDamage([[20, 7], [21, 3]]);
    });

    it("disables the preset when there is no damage to assign", () => {
      renderDamagePrompt({ total_damage: 0 });
      expect(button("Lethal to blockers")).toBeDisabled();
    });
  });

  describe("row shortcuts", () => {
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
      fireEvent.click(button("Lethal to blockers, rest tramples over"));
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
