import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { cleanup, render, screen } from "@testing-library/react";

import type { LoopCollapseAxis, WaitingFor } from "../../../adapter/types";
import { buildGameState } from "../../../test/factories/gameStateFactory.ts";
import { setGameStoreForTest } from "../../../test/helpers/gameStoreHelpers.ts";
import { useGameStore } from "../../../stores/gameStore";
import { PayAmountChoiceUI } from "../PayAmountChoiceUI.tsx";

// CR 732.2a: the LoopCollapse prompt must name the axis the loop collapses. The
// counter/life labels are iteration-framed (×N), never a raw token count.
function loopCollapseWaitingFor(axis: LoopCollapseAxis): WaitingFor {
  return {
    type: "PayAmountChoice",
    data: {
      player: 0,
      resource: { type: "LoopCollapse", data: { axis } },
      min: 0,
      max: 1000,
      source_id: 0,
    },
  };
}

// player 0 == local PLAYER_ID, and turn_decision_controller/active_player are the
// local seat, so `useCanActForWaitingState` returns true and the prompt renders
// (else it renders null and every text query passes VACUOUSLY).
function renderWithAxis(axis: LoopCollapseAxis) {
  const waitingFor = loopCollapseWaitingFor(axis);
  setGameStoreForTest({
    gameState: buildGameState({
      waiting_for: waitingFor,
      active_player: 0,
      turn_decision_controller: 0,
    }),
    waitingFor,
  });
  return render(<PayAmountChoiceUI />);
}

describe("PayAmountChoiceUI — LoopCollapse axis label", () => {
  beforeEach(() => {
    useGameStore.getState().reset();
  });

  afterEach(() => {
    cleanup();
  });

  it("labels a Counters loop with iteration framing, never a raw token count", () => {
    renderWithAxis("Counters");
    // Positive reach-guard: the prompt actually rendered (canAct true) — the commit
    // button is iteration-framed ("Add counters N×"), not a raw resource count.
    expect(
      screen.getByRole("button", { name: /add counters/i }),
    ).toBeInTheDocument();
    // Negative (the pre-fix bug): a counter loop must NOT say "tokens".
    expect(screen.queryByText(/tokens/i)).not.toBeInTheDocument();
  });

  it("labels a Tokens loop with token framing", () => {
    renderWithAxis("Tokens");
    expect(
      screen.getByRole("button", { name: /create .* tokens/i }),
    ).toBeInTheDocument();
  });
});
