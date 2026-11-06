import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { MemoryRouter, Route, Routes } from "react-router";

import type {
  PhrasePace,
  SayPhrasePayment,
  WaitingFor,
} from "../../adapter/types.ts";
import { useGameStore } from "../../stores/gameStore.ts";
import { useMultiplayerStore } from "../../stores/multiplayerStore.ts";
import { usePreferencesStore } from "../../stores/preferencesStore.ts";
import { useUiStore } from "../../stores/uiStore.ts";
import { gameStateFactory } from "../../test/factories/gameStateFactory.ts";
import { GamePage } from "../GamePage.tsx";

const { dispatchMock } = vi.hoisted(() => ({ dispatchMock: vi.fn() }));

vi.mock("../../providers/GameProvider.tsx", () => ({
  GameProvider: ({ children }: { children: React.ReactNode }) => <>{children}</>,
}));

vi.mock("../../game/sessionCleanup.ts", () => ({ clearPromptOverlayState: vi.fn() }));
vi.mock("../../hooks/useGameDispatch.ts", () => ({ useGameDispatch: () => dispatchMock }));
vi.mock("../../game/dispatch.ts", () => ({
  dispatchAction: vi.fn(),
  dispatchResolveAll: vi.fn(),
  processRemoteUpdate: vi.fn(),
  restoreGameState: vi.fn(),
  currentSnapshot: new Map(),
}));

vi.mock("../../hooks/useCardImage.ts", () => ({
  useCardImage: vi.fn(() => ({
    src: null,
    isLoading: false,
    isRotated: false,
    isFlip: false,
  })),
}));

vi.mock("../../hooks/useIsMobile.ts", () => ({
  useIsMobile: () => false,
  useIsCompactHeight: () => false,
}));
vi.mock("../../audio/useAudioContext.ts", () => ({ useAudioContext: () => undefined }));
vi.mock("../../hooks/useGameplayPreferencesSync.ts", () => ({
  useGameplayPreferencesSync: () => undefined,
}));
vi.mock("../../hooks/useCardDataMeta.ts", () => ({
  useCardDataMeta: () => null,
  formatRelativeDate: () => "",
}));

vi.mock("../../components/board/BattlefieldBackground.tsx", () => ({
  BattlefieldBackground: () => null,
}));
vi.mock("../../components/stack/StackDisplay.tsx", () => ({ StackDisplay: () => null }));
vi.mock("../../components/debug/DebugPanel.tsx", () => ({ DebugPanel: () => null }));
vi.mock("../../components/hud/HUD.tsx", () => ({ HUD: () => null }));
vi.mock("../../components/board/GameBoard.tsx", () => ({ GameBoard: () => null }));
vi.mock("../../components/modal/EngineLostModal.tsx", () => ({ EngineLostModal: () => null }));
vi.mock("../../components/modal/CardDataMissingModal.tsx", () => ({
  CardDataMissingModal: () => null,
}));
vi.mock("../../components/multiplayer/ConcedeDialog.tsx", () => ({
  ConcedeDialog: () => null,
}));
vi.mock("../../components/chrome/GameMenu.tsx", () => ({ GameMenu: () => null }));

vi.mock("../../stores/draftStore.ts", () => ({
  useDraftStore: vi.fn(() => ({
    phase: "idle",
    pool: [],
    picks: [],
    packs: [],
    currentPack: null,
    currentPickIndex: 0,
    draftComplete: false,
  })),
}));
vi.mock("../../services/quickDraftPersistence.ts", () => ({
  loadActiveQuickDraft: vi.fn(() => null),
  saveQuickDraftRun: vi.fn(),
  deleteQuickDraftRun: vi.fn(),
}));
vi.mock("../../adapter/draft-adapter.ts", async (importOriginal) => ({
  ...(await importOriginal<typeof import("../../adapter/draft-adapter.ts")>()),
  createDraftAdapter: vi.fn(),
}));

const PHRASE = "Toy Boat";
const PAYMENT_ID = 7;
/** Deliberately far below the engine's printed second: the panel only reads the
 *  window the engine publishes, so a short one exercises the same code path
 *  without a multi-second test. */
const GAP_MS = 40;

