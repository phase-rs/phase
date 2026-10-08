import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen } from "@testing-library/react";

import type { GameState, WaitingFor } from "../../../adapter/types";
import { isWaitingForHandled } from "../../../game/waitingForRegistry.ts";
import { useGameStore } from "../../../stores/gameStore";
import {
  buildAssistPaymentWaitingFor,
  buildGameState,
  buildManaPaymentWaitingFor,
} from "../../../test/factories/gameStateFactory.ts";
import { setGameStoreForTest } from "../../../test/helpers/gameStoreHelpers.ts";
import { AssistPaymentUI } from "../AssistPaymentUI.tsx";

function createGameState(overrides: Partial<GameState> = {}): GameState {
  return buildGameState({
    next_object_id: 100,
    waiting_for: buildManaPaymentWaitingFor(),
    has_pending_cast: true,
    ...overrides,
  });
}

// caster = 0, chosen = 0 so the local PLAYER_ID (0) is the acting helper and
// `useCanActForWaitingState` returns true (CR 702.132a routes to `chosen`).
function assistPaymentWaitingFor(max: number): WaitingFor {
  return buildAssistPaymentWaitingFor({
    data: { caster: 1, chosen: 0, max_generic: max },
  });
}

describe("AssistPaymentUI", () => {
  beforeEach(() => {
    useGameStore.getState().reset();
  });

  afterEach(() => {
    cleanup();
  });

  it("registers the waiting state as handled", () => {
    expect(isWaitingForHandled(assistPaymentWaitingFor(3))).toBe(true);
  });

  it("renders nothing when not in AssistPayment state", () => {
    setGameStoreForTest({
      gameState: createGameState(),
    });

    const { container } = render(<AssistPaymentUI />);
    expect(container).toBeEmptyDOMElement();
  });

  // REWRITE: the `getByRole("slider")` subject no longer exists. The engine's [0, max]
  // window is now shown by the shared hint rather than by the control's native min/max.
  it("defaults to 0 and shows the engine [0, max] window", () => {
    const waitingFor = assistPaymentWaitingFor(4);
    setGameStoreForTest({
      gameState: createGameState({ waiting_for: waitingFor }),
      waitingFor,
    });

    render(<AssistPaymentUI />);

    // The box keeps the slider's accessible name byte-for-byte.
    expect(screen.getByLabelText("Assist: Pay Generic Mana")).toHaveValue("0");
    expect(screen.getByText("max 4")).toBeInTheDocument();
    // At the 0 default the lower bound is already reached.
    expect(screen.getByLabelText("Decrease amount")).toBeDisabled();
  });

  // `max_generic` is held CONSTANT so the caster is the only changing dep: keyed on the bound
  // alone the effect would not fire, and the amount chosen for the previous assist prompt would
  // carry into this one. The PAYER axis is covered separately by AP/successor-seat below — an
  // earlier version of this note claimed a same-caster successor was indistinguishable, which
  // `chosen` disproves.
  it("AP/successor: a same-bound prompt for a different caster resets the entry", () => {
    const promptFor = (caster: number) =>
      buildAssistPaymentWaitingFor({ data: { caster, chosen: 0, max_generic: 4 } });

    const first = promptFor(1);
    setGameStoreForTest({
      gameState: createGameState({ waiting_for: first }),
      waitingFor: first,
    });

    const { rerender } = render(<AssistPaymentUI />);
    const box = screen.getByLabelText("Assist: Pay Generic Mana");
    fireEvent.change(box, { target: { value: "3" } });
    expect(box).toHaveValue("3");

    const successor = promptFor(2);
    setGameStoreForTest({
      gameState: createGameState({ waiting_for: successor }),
      waitingFor: successor,
    });
    rerender(<AssistPaymentUI />);

    expect(screen.getByLabelText("Assist: Pay Generic Mana")).toHaveValue("0");
  });

  // The seat axis for assist: same caster, same bound, different payer. Uses the CR 723.5 +
  // CR 723.3 control shape (the local seat stays `turn_decision_controller` while the semantic
  // player moves) so one client renders both prompts in a row — otherwise the panel hides and
  // the row would be vacuous, which is how the shape was pinned.
  it("AP/successor-seat: a same-caster prompt for a different PAYER resets the entry", () => {
    const promptFor = (chosen: number) =>
      buildAssistPaymentWaitingFor({ data: { caster: 1, chosen, max_generic: 4 } });

    const seat = (waitingFor: WaitingFor, active: number) => {
      setGameStoreForTest({
        gameState: createGameState({
          waiting_for: waitingFor,
          active_player: active,
          turn_decision_controller: 0,
        }),
        waitingFor,
      });
    };

    const first = promptFor(0);
    seat(first, 0);
    const { rerender } = render(<AssistPaymentUI />);
    fireEvent.change(screen.getByLabelText("Assist: Pay Generic Mana"), {
      target: { value: "3" },
    });
    expect(screen.getByLabelText("Assist: Pay Generic Mana")).toHaveValue("3");

    const second = promptFor(1);
    seat(second, 1);
    rerender(<AssistPaymentUI />);

    expect(screen.getByLabelText("Assist: Pay Generic Mana")).toHaveValue("0");
  });

  it("dispatches CommitAssistPayment with the selected value", () => {
    const dispatch = vi.fn().mockResolvedValue([]);
    const waitingFor = assistPaymentWaitingFor(4);
    setGameStoreForTest({
      gameState: createGameState({ waiting_for: waitingFor }),
      waitingFor,
      dispatch,
    });

    render(<AssistPaymentUI />);

    // Name-filtered: the panel now has 3 buttons, so a bare getByRole("button") throws
    // "Found multiple elements".
    fireEvent.change(screen.getByLabelText("Assist: Pay Generic Mana"), {
      target: { value: "3" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Pay 3" }));

    expect(dispatch).toHaveBeenCalledWith({
      type: "CommitAssistPayment",
      data: { generic: 3 },
    });
  });

  it("commits generic:0 when paying nothing", () => {
    const dispatch = vi.fn().mockResolvedValue([]);
    const waitingFor = assistPaymentWaitingFor(4);
    setGameStoreForTest({
      gameState: createGameState({ waiting_for: waitingFor }),
      waitingFor,
      dispatch,
    });

    render(<AssistPaymentUI />);

    fireEvent.click(screen.getByRole("button", { name: "Pay nothing" }));

    expect(dispatch).toHaveBeenCalledWith({
      type: "CommitAssistPayment",
      data: { generic: 0 },
    });
  });

  // Enter bypasses the button's `disabled` attribute entirely, so this is the ONLY route on
  // which `handleCommit`'s null-guard is observable. Matched pair: the negative alone would
  // be satisfied by a component that never dispatches at all.
  it("AP/enter: Enter submits a valid amount and refuses to coerce one above max_generic", () => {
    const dispatch = vi.fn().mockResolvedValue([]);
    const waitingFor = assistPaymentWaitingFor(4);
    setGameStoreForTest({
      gameState: createGameState({ waiting_for: waitingFor }),
      waitingFor,
      dispatch,
    });

    render(<AssistPaymentUI />);

    const box = screen.getByLabelText("Assist: Pay Generic Mana");
    fireEvent.change(box, { target: { value: "5" } });
    fireEvent.keyDown(box, { key: "Enter" });
    expect(dispatch).not.toHaveBeenCalled();

    fireEvent.change(box, { target: { value: "3" } });
    fireEvent.keyDown(box, { key: "Enter" });
    expect(dispatch).toHaveBeenCalledWith({
      type: "CommitAssistPayment",
      data: { generic: 3 },
    });
  });

  // The assist domain is 0..max_generic (CR 702.132a), and the engine's own
  // `number_projection` synthesizes `min: 0`. An entry above max must not reach the engine.
  it("AP/above-max: an entry over max_generic cannot be committed", () => {
    const dispatch = vi.fn().mockResolvedValue([]);
    const waitingFor = assistPaymentWaitingFor(4);
    setGameStoreForTest({
      gameState: createGameState({ waiting_for: waitingFor }),
      waitingFor,
      dispatch,
    });

    render(<AssistPaymentUI />);

    fireEvent.change(screen.getByLabelText("Assist: Pay Generic Mana"), {
      target: { value: "5" },
    });

    // "Pay nothing" here was the OPPOSITE of the pending intent: the player typed 5, and
    // `(amount ?? 0) === 0` collapsed the invalid entry into the decline label. Declining is
    // amount === 0 exactly (pinned separately above); an invalid entry names no amount at all.
    expect(screen.getByRole("button", { name: "Confirm" })).toBeDisabled();
    // DOMINATED by the assertion above, labelled rather than deleted. MEASURED on the sibling
    // prompt: removing the null-guard reds only the Enter row, and a click here cannot help
    // because React does not dispatch `onClick` to a disabled `button` (a React-level
    // suppression, not a property of the test environment). AP/enter discriminates.
    expect(dispatch).not.toHaveBeenCalled();
  });
});
