import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { GameObject, WaitingFor } from "../../../adapter/types.ts";
import { useGameStore } from "../../../stores/gameStore.ts";
import { useMultiplayerStore } from "../../../stores/multiplayerStore.ts";
import { buildGameObjectWithCoreTypes, buildObjectMap } from "../../../test/factories/gameObjectFactory.ts";
import { buildGameState, buildPlayer } from "../../../test/factories/gameStateFactory.ts";
import { CardChoiceModal } from "../CardChoiceModal.tsx";

const dispatchMock = vi.fn();

vi.mock("../../../hooks/useGameDispatch.ts", () => ({
  useGameDispatch: () => dispatchMock,
}));

function creature(id: number, name: string, power: number, toughness: number): GameObject {
  return buildGameObjectWithCoreTypes(["Creature"], {
    id,
    card_id: id,
    owner: id < 100 ? 0 : 2,
    controller: id < 100 ? 0 : 2,
    zone: "Battlefield",
    name,
    power,
    toughness,
    base_power: power,
    base_toughness: toughness,
    timestamp: id,
    entered_battlefield_turn: 1,
  });
}

/** Multi-blocker attacker prompt — a plain 2+ blocker division (CR 510.1c). */
function assignPrompt(attackerId: number, blockerA: number, blockerB: number): WaitingFor {
  return {
    type: "AssignCombatDamage",
    data: {
      player: 0,
      attacker_id: attackerId,
      total_damage: 4,
      blockers: [
        { blocker_id: blockerA, lethal_minimum: 2 },
        { blocker_id: blockerB, lethal_minimum: 2 },
      ],
      trample: null,
      defending_player: 2,
      attack_target: { type: "Player", data: 2 },
    },
  } as WaitingFor;
}

function setWaitingFor(waitingFor: WaitingFor) {
  const objects = buildObjectMap(
    creature(10, "Rampager A", 4, 4),
    creature(11, "Rampager B", 4, 4),
    creature(100, "Guard 1", 2, 2),
    creature(101, "Guard 2", 2, 2),
    creature(102, "Guard 3", 2, 2),
    creature(103, "Guard 4", 2, 2),
  );
  useGameStore.setState({
    gameMode: "online",
    gameState: buildGameState({
      players: [buildPlayer({ id: 0 }), buildPlayer({ id: 2 })],
      objects,
      waiting_for: waitingFor,
    }),
    waitingFor,
  });
}

describe("sequential combat damage prompts", () => {
  beforeEach(() => {
    dispatchMock.mockClear();
    useMultiplayerStore.setState({ activePlayerId: 0 });
  });

  afterEach(() => {
    cleanup();
    useGameStore.setState({ gameState: null, waitingFor: null });
  });

  // Repro: 10+ attackers under Stonehoof Chieftain each need a trample/division
  // prompt. After submitting the first, the engine advances to the next
  // AssignCombatDamage prompt. Without a per-prompt `key`, React reuses the modal
  // instance and its `submitted` guard stays true, so every later prompt renders
  // nothing and the game appears frozen.
  it("re-renders a fresh modal when the engine advances to the next attacker", () => {
    setWaitingFor(assignPrompt(10, 100, 101));
    render(<CardChoiceModal />);

    // First prompt: assign lethal to both blockers and submit.
    const incrementButtons = screen.getAllByRole("button", { name: "+" });
    fireEvent.click(incrementButtons[0]);
    fireEvent.click(incrementButtons[0]);
    fireEvent.click(incrementButtons[1]);
    fireEvent.click(incrementButtons[1]);
    fireEvent.click(screen.getByRole("button", { name: "Assign Damage" }));
    expect(dispatchMock).toHaveBeenCalledTimes(1);

    // Engine advances to the next attacker's prompt.
    act(() => setWaitingFor(assignPrompt(11, 102, 103)));

    // The second modal must render (not be swallowed by the stale `submitted`
    // guard of a reused instance) and must start fresh (button disabled at 0/4).
    const secondButton = screen.getByRole("button", { name: "Assign Damage" });
    expect(secondButton).toBeInTheDocument();
    expect(secondButton).toBeDisabled();
  });
});
