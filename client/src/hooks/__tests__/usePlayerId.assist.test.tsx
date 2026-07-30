/**
 * Runtime test for the CR 702.132a Assist acting-player routing fix in
 * `waitingPlayer`. Under an `AssistPayment` waiting state the CHOSEN helper is
 * the actor (the prompt carries `caster`/`chosen`, no `player` field). These
 * tests drive `useCanActForWaitingState` through that branch: it must be true
 * for the `chosen` seat and false for the `caster` seat. Reverting the
 * `AssistPayment` branch makes `waitingPlayer` return null (no `player` field),
 * which flips the `chosen`-seat assertion to false.
 */
import { renderHook } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it } from "vitest";

import type { GameState, WaitingFor } from "../../adapter/types";
import { useGameStore } from "../../stores/gameStore";
import { useMultiplayerStore } from "../../stores/multiplayerStore";
import {
  buildAssistPaymentWaitingFor,
  buildGameState,
  buildManaPaymentWaitingFor,
  buildPlayers,
} from "../../test/factories/gameStateFactory.ts";
import { useCanActForWaitingState } from "../usePlayerId";

function createGameState(): GameState {
  return buildGameState({
    active_player: 1,
    players: buildPlayers([0, 1]),
    priority_player: 1,
    next_object_id: 100,
    waiting_for: buildManaPaymentWaitingFor(),
    has_pending_cast: true,
    turn_decision_controller: 1,
  });
}

// caster = 1, chosen = 0: the helper (seat 0) is the one who must act.
const ASSIST_PAYMENT: WaitingFor = buildAssistPaymentWaitingFor({
  data: { caster: 1, chosen: 0, max_generic: 3 },
});

function setLocalSeat(seat: number) {
  useGameStore.setState({
    gameMode: "online",
    gameState: createGameState(),
    waitingFor: ASSIST_PAYMENT,
  });
  useMultiplayerStore.setState({ activePlayerId: seat, isSpectator: false });
}

describe("useCanActForWaitingState — Assist payment routing (CR 702.132a)", () => {
  beforeEach(() => {
    useGameStore.getState().reset();
    useMultiplayerStore.setState({ activePlayerId: null, isSpectator: false });
  });

  afterEach(() => {
    useGameStore.getState().reset();
    useMultiplayerStore.setState({ activePlayerId: null, isSpectator: false });
  });

  it("is true for the chosen helper seat", () => {
    setLocalSeat(0); // chosen
    const { result } = renderHook(() => useCanActForWaitingState());
    expect(result.current).toBe(true);
  });

  it("is false for the caster seat", () => {
    setLocalSeat(1); // caster, not the helper
    const { result } = renderHook(() => useCanActForWaitingState());
    expect(result.current).toBe(false);
  });
});
