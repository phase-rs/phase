import { act, cleanup, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { MemoryRouter, Route, Routes } from "react-router";

import { useGameStore } from "../../stores/gameStore.ts";
import { useMultiplayerStore } from "../../stores/multiplayerStore.ts";
import { usePreferencesStore } from "../../stores/preferencesStore.ts";
import { useUiStore } from "../../stores/uiStore.ts";
import type { GameState } from "../../adapter/types.ts";
import { buildGameObject, buildObjectMap } from "../../test/factories/gameObjectFactory.ts";
import {
  buildGameState,
  buildPendingCast,
  buildPlayers,
  buildPriorityWaitingFor,
  buildTargetSelectionProgress,
  buildTargetSelectionSlot,
  buildTargetSelectionWaitingFor,
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
  useCardBackImage: () => ({ src: null, isLoading: false }),
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

function gamePageTree() {
  return (
    <MemoryRouter initialEntries={["/game/shared-piles?mode=join"]}>
      <Routes>
        <Route path="/game/:id" element={<GamePage />} />
      </Routes>
    </MemoryRouter>
  );
}

const SHARED = { shared_piles: { library: 0, graveyard: 0 } };

function seed(state: GameState, viewerSeat: number) {
  act(() => {
    useGameStore.setState({
      gameId: "shared-piles",
      gameMode: "online",
      gameState: state,
      waitingFor: state.waiting_for,
    });
    useMultiplayerStore.setState({ activePlayerId: viewerSeat, isSpectator: false });
    usePreferencesStore.setState({
      multiplayerBoardLayout: "focused",
      multiplayerSplitLayoutNudgeDismissed: true,
    });
    useUiStore.setState({ pendingAbilityChoice: null, enchantmentsDialogPlayer: null });
  });
}

function pileState({
  shared,
  waitingFor = buildPriorityWaitingFor(),
}: {
  shared: boolean;
  waitingFor?: GameState["waiting_for"];
}): GameState {
  const gy0 = buildGameObject({ id: 101, owner: 0, zone: "Graveyard" });
  const gy1 = buildGameObject({ id: 102, owner: 1, zone: "Graveyard" });
  const lib0 = buildGameObject({ id: 11, owner: 0, zone: "Library" });
  const lib1 = buildGameObject({ id: 21, owner: 1, zone: "Library" });
  const ex0 = buildGameObject({ id: 301, owner: 0, zone: "Exile", display_visible_to_viewer: true });
  const ex1 = buildGameObject({ id: 302, owner: 1, zone: "Exile", display_visible_to_viewer: true });
  return buildGameState({
    players: buildPlayers(
      shared
        ? [{ id: 0, graveyard: [101, 102], library: [11, 21] }, { id: 1 }]
        : [{ id: 0, graveyard: [101], library: [11] }, { id: 1, graveyard: [102], library: [21] }],
    ),
    objects: buildObjectMap(gy0, gy1, lib0, lib1, ex0, ex1),
    battlefield: [],
    exile: [301, 302],
    stack: [],
    waiting_for: waitingFor,
    ...(shared ? { derived: SHARED } : {}),
  });
}

const anchors = (attr: string) =>
  [...document.querySelectorAll(`[${attr}]`)].map((el) => el.getAttribute(attr));

function bothGraveyardsPrompt() {
  return buildTargetSelectionWaitingFor({
    data: {
      player: 1,
      pending_cast: buildPendingCast(),
      target_slots: [
        buildTargetSelectionSlot({ legal_targets: [{ Object: 101 }, { Object: 102 }] }),
      ],
      selection: buildTargetSelectionProgress({
        current_legal_targets: [{ Object: 101 }, { Object: 102 }],
      }),
    },
  });
}

describe("GamePage shared piles", () => {
  afterEach(() => {
    cleanup();
    act(() => {
      useGameStore.setState({ gameId: null, gameState: null, waitingFor: null, adapter: null });
      useMultiplayerStore.setState({ activePlayerId: null, isSpectator: false });
    });
  });

  it.each([0, 1])("renders the shared library and graveyard once, for viewer seat %i", (seat) => {
    seed(pileState({ shared: true }), seat);
    render(gamePageTree());

    expect(anchors("data-library-pile")).toEqual(["0"]);
    expect(anchors("data-graveyard-pile")).toEqual(["0"]);
    // Exile stays per seat: one pile each.
    expect(anchors("data-exile-pile").sort()).toEqual(["0", "1"]);
  });

  it("renders each seat's own library and graveyard when nothing is shared", () => {
    seed(pileState({ shared: false }), 0);
    render(gamePageTree());

    expect(anchors("data-library-pile").sort()).toEqual(["0", "1"]);
    expect(anchors("data-graveyard-pile").sort()).toEqual(["0", "1"]);
  });

  it("opens one graveyard viewer for a prompt over a shared graveyard's cards of both owners", async () => {
    const prompt = bothGraveyardsPrompt();
    seed(pileState({ shared: true, waitingFor: prompt }), 1);
    render(gamePageTree());

    expect(await screen.findByRole("dialog", { name: /Graveyard/ })).toBeInTheDocument();
  });

  it("leaves a prompt over two seats' own graveyards for the player to choose", () => {
    const prompt = bothGraveyardsPrompt();
    seed(pileState({ shared: false, waitingFor: prompt }), 1);
    render(gamePageTree());

    expect(screen.queryByRole("dialog", { name: /Graveyard/ })).not.toBeInTheDocument();
    // Reach-guard: the prompt itself rendered.
    expect(screen.getByText("Choose a target")).toBeInTheDocument();
  });
});
