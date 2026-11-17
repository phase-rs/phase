import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type {
  GameEvent,
  GameLogEntry,
  GameState,
  PersistedGameState,
  SubmitResult,
} from "../../adapter/types";
import { persistedGameStateView } from "../../adapter/types";
import { UndoButton } from "../../components/board/UndoButton";
import { useKeyboardShortcuts } from "../../hooks/useKeyboardShortcuts";
import { useGameStore } from "../../stores/gameStore";
import { usePreferencesStore } from "../../stores/preferencesStore";
import { buildEngineAdapterMock } from "../../test/factories/engineAdapterFactory";
import { gameStateFactory } from "../../test/factories/gameStateFactory";
import { abandonPendingDispatches, dispatchAction, isDispatchIdle, undoLastAction } from "../dispatch";

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
const FIRST_PASS = logEntry("Player 1 passes for the first time");
const SECOND_PASS = logEntry("Player 1 passes for the second time");
/** Normalizes to an animation step, so the action that carries it waits out a real animation. */
const LIFE_LOSS: GameEvent = { type: "LifeChanged", data: { player_id: AI_SEAT, amount: -3 } };

/** The store's log as text, oldest first. */
function loggedLines(): string[] {
  return useGameStore.getState().logHistory.map((entry) =>
    entry.segments.map((segment) => (segment.type === "Text" ? segment.value : "")).join(""),
  );
}

const EARLIER_LINES = ["Player 1 casts Sensei's Divining Top", "Player 2 casts Sol Ring"];

/**
 * A solo AI game whose engine is a variable: `submitAction` and `restoreState`
 * change it as they are called, as the real engine applies requests in call
 * order, and every read sees it as it stands.
 */
async function startGame() {
  let live: GameState = BEFORE_FIRST_PASS;
  const adapter = buildEngineAdapterMock(BEFORE_FIRST_PASS, {
    getState: vi.fn(async () => live),
    exportPersistenceState: vi.fn(async () =>
      JSON.stringify({ state: live, precast_shortcut_runtime: null }),
    ),
    restoreState: vi.fn((restored: PersistedGameState) => {
      live = persistedGameStateView(restored);
    }),
  });
  /** The engine applies an action that logs `entry` and moves the game to `next`. */
  const applies = (next: GameState, entry: GameLogEntry, events: GameEvent[] = []) => {
    adapter.submitAction.mockImplementationOnce(async () => {
      live = next;
      return { events, log_entries: [entry] };
    });
  };
  /** As `applies`, but the engine's reply waits until the returned function sends it. */
  const appliesAndHoldsReply = (next: GameState, entry: GameLogEntry) => {
    let reply!: (result: SubmitResult) => void;
    adapter.submitAction.mockImplementationOnce(() => {
      live = next;
      return new Promise<SubmitResult>((resolve) => {
        reply = resolve;
      });
    });
    return () => reply({ events: [], log_entries: [entry] });
  };
  useGameStore.setState({ adapter, gameMode: "ai", aiSeatIds: [AI_SEAT] });
  // The game so far, committed the way every action's log lines are.
  useGameStore.getState().commitEngineSnapshot(await adapter.getSnapshot(), {
    logEntries: EARLIER_PLAYS,
  });
  return { adapter, applies, appliesAndHoldsReply };
}

function KeyboardHarness() {
  useKeyboardShortcuts();
  return null;
}

describe("work in flight when Undo is pressed", () => {
  beforeEach(() => {
    vi.useFakeTimers();
    // A real animation window, which is where an in-flight play waits.
    usePreferencesStore.setState({ animationSpeedMultiplier: 1 });
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

  it("adds no log lines or checkpoint for a store dispatch in flight when Z is pressed", async () => {
    const { adapter, applies, appliesAndHoldsReply } = await startGame();

    // The store's own dispatch (the Pass button, mana payment, most modals)
    // checkpoints the first pass.
    applies(AFTER_FIRST_PASS, FIRST_PASS);
    await useGameStore.getState().dispatch({ type: "PassPriority" });
    expect(useGameStore.getState().stateHistory).toHaveLength(1);

    // The engine applies the second pass when it is submitted; its reply is
    // still on the way when Z is pressed.
    const replySecondPass = appliesAndHoldsReply(AFTER_SECOND_PASS, SECOND_PASS);
    const secondPass = useGameStore.getState().dispatch({ type: "PassPriority" });
    await vi.advanceTimersByTimeAsync(0);

    render(<KeyboardHarness />);
    act(() => {
      window.dispatchEvent(new KeyboardEvent("keydown", { key: "z" }));
    });
    await vi.advanceTimersByTimeAsync(0);
    expect(adapter.restoreState).toHaveBeenCalledOnce();
    expect(useGameStore.getState().gameState).toEqual(BEFORE_FIRST_PASS);

    // The second pass's reply arrives after the undo.
    replySecondPass();
    await secondPass;

    expect(loggedLines()).toEqual(EARLIER_LINES);
    expect(useGameStore.getState().stateHistory).toEqual([]);
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
    const { adapter, applies } = await startGame();
    applies(AFTER_FIRST_PASS, FIRST_PASS);
    await dispatchAction({ type: "PassPriority" }, HUMAN);
    applies(AFTER_SECOND_PASS, SECOND_PASS);
    await dispatchAction({ type: "PassPriority" }, HUMAN);
    expect(useGameStore.getState().stateHistory).toHaveLength(2);

    // The first undo's restore is still running when the second press lands.
    const restore = adapter.restoreState.getMockImplementation()!;
    let finishRestore!: () => void;
    adapter.restoreState.mockImplementationOnce((restored: PersistedGameState) =>
      new Promise<void>((resolve) => {
        finishRestore = () => {
          restore(restored);
          resolve();
        };
      }),
    );
    const firstUndo = undoLastAction();
    await undoLastAction();
    expect(adapter.restoreState).toHaveBeenCalledOnce();

    finishRestore();
    await firstUndo;
    expect(useGameStore.getState().gameState).toEqual(AFTER_FIRST_PASS);
    expect(useGameStore.getState().stateHistory).toHaveLength(1);
    expect(useGameStore.getState().undoInFlight).toBe(false);
  });

  it("commits nothing when the game is torn down while the undo restores", async () => {
    const { adapter, applies } = await startGame();
    applies(AFTER_FIRST_PASS, FIRST_PASS);
    await dispatchAction({ type: "PassPriority" }, HUMAN);

    let finishRestore!: () => void;
    adapter.restoreState.mockImplementationOnce(() =>
      new Promise<void>((resolve) => {
        finishRestore = resolve;
      }),
    );
    const undoing = undoLastAction();
    // The player leaves the game mid-undo.
    useGameStore.getState().reset();

    finishRestore();
    await undoing;
    expect(useGameStore.getState().gameState).toBeNull();
    expect(useGameStore.getState().logHistory).toEqual([]);
    expect(useGameStore.getState().undoInFlight).toBe(false);
  });
});
