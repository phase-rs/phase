import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { MemoryRouter, Route, Routes } from "react-router";

import type { PhrasePace, WaitingFor } from "../../adapter/types.ts";
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
 *  phrase still owed, each reported one click at a time inside the engine's
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

function phraseBox() {
  return screen.getByRole("button", { name: new RegExp(PHRASE) });
}

function payUnlessCostCalls() {
  return dispatchMock.mock.calls
    .map(([action]) => action as { type: string; data?: { pay?: boolean } })
    .filter((action) => action.type === "PayUnlessCost")
    .map((action) => action.data?.pay);
}

/** Stands in for the engine's spoken-payment bookkeeping: each accepted saying
 *  is one `PayUnlessCost`, the sayings still owed shrink by one, and the last
 *  one closes the prompt. */
function acknowledgeSayings() {
  dispatchMock.mockImplementation((action: { type: string; data?: { pay?: boolean } }) => {
    const current = useGameStore.getState().waitingFor;
    if (
      action.type !== "PayUnlessCost" ||
      action.data?.pay !== true ||
      current?.type !== "UnlessPayment" ||
      current.data.cost.type !== "SayPhrase"
    ) {
      return Promise.resolve();
    }
    const owed = Math.max(1, current.data.cost.repetitions ?? 1);
    const next: WaitingFor | null =
      owed <= 1
        ? null
        : {
            ...current,
            data: {
              ...current.data,
              cost: { ...current.data.cost, repetitions: owed - 1 },
            },
          };
    act(() => {
      useGameStore.setState({ waitingFor: next });
    });
    return Promise.resolve();
  });
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
    expect(payUnlessCostCalls()).toEqual([true]);
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
    expect(payUnlessCostCalls()).toEqual([true]);
  });

  it("reports one saying per click and lets the engine ask for the rest", async () => {
    setup(2);
    render(gamePageTree());

    await screen.findByText(PHRASE);
    fireEvent.click(phraseBox());
    expect(payUnlessCostCalls()).toEqual([true]);
    // The engine still owes a saying, so the prompt is still up for it.
    expect(phraseBox()).toBeInTheDocument();

    fireEvent.click(phraseBox());
    expect(payUnlessCostCalls()).toEqual([true, true]);
    expect(screen.queryByText(PHRASE)).toBeNull();
  });

  it("declines when the next saying does not follow inside the window", async () => {
    setup(2);
    render(gamePageTree());

    await screen.findByText(PHRASE);
    vi.useFakeTimers();
    fireEvent.click(phraseBox());
    expect(payUnlessCostCalls()).toEqual([true]);

    advance(GAP_MS * 4);
    expect(payUnlessCostCalls()).toEqual([true, false]);
  });

  it("pays after one saying per repetition inside the window", async () => {
    setup(2);
    render(gamePageTree());

    await screen.findByText(PHRASE);
    vi.useFakeTimers();
    fireEvent.click(phraseBox());
    advance(GAP_MS / 4);
    fireEvent.click(phraseBox());

    expect(payUnlessCostCalls()).toEqual([true, true]);
  });

  it("lets a following prompt start a fresh payment", async () => {
    setup(1);
    render(gamePageTree());

    await screen.findByText(PHRASE);
    fireEvent.click(phraseBox());
    expect(payUnlessCostCalls()).toEqual([true]);

    // Successive Toy Boat upkeeps replace the prompt at the same position, so
    // the retained panel must not inherit the finished payment.
    act(() => {
      useGameStore.setState({ waitingFor: toyBoatUnlessPayment(1) });
    });
    fireEvent.click(phraseBox());
    expect(payUnlessCostCalls()).toEqual([true, true]);
  });
});
