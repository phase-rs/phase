import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { MemoryRouter, Route, Routes } from "react-router";

import type { PlayerId, WaitingFor } from "../../adapter/types.ts";
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
// The overlay's renderer is not under test; only its `diceRoll` gate is.
vi.mock("../../components/animation/DiceRollOverlay.tsx", () => ({
  DiceRollOverlay: () => null,
}));

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

const TITLE = "Choose Who Plays First";
const PLAY_DRAW_TITLE = "Game 2: Choose Play or Draw";

function startingPlayerPrompt(chooser: PlayerId, candidates: PlayerId[]): WaitingFor {
  return { type: "ChooseStartingPlayer", data: { player: chooser, candidates } };
}

function showPrompt(waitingFor: WaitingFor, seats: PlayerId[]) {
  const state = gameStateFactory
    .withPlayers(...seats)
    .waitingFor(waitingFor)
    .build();
  act(() => {
    useGameStore.setState({
      gameId: "starting-player-choice",
      gameMode: "online",
      gameState: state,
      waitingFor,
      legalActions: [],
    });
  });
}

function renderGamePage() {
  return render(
    <MemoryRouter initialEntries={["/game/starting-player-choice?mode=join"]}>
      <Routes>
        <Route path="/game/:id" element={<GamePage />} />
      </Routes>
    </MemoryRouter>,
  );
}

describe("GamePage starting-player choice", () => {
  beforeEach(() => {
    dispatchMock.mockClear();
    act(() => {
      useMultiplayerStore.setState({
        activePlayerId: 0,
        isSpectator: false,
        playerNames: new Map([
          [0, "Alice"],
          [1, "Bob"],
          [2, "Cara"],
        ]),
      });
      usePreferencesStore.setState({
        multiplayerBoardLayout: "focused",
        multiplayerSplitLayoutNudgeDismissed: true,
      });
      useUiStore.setState({
        pendingAbilityChoice: null,
        enchantmentsDialogPlayer: null,
        diceRoll: null,
      });
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
      useMultiplayerStore.setState({
        activePlayerId: null,
        isSpectator: false,
        playerNames: new Map(),
      });
      useUiStore.setState({ diceRoll: null });
    });
  });

  it("chooser dispatches the chosen seat", async () => {
    showPrompt(startingPlayerPrompt(0, [0, 1]), [0, 1]);
    renderGamePage();

    expect(await screen.findByText(TITLE)).toBeInTheDocument();
    expect(screen.getByRole("button", { name: /Play First/ })).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: /Bob plays first/ }));

    expect(dispatchMock).toHaveBeenCalledWith({
      type: "ChooseStartingPlayer",
      data: { player: 1 },
    });
  });

  it("renders one option per engine candidate, in engine order", async () => {
    showPrompt(startingPlayerPrompt(0, [2, 0, 1]), [0, 1, 2]);
    renderGamePage();

    expect(await screen.findByText(TITLE)).toBeInTheDocument();
    const labels = screen
      .getAllByRole("button", { name: /plays first|Play First/ })
      .map((button) => button.textContent);
    expect(labels).toEqual([
      "Cara plays first",
      "Play FirstTake the first turn",
      "Bob plays first",
    ]);
  });

  it("non-chooser sees the chooser's name", async () => {
    act(() => {
      useMultiplayerStore.setState({ activePlayerId: 1 });
    });
    showPrompt(startingPlayerPrompt(0, [0, 1]), [0, 1]);
    renderGamePage();

    expect(
      await screen.findByText("Alice is choosing who plays first…"),
    ).toBeInTheDocument();
    expect(screen.queryByText(TITLE)).toBeNull();
    expect(screen.queryByRole("button", { name: /plays first|Play First/ })).toBeNull();
  });

  it("spectator sees the chooser's name, not the chooser's prompt", async () => {
    act(() => {
      useMultiplayerStore.setState({ activePlayerId: 0, isSpectator: true });
    });
    showPrompt(startingPlayerPrompt(0, [0, 1]), [0, 1]);
    renderGamePage();

    expect(
      await screen.findByText("Alice is choosing who plays first…"),
    ).toBeInTheDocument();
    expect(screen.queryByText(TITLE)).toBeNull();
    expect(screen.queryByRole("button", { name: /plays first|Play First/ })).toBeNull();
  });

  it("spectator is not shown the between-games play/draw prompt", async () => {
    act(() => {
      useMultiplayerStore.setState({ activePlayerId: 0, isSpectator: true });
    });
    showPrompt(
      {
        type: "BetweenGamesChoosePlayDraw",
        data: { player: 0, game_number: 2, score: { p0_wins: 1, p1_wins: 0, draws: 0 } },
      },
      [0, 1],
    );
    renderGamePage();

    expect(screen.queryByText(PLAY_DRAW_TITLE)).toBeNull();
    expect(screen.queryByRole("button", { name: /Play First|Draw First/ })).toBeNull();

    // Reach guard: the same seat, seated rather than spectating, is shown the prompt.
    act(() => {
      useMultiplayerStore.setState({ isSpectator: false });
    });
    expect(await screen.findByText(PLAY_DRAW_TITLE)).toBeInTheDocument();
  });

  it("prompt waits for the starting contest", async () => {
    act(() => {
      useUiStore.setState({
        diceRoll: {
          kind: "die",
          sides: 20,
          rolls: [
            { playerId: 0, value: 17 },
            { playerId: 1, value: 4 },
          ],
          context: "startingPlayer",
          winner: 0,
        },
      });
    });
    showPrompt(startingPlayerPrompt(0, [0, 1]), [0, 1]);
    renderGamePage();

    expect(screen.queryByText(TITLE)).toBeNull();
    expect(screen.queryByText(/is choosing who plays first/)).toBeNull();

    act(() => {
      useUiStore.setState({ diceRoll: null });
    });
    expect(await screen.findByText(TITLE)).toBeInTheDocument();
  });
});
