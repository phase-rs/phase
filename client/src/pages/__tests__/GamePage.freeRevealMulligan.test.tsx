import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { MemoryRouter, Route, Routes } from "react-router";

import type { GameAction, WaitingFor } from "../../adapter/types.ts";
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

const FREE_REVEAL: GameAction = {
  type: "MulliganDecision",
  data: { choice: { type: "FreeReveal" } },
};
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

// The viewer (seat 0) has declared a free reveal and waits on seat 1.
const VIEWER_DECLARED: WaitingFor = {
  type: "MulliganDecision",
  data: {
    pending: [{ player: 1, mulligan_count: 0, phase: { type: "Declare" } }],
    free_first_mulligan: false,
    declared: [{ player: 0, mulligan_count: 0, kind: { type: "FreeReveal" } }],
  },
};

function showMulligan(waitingFor: WaitingFor, legalActions: GameAction[]) {
  const state = gameStateFactory.withPlayers(0, 1).waitingFor(waitingFor).build();
  act(() => {
    useGameStore.setState({
      gameId: "free-reveal",
      gameMode: "online",
      gameState: state,
      waitingFor,
      legalActions,
    });
  });
}

function renderGamePage() {
  return render(
    <MemoryRouter initialEntries={["/game/free-reveal?mode=join"]}>
      <Routes>
        <Route path="/game/:id" element={<GamePage />} />
      </Routes>
    </MemoryRouter>,
  );
}

describe("GamePage free-reveal mulligan", () => {
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

  it("shows the free-reveal button for an engine-issued action and dispatches it", async () => {
    showMulligan(OPEN_ROUND, [KEEP, MULLIGAN, FREE_REVEAL]);
    renderGamePage();

    fireEvent.click(await screen.findByRole("button", { name: "Reveal and redraw (free)" }));

    expect(dispatchMock).toHaveBeenCalledTimes(1);
    expect(dispatchMock).toHaveBeenCalledWith(FREE_REVEAL);
  });

  it("derives no free-reveal button when the engine issued none", async () => {
    showMulligan(OPEN_ROUND, [KEEP, MULLIGAN]);
    renderGamePage();

    expect(await screen.findByRole("button", { name: "Keep Hand" })).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Reveal and redraw (free)" })).toBeNull();
  });

  it("offers the free reveal in the fallback modal when the viewer's seat is unknown", async () => {
    const state = gameStateFactory.withPlayers(1).waitingFor(OPEN_ROUND).build();
    act(() => {
      useGameStore.setState({
        gameId: "free-reveal",
        gameMode: "online",
        gameState: state,
        waitingFor: OPEN_ROUND,
        legalActions: [KEEP, MULLIGAN, FREE_REVEAL],
      });
    });
    renderGamePage();

    fireEvent.click(await screen.findByText("Reveal and redraw (free)"));

    expect(dispatchMock).toHaveBeenCalledWith(FREE_REVEAL);
  });

  it("shows a seat that already declared no prompt at all", async () => {
    showMulligan(OPEN_ROUND, [KEEP, MULLIGAN, FREE_REVEAL]);
    renderGamePage();
    expect(await screen.findByRole("button", { name: "Keep Hand" })).toBeInTheDocument();

    showMulligan(VIEWER_DECLARED, [FREE_REVEAL]);

    await waitFor(() => expect(screen.queryByRole("button", { name: "Keep Hand" })).toBeNull());
    expect(screen.queryByRole("button", { name: "Reveal and redraw (free)" })).toBeNull();
  });
});