function gamePageTree() {
  return (
    <MemoryRouter initialEntries={["/game/say-phrase?mode=join"]}>
      <Routes>
        <Route path="/game/:id" element={<GamePage />} />
      </Routes>
    </MemoryRouter>
  );
}

/** Toy Boat's cumulative-upkeep say-it payment: `repetitions` sayings of the
 *  phrase still owed, each a host-stamped observation inside the engine's
 *  `pace` window. */
function toyBoatUnlessPayment(repetitions: number): WaitingFor {
  return {
    type: "UnlessPayment",
    data: {
      player: 0,
      cost: {
        type: "SayPhrase",
        phrase: PHRASE,
        pace: { type: "Quickly", max_gap_ms: GAP_MS } satisfies PhrasePace,
        repetitions,
      },
      pending_effect: {},
      effect_description: "Sacrifice Toy Boat",
    },
  };
}

/** The engine-owned instance a freshly surfaced timed payment carries. */
function paymentFor(owed: number, said = 0): SayPhrasePayment {
  return { id: PAYMENT_ID, owed, said, clock_ms: 0 };
}

function phraseBox() {
  return screen.getByRole("button", { name: new RegExp(PHRASE) });
}

type SayPhraseAttempt = {
  type: string;
  data?: {
    pay?: boolean;
    payment?: number;
    seq?: number;
    at_ms?: number;
  };
};

function sayPhraseCalls() {
  return dispatchMock.mock.calls
    .map(([action]) => action as SayPhraseAttempt)
    .filter((action) => action.type === "SayPhrase")
    .map((action) => action.data ?? {});
}

function payUnlessCostCalls() {
  return dispatchMock.mock.calls
    .map(([action]) => action as SayPhraseAttempt)
    .filter((action) => action.type === "PayUnlessCost")
    .map((action) => action.data?.pay);
}

function setPrompt(next: WaitingFor | null, said: number, owed: number) {
  const state = useGameStore.getState().gameState;
  act(() => {
    useGameStore.setState({
      waitingFor: next,
      gameState: state
        ? {
            ...state,
            say_payment: next ? paymentFor(owed, said) : null,
          }
        : state,
    });
  });
}

/** Stands in for the engine's spoken-payment bookkeeping: each `SayPhrase` is
 *  one accepted saying, the engine's `said` grows, the sayings still owed
 *  shrink, and the last one closes the prompt. `PayUnlessCost` is the explicit
 *  decline path a player chooses; a lapsed window arrives as a `SayPhrase`
 *  stamped past the engine's deadline. */
function acknowledgeSayings() {
  dispatchMock.mockImplementation((action: SayPhraseAttempt) => {
    const current = useGameStore.getState().waitingFor;
    if (
      current?.type !== "UnlessPayment" ||
      current.data.cost.type !== "SayPhrase"
    ) {
      return Promise.resolve();
    }
    if (action.type === "PayUnlessCost") {
      if (action.data?.pay === false) setPrompt(null, 0, 1);
      return Promise.resolve();
    }
    if (action.type !== "SayPhrase") return Promise.resolve();
    const payment = useGameStore.getState().gameState?.say_payment;
    const owed = payment?.owed ?? 1;
    const said = (payment?.said ?? 0) + 1;
    const next: WaitingFor | null =
      said >= owed
        ? null
        : {
            ...current,
            data: {
              ...current.data,
              cost: { ...current.data.cost, repetitions: owed - said },
            },
          };
    setPrompt(next, said, owed);
    return Promise.resolve();
  });
}

/** Acknowledge each accepted saying only when the test flushes it: models a
 *  multiplayer round trip whose reply lands after the pace window was armed. */
function deferAcknowledgements() {
  const pending: Array<() => void> = [];
  dispatchMock.mockImplementation((action: SayPhraseAttempt) => {
    if (action.type !== "SayPhrase") return Promise.resolve();
    pending.push(() => {
      const current = useGameStore.getState().waitingFor;
      if (
        current?.type !== "UnlessPayment" ||
        current.data.cost.type !== "SayPhrase"
      ) {
        return;
      }
      const payment = useGameStore.getState().gameState?.say_payment;
      const owed = payment?.owed ?? 1;
      const said = (payment?.said ?? 0) + 1;
      const next: WaitingFor | null =
        said >= owed
          ? null
          : {
              ...current,
              data: {
                ...current.data,
                cost: { ...current.data.cost, repetitions: owed - said },
              },
            };
      setPrompt(next, said, owed);
    });
    return Promise.resolve();
  });
  return () => {
    const acknowledge = pending.shift();
    if (!acknowledge) throw new Error("no saying is awaiting acknowledgement");
    acknowledge();
  };
}

