import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { AttackTarget, GameObject, GameState, ObjectId } from "../../../adapter/types.ts";
import { AttackTargetPicker } from "../AttackTargetPicker.tsx";
import { useGameStore } from "../../../stores/gameStore.ts";
import { useMultiplayerStore } from "../../../stores/multiplayerStore.ts";

const P1: AttackTarget = { type: "Player", data: 1 };
const P2: AttackTarget = { type: "Player", data: 2 };
const TARGETS: AttackTarget[] = [P1, P2];
const ATTACKERS: ObjectId[] = [101, 102, 103];

function makeObject(id: ObjectId, name: string): GameObject {
  return {
    id,
    card_id: 1,
    owner: 0,
    controller: 0,
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
    power: 1,
    toughness: 1,
    loyalty: null,
    card_types: { supertypes: [], core_types: ["Creature"], subtypes: [] },
    mana_cost: { type: "NoCost" },
    keywords: [],
    abilities: [],
    trigger_definitions: [],
    replacement_definitions: [],
    static_definitions: [],
    color: ["Red"],
    base_power: 1,
    base_toughness: 1,
    base_keywords: [],
    base_color: ["Red"],
    timestamp: 1,
    entered_battlefield_turn: 1,
  };
}

function makeState(): GameState {
  return {
    seat_order: [0, 1, 2],
    objects: {
      101: makeObject(101, "Goblin"),
      102: makeObject(102, "Goblin"),
      103: makeObject(103, "Goblin"),
    },
  } as unknown as GameState;
}

function renderPicker() {
  const onConfirm = vi.fn();
  const onCancel = vi.fn();
  render(
    <AttackTargetPicker
      validTargets={TARGETS}
      selectedAttackers={ATTACKERS}
      onConfirm={onConfirm}
      onCancel={onCancel}
    />,
  );
  return { onConfirm, onCancel };
}

function enterDistribute() {
  fireEvent.click(screen.getByRole("button", { name: "Distribute" }));
}

describe("AttackTargetPicker", () => {
  beforeEach(() => {
    // Opponents fall back to "Opp N" labels with an empty name map.
    useMultiplayerStore.setState({ activePlayerId: 0, playerNames: new Map() });
    useGameStore.setState({ gameState: makeState() });
  });

  afterEach(() => cleanup());

  it("keeps Attack All mode working (one click sends every attacker to a target)", () => {
    const { onConfirm } = renderPicker();
    fireEvent.click(screen.getByRole("button", { name: /Attack Opp 2 with 3 creatures/ }));
    expect(onConfirm).toHaveBeenCalledWith([
      [101, P1],
      [102, P1],
      [103, P1],
    ]);
  });

  it("disables Confirm until Unassigned is empty, then even-splits across targets", () => {
    const { onConfirm } = renderPicker();
    enterDistribute();

    // Everything starts Unassigned → Confirm is gated.
    const gated = screen.getByRole("button", { name: /Assign 3 more/ });
    expect(gated).toBeDisabled();

    // Even split of 3 across 2 targets → 2 to the first, 1 to the second.
    fireEvent.click(screen.getByRole("button", { name: "Even Split All" }));

    const confirm = screen.getByRole("button", { name: /Declare 3 Attackers/ });
    expect(confirm).not.toBeDisabled();
    fireEvent.click(confirm);

    expect(onConfirm).toHaveBeenCalledTimes(1);
    expect(onConfirm).toHaveBeenCalledWith([
      [101, P1],
      [102, P1],
      [103, P2],
    ]);
  });

  it("steppers claim the lowest-id unassigned member deterministically", () => {
    const { onConfirm } = renderPicker();
    enterDistribute();

    fireEvent.click(screen.getByRole("button", { name: "Assign one to Opp 2" }));
    fireEvent.click(screen.getByRole("button", { name: "Assign one to Opp 2" }));
    fireEvent.click(screen.getByRole("button", { name: "Assign one to Opp 3" }));

    fireEvent.click(screen.getByRole("button", { name: /Declare 3 Attackers/ }));
    expect(onConfirm).toHaveBeenCalledWith([
      [101, P1],
      [102, P1],
      [103, P2],
    ]);
  });

  it("'-1' releases the highest-id member back to Unassigned", () => {
    const { onConfirm } = renderPicker();
    enterDistribute();

    // Send the whole stack to Opp 2, then pull one back and place it on Opp 3.
    fireEvent.click(screen.getByRole("button", { name: "Send all to Opp 2" }));
    fireEvent.click(screen.getByRole("button", { name: "Remove one from Opp 2" }));
    fireEvent.click(screen.getByRole("button", { name: "Assign one to Opp 3" }));

    fireEvent.click(screen.getByRole("button", { name: /Declare 3 Attackers/ }));
    expect(onConfirm).toHaveBeenCalledWith([
      [101, P1],
      [102, P1],
      [103, P2],
    ]);
  });

  it("'send all to target' assigns the whole stack at once", () => {
    const { onConfirm } = renderPicker();
    enterDistribute();

    fireEvent.click(screen.getByRole("button", { name: "Send all to Opp 2" }));
    fireEvent.click(screen.getByRole("button", { name: /Declare 3 Attackers/ }));

    expect(onConfirm).toHaveBeenCalledWith([
      [101, P1],
      [102, P1],
      [103, P1],
    ]);
  });
});
