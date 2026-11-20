/**
 * Undo rewinds the game log along with the engine state (#10993).
 *
 * The log lives in this store, outside the engine state a restore puts back.
 * When undo left it alone, every line of an undone play stayed in the log, and
 * so in the history each LLM seat's next prompt is built from: a model was told
 * it had cast a spell that was back in its hand.
 *
 * Each undo here is pressed with nothing in flight. Work still in flight when
 * Undo is pressed (#11014) is covered in `undoInFlightWork.test.tsx`.
 *
 * These tests checkpoint through `gameStore.dispatch`; the `dispatch.ts`
 * pipeline's checkpoints are covered from the LLM seat's side in
 * `aiControllerLlmUndoHistory.test.ts`.
 */
import { beforeEach, describe, expect, it, vi } from "vitest";

import type { GameLogEntry, GameState, PersistedGameState } from "../../adapter/types";
import { persistedGameStateView } from "../../adapter/types";
import { buildEngineAdapterMock } from "../../test/factories/engineAdapterFactory";
import { gameStateFactory } from "../../test/factories/gameStateFactory";
import { undoLastAction } from "../../game/dispatch";
import { useGameStore } from "../gameStore";

const HUMAN = 0;
const AI_SEAT = 1;

const HUMAN_TO_ACT = gameStateFactory
  .withPlayers(HUMAN, AI_SEAT)
  .onTurn(9)
  .activePlayer(HUMAN)
  .priority(HUMAN)
  .build();
const AI_TO_ACT = gameStateFactory
  .withPlayers(HUMAN, AI_SEAT)
  .onTurn(9)
  .activePlayer(AI_SEAT)
  .priority(AI_SEAT)
  .build();

function logEntry(turn: number, text: string): GameLogEntry {
  return {
    seq: 0,
    turn,
    phase: "PreCombatMain",
    category: "Stack",
    segments: [{ type: "Text", value: text }],
    presentation: { importance: "Essential", tone: "Neutral", boundary: "None", visibility: "Public" },
  };
}

// The line the engine logs for a pass (category Turn), naming who receives priority.
const HUMAN_PASS = { ...logEntry(9, "Priority passes to Player 2"), category: "Turn" as const };

/** The store's log as text, oldest first. */
function loggedLines(): string[] {
  return useGameStore.getState().logHistory.map((entry) =>
    entry.segments.map((segment) => (segment.type === "Text" ? segment.value : "")).join(""),
  );
}

/**
 * A solo game against a scripted engine: `restoreState` really rewinds it and
 * the trusted export is its live state, as the WASM engine's are.
 */
async function startGame(earlierPlays: GameLogEntry[]) {
  let live: GameState = HUMAN_TO_ACT;
  const adapter = buildEngineAdapterMock(HUMAN_TO_ACT, {
    getState: vi.fn(async () => live),
    exportPersistenceState: vi.fn(async () =>
      JSON.stringify({ state: live, precast_shortcut_runtime: null }),
    ),
    restoreState: vi.fn((restored: PersistedGameState) => {
      live = persistedGameStateView(restored);
    }),
  });
  useGameStore.setState({ adapter, gameMode: "ai", aiSeatIds: [AI_SEAT] });
  // The game so far, committed the way every action's log lines are.
  useGameStore.getState().commitEngineSnapshot(await adapter.getSnapshot(), {
    logEntries: earlierPlays,
  });

  return {
    /** The human passes priority, an undoable action, so the store checkpoints it. */
    async humanPasses(lines: GameLogEntry[]): Promise<void> {
      adapter.submitAction.mockImplementationOnce(async () => {
        live = AI_TO_ACT;
        return { events: [], log_entries: lines };
      });
      await useGameStore.getState().dispatch({ type: "PassPriority" });
    },
    /** The AI seat's play lands as `dispatch.ts` commits one: its log lines, and no checkpoint. */
    async aiPlays(lines: GameLogEntry[]): Promise<void> {
      live = HUMAN_TO_ACT;
      useGameStore.getState().commitEngineSnapshot(await adapter.getSnapshot(), {
        events: [],
        logEntries: lines,
      });
    },
  };
}

describe("undo rewinds the game log", () => {
  beforeEach(() => {
    useGameStore.getState().reset();
  });

  it("drops the undone action's lines and every line logged after it", async () => {
    const game = await startGame([
      logEntry(8, "Player 2 plays Plains"),
      logEntry(9, "Player 1 casts Sensei's Divining Top"),
    ]);
    await game.humanPasses([HUMAN_PASS]);
    await game.aiPlays([
      logEntry(9, "Player 2 casts Loran of the Third Path"),
      logEntry(9, "Sensei's Divining Top is destroyed"),
    ]);
    expect(loggedLines()).toEqual([
      "Player 2 plays Plains",
      "Player 1 casts Sensei's Divining Top",
      "Priority passes to Player 2",
      "Player 2 casts Loran of the Third Path",
      "Sensei's Divining Top is destroyed",
    ]);

    await undoLastAction();

    expect(useGameStore.getState().gameState).toEqual(HUMAN_TO_ACT);
    expect(loggedLines()).toEqual([
      "Player 2 plays Plains",
      "Player 1 casts Sensei's Divining Top",
    ]);
    expect(useGameStore.getState().stateHistory).toEqual([]);
  });

  it("lists a replay after an undo once, and rewinds it again on the next undo", async () => {
    const game = await startGame([logEntry(9, "Player 1 casts Steel Seraph")]);
    await game.humanPasses([HUMAN_PASS]);
    await game.aiPlays([logEntry(10, "Elspeth Conquers Death exiles Steel Seraph")]);
    await undoLastAction();

    // The same turn, played again.
    await game.humanPasses([HUMAN_PASS]);
    await game.aiPlays([logEntry(10, "Player 2 casts Wrath of God")]);
    expect(loggedLines()).toEqual([
      "Player 1 casts Steel Seraph",
      "Priority passes to Player 2",
      "Player 2 casts Wrath of God",
    ]);

    await undoLastAction();

    expect(loggedLines()).toEqual(["Player 1 casts Steel Seraph"]);
  });

  it("never gives a later entry the seq of an undone one", async () => {
    const game = await startGame([logEntry(9, "Player 1 casts Steel Seraph")]);
    await game.humanPasses([HUMAN_PASS]);
    await undoLastAction();
    await game.humanPasses([HUMAN_PASS]);

    // Seq 1 named the undone line, so the replayed one is 2: anything that
    // remembers a seq, such as the log panel's row keys, takes it to name one
    // entry only.
    expect(useGameStore.getState().logHistory.map((entry) => entry.seq)).toEqual([0, 2]);
    expect(useGameStore.getState().nextLogSeq).toBe(3);
  });
});
