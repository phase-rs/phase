import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { GameObject, WaitingFor } from "../../../adapter/types.ts";
import { useGameStore } from "../../../stores/gameStore.ts";
import { useMultiplayerStore } from "../../../stores/multiplayerStore.ts";
import { buildGameObject } from "../../../test/factories/gameObjectFactory.ts";
import { buildGameState } from "../../../test/factories/gameStateFactory.ts";
import { CardChoiceModal } from "../CardChoiceModal.tsx";

const dispatchMock = vi.fn();

vi.mock("../../../hooks/useGameDispatch.ts", () => ({
  useGameDispatch: () => dispatchMock,
}));

function makeCreature(id: number, name: string): GameObject {
  return buildGameObject({
    id,
    card_id: id,
    zone: "Battlefield",
    tapped: true,
    name,
    power: 2,
    toughness: 2,
    card_types: { supertypes: [], core_types: ["Creature"], subtypes: [] },
    mana_cost: { type: "Cost", shards: [], generic: 1 },
    base_power: 2,
    base_toughness: 2,
    timestamp: id,
  });
}

function makeState(waitingFor: WaitingFor, objects: Record<string, GameObject>) {
  return buildGameState({
    phase: "Untap",
    objects,
    next_object_id: 100,
    battlefield: Object.keys(objects).map((k) => Number(k)),
    waiting_for: waitingFor,
    next_timestamp: 2,
  });
}

function setWaitingFor(waitingFor: WaitingFor, objects: Record<string, GameObject>) {
  const state = makeState(waitingFor, objects);
  useGameStore.setState({
    gameMode: "online",
    gameState: state,
    waitingFor,
  });
}

describe("ChooseUntapSubset modal", () => {
  beforeEach(() => {
    dispatchMock.mockClear();
    useMultiplayerStore.setState({ activePlayerId: 0 });
  });

  afterEach(() => {
    cleanup();
  });

  // CR 502.3: a max-untap cap ("can't untap more than one <type>") bounds the
  // untap count from above only — choosing ZERO is legal (the whole group stays
  // tapped). The Confirm button must be enabled with an empty selection so a
  // human can decline to untap any of the capped permanents.
  it("confirms an empty selection (untap zero permanents)", () => {
    setWaitingFor(
      {
        type: "ChooseUntapSubset",
        data: { player: 0, group: [10, 11], max: 1 },
      },
      { 10: makeCreature(10, "Bear A"), 11: makeCreature(11, "Bear B") },
    );

    render(<CardChoiceModal />);

    // With nothing selected the confirm control is still actionable.
    const confirm = screen.getByRole("button", { name: /untap \(0\/1\)/i });
    expect(confirm).not.toBeDisabled();

    fireEvent.click(confirm);

    expect(dispatchMock).toHaveBeenCalledTimes(1);
    expect(dispatchMock).toHaveBeenCalledWith({
      type: "SelectCards",
      data: { cards: [] },
    });
  });

  it("confirms a bounded non-empty selection", () => {
    setWaitingFor(
      {
        type: "ChooseUntapSubset",
        data: { player: 0, group: [10, 11], max: 1 },
      },
      { 10: makeCreature(10, "Bear A"), 11: makeCreature(11, "Bear B") },
    );

    render(<CardChoiceModal />);

    // Select the first capped permanent (cap of 1 allows exactly one).
    fireEvent.click(screen.getByRole("button", { name: /Bear A/i }));

    const confirm = screen.getByRole("button", { name: /untap \(1\/1\)/i });
    expect(confirm).not.toBeDisabled();
    fireEvent.click(confirm);

    expect(dispatchMock).toHaveBeenCalledWith({
      type: "SelectCards",
      data: { cards: [10] },
    });
  });
});
