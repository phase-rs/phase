import { act, cleanup, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { MemoryRouter, Route, Routes } from "react-router";

import type { DerivedViews, GameAction, WaitingFor } from "../../adapter/types.ts";
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

const KEEP: GameAction = { type: "MulliganDecision", data: { choice: { type: "Keep" } } };
const MULLIGAN: GameAction = {
  type: "MulliganDecision",
  data: { choice: { type: "Mulligan" } },
};

const OPEN_ROUND: WaitingFor = {
  type: "MulliganDecision",
  data: {
    pending: [
      { player: 0, mulligan_count: 0, phase: { type: "Declare" } },
      { player: 1, mulligan_count: 0, phase: { type: "Declare" } },
    ],
    free_first_mulligan: false,
  },
};

// The viewer (seat 0) took one mulligan and now puts a card on the bottom.
const BOTTOM_ONE: WaitingFor = {
  type: "MulliganDecision",
  data: {
    pending: [
      {
        player: 0,
        mulligan_count: 1,
        phase: { type: "BottomCards", count: 1, then: { type: "Keep" } },
      },
    ],
    free_first_mulligan: false,
  },
};

function showOpeningHand(waitingFor: WaitingFor, derived?: DerivedViews) {
  const state = gameStateFactory
    .withPlayers(0, 1)
    .waitingFor(waitingFor)
    .build(derived ? { derived } : {});
  act(() => {
    useGameStore.setState({
      gameId: "opening-turn-position",
      gameMode: "online",
      gameState: state,
      waitingFor,
      legalActions: [KEEP, MULLIGAN],
    });
  });
}

function renderGamePage() {
  return render(
    <MemoryRouter initialEntries={["/game/opening-turn-position?mode=join"]}>
      <Routes>
        <Route path="/game/:id" element={<GamePage />} />
      </Routes>
    </MemoryRouter>,
  );
}

describe("GamePage opening turn position", () => {
  beforeEach(() => {
    dispatchMock.mockClear();
    act(() => {
      useMultiplayerStore.setState({ activePlayerId: 0, isSpectator: false });
      usePreferencesStore.setState({
        multiplayerBoardLayout: "focused",
        multiplayerSplitLayoutNudgeDismissed: true,
      });
      useUiStore.setState({ pendingAbilityChoice: null, enchantmentsDialogPlayer: null });
    });
  });

  afterEach(() => {
    cleanup();
    act(() => {
      useGameStore.setState({
        gameId: null,
        gameState: null,
        waitingFor: null,
        adapter: null,
        legalActions: [],
      });
      useMultiplayerStore.setState({ activePlayerId: null, isSpectator: false });
    });
  });

  it("shows the draw position on the opening-hand panel", async () => {
    showOpeningHand(OPEN_ROUND, { viewer_play_draw: "OnTheDraw" });
    renderGamePage();

    expect(await screen.findByText("You're on the draw")).toBeInTheDocument();
    expect(screen.getByRole("heading", { name: "Review your opening hand" })).toBeInTheDocument();
    expect(screen.queryByText("You're on the play")).toBeNull();
  });

  it("shows the play position on the opening-hand panel", async () => {
    showOpeningHand(OPEN_ROUND, { viewer_play_draw: "OnThePlay" });
    renderGamePage();

    expect(await screen.findByText("You're on the play")).toBeInTheDocument();
    expect(screen.getByRole("heading", { name: "Review your opening hand" })).toBeInTheDocument();
    expect(screen.queryByText("You're on the draw")).toBeNull();
  });

  it("shows the viewer's turn number when there is no play/draw position", async () => {
    showOpeningHand(OPEN_ROUND, { viewer_turn_number: 3 });
    renderGamePage();

    expect(await screen.findByText("You take turn 3")).toBeInTheDocument();
    expect(screen.getByRole("heading", { name: "Review your opening hand" })).toBeInTheDocument();
  });

  it("prefers the play/draw position over the turn number", async () => {
    showOpeningHand(OPEN_ROUND, { viewer_play_draw: "OnThePlay", viewer_turn_number: 2 });
    renderGamePage();

    expect(await screen.findByText("You're on the play")).toBeInTheDocument();
    expect(screen.getByRole("heading", { name: "Review your opening hand" })).toBeInTheDocument();
    expect(screen.queryByText(/You take turn/)).toBeNull();
  });

  it("renders no position badge without an engine value", async () => {
    showOpeningHand(OPEN_ROUND);
    renderGamePage();

    expect(
      await screen.findByRole("heading", { name: "Review your opening hand" }),
    ).toBeInTheDocument();
    expect(screen.queryByText(/You're on the/)).toBeNull();
    expect(screen.queryByText(/You take turn/)).toBeNull();
  });

  it("shows the position on the bottom-cards panel too", async () => {
    showOpeningHand(BOTTOM_ONE, { viewer_play_draw: "OnThePlay" });
    renderGamePage();

    expect(await screen.findByText("You're on the play")).toBeInTheDocument();
    expect(screen.getByRole("heading", { name: "Put 1 card on the bottom" })).toBeInTheDocument();
  });
});
