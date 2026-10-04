import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { MemoryRouter, Route, Routes } from "react-router";

import { useGameStore } from "../../stores/gameStore.ts";
import { useMultiplayerStore } from "../../stores/multiplayerStore.ts";
import { usePreferencesStore } from "../../stores/preferencesStore.ts";
import { useUiStore } from "../../stores/uiStore.ts";
import { gameObjectFactory } from "../../test/factories/gameObjectFactory.ts";
import {
  gameStateFactory,
  targetSelectionProgressFactory,
  targetSelectionSlotFactory,
} from "../../test/factories/gameStateFactory.ts";
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

const SOURCE_ID = 100;
const SUBJECT_ID = 44;

function gamePageTree() {
  return (
    <MemoryRouter initialEntries={["/game/optional-choice?mode=join"]}>
      <Routes>
        <Route path="/game/:id" element={<GamePage />} />
      </Routes>
    </MemoryRouter>
  );
}

function renderGamePage() {
  return render(gamePageTree());
}

describe("GamePage optional-effect routed player gate", () => {
  beforeEach(() => {
    dispatchMock.mockClear();
    const source = gameObjectFactory
      .creature(3, 3)
      .legendary()
      .named("Bre of Clan Stoutarm")
      .withId(SOURCE_ID)
      .build();
    const subject = gameObjectFactory
      .creature(2, 2)
      .inExile()
      .named("Grizzly Bears")
      .withId(SUBJECT_ID)
      .build();
    const state = gameStateFactory
      .withPlayers(0, 1)
      .withObjects(source, subject)
      .optionalEffectChoice({
        player: 0,
        source_id: source.id,
        decision_subject_id: subject.id,
        may_trigger_key: {
          player: 0,
          source_id: source.id,
          origin: { type: "Printed", trigger_index: 0 },
        },
      })
      .build();

    act(() => {
      useGameStore.setState({
        gameId: "optional-choice",
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
  });

  afterEach(() => {
    cleanup();
    act(() => {
      useGameStore.setState({ gameId: null, gameState: null, waitingFor: null, adapter: null });
      useMultiplayerStore.setState({ activePlayerId: null, isSpectator: false });
    });
  });

  it("shows the projected subject only for the routed acting seat", async () => {
    const view = renderGamePage();

    expect(useGameStore.getState().waitingFor?.type).toBe("OptionalEffectChoice");
    expect(
      await screen.findByRole("dialog", { name: "Bre of Clan Stoutarm - Optional Effect" }),
    ).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Yes" })).toBeInTheDocument();
    expect(screen.getByRole("img", { name: "Grizzly Bears" })).toBeInTheDocument();
    expect(screen.queryByRole("img", { name: "Bre of Clan Stoutarm" })).not.toBeInTheDocument();

    act(() => {
      useMultiplayerStore.setState({ activePlayerId: 1 });
      view.rerender(gamePageTree());
    });

    await waitFor(() => {
      expect(
        screen.queryByRole("dialog", { name: "Bre of Clan Stoutarm - Optional Effect" }),
      ).not.toBeInTheDocument();
      expect(screen.queryByRole("button", { name: "Yes" })).not.toBeInTheDocument();
      expect(screen.queryByRole("img", { name: "Grizzly Bears" })).not.toBeInTheDocument();
    });
  });

  it("preserves the remembered choice across snapshot updates and peek/restore", async () => {
    renderGamePage();

    const remember = await screen.findByRole("checkbox", { name: "Don't ask again this game" });
    fireEvent.click(remember);
    expect(remember).toBeChecked();

    act(() => {
      const state = useGameStore.getState().gameState!;
      useGameStore.setState({ gameState: { ...state, turn_number: state.turn_number + 1 } });
    });

    expect(screen.getByRole("checkbox", { name: "Don't ask again this game" })).toBeChecked();
    fireEvent.click(screen.getByRole("button", { name: "Move dialog out of the way" }));
    fireEvent.click(screen.getByRole("button", { name: "Restore dialog" }));
    expect(screen.getByRole("checkbox", { name: "Don't ask again this game" })).toBeChecked();
    fireEvent.click(screen.getByRole("button", { name: "Yes" }));

    expect(dispatchMock).toHaveBeenCalledExactlyOnceWith({
      type: "DecideOptionalEffectAndRemember",
      data: { choice: { type: "Accept" }, scope: { type: "ExactInstance" } },
    });
  });

  it("updates the targeting prompt offset when the multiplayer layout changes", async () => {
    const target = gameObjectFactory.creature(2, 2).onBattlefield().build();
    const state = gameStateFactory
      .withPlayers(0, 1, 2)
      .withObjects(target)
      .targetSelection({
        player: 0,
        target_slots: [targetSelectionSlotFactory.build({ legal_targets: [{ Object: target.id }] })],
        selection: targetSelectionProgressFactory.build({ current_legal_targets: [{ Object: target.id }] }),
      })
      .build();
    act(() => useGameStore.setState({ gameState: state, waitingFor: state.waiting_for }));
    renderGamePage();

    const prompt = await screen.findByText("Choose a target");
    // The DOM test checks the live inherited value; browser paint/layout is
    // verified separately because the test environment does not lay out CSS.
    const offsetSource = prompt.closest<HTMLElement>("[style*='--game-targeting-prompt-top']");
    expect(offsetSource?.style.getPropertyValue("--game-targeting-prompt-top")).toBe("0.25rem");

    act(() => usePreferencesStore.setState({ multiplayerBoardLayout: "split" }));
    expect(offsetSource?.style.getPropertyValue("--game-targeting-prompt-top")).toBe("4.75rem");
    expect(prompt).toBeInTheDocument();

    act(() => usePreferencesStore.setState({ multiplayerBoardLayout: "focused" }));
    expect(offsetSource?.style.getPropertyValue("--game-targeting-prompt-top")).toBe("0.25rem");
  });
});
