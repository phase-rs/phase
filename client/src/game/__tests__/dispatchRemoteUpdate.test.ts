import { beforeEach, describe, expect, it } from "vitest";

import type { GameLogEntry, GameState, LegalActionsResult } from "../../adapter/types";
import { useGameStore } from "../../stores/gameStore";
import { processRemoteUpdate } from "../dispatch";

function priorityState(): GameState {
  return {
    turn_number: 1,
    active_player: 0,
    phase: "PreCombatMain",
    players: [],
    priority_player: 0,
    objects: {},
    next_object_id: 1,
    battlefield: [],
    stack: [],
    exile: [],
    rng_seed: 42,
    combat: null,
    waiting_for: { type: "Priority", data: { player: 0 } },
    has_pending_cast: false,
    lands_played_this_turn: 0,
    max_lands_per_turn: 1,
    priority_pass_count: 0,
    pending_replacement: null,
    layers_dirty: false,
    next_timestamp: 1,
  };
}

function noLegalActions(): LegalActionsResult {
  return {
    actions: [],
    autoPassRecommended: false,
  };
}

describe("processRemoteUpdate", () => {
  beforeEach(() => {
    useGameStore.getState().reset();
  });

  it("appends log entries from remote AI state updates", async () => {
    const aiGuessLog: GameLogEntry = {
      seq: 99,
      turn: 1,
      phase: "PreCombatMain",
      category: "Debug",
      segments: [{ type: "Text", value: "AI guesses Nonland" }],
    };

    await processRemoteUpdate(priorityState(), [], noLegalActions(), [aiGuessLog]);

    expect(useGameStore.getState().logHistory).toEqual([
      {
        ...aiGuessLog,
        seq: 0,
      },
    ]);
    expect(useGameStore.getState().nextLogSeq).toBe(1);
  });
});
