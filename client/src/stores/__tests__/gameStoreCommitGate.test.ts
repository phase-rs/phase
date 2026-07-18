/**
 * `commitEngineSnapshot` is the single writer of the live-game engine pair
 * (`gameState`, `waitingFor`, and the `legalResultState(...)` fields). These
 * tests pin its two load-bearing properties:
 *
 *  1. **Revision gate** — a pair derived from an OLDER engine version never
 *     clobbers a newer one already committed. This is what makes a mid-animation
 *     commit safe: whichever pair is newer wins, regardless of arrival order.
 *  2. **History is not gated** — events, log entries, and undo checkpoints are
 *     ordered by arrival, not by engine epoch, so they apply even when the pair
 *     they arrived with is dropped.
 */
import { beforeEach, describe, expect, it } from "vitest";

import type { EngineSnapshot, GameEvent, GameLogEntry, GameState } from "../../adapter/types";
import { nextSnapshotSeq } from "../../adapter/types";
import {
  buildGameState,
  buildLegalActionsResult,
  buildPriorityWaitingFor,
} from "../../test/factories/gameStateFactory";
import { useGameStore } from "../gameStore";

const PRIORITY = buildPriorityWaitingFor({ data: { player: 0 } });

const OPTIONAL_EFFECT_CHOICE = {
  type: "OptionalEffectChoice",
  data: { player: 0, source_id: 100, description: "you may have target player lose 3 life." },
} as unknown as GameState["waiting_for"];

/** A coherent pair: the state and the legal actions the engine derived from it. */
function snapshotOf(
  waitingFor: GameState["waiting_for"],
  actions: { type: string }[],
  seq: number = nextSnapshotSeq(),
): EngineSnapshot {
  return {
    state: buildGameState({ waiting_for: waitingFor }),
    legalResult: buildLegalActionsResult({ actions: actions as never }),
    seq,
  };
}

function logEntry(text: string): GameLogEntry {
  return {
    seq: 0,
    turn: 1,
    phase: "PreCombatMain",
    category: "Debug",
    segments: [{ type: "Text", value: text }],
  } as GameLogEntry;
}

