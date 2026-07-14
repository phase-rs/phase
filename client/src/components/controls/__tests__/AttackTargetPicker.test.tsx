import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { act } from "react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { AttackTarget, CombatRequirement, ObjectId } from "../../../adapter/types.ts";
import { AttackTargetPicker } from "../AttackTargetPicker.tsx";
import { useGameStore } from "../../../stores/gameStore.ts";
import { useMultiplayerStore } from "../../../stores/multiplayerStore.ts";
import {
  buildGameObjectWithCoreTypes,
  buildObjectMap,
} from "../../../test/factories/gameObjectFactory.ts";
import { buildGameState, buildPlayers } from "../../../test/factories/gameStateFactory.ts";

const P1: AttackTarget = { type: "Player", data: 1 };
const P2: AttackTarget = { type: "Player", data: 2 };
const TARGETS: AttackTarget[] = [P1, P2];
const ATTACKERS: ObjectId[] = [101, 102, 103];

function makeCreature(id: ObjectId, name: string) {
  return buildGameObjectWithCoreTypes(["Creature"], {
    id,
    name,
    color: ["Red"],
    base_color: ["Red"],
    power: 1,
    toughness: 1,
    base_power: 1,
    base_toughness: 1,
  });
}

function makeState() {
  return buildGameState({
    players: buildPlayers([0, 1, 2]),
    seat_order: [0, 1, 2],
    objects: buildObjectMap(
      makeCreature(101, "Goblin"),
      makeCreature(102, "Goblin"),
      makeCreature(103, "Goblin"),
    ),
  });
}

function makeMixedState() {
  return buildGameState({
    players: buildPlayers([0, 1, 2]),
    seat_order: [0, 1, 2],
    objects: buildObjectMap(
      makeCreature(101, "Goblin"),
      makeCreature(102, "Elf"),
      makeCreature(103, "Dragon"),
    ),
  });
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
    fireEvent.click(screen.getByRole("button", { name: /Attack Opp 2 \(Player — 20 life\) with 3 creatures/ }));
    expect(onConfirm).toHaveBeenCalledWith([
      [101, P1],
      [102, P1],
      [103, P1],
    ]);
  });

  it("updates player names after mount while preserving the live life total", () => {
    const planeswalker = buildGameObjectWithCoreTypes(["Planeswalker"], {
      id: 201,
      name: "Jace",
      card_types: { supertypes: [], core_types: ["Planeswalker"], subtypes: [] },
    });
    const battle = buildGameObjectWithCoreTypes(["Battle"], {
      id: 202,
      name: "Jace",
      card_types: { supertypes: [], core_types: ["Battle"], subtypes: [] },
    });
    const state = makeState();
    state.objects = { ...state.objects, 201: planeswalker, 202: battle };
    state.players = state.players.map((player) =>
      player.id === 1 ? { ...player, life: 37 } : player,
    );
    useGameStore.setState({ gameState: state });
    render(
      <AttackTargetPicker
        validTargets={[P1, { type: "Planeswalker", data: 201 }, { type: "Battle", data: 202 }]}
        selectedAttackers={ATTACKERS}
        onConfirm={vi.fn()}
        onCancel={vi.fn()}
      />,
    );

    expect(screen.getByRole("button", { name: /Opp 2 \(Player — 37 life\)/ })).toBeInTheDocument();
    act(() => {
      useMultiplayerStore.setState({ activePlayerId: 0, playerNames: new Map([[1, "Jace"]]) });
    });
    expect(screen.getByRole("button", { name: /Jace \(Player — 37 life\)/ })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: /Jace \(Planeswalker\)/ })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: /Jace \(Battle\)/ })).toBeInTheDocument();

    act(() => {
      useGameStore.setState({
        gameState: { ...state, players: state.players.map((player) => player.id === 1 ? { ...player, life: 12 } : player) },
      });
    });
    expect(screen.getByRole("button", { name: /Jace \(Player — 12 life\)/ })).toBeInTheDocument();
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

  it("even-splits all attackers globally instead of front-loading each singleton stack", () => {
    useGameStore.setState({ gameState: makeMixedState() });
    const { onConfirm } = renderPicker();
    enterDistribute();

    fireEvent.click(screen.getByRole("button", { name: "Even Split All" }));
    fireEvent.click(screen.getByRole("button", { name: /Declare 3 Attackers/ }));

    expect(onConfirm).toHaveBeenCalledWith([
      [101, P1],
      [102, P1],
      [103, P2],
    ]);
  });

  it("steppers claim the lowest-id unassigned member deterministically", () => {
    const { onConfirm } = renderPicker();
    enterDistribute();

    fireEvent.click(screen.getByRole("button", { name: /Assign one to Opp 2/ }));
    fireEvent.click(screen.getByRole("button", { name: /Assign one to Opp 2/ }));
    fireEvent.click(screen.getByRole("button", { name: /Assign one to Opp 3/ }));

    fireEvent.click(screen.getByRole("button", { name: /Declare 3 Attackers/ }));
    expect(onConfirm).toHaveBeenCalledWith([
      [101, P1],
      [102, P1],
      [103, P2],
    ]);
  });

  it("shows a compact life projection and lethal state for each assigned opponent", () => {
    const state = makeState();
    state.players = state.players.map((player) =>
      player.id === 1 ? { ...player, life: 2 } : player,
    );
    useGameStore.setState({ gameState: state });
    renderPicker();
    enterDistribute();

    fireEvent.click(screen.getByRole("button", { name: /Assign one to Opp 2/ }));
    expect(screen.getAllByText("Opp 2 (Player — 2 → 1)").length).toBeGreaterThan(0);

    fireEvent.click(screen.getByRole("button", { name: /Assign one to Opp 2/ }));
    expect(screen.getAllByText("Opp 2 (Player — 2 → 0 · Lethal)").length).toBeGreaterThan(0);
  });

  it("'-1' releases the highest-id member back to Unassigned", () => {
    const { onConfirm } = renderPicker();
    enterDistribute();

    // Send the whole stack to Opp 2, then pull one back and place it on Opp 3.
    fireEvent.click(screen.getByRole("button", { name: /Send all to Opp 2/ }));
    fireEvent.click(screen.getByRole("button", { name: /Remove one from Opp 2/ }));
    fireEvent.click(screen.getByRole("button", { name: /Assign one to Opp 3/ }));

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

    fireEvent.click(screen.getByRole("button", { name: /Send all to Opp 2/ }));
    fireEvent.click(screen.getByRole("button", { name: /Declare 3 Attackers/ }));

    expect(onConfirm).toHaveBeenCalledWith([
      [101, P1],
      [102, P1],
      [103, P1],
    ]);
  });
});

