import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { TargetRef, WaitingFor } from "../../../adapter/types.ts";
import { useGameStore } from "../../../stores/gameStore.ts";
import { useMultiplayerStore } from "../../../stores/multiplayerStore.ts";
import {
  buildGameState,
  buildPendingCast,
  buildPlayers,
  buildTargetSelectionProgress,
  buildTargetSelectionSlot,
  buildTargetSelectionWaitingFor,
} from "../../../test/factories/gameStateFactory.ts";
import { OpponentSeatHeader } from "../OpponentSeatHeader.tsx";

function targetSelectionWaitingFor(legalPlayers: number[]): WaitingFor {
  const targets: TargetRef[] = legalPlayers.map((player) => ({ Player: player }));
  return buildTargetSelectionWaitingFor({
    data: {
      player: 0,
      selection: buildTargetSelectionProgress({ current_legal_targets: targets }),
      target_slots: [buildTargetSelectionSlot({ legal_targets: targets })],
      pending_cast: buildPendingCast(),
    },
  });
}

function createGameState(waitingFor: WaitingFor) {
  return buildGameState({
    players: buildPlayers([
      { id: 0, life: 40 },
      { id: 1, life: 40 },
      { id: 2, life: 40 },
      { id: 3, life: 40 },
    ]),
    waiting_for: waitingFor,
    seat_order: [0, 1, 2, 3],
    eliminated_players: [],
  });
}

describe("OpponentSeatHeader", () => {
  beforeEach(() => {
    // `useCanActForWaitingState` short-circuits on EITHER `gameMode === "spectate"`
    // OR `isSpectator`, and the two live in different module-singleton stores that
    // persist across tests in this file. Both are reset here, not only in
    // `afterEach`, so one spectator row cannot make every later seated row inert.
    useMultiplayerStore.setState({ activePlayerId: 0, isSpectator: false });
    useGameStore.setState({ gameMode: null });
  });

  afterEach(() => {
    cleanup();
    vi.restoreAllMocks();
  });

  it("targets the opponent when the whole legal target plate is clicked", () => {
    const dispatch = vi.fn();
    const waitingFor = targetSelectionWaitingFor([1]);
    useGameStore.setState({
      dispatch,
      gameState: createGameState(waitingFor),
      waitingFor,
    });

    render(<OpponentSeatHeader playerId={1} />);

    fireEvent.click(screen.getByRole("button", { name: "Target Opp 2" }));

    expect(dispatch).toHaveBeenCalledWith({
      type: "ChooseTarget",
      data: { target: { Player: 1 } },
    });
  });

  it("does not target when the opponent player is not legal", () => {
    const dispatch = vi.fn();
    const waitingFor = targetSelectionWaitingFor([2]);
    useGameStore.setState({
      dispatch,
      gameState: createGameState(waitingFor),
      waitingFor,
    });

    render(<OpponentSeatHeader playerId={1} />);

    fireEvent.click(screen.getByTestId("opponent-seat-header-1"));

    expect(screen.queryByRole("button", { name: "Target Opp 2" })).not.toBeInTheDocument();
    expect(dispatch).not.toHaveBeenCalled();
  });

  it("surfaces player-attached Auras (curses) for parity with the legacy HUD", () => {
    const waitingFor = targetSelectionWaitingFor([]);
    useGameStore.setState({
      gameState: {
        ...createGameState(waitingFor),
        derived: {
          auras_attached_to_player: { "1": [101, 102] },
        },
      },
      waitingFor,
    });

    render(<OpponentSeatHeader playerId={1} />);

    expect(
      screen.getByRole("button", { name: "2 enchantments on this player" }),
    ).toBeInTheDocument();
  });

  it("renders Next Up badge with tooltip text", () => {
    const waitingFor = targetSelectionWaitingFor([]);
    useGameStore.setState({
      gameState: {
        ...createGameState(waitingFor),
        derived: {
          turn_order: [{ player: 1, slot_index: 1, turns_from_now: 1, turn_number: 2 }],
        },
      },
      waitingFor,
    });

    render(<OpponentSeatHeader playerId={1} />);

    expect(screen.getByTitle("This player's turn is next.")).toHaveTextContent("Next Up");
  });

  // V8 — the actor-gate fix on the split-board seat surface. The target
  // `<button>` renders only when `isValidPlayerTarget`, which is now
  // `useCanActForWaitingState() && getWaitingForPlayerChoiceIds(waitingFor).includes(playerId)`.
  // `dispatch.ts` silently refuses a spectator's action, so offering the control
  // at all is a false live-looking affordance. The two seated targeting tests
  // above are this row's reach guards.
  it("offers no target control to a spectating client", () => {
    const dispatch = vi.fn();
    const waitingFor = targetSelectionWaitingFor([1]);
    useMultiplayerStore.setState({ isSpectator: true });
    useGameStore.setState({
      dispatch,
      gameMode: "spectate",
      gameState: createGameState(waitingFor),
      waitingFor,
    });

    render(<OpponentSeatHeader playerId={1} />);

    fireEvent.click(screen.getByTestId("opponent-seat-header-1"));

    expect(screen.queryByRole("button", { name: "Target Opp 2" })).not.toBeInTheDocument();
    expect(dispatch).not.toHaveBeenCalled();
  });

  // V12 row (c) — CR 723.1 turn control; CR 723.3: only control of the player
  // changes, so a controlled player is still the active player.
  // Real seat 0 pilots seat 1's turn, so
  // the perspective seat is 1 and seat 0 is rendered as an *opponent* header.
  // The prompt is addressed to seat 0 and offers seat 0: the actor gate resolves
  // the real seat and accepts, and the membership test resolves the rendered
  // seat and matches. The old per-surface `data.player === usePerspectivePlayerId()`
  // test refused this, so no surface offered the seat and no click could answer.
  describe("under a turn-control effect (CR 723.1 / CR 723.3)", () => {
    function turnControlState(waitingFor: WaitingFor) {
      return {
        ...createGameState(waitingFor),
        turn_decision_controller: 0,
        active_player: 1,
      };
    }

    it("offers the real seat that the perspective seat sees as an opponent", () => {
      const dispatch = vi.fn();
      const waitingFor = targetSelectionWaitingFor([0]);
      useGameStore.setState({
        dispatch,
        gameState: turnControlState(waitingFor),
        waitingFor,
      });

      render(<OpponentSeatHeader playerId={0} />);

      fireEvent.click(screen.getByRole("button", { name: "Target Opp 1" }));

      expect(dispatch).toHaveBeenCalledWith({
        type: "ChooseTarget",
        data: { target: { Player: 0 } },
      });
    });

    // Paired negative: the same turn-control state where the engine names a seat
    // this header does not render. Without it, the row above could pass because
    // every header became clickable under turn control.
    it("stays inert when the engine names a seat this header does not render", () => {
      const dispatch = vi.fn();
      const waitingFor = targetSelectionWaitingFor([1]);
      useGameStore.setState({
        dispatch,
        gameState: turnControlState(waitingFor),
        waitingFor,
      });

      render(<OpponentSeatHeader playerId={0} />);

      fireEvent.click(screen.getByTestId("opponent-seat-header-0"));

      expect(screen.queryByRole("button", { name: "Target Opp 1" })).not.toBeInTheDocument();
      expect(dispatch).not.toHaveBeenCalled();
    });
  });
});
