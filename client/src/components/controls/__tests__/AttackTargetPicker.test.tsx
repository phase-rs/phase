import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { act } from "react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { AttackTarget, ObjectId } from "../../../adapter/types.ts";
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

// Engine-authoritative per-attacker legal targets (CR 508.1a–d): the picker is
// pure presentation over the `valid_attack_targets_by_attacker` map — it offers
// only engine-provided legal targets and does NO client-side legality. `P1` =
// player id 1 (labeled "Opp 2"); `P2` = player id 2 (labeled "Opp 3").
describe("AttackTargetPicker — per-attacker legal targets", () => {
  function renderWithMap(
    selected: ObjectId[],
    byAttacker: Record<string, AttackTarget[]>,
    aggregate: AttackTarget[] = TARGETS,
  ) {
    const onConfirm = vi.fn();
    const onCancel = vi.fn();
    render(
      <AttackTargetPicker
        validTargets={aggregate}
        validTargetsByAttacker={byAttacker}
        selectedAttackers={selected}
        onConfirm={onConfirm}
        onCancel={onCancel}
      />,
    );
    return { onConfirm, onCancel };
  }

  beforeEach(() => {
    useMultiplayerStore.setState({ activePlayerId: 0, playerNames: new Map() });
    useGameStore.setState({ gameState: makeState() });
  });

  afterEach(() => cleanup());

  it("Attack All offers only targets EVERY selected attacker can legally attack (intersection)", () => {
    // 101 may attack both players; 102 only Opp 2 (player 1). The intersection
    // is {Opp 2}, so Opp 3 must NOT be offered as an attack-all target.
    const { onConfirm } = renderWithMap([101, 102], { "101": [P1, P2], "102": [P1] });

    const common = screen.getByRole("button", { name: /Attack Opp 2 \(Player — 20 life\) with 2 creatures/ });
    expect(common).toBeInTheDocument();
    // Discriminating: aggregate exposes Opp 3, but it is illegal for 102, so the
    // intersection hides it. Reverting to aggregate would render this button.
    expect(screen.queryByRole("button", { name: /Attack Opp 3 \(Player — 20 life\) with 2 creatures/ })).toBeNull();

    fireEvent.click(common);
    expect(onConfirm).toHaveBeenCalledWith([[101, P1], [102, P1]]);
  });

  it("Attack All shows the no-common-target hint when the selected attackers share no target", () => {
    // Disjoint legal sets → empty intersection.
    renderWithMap([101, 102], { "101": [P1], "102": [P2] });
    expect(screen.getByText("No shared target — switch to Distribute to aim each attacker.")).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: /Attack Opp 2 .* with 2 creatures/ })).toBeNull();
    expect(screen.queryByRole("button", { name: /Attack Opp 3 .* with 2 creatures/ })).toBeNull();
  });

  it("distribute: an attacker's illegal target column exposes no stepper (splits incompatible same-name stacks)", () => {
    // Three Goblins. 101/102 may attack both; 103 only Opp 2 (player 1). 103's
    // differing legal set must split it into its own stack, and Opp 3's stepper
    // must appear only for the {101,102} stack — never for 103.
    renderWithMap([101, 102, 103], { "101": [P1, P2], "102": [P1, P2], "103": [P1] });
    enterDistribute();

    // Opp 2 (P1) is legal for both stacks → two steppers (desktop matrix only;
    // the mobile accordion is collapsed). Opp 3 (P2) is legal only for the
    // {101,102} stack → exactly one stepper. Reverting the split / per-bucket
    // enforcement would render two Opp 3 steppers over the aggregate.
    expect(screen.getAllByRole("button", { name: /Assign one to Opp 2/ }).length).toBe(2);
    expect(screen.getAllByRole("button", { name: /Assign one to Opp 3/ }).length).toBe(1);
  });

  it("distribute: even-split respects each attacker's own bucket, never assigning an illegal target", () => {
    // 101 may attack both; 102 only Opp 3 (player 2). A global even split would
    // put 101→Opp 2 and 102→Opp 2, but Opp 2 is illegal for 102 — per-bucket
    // splitting must instead land 102 on its only legal target (Opp 3).
    const { onConfirm } = renderWithMap([101, 102], { "101": [P1, P2], "102": [P2] });
    enterDistribute();

    fireEvent.click(screen.getByRole("button", { name: "Even Split All" }));
    const confirm = screen.getByRole("button", { name: /Declare 2 Attackers/ });
    expect(confirm).not.toBeDisabled();
    fireEvent.click(confirm);

    expect(onConfirm).toHaveBeenCalledWith([[101, P1], [102, P2]]);
  });

  it("legacy payload (no per-attacker map) falls back to the aggregate for every attacker", () => {
    // With no map the picker treats the aggregate list as each attacker's legal
    // set — both targets remain offered in Attack All.
    const onConfirm = vi.fn();
    render(
      <AttackTargetPicker
        validTargets={TARGETS}
        selectedAttackers={[101, 102]}
        onConfirm={onConfirm}
        onCancel={vi.fn()}
      />,
    );
    expect(screen.getByRole("button", { name: /Attack Opp 2 \(Player — 20 life\) with 2 creatures/ })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: /Attack Opp 3 \(Player — 20 life\) with 2 creatures/ })).toBeInTheDocument();
  });
});