// Engine parity for the MustAttackPlayer lure (CR 508.1d): a constrained
// creature must be aimed *directly* at a required player, and the picker must
// gate Confirm exactly the way the engine's declare-attackers validator does.
// `P1` = player id 1 (labeled "Opp 2"); `P2` = player id 2 (labeled "Opp 3").
describe("AttackTargetPicker — MustAttackPlayer gating", () => {
  // Single constrained attacker so the per-target stepper/button labels are
  // unambiguous (identical labels repeat once per stack otherwise).
  const LURED: ObjectId[] = [101];

  function renderLured(constraints: Record<string, CombatRequirement>) {
    const onConfirm = vi.fn();
    const onCancel = vi.fn();
    render(
      <AttackTargetPicker
        validTargets={TARGETS}
        selectedAttackers={LURED}
        attackerConstraints={constraints}
        onConfirm={onConfirm}
        onCancel={onCancel}
      />,
    );
    return { onConfirm, onCancel };
  }

  beforeEach(() => {
    useMultiplayerStore.setState({ activePlayerId: 0, playerNames: new Map() });
    // 101 is "Goblin" — the message names it.
    useGameStore.setState({ gameState: makeMixedState() });
  });

  afterEach(() => cleanup());

  it("distribute: a lured creature on the wrong player blocks Confirm, and the right player enables it", () => {
    // Must attack player 2 ("Opp 3").
    const { onConfirm } = renderLured({ "101": { kind: "MustAttack", players: [2] } });
    enterDistribute();

    // Aim it at Opp 2 (player 1) — fully assigned but the WRONG player.
    fireEvent.click(screen.getByRole("button", { name: /Send all to Opp 2/ }));

    // Discriminating: without the gate this would be enabled (nothing is
    // unassigned). The engine would reject the P1 target, so Confirm must block.
    const confirm = screen.getByRole("button", { name: /Declare 1 Attacker/ });
    expect(confirm).toBeDisabled();
    expect(screen.getByText("Goblin must attack Opp 3 (Player — 20 life)")).toBeInTheDocument();

    // Re-aim at the required player (Opp 3) — now legal.
    fireEvent.click(screen.getByRole("button", { name: /Send all to Opp 3/ }));
    const enabled = screen.getByRole("button", { name: /Declare 1 Attacker/ });
    expect(enabled).not.toBeDisabled();

    fireEvent.click(enabled);
    expect(onConfirm).toHaveBeenCalledWith([[101, P2]]);
  });

  it("distribute: MustAttack with an empty players list imposes no target restriction", () => {
    // Generic must-attack / goad — any target is legal.
    const { onConfirm } = renderLured({ "101": { kind: "MustAttack", players: [] } });
    enterDistribute();

    fireEvent.click(screen.getByRole("button", { name: /Send all to Opp 2/ }));
    const confirm = screen.getByRole("button", { name: /Declare 1 Attacker/ });
    expect(confirm).not.toBeDisabled();

    fireEvent.click(confirm);
    expect(onConfirm).toHaveBeenCalledWith([[101, P1]]);
  });

  it("attack-all: the disallowed target is disabled while the required player stays clickable", () => {
    const { onConfirm } = renderLured({ "101": { kind: "MustAttack", players: [2] } });

    // "Attack All" would send 101 to a single target; the required-player check
    // disables the Opp 2 button (engine would reject) but not Opp 3.
    const wrong = screen.getByRole("button", { name: /Attack Opp 2 \(Player — 20 life\) with 1 creature/ });
    expect(wrong).toBeDisabled();

    const right = screen.getByRole("button", { name: /Attack Opp 3 \(Player — 20 life\) with 1 creature/ });
    expect(right).not.toBeDisabled();

    fireEvent.click(right);
    expect(onConfirm).toHaveBeenCalledWith([[101, P2]]);
  });
});