function advance(ms: number) {
  act(() => {
    vi.advanceTimersByTime(ms);
  });
}

function setup(repetitions: number) {
  const state = gameStateFactory
    .withPlayers(0, 1)
    .waitingFor(toyBoatUnlessPayment(repetitions))
    .build();
  state.say_payment = paymentFor(repetitions);
  act(() => {
    useGameStore.setState({
      gameId: "say-phrase",
      gameMode: "online",
      gameState: state,
      waitingFor: state.waiting_for,
    });
    useMultiplayerStore.setState({ activePlayerId: 0, isSpectator: false });
    usePreferencesStore.setState({
      multiplayerBoardLayout: "focused",
      multiplayerSplitLayoutNudgeDismissed: true,
    });
    useUiStore.setState({ pendingAbilityChoice: null, enchantmentsDialogPlayer: null });
  });
}

describe("GamePage rapid say-it payment (Toy Boat)", () => {
  beforeEach(() => {
    dispatchMock.mockReset();
    acknowledgeSayings();
  });

  afterEach(() => {
    vi.useRealTimers();
    cleanup();
    act(() => {
      useGameStore.setState({ gameId: null, gameState: null, waitingFor: null, adapter: null });
      useMultiplayerStore.setState({ activePlayerId: null, isSpectator: false });
    });
  });

  it("does not expire a single-saying payment before the player says it", async () => {
    setup(1);
    render(gamePageTree());

    expect(await screen.findByText(PHRASE)).toBeInTheDocument();
    vi.useFakeTimers();
    // A one-age-counter upkeep asks for exactly one saying, so there is no
    // pause between sayings to time: reading the prompt must not decline.
    advance(GAP_MS * 5);
    expect(payUnlessCostCalls()).toEqual([]);

    fireEvent.click(phraseBox());
    expect(sayPhraseCalls()).toHaveLength(1);
    expect(payUnlessCostCalls()).toEqual([]);
  });

  it("does not start the window until the first saying, even when more are owed", async () => {
    setup(3);
    render(gamePageTree());

    expect(await screen.findByText(PHRASE)).toBeInTheDocument();
    vi.useFakeTimers();
    // "Without pausing between" times the gaps BETWEEN sayings, so reading the
    // prompt before the first one is not a pause, however long it takes.
    advance(GAP_MS * 10);
    expect(payUnlessCostCalls()).toEqual([]);

    fireEvent.click(phraseBox());
    expect(sayPhraseCalls()).toHaveLength(1);
  });

  it("reports one host-stamped observation per click", async () => {
    setup(2);
    render(gamePageTree());

    await screen.findByText(PHRASE);
    fireEvent.click(phraseBox());
    fireEvent.click(phraseBox());

    const calls = sayPhraseCalls();
    expect(calls).toHaveLength(2);
    expect(calls.map((call) => call.seq)).toEqual([1, 2]);
    expect(calls[0].payment).toBe(PAYMENT_ID);
    expect(calls[1].payment).toBe(PAYMENT_ID);
    // Each observation carries the host's own monotonic instant.
    expect(calls[0].at_ms).toBeTypeOf("number");
    expect(calls[1].at_ms).toBeGreaterThanOrEqual(calls[0].at_ms as number);
    expect(screen.queryByText(PHRASE)).toBeNull();
  });

  it("submits both rapid clicks as distinct saying occurrences", async () => {
    // The real dispatch boundary drops a repeated same-action/same-prompt
    // submission while the first is in flight. Two sayings must therefore be
    // two DIFFERENT observations, or the second saying is silently lost and the
    // payment strands with one saying owed and one in flight.
    setup(2);
    render(gamePageTree());

    await screen.findByText(PHRASE);
    fireEvent.click(phraseBox());
    fireEvent.click(phraseBox());

    const calls = sayPhraseCalls();
    expect(calls).toHaveLength(2);
    expect(calls[0]).not.toEqual(calls[1]);
    expect(new Set(calls.map((call) => call.seq)).size).toBe(2);
  });

  it("reports a lapsed window as a stamped observation for the engine", async () => {
    setup(2);
    render(gamePageTree());

    await screen.findByText(PHRASE);
    vi.useFakeTimers();
    fireEvent.click(phraseBox());
    expect(sayPhraseCalls()).toHaveLength(1);

    advance(GAP_MS * 4);
    // The client observes the lapse; the engine's stored deadline decides that
    // it is a pause and applies the sacrifice. The client does not decline the
    // payment itself.
    expect(payUnlessCostCalls()).toEqual([]);
    const calls = sayPhraseCalls();
    expect(calls).toHaveLength(2);
    expect(calls[1].seq).toBe(2);
  });

  it("arms the window from the click, not from the acknowledgement", async () => {
    const acknowledgeNextSaying = deferAcknowledgements();
    setup(3);
    render(gamePageTree());

    await screen.findByText(PHRASE);
    vi.useFakeTimers();
    fireEvent.click(phraseBox());
    // The first saying comes back: the window between sayings is now live.
    acknowledgeNextSaying();

    advance(GAP_MS / 2);
    fireEvent.click(phraseBox());
    expect(sayPhraseCalls()).toHaveLength(2);

    // Its acknowledgement is slower than the window. The player already said
    // the phrase, so an in-flight observation must not be declined.
    advance(GAP_MS * 4);
    expect(payUnlessCostCalls()).toEqual([]);

    // Once the engine answers, the window is measured from the second click's
    // own host stamp; not saying anything more is the pause the card punishes.
    acknowledgeNextSaying();
    advance(GAP_MS * 4);
    expect(payUnlessCostCalls()).toEqual([]);
    const calls = sayPhraseCalls();
    expect(calls).toHaveLength(3);
    expect(calls[2].seq).toBe(3);
  });

  it("continues the host sequence after a reconnect", async () => {
    setup(2);
    // Mount into a payment already in progress: the engine accepted saying 1 and
    // publishes `last_seq`, so the reconnected panel must continue at 2 rather
    // than replay 1 (which the engine rejects as a duplicate).
    const state = useGameStore.getState().gameState!;
    act(() => {
      useGameStore.setState({
        waitingFor: toyBoatUnlessPayment(1),
        gameState: {
          ...state,
          say_payment: {
            ...paymentFor(2, 1),
            last_seq: 1,
            last_said_ms: 1_000,
            deadline_ms: 2_000,
          },
        },
      });
    });
    render(gamePageTree());

    expect(await screen.findByText(PHRASE)).toBeInTheDocument();
    fireEvent.click(phraseBox());

    const calls = sayPhraseCalls();
    expect(calls).toHaveLength(1);
    expect(calls[0].seq).toBe(2);
    expect(calls[0].payment).toBe(PAYMENT_ID);
  });

  it("renders the engine's own saying progress", async () => {
    setup(2);
    render(gamePageTree());

    // Longer-than-default waits: the full suite runs this file alongside
    // hundreds of others, and a 1s default can lapse under that load.
    expect(await screen.findByText("0 of 2 said", {}, { timeout: 5_000 })).toBeInTheDocument();
    fireEvent.click(phraseBox());
    expect(await screen.findByText("1 of 2 said", {}, { timeout: 5_000 })).toBeInTheDocument();
  });

  it("lets a following prompt start a fresh payment", async () => {
    setup(1);
    render(gamePageTree());

    await screen.findByText(PHRASE);
    fireEvent.click(phraseBox());
    expect(sayPhraseCalls()).toHaveLength(1);

    // Successive Toy Boat upkeeps replace the prompt at the same position, so
    // the retained panel must not inherit the finished payment.
    const state = useGameStore.getState().gameState!;
    act(() => {
      useGameStore.setState({
        waitingFor: toyBoatUnlessPayment(1),
        gameState: { ...state, say_payment: { ...paymentFor(1), id: PAYMENT_ID + 1 } },
      });
    });
    fireEvent.click(phraseBox());

    const calls = sayPhraseCalls();
    expect(calls).toHaveLength(2);
    expect(calls[1].payment).toBe(PAYMENT_ID + 1);
    expect(calls[1].seq).toBe(1);
  });
});
