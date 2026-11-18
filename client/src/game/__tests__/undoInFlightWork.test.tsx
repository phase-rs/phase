import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type {
  AiActionProposal,
  GameEvent,
  GameLogEntry,
  GameState,
  PersistedGameState,
  SubmitResult,
} from "../../adapter/types";
import { AdapterError, AdapterErrorCode, persistedGameStateView } from "../../adapter/types";
import { UndoButton } from "../../components/board/UndoButton";
import { useKeyboardShortcuts } from "../../hooks/useKeyboardShortcuts";
import { useAppNotificationStore } from "../../stores/appToastStore";
import { useGameStore } from "../../stores/gameStore";
import { usePreferencesStore } from "../../stores/preferencesStore";
import { buildEngineAdapterMock } from "../../test/factories/engineAdapterFactory";
import { gameStateFactory } from "../../test/factories/gameStateFactory";
import {
  abandonPendingDispatches,
  dispatchAction,
  dispatchAiActionProposal,
  isDispatchIdle,
  undoLastAction,
} from "../dispatch";
import { onEngineLost, onNonFatalPanic, type EngineLostEvent } from "../engineRecovery";

// `debugLog` writes into the game log, the very history under test.
vi.mock("../debugLog", () => ({ debugLog: vi.fn() }));

const HUMAN = 0;
const AI_SEAT = 1;

/** The human holds priority throughout; the turn number tells the states apart. */
function humanToAct(turn: number): GameState {
  return gameStateFactory
    .withPlayers(HUMAN, AI_SEAT)
    .onTurn(turn)
    .activePlayer(HUMAN)
    .priority(HUMAN)
    .build();
}

const BEFORE_FIRST_PASS = humanToAct(3);
const AFTER_FIRST_PASS = humanToAct(4);
const AFTER_SECOND_PASS = humanToAct(5);

function logEntry(text: string): GameLogEntry {
  return {
    seq: 0,
    turn: 3,
    phase: "PreCombatMain",
    category: "Turn",
    segments: [{ type: "Text", value: text }],
    presentation: { importance: "Essential", tone: "Neutral", boundary: "None", visibility: "Public" },
  };
}

const EARLIER_PLAYS = [logEntry("Player 1 casts Sensei's Divining Top"), logEntry("Player 2 casts Sol Ring")];
const EARLIER_LINES = ["Player 1 casts Sensei's Divining Top", "Player 2 casts Sol Ring"];
const FIRST_PASS = logEntry("Player 1 passes for the first time");
const SECOND_PASS = logEntry("Player 1 passes for the second time");
const AI_PLAY = logEntry("Player 2 casts Lightning Bolt");
const AI_PROPOSAL: AiActionProposal = {
  token: "bolt",
  semanticOwner: AI_SEAT,
  actor: AI_SEAT,
  action: { type: "CastSpell", data: { object_id: 40, card_id: 7, targets: [] } },
};
/** Normalizes to an animation step, so the action that carries it waits out a real animation. */
const LIFE_LOSS: GameEvent = { type: "LifeChanged", data: { player_id: HUMAN, amount: -3 } };

/** The store's log as text, oldest first. */
function loggedLines(): string[] {
  return useGameStore.getState().logHistory.map((entry) =>
    entry.segments.map((segment) => (segment.type === "Text" ? segment.value : "")).join(""),
  );
}

/**
 * A solo AI game whose engine is a variable that its requests read and change.
 * Like both engine transports, it answers requests one at a time, in the order
 * they were made.
 */