describe("gameStore commitEngineSnapshot — revision gate", () => {
  beforeEach(() => {
    useGameStore.getState().reset();
  });

  it("commits a fresh pair and records its seq", () => {
    const snapshot = snapshotOf(PRIORITY, [{ type: "PassPriority" }]);

    const accepted = useGameStore.getState().commitEngineSnapshot(snapshot);

    const store = useGameStore.getState();
    expect(accepted).toBe(true);
    expect(store.gameState).toEqual(snapshot.state);
    expect(store.waitingFor).toEqual(PRIORITY);
    expect(store.legalActions).toEqual(snapshot.legalResult.actions);
    expect(store.lastCommittedSeq).toBe(snapshot.seq);
  });

  it("drops a stale pair rather than letting it clobber a newer one", () => {
    // The exact softlock shape: the NEWER engine version is at
    // OptionalEffectChoice; an older in-flight commit still holds Priority.
    const newer = snapshotOf(OPTIONAL_EFFECT_CHOICE, [
      { type: "DecideOptionalEffect" },
    ]);
    const stale = snapshotOf(PRIORITY, [{ type: "PassPriority" }], newer.seq - 1);

    useGameStore.getState().commitEngineSnapshot(newer);
    const accepted = useGameStore.getState().commitEngineSnapshot(stale);

    const store = useGameStore.getState();
    expect(accepted).toBe(false);
    // Every pair field still reflects the NEWER snapshot, not the stale one.
    expect(store.waitingFor).toEqual(OPTIONAL_EFFECT_CHOICE);
    expect(store.legalActions).toEqual(newer.legalResult.actions);
    expect(store.gameState).toEqual(newer.state);
    expect(store.lastCommittedSeq).toBe(newer.seq);
  });

  it("accepts an equal seq (idempotent re-commit of one cached wire snapshot)", () => {
    // A guest/ws snapshot can be committed twice — once by the remote-update
    // path, once by a local read of the same cached object. The pairs are
    // byte-identical, so `>=` must let both through rather than wedging the gate.
    const snapshot = snapshotOf(PRIORITY, [{ type: "PassPriority" }]);

    expect(useGameStore.getState().commitEngineSnapshot(snapshot)).toBe(true);
    expect(useGameStore.getState().commitEngineSnapshot(snapshot)).toBe(true);

    expect(useGameStore.getState().waitingFor).toEqual(PRIORITY);
    expect(useGameStore.getState().lastCommittedSeq).toBe(snapshot.seq);
  });

  it("drops a leftover cross-match commit (Bo3 game-2 case)", () => {
    // Game 1's dispatch queue captured a snapshot, then the match ended and a new
    // adapter was built for game 2. Because the seq counter is module-GLOBAL (not
    // per-adapter), game 2's fresh post-init fetch outranks the leftover — so the
    // late game-1 commit is dropped instead of latching the gate above game 2 and
    // softlocking it permanently.
    const leftoverGame1 = snapshotOf(PRIORITY, [{ type: "PassPriority" }]);
    const game2Install = snapshotOf(OPTIONAL_EFFECT_CHOICE, [
      { type: "DecideOptionalEffect" },
    ]);
    expect(game2Install.seq).toBeGreaterThan(leftoverGame1.seq);

    useGameStore.getState().commitEngineSnapshot(game2Install);
    const accepted = useGameStore.getState().commitEngineSnapshot(leftoverGame1);

    expect(accepted).toBe(false);
    expect(useGameStore.getState().gameState).toEqual(game2Install.state);
    expect(useGameStore.getState().waitingFor).toEqual(OPTIONAL_EFFECT_CHOICE);
  });

  it("applies events, log entries, and undo checkpoints even when the pair is dropped", () => {
    const newer = snapshotOf(OPTIONAL_EFFECT_CHOICE, [{ type: "DecideOptionalEffect" }]);
    useGameStore.getState().commitEngineSnapshot(newer);

    const stale = snapshotOf(PRIORITY, [{ type: "PassPriority" }], newer.seq - 1);
    const events: GameEvent[] = [{ type: "PriorityPassed", data: { player_id: 0 } }];
    const checkpoint = buildGameState({ turn_number: 7 });

    const accepted = useGameStore.getState().commitEngineSnapshot(stale, {
      events,
      logEntries: [logEntry("stale-but-real")],
      stateHistory: [checkpoint],
    });

    const store = useGameStore.getState();
    expect(accepted).toBe(false);
    // History is ordered by ARRIVAL, not engine epoch — it lands regardless.
    expect(store.events).toEqual(events);
    expect(store.eventHistory).toEqual(events);
    expect(store.logHistory).toEqual([{ ...logEntry("stale-but-real"), seq: 0 }]);
    expect(store.nextLogSeq).toBe(1);
    // A checkpoint is a PRE-action state — valid whichever pair wins. Dropping it
    // would silently lose an undo step.
    expect(store.stateHistory).toEqual([checkpoint]);
    // …but the pair itself is still the newer one.
    expect(store.waitingFor).toEqual(OPTIONAL_EFFECT_CHOICE);
  });

  it("applies extraState only on an accepted commit, after the base commit", () => {
    const first = snapshotOf(PRIORITY, [{ type: "PassPriority" }]);
    useGameStore.getState().commitEngineSnapshot(first, {
      events: [{ type: "PriorityPassed", data: { player_id: 0 } }],
    });

    // An init/restore-shaped commit: newest-by-construction, and its extraState
    // resets the histories atomically with its pair.
    const reinit = snapshotOf(OPTIONAL_EFFECT_CHOICE, [{ type: "DecideOptionalEffect" }]);
    useGameStore.getState().commitEngineSnapshot(reinit, {
      extraState: { gameId: "game-2", events: [], eventHistory: [], logHistory: [], nextLogSeq: 0 },
    });

    const store = useGameStore.getState();
    expect(store.gameId).toBe("game-2");
    expect(store.eventHistory).toEqual([]);
    expect(store.waitingFor).toEqual(OPTIONAL_EFFECT_CHOICE);
  });

  it("reset returns the gate counter to zero", () => {
    useGameStore.getState().commitEngineSnapshot(snapshotOf(PRIORITY, []));
    expect(useGameStore.getState().lastCommittedSeq).toBeGreaterThan(0);

    useGameStore.getState().reset();

    expect(useGameStore.getState().lastCommittedSeq).toBe(0);
  });
});
