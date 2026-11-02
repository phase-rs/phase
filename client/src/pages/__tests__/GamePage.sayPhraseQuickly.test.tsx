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
 *  phrase, each after the previous one inside the engine's `pace` window. */
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

async function settle(ms: number) {
  await act(async () => {
    await new Promise((resolve) => setTimeout(resolve, ms));
  });
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
    dispatchMock.mockClear();
  });

  afterEach(() => {
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
    // A one-age-counter upkeep asks for exactly one saying, so there is no
    // pause between sayings to time: reading the prompt must not decline.
    await settle(GAP_MS * 5);
    expect(payUnlessCostCalls()).toEqual([]);

    fireEvent.click(phraseBox());
    expect(payUnlessCostCalls()).toEqual([true]);
  });

  it("declines when the next saying does not follow inside the window", async () => {
    setup(2);
    render(gamePageTree());

    await screen.findByText(PHRASE);
    fireEvent.click(phraseBox());
    expect(payUnlessCostCalls()).toEqual([]);

    await settle(GAP_MS * 4);
    expect(payUnlessCostCalls()).toEqual([false]);
  });

  it("pays after one saying per repetition inside the window", async () => {
    setup(2);
    render(gamePageTree());

    await screen.findByText(PHRASE);
    fireEvent.click(phraseBox());
    await settle(GAP_MS / 4);
    fireEvent.click(phraseBox());

    expect(payUnlessCostCalls()).toEqual([true]);
  });
});
