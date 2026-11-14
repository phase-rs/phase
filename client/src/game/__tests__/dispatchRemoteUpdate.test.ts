import { beforeEach, describe, expect, it } from "vitest";

import type { EngineSnapshot, GameLogEntry, GameState, LegalActionsResult } from "../../adapter/types";
import { nextSnapshotSeq } from "../../adapter/types";
import { useGameStore } from "../../stores/gameStore";
import { logPresentation } from "../../viewmodel/logFormatting";
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

function prioritySnapshot(): EngineSnapshot {
  return {
    state: priorityState(),
    legalResult: noLegalActions(),
    seq: nextSnapshotSeq(),
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

    await processRemoteUpdate(prioritySnapshot(), [], [aiGuessLog]);

    expect(useGameStore.getState().logHistory).toEqual([
      {
        ...aiGuessLog,
        seq: 0,
      },
    ]);
    expect(useGameStore.getState().nextLogSeq).toBe(1);
  });

  it("accepts a legacy remote log payload without presentation metadata", async () => {
    const legacyLog: GameLogEntry = {
      seq: 99,
      turn: 1,
      phase: "PreCombatMain",
      category: "Stack",
      segments: [{ type: "Text", value: "Legacy cast" }],
    };

    await processRemoteUpdate(prioritySnapshot(), [], [legacyLog]);

    const [stored] = useGameStore.getState().logHistory;
    expect(stored.category).toBe("Stack");
    expect(logPresentation(stored)).toEqual({
      importance: "Detail",
      tone: "Neutral",
      boundary: "None",
      visibility: "Public",
    });
  });

  // F3. The server-published rewind list must reach the store through the SAME
  // path as the snapshot it describes, so it sits under the generation gate.
  describe("rewindTargets threading", () => {
    it("writes the server list into the store", async () => {
      const targets = [{ turn_number: 3, active_player: 1 }];

      await processRemoteUpdate(prioritySnapshot(), [], [], targets);

      expect(useGameStore.getState().rewindTargets).toEqual(targets);
    });

    it("clears the list when the server publishes an empty one", async () => {
      // Paired negative: `[]` is a real value meaning "no boundaries left",
      // most obviously right after an approved rewind prunes the ring.
      await processRemoteUpdate(prioritySnapshot(), [], [], [
        { turn_number: 3, active_player: 1 },
      ]);
      expect(useGameStore.getState().rewindTargets).toHaveLength(1);

      await processRemoteUpdate(prioritySnapshot(), [], [], []);

      expect(useGameStore.getState().rewindTargets).toEqual([]);
    });

    it("leaves the list untouched when the transport does not publish", async () => {
      // Hostile: `undefined` and `[]` are NOT the same. P2P and draft call
      // `processRemoteUpdate` with three arguments; collapsing the two would
      // make every p2p update wipe a list it knows nothing about.
      await processRemoteUpdate(prioritySnapshot(), [], [], [
        { turn_number: 3, active_player: 1 },
      ]);

      await processRemoteUpdate(prioritySnapshot(), [], []);

      expect(useGameStore.getState().rewindTargets).toEqual([
        { turn_number: 3, active_player: 1 },
      ]);
    });

    // NOT TESTED HERE: that a *superseded* update declines to write. The
    // generation is bumped only by `abandonPendingDispatches` (reachable via
    // `restoreGameState` and `clearPromptOverlayState`), and an event-free
    // update runs to completion synchronously, so there is no window in which
    // to supersede it from this harness. The property is structural instead:
    // the write sits immediately after `processRemoteUpdateInner`'s second
    // `isCurrentDispatchGeneration` guard, next to `commitEngineSnapshot`, and
    // is the reason the list is threaded through dispatch rather than written
    // from `GameProvider`. A `reset()`-based version of this test passes
    // vacuously — `reset()` neither bumps the generation nor leaves the store
    // untouched — so it is deliberately absent rather than green.
  });
});
