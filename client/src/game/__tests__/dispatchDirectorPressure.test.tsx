import { act, cleanup, render } from "@testing-library/react";
import { createRef } from "react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { EngineSnapshot, GameAction, GameEvent } from "../../adapter/types";
import { nextSnapshotSeq } from "../../adapter/types";
import { normalizeEvents } from "../../animation/eventNormalizer";
import { AnimationOverlay } from "../../components/animation/AnimationOverlay";
import { useAnimationStore } from "../../stores/animationStore";
import { useGameStore } from "../../stores/gameStore";
import { usePreferencesStore } from "../../stores/preferencesStore";
import {
  buildGameState,
  buildLegalActionsResult,
  buildPriorityWaitingFor,
  buildStackEntry,
} from "../../test/factories/gameStateFactory";
import { resetStackThroughput } from "../../utils/stackThroughput";
import { abandonPendingDispatches, processRemoteUpdate } from "../dispatch";

const PASS = { type: "PassPriority" } as unknown as GameAction;

/** A snapshot whose 12-entry stack is Elevated pressure (pace x0.5). */
function pressuredSnapshot(): EngineSnapshot {
  return {
    state: buildGameState({
      waiting_for: buildPriorityWaitingFor({ data: { player: 0 } }),
      turn_number: 9,
      stack: Array.from({ length: 12 }, (_, index) => buildStackEntry({ id: index + 1 })),
    }),
    legalResult: buildLegalActionsResult({ actions: [PASS] }),
    seq: nextSnapshotSeq(),
  };
}

const EVENTS: GameEvent[] = [
  { type: "LifeChanged", data: { player_id: 1, amount: -1 } },
  { type: "TurnStarted", data: { player_id: 1, turn_number: 2 } },
  { type: "LifeChanged", data: { player_id: 1, amount: -2 } },
];

describe("commit waits for the presented steps under stack pressure", () => {
  beforeEach(() => {
    vi.useFakeTimers();
    resetStackThroughput();
    usePreferencesStore.setState({
      animationSpeedMultiplier: 1,
      cardAnimationStyle: "classic",
      vfxQuality: "minimal",
    });
    useAnimationStore.getState().clearQueue();
    useGameStore.setState({ gameState: buildGameState({ turn_number: 1 }) });
  });

  afterEach(() => {
    cleanup();
    abandonPendingDispatches();
    vi.useRealTimers();
  });

  it("commits at the paced end of the last step, with the overlay mounted", async () => {
    const steps = normalizeEvents(EVENTS);
    expect(steps.length).toBeGreaterThan(1);
    render(<AnimationOverlay containerRef={createRef<HTMLDivElement>()} />);

    let running!: Promise<void>;
    await act(async () => {
      running = processRemoteUpdate(pressuredSnapshot(), EVENTS, []);
      await vi.advanceTimersByTimeAsync(0);
    });
    // Advance step by step so each one is presented before its clock runs out.
    for (const step of steps.slice(0, -1)) {
      await act(async () => {
        await vi.advanceTimersByTimeAsync(step.duration * 0.5);
      });
      expect(useGameStore.getState().gameState?.turn_number).toBe(1);
    }
    const last = steps[steps.length - 1].duration * 0.5;
    await act(async () => {
      await vi.advanceTimersByTimeAsync(last - 1);
    });
    expect(useGameStore.getState().gameState?.turn_number).toBe(1);
    expect(useAnimationStore.getState().activeStep).not.toBeNull();

    await act(async () => {
      await vi.advanceTimersByTimeAsync(1);
    });
    // Asserted before awaiting, so a late commit fails here instead of hanging.
    expect(useGameStore.getState().gameState?.turn_number).toBe(9);
    expect(useAnimationStore.getState().activeStep).toBeNull();
    await running;
  });
});