async function startGame() {
  let live: GameState = BEFORE_FIRST_PASS;
  let lastRequest: Promise<unknown> = Promise.resolve();
  function inOrder<Answer>(request: () => Answer | Promise<Answer>): Promise<Answer> {
    const answer = lastRequest.then(request);
    lastRequest = answer.catch(() => undefined);
    return answer;
  }

  const adapter = buildEngineAdapterMock(BEFORE_FIRST_PASS, {
    getState: vi.fn(() => inOrder(() => live)),
    exportPersistenceState: vi.fn(() =>
      inOrder(() => JSON.stringify({ state: live, precast_shortcut_runtime: null })),
    ),
    restoreState: vi.fn((restored: PersistedGameState) =>
      inOrder(() => {
        live = persistedGameStateView(restored);
      }),
    ),
  });

  /** The engine applies an action that logs `entry` and moves the game to `next`. */
  const applies = (next: GameState, entry: GameLogEntry, events: GameEvent[] = []) => {
    adapter.submitAction.mockImplementationOnce(() =>
      inOrder(() => {
        live = next;
        return { events, log_entries: [entry] };
      }),
    );
  };
  /** As `applies`, but the engine is still answering until the returned function finishes it. */
  const appliesSlowly = (next: GameState, entry: GameLogEntry) => {
    let finish!: () => void;
    adapter.submitAction.mockImplementationOnce(() =>
      inOrder(() => {
        live = next;
        return new Promise<SubmitResult>((resolve) => {
          finish = () => resolve({ events: [], log_entries: [entry] });
        });
      }),
    );
    return () => finish();
  };
  /** The LLM-free AI seat's play, which hands the human priority back. */
  const aiPlays = (events: GameEvent[]) => {
    adapter.submitAiActionProposal.mockImplementationOnce(() =>
      inOrder(() => {
        live = AFTER_FIRST_PASS;
        return { status: "applied", result: { events, log_entries: [AI_PLAY] } };
      }),
    );
  };
  /** The next restore is still running until the test finishes it, or fails it. */
  const restoresSlowly = () => {
    let settle!: { finish: () => void; fail: (error: Error) => void };
    adapter.restoreState.mockImplementationOnce((restored: PersistedGameState) =>
      inOrder(
        () =>
          new Promise<void>((resolve, reject) => {
            settle = {
              finish: () => {
                live = persistedGameStateView(restored);
                resolve();
              },
              fail: reject,
            };
          }),
      ),
    );
    return {
      finish: () => settle.finish(),
      fail: (error: Error) => settle.fail(error),
    };
  };
  /** The next restore fails, leaving the engine as it was. */
  const restoreFails = (error: Error) => {
    adapter.restoreState.mockImplementationOnce(() =>
      inOrder(() => {
        throw error;
      }),
    );
  };

  useGameStore.setState({ adapter, gameMode: "ai", aiSeatIds: [AI_SEAT] });
  // The game so far, committed the way every action's log lines are.
  useGameStore.getState().commitEngineSnapshot(await adapter.getSnapshot(), {
    logEntries: EARLIER_PLAYS,
  });
  return { adapter, applies, appliesSlowly, aiPlays, restoresSlowly, restoreFails };
}

function KeyboardHarness() {
  useKeyboardShortcuts();
  return null;
}

function pressZ(): void {
  act(() => {
    window.dispatchEvent(new KeyboardEvent("keydown", { key: "z" }));
  });
}

