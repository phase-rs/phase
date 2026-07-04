import { act } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type {
  EngineAdapter,
  GameAction,
  GameEvent,
  GameState,
  LegalActionsResult,
  SubmitResult,
  WaitingFor,
} from "../../adapter/types";
import { dispatchAction } from "../../game/dispatch";
import { useGameStore } from "../../stores/gameStore";
import { usePreferencesStore } from "../../stores/preferencesStore";
import { useUiStore } from "../../stores/uiStore";
import { buildGameObjectWithCoreTypes, buildObjectMap } from "../../test/factories/gameObjectFactory";
import { buildGameState, buildPlayers } from "../../test/factories/gameStateFactory";

const OB_NIXILIS_ID = 100;
const PLAYER_ID = 0;

const FIRST_OPTIONAL: WaitingFor = {
  type: "OptionalEffectChoice",
  data: {
    player: PLAYER_ID,
    source_id: OB_NIXILIS_ID,
    description: "Ob Nixilis - lose 3 life",
  },
};

const SECOND_OPTIONAL: WaitingFor = {
  type: "OptionalEffectChoice",
  data: {
    player: PLAYER_ID,
    source_id: OB_NIXILIS_ID,
    description: "Ob Nixilis - lose 3 life",
  },
};

const DECIDE_OPTIONAL: GameAction = {
  type: "DecideOptionalEffect",
  data: { accept: true },
} as GameAction;

const LIFE_CHANGED_EVENT = {
  type: "LifeChanged",
  data: { player_id: PLAYER_ID, amount: -3 },
} as GameEvent;

interface DeliveredAction {
  action: GameAction;
  actor: number;
}

function baseState(waitingFor: WaitingFor): GameState {
  const source = buildGameObjectWithCoreTypes(["Creature"], {
    id: OB_NIXILIS_ID,
    name: "Ob Nixilis, the Fallen",
    zone: "Battlefield",
  });
  return buildGameState({
    turn_number: 3,
    active_player: PLAYER_ID,
    phase: "PreCombatMain",
    players: buildPlayers([{ id: PLAYER_ID, life: 20, turns_taken: 1 }]),
    priority_player: PLAYER_ID,
    objects: buildObjectMap(source),
    next_object_id: 300,
    battlefield: [OB_NIXILIS_ID],
    stack: [],
    exile: [],
    rng_seed: 42,
    waiting_for: waitingFor,
    lands_played_this_turn: 1,
    turn_decision_controller: PLAYER_ID,
    phase_stops: {},
  });
}

function makeAdapter(
  delivered: DeliveredAction[],
  nextWaitingFor: WaitingFor,
): EngineAdapter {
  return {
    initialize: vi.fn().mockResolvedValue(undefined),
    initializeGame: vi.fn().mockResolvedValue({ events: [] } as SubmitResult),
    submitAction: vi.fn(async (action: GameAction, actor: number): Promise<SubmitResult> => {
      delivered.push({ action, actor });
      return { events: [LIFE_CHANGED_EVENT] };
    }),
    getState: vi.fn(async () => baseState(nextWaitingFor)),
    getLegalActions: vi.fn(async (): Promise<LegalActionsResult> => ({
      actions: [],
      autoPassRecommended: false,
    })),
    restoreState: vi.fn(),
    getAiAction: vi.fn().mockReturnValue(null),
    estimateBracket: vi.fn().mockResolvedValue(null),
    dispose: vi.fn(),
  };
}

function seedStore(waitingFor: WaitingFor, adapter: EngineAdapter): void {
  act(() => {
    useGameStore.setState({
      gameId: null,
      gameMode: "ai",
      adapter,
      gameState: baseState(waitingFor),
      waitingFor,
      events: [],
      eventHistory: [],
      logHistory: [],
      nextLogSeq: 0,
      stateHistory: [],
      turnCheckpoints: [],
      legalActions: [],
      autoPassRecommended: false,
    });
  });
}

async function flushMicrotasks(): Promise<void> {
  for (let i = 0; i < 20; i++) {
    await Promise.resolve();
  }
}

async function finishAnimation(): Promise<void> {
  await vi.advanceTimersByTimeAsync(350);
  await flushMicrotasks();
}

describe("Greenwarden doubled-trigger dispatch de-dup", () => {
  beforeEach(() => {
    vi.useFakeTimers();
    usePreferencesStore.setState({ animationSpeedMultiplier: 1.0 });
    useUiStore.setState({ pendingAbilityChoice: null, enchantmentsDialogPlayer: null });
  });

  afterEach(() => {
    vi.useRealTimers();
    act(() => {
      useGameStore.setState({ gameState: null, waitingFor: null, adapter: null });
    });
  });

  it("does not suppress the same action when the WaitingFor reference changed", async () => {
    const delivered: DeliveredAction[] = [];
    const adapter = makeAdapter(delivered, SECOND_OPTIONAL);
    seedStore(FIRST_OPTIONAL, adapter);

    const firstDispatch = dispatchAction(DECIDE_OPTIONAL, PLAYER_ID);
    await flushMicrotasks();

    // Simulates processQueue advancing to the second structurally identical
    // OptionalEffectChoice while the first dispatch still owns the mutex.
    act(() => {
      useGameStore.setState({
        waitingFor: SECOND_OPTIONAL,
        gameState: baseState(SECOND_OPTIONAL),
      });
    });

    const secondDispatch = dispatchAction(DECIDE_OPTIONAL, PLAYER_ID);
    await flushMicrotasks();

    await finishAnimation();
    await firstDispatch;
    await finishAnimation();
    await secondDispatch;

    expect(delivered).toHaveLength(2);
    expect(delivered[0]).toMatchObject({
      action: { type: "DecideOptionalEffect" },
      actor: PLAYER_ID,
    });
    expect(delivered[1]).toMatchObject({
      action: { type: "DecideOptionalEffect" },
      actor: PLAYER_ID,
    });
  });

  it("still suppresses a double-click on the same WaitingFor reference", async () => {
    const delivered: DeliveredAction[] = [];
    const adapter = makeAdapter(delivered, FIRST_OPTIONAL);
    seedStore(FIRST_OPTIONAL, adapter);

    const firstDispatch = dispatchAction(DECIDE_OPTIONAL, PLAYER_ID);
    await flushMicrotasks();

    const secondDispatch = dispatchAction(DECIDE_OPTIONAL, PLAYER_ID);
    await flushMicrotasks();

    await vi.advanceTimersByTimeAsync(400);
    await flushMicrotasks();
    await firstDispatch;
    await secondDispatch;

    expect(delivered).toHaveLength(1);
  });
});
