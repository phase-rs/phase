import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { MemoryRouter, Route, Routes } from "react-router";

import { useGameStore } from "../../stores/gameStore.ts";
import { useMultiplayerStore } from "../../stores/multiplayerStore.ts";
import { usePreferencesStore } from "../../stores/preferencesStore.ts";
import { useUiStore } from "../../stores/uiStore.ts";
import { gameObjectFactory } from "../../test/factories/gameObjectFactory.ts";
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

const CARD_A = 201;
const CARD_B = 202;

/** CR 118.12a: the same prompt text for every card in the loop; only the named
 *  object tells the payer which card their payment decides. */
function paymentPrompt(player: number, affectedObject: number | null) {
  return {
    type: "UnlessPayment",
    data: {
      player,
      cost: { type: "PayLife", amount: 3 },
      pending_effect: null,
      effect_description: "put that card into your hand",
      remaining: [],
      affected_object: affectedObject,
    },
  };
}

function gamePageTree() {
  return (
    <MemoryRouter initialEntries={["/game/unless-identity?mode=join"]}>
      <Routes>
        <Route path="/game/:id" element={<GamePage />} />
      </Routes>
    </MemoryRouter>
  );
}

describe("GamePage unless-payment card identity", () => {
  beforeEach(() => {
    dispatchMock.mockClear();
    const cardA = gameObjectFactory
      .creature(1, 1)
      .named("Revealed Alpha")
      .withId(CARD_A)
      .build();
    const cardB = gameObjectFactory
      .creature(1, 1)
      .named("Revealed Beta")
      .withId(CARD_B)
      .build();
    const state = gameStateFactory.withPlayers(0, 1, 2).withObjects(cardA, cardB).build();
    act(() => {
      useGameStore.setState({
        gameId: "unless-identity",
        gameMode: "online",
        gameState: state,
        waitingFor: paymentPrompt(1, CARD_A) as never,
      });
      useMultiplayerStore.setState({ activePlayerId: 1, isSpectator: false });
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

  it("names the card each prompt decides for every opponent and after a restore", async () => {
    render(gamePageTree());

    // First opponent's prompt for the first card.
    expect(await screen.findByText("Revealed Alpha")).toBeInTheDocument();
    expect(screen.queryByText("Revealed Beta")).not.toBeInTheDocument();

    // The next opponent's client is asked about the SAME card: identity carries.
    act(() => {
      useMultiplayerStore.setState({ activePlayerId: 2 });
      useGameStore.setState({ waitingFor: paymentPrompt(2, CARD_A) as never });
    });
    expect(await screen.findByText("Revealed Alpha")).toBeInTheDocument();

    // The following card of the sequence replaces the prompt's identity.
    act(() => {
      useGameStore.setState({ waitingFor: paymentPrompt(2, CARD_B) as never });
    });
    expect(await screen.findByText("Revealed Beta")).toBeInTheDocument();
    expect(screen.queryByText("Revealed Alpha")).not.toBeInTheDocument();

    // A reconnect restore republishes the engine prompt; the identity survives.
    act(() => {
      useGameStore.setState({ waitingFor: paymentPrompt(2, CARD_B) as never });
    });
    expect(await screen.findByText("Revealed Beta")).toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: /^Pay/ }));
    expect(dispatchMock).toHaveBeenCalledWith({
      type: "PayUnlessCost",
      data: { pay: true },
    });
  });
});