describe("work in flight when Undo is pressed", () => {
  beforeEach(() => {
    vi.useFakeTimers();
    // A real animation window, which is where an in-flight play waits.
    usePreferencesStore.setState({ animationSpeedMultiplier: 1 });
    useAppNotificationStore.getState().clearNotification();
    abandonPendingDispatches();
    useGameStore.getState().reset();
  });

  afterEach(() => {
    cleanup();
    useGameStore.getState().reset();
    vi.restoreAllMocks();
    vi.useRealTimers();
  });

  it("adds no log lines or checkpoint for a pass still animating when Undo is clicked", async () => {
    const { adapter, applies } = await startGame();

    // The first pass is checkpointed and commits at once: it plays no animation.
    applies(AFTER_FIRST_PASS, FIRST_PASS);
    await dispatchAction({ type: "PassPriority" }, HUMAN);
    expect(useGameStore.getState().stateHistory).toHaveLength(1);

    // The second pass reaches the engine and waits out its animation. It is
    // checkpointed too, but its checkpoint lands only when it commits.
    applies(AFTER_SECOND_PASS, SECOND_PASS, [LIFE_LOSS]);
    const secondPass = dispatchAction({ type: "PassPriority" }, HUMAN);
    await vi.advanceTimersByTimeAsync(0);
    expect(isDispatchIdle()).toBe(false);

    // Undo takes back the first pass, the one on the ring.
    render(<UndoButton />);
    fireEvent.click(screen.getByRole("button", { name: /undo/i }));
    await vi.advanceTimersByTimeAsync(0);
    expect(adapter.restoreState).toHaveBeenCalledOnce();
    expect(useGameStore.getState().gameState).toEqual(BEFORE_FIRST_PASS);

    // The second pass's animation ends after the undo.
    await vi.runAllTimersAsync();
    await secondPass;

    expect(loggedLines()).toEqual(EARLIER_LINES);
    expect(useGameStore.getState().stateHistory).toEqual([]);
    expect(useGameStore.getState().gameState).toEqual(BEFORE_FIRST_PASS);
  });

  it("adds no log lines for an AI play still animating when Z is pressed", async () => {
    const { applies, aiPlays } = await startGame();
    applies(AFTER_FIRST_PASS, FIRST_PASS);
    await dispatchAction({ type: "PassPriority" }, HUMAN);

    aiPlays([LIFE_LOSS]);
    const play = dispatchAiActionProposal(AI_PROPOSAL);
    await vi.advanceTimersByTimeAsync(0);
    expect(isDispatchIdle()).toBe(false);

    render(<KeyboardHarness />);
    pressZ();
    await vi.advanceTimersByTimeAsync(0);
    expect(useGameStore.getState().gameState).toEqual(BEFORE_FIRST_PASS);

    // The play's animation ends after the undo.
    await vi.runAllTimersAsync();
    await play;

    expect(loggedLines()).toEqual(EARLIER_LINES);
    expect(useGameStore.getState().gameState).toEqual(BEFORE_FIRST_PASS);
  });

  it("takes back a store dispatch the engine is still answering when Z is pressed", async () => {
    const { adapter, applies, appliesSlowly } = await startGame();

    // The store's own dispatch (the Pass button, mana payment, most modals)
    // checkpoints the first pass.
    applies(AFTER_FIRST_PASS, FIRST_PASS);
    await useGameStore.getState().dispatch({ type: "PassPriority" });
    expect(useGameStore.getState().stateHistory).toHaveLength(1);

    // The engine is still answering the second pass when Z is pressed.
    const finishSecondPass = appliesSlowly(AFTER_SECOND_PASS, SECOND_PASS);
    const secondPass = useGameStore.getState().dispatch({ type: "PassPriority" });
    await vi.advanceTimersByTimeAsync(0);

    render(<KeyboardHarness />);
    pressZ();
    await vi.advanceTimersByTimeAsync(0);
    expect(adapter.restoreState).toHaveBeenCalledOnce();

    // The engine answers in call order: the second pass, then the restore. So
    // the pass commits before the undo does, and the undo's commit takes its
    // log lines and its checkpoint back with the rest of the undone timeline.
    finishSecondPass();
    await secondPass;
    await vi.advanceTimersByTimeAsync(0);

    expect(loggedLines()).toEqual(EARLIER_LINES);
    expect(useGameStore.getState().stateHistory).toEqual([]);
    expect(useGameStore.getState().gameState).toEqual(BEFORE_FIRST_PASS);
  });

  it("leaves a play in flight to commit when there is nothing to undo", async () => {
    const { applies } = await startGame();
    expect(useGameStore.getState().stateHistory).toEqual([]);

    applies(AFTER_FIRST_PASS, FIRST_PASS, [LIFE_LOSS]);
    const pass = dispatchAction({ type: "PassPriority" }, HUMAN);
    await vi.advanceTimersByTimeAsync(0);
    expect(isDispatchIdle()).toBe(false);

    // A press with an empty ring, such as a second click landing after the
    // first undo emptied it, must not abandon the pass.
    await undoLastAction();

    await vi.runAllTimersAsync();
    await pass;
    expect(loggedLines()).toEqual([...EARLIER_LINES, "Player 1 passes for the first time"]);
    expect(useGameStore.getState().gameState).toEqual(AFTER_FIRST_PASS);
  });

  it("ignores a second Undo pressed while the first is restoring", async () => {
    const { adapter, applies, restoresSlowly } = await startGame();
    applies(AFTER_FIRST_PASS, FIRST_PASS);
    await dispatchAction({ type: "PassPriority" }, HUMAN);
    applies(AFTER_SECOND_PASS, SECOND_PASS);
    await dispatchAction({ type: "PassPriority" }, HUMAN);
    expect(useGameStore.getState().stateHistory).toHaveLength(2);

    const restore = restoresSlowly();
    const firstUndo = undoLastAction();
    await vi.advanceTimersByTimeAsync(0);
    await undoLastAction();
    expect(adapter.restoreState).toHaveBeenCalledOnce();

    restore.finish();
    await firstUndo;
    expect(useGameStore.getState().gameState).toEqual(AFTER_FIRST_PASS);
    expect(useGameStore.getState().stateHistory).toHaveLength(1);
    expect(useGameStore.getState().pendingUndoId).toBeNull();
  });

  it("commits nothing when the game is torn down while the undo restores", async () => {
    const { applies, restoresSlowly } = await startGame();
    applies(AFTER_FIRST_PASS, FIRST_PASS);
    await dispatchAction({ type: "PassPriority" }, HUMAN);

    const restore = restoresSlowly();
    const undoing = undoLastAction();
    await vi.advanceTimersByTimeAsync(0);
    // The player leaves the game mid-undo.
    useGameStore.getState().reset();

    restore.finish();
    await undoing;
    expect(useGameStore.getState().gameState).toBeNull();
    expect(useGameStore.getState().logHistory).toEqual([]);
    expect(useGameStore.getState().pendingUndoId).toBeNull();
  });

  it("reports nothing when the game torn down mid-undo fails the restore", async () => {
    const { adapter, applies, restoresSlowly } = await startGame();
    applies(AFTER_FIRST_PASS, FIRST_PASS);
    await dispatchAction({ type: "PassPriority" }, HUMAN);

    const restore = restoresSlowly();
    const undoing = undoLastAction();
    await vi.advanceTimersByTimeAsync(0);
    // Leaving the game disposes the engine, which fails the restore it held.
    useGameStore.getState().reset();
    const readsBefore = adapter.getSnapshot.mock.calls.length;

    restore.fail(new Error("Worker disposed"));
    await undoing;
    expect(useAppNotificationStore.getState().notification).toBeNull();
    expect(adapter.getSnapshot).toHaveBeenCalledTimes(readsBefore);
    expect(useGameStore.getState().gameState).toBeNull();
  });

  it("keeps a newer game's undo in flight when one from a torn-down game settles", async () => {
    const firstGame = await startGame();
    firstGame.applies(AFTER_FIRST_PASS, FIRST_PASS);
    await dispatchAction({ type: "PassPriority" }, HUMAN);
    const firstRestore = firstGame.restoresSlowly();
    const firstUndo = undoLastAction();
    await vi.advanceTimersByTimeAsync(0);

    // The player leaves, starts another game, and presses Undo there too.
    useGameStore.getState().reset();
    const secondGame = await startGame();
    secondGame.applies(AFTER_FIRST_PASS, FIRST_PASS);
    await dispatchAction({ type: "PassPriority" }, HUMAN);
    const secondRestore = secondGame.restoresSlowly();
    const secondUndo = undoLastAction();
    await vi.advanceTimersByTimeAsync(0);
    expect(useGameStore.getState().pendingUndoId).not.toBeNull();

    // The first game's undo settles while the second's is still restoring.
    firstRestore.finish();
    await firstUndo;
    expect(useGameStore.getState().pendingUndoId).not.toBeNull();

    secondRestore.finish();
    await secondUndo;
    expect(useGameStore.getState().pendingUndoId).toBeNull();
  });

  it("shows the engine's own state and reports the error when the restore fails", async () => {
    const { applies, restoreFails } = await startGame();
    applies(AFTER_FIRST_PASS, FIRST_PASS);
    await dispatchAction({ type: "PassPriority" }, HUMAN);

    // The second pass reaches the engine and waits out its animation.
    applies(AFTER_SECOND_PASS, SECOND_PASS, [LIFE_LOSS]);
    const secondPass = dispatchAction({ type: "PassPriority" }, HUMAN);
    await vi.advanceTimersByTimeAsync(0);
    expect(isDispatchIdle()).toBe(false);

    // The undo abandons the second pass, then its restore fails: the engine
    // keeps the second pass, which the store never committed.
    restoreFails(new Error("restore failed"));
    await undoLastAction();
    expect(useGameStore.getState().gameState).toEqual(AFTER_SECOND_PASS);
    expect(useAppNotificationStore.getState().notification?.description).toBe("restore failed");
    expect(useGameStore.getState().pendingUndoId).toBeNull();
    // Nothing was undone, so the history is not rewound: the first pass keeps
    // its line and its checkpoint. The abandoned second pass never committed
    // its own, and they are lost.
    expect(loggedLines()).toEqual([...EARLIER_LINES, "Player 1 passes for the first time"]);
    expect(useGameStore.getState().stateHistory).toHaveLength(1);

    await vi.runAllTimersAsync();
    await secondPass;
    expect(useGameStore.getState().gameState).toEqual(AFTER_SECOND_PASS);
  });

  it("rewinds the log and the ring when the read after a restore fails", async () => {
    const { adapter, applies } = await startGame();
    applies(AFTER_FIRST_PASS, FIRST_PASS);
    await dispatchAction({ type: "PassPriority" }, HUMAN);
    expect(loggedLines()).toEqual([...EARLIER_LINES, "Player 1 passes for the first time"]);

    // The restore succeeds; the snapshot read after it fails.
    const read = adapter.getSnapshot.getMockImplementation()!;
    adapter.getSnapshot.mockImplementationOnce(async () => {
      await read();
      throw new Error("snapshot failed");
    });
    await undoLastAction();

    // The engine holds the restored game, and so does the store: its log and
    // ring are rewound with it, and the board is read again.
    expect(adapter.restoreState).toHaveBeenCalledOnce();
    expect(loggedLines()).toEqual(EARLIER_LINES);
    expect(useGameStore.getState().stateHistory).toEqual([]);
    expect(useGameStore.getState().gameState).toEqual(BEFORE_FIRST_PASS);
    expect(useAppNotificationStore.getState().notification?.description).toBe("snapshot failed");
  });

  it("asks for a reload, and leaves the history alone, when the engine loses its state after the restore", async () => {
    const { adapter, applies } = await startGame();
    applies(AFTER_FIRST_PASS, FIRST_PASS);
    await dispatchAction({ type: "PassPriority" }, HUMAN);
    const lost: EngineLostEvent[] = [];
    const stopListening = onEngineLost((event) => lost.push(event));

    const read = adapter.getSnapshot.getMockImplementation()!;
    adapter.getSnapshot.mockImplementationOnce(async () => {
      await read();
      throw new AdapterError(AdapterErrorCode.STATE_LOST, "engine state lost", false);
    });
    await undoLastAction();
    stopListening();

    expect(adapter.restoreState).toHaveBeenCalledOnce();
    expect(lost).toEqual([{ reason: "undo", panic: undefined }]);
    expect(useAppNotificationStore.getState().notification).toBeNull();
    // The reload resumes from the saved game; until then nothing is rewound.
    expect(loggedLines()).toEqual([...EARLIER_LINES, "Player 1 passes for the first time"]);
    expect(useGameStore.getState().stateHistory).toHaveLength(1);
    expect(useGameStore.getState().pendingUndoId).toBeNull();
  });

  it("hands a panic after the restore to the panic triage, and leaves the history alone", async () => {
    const { adapter, applies } = await startGame();
    applies(AFTER_FIRST_PASS, FIRST_PASS);
    await dispatchAction({ type: "PassPriority" }, HUMAN);
    const triaged: EngineLostEvent[] = [];
    const stopNonFatal = onNonFatalPanic((event) => triaged.push(event));
    const stopLost = onEngineLost((event) => triaged.push(event));

    const read = adapter.getSnapshot.getMockImplementation()!;
    adapter.getSnapshot.mockImplementationOnce(async () => {
      await read();
      throw new AdapterError(AdapterErrorCode.ENGINE_PANIC, "engine panicked", false, "panicked at state.rs");
    });
    await undoLastAction();
    stopNonFatal();
    stopLost();

    // Whether the triage keeps the game going or asks for a reload, the store
    // keeps the board it showed, and the history that goes with that board.
    expect(triaged).toEqual([{ reason: "undo-panic", panic: "panicked at state.rs" }]);
    expect(useAppNotificationStore.getState().notification).toBeNull();
    expect(useGameStore.getState().gameState).toEqual(AFTER_FIRST_PASS);
    expect(loggedLines()).toEqual([...EARLIER_LINES, "Player 1 passes for the first time"]);
    expect(useGameStore.getState().stateHistory).toHaveLength(1);
    expect(useGameStore.getState().pendingUndoId).toBeNull();
  });

  it("asks for a reload when the board cannot be read again after a failed restore", async () => {
    const { adapter, applies, restoreFails } = await startGame();
    applies(AFTER_FIRST_PASS, FIRST_PASS);
    await dispatchAction({ type: "PassPriority" }, HUMAN);
    const lost: EngineLostEvent[] = [];
    const stopListening = onEngineLost((event) => lost.push(event));

    restoreFails(new Error("restore failed"));
    adapter.getSnapshot.mockImplementationOnce(() => Promise.reject(new Error("worker crashed")));
    await undoLastAction();
    stopListening();

    expect(useAppNotificationStore.getState().notification?.description).toBe("restore failed");
    expect(lost).toEqual([{ reason: "undo-resync", panic: undefined }]);
  });
});
