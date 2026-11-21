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

/** CR 118.12a: a disjunctive unless prompt ("unless [player] pays A or B")
 *  names its object the same way, through the cost-choice modal. */
function chooseCostPrompt(player: number, affectedObject: number | null) {
  return {
    type: "UnlessPaymentChooseCost",
    data: {
      player,
      costs: [
        { type: "PayLife", amount: 3 },
        { type: "PayLife", amount: 5 },
      ],
      pending_effect: null,
      effect_description: "put that card into your hand",
      affected_object: affectedObject,
    },
  };
}

function seedPrompt(waitingFor: unknown, activePlayerId: number) {
  const cardA = gameObjectFactory.creature(1, 1).named("Revealed Alpha").withId(CARD_A).build();
  const cardB = gameObjectFactory.creature(1, 1).named("Revealed Beta").withId(CARD_B).build();
  const state = gameStateFactory.withPlayers(0, 1, 2).withObjects(cardA, cardB).build();
  act(() => {
    useGameStore.setState({
      gameId: "unless-identity",
      gameMode: "online",
      gameState: state,
      waitingFor: waitingFor as never,
    });
    useMultiplayerStore.setState({ activePlayerId, isSpectator: false });
  });
}

function clearSession() {
  act(() => {
    useGameStore.setState({ gameId: null, gameState: null, waitingFor: null, adapter: null });
    useMultiplayerStore.setState({ activePlayerId: null, isSpectator: false });
  });
}

describe("GamePage unless-payment card identity", () => {
  beforeEach(() => {
    dispatchMock.mockClear();
    act(() => {
      usePreferencesStore.setState({
        multiplayerBoardLayout: "focused",
        multiplayerSplitLayoutNudgeDismissed: true,
      });
      useUiStore.setState({ pendingAbilityChoice: null, enchantmentsDialogPlayer: null });
    });
  });

  afterEach(() => {
    cleanup();
    clearSession();
  });

  it("names the card each prompt decides for every opponent and after a restore", async () => {
    seedPrompt(paymentPrompt(1, CARD_A), 1);
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
    expect(screen.queryByText("Revealed Beta")).not.toBeInTheDocument();

    // The following card of the sequence replaces the prompt's identity.
    act(() => {
      useGameStore.setState({ waitingFor: paymentPrompt(2, CARD_B) as never });
    });
    expect(await screen.findByText("Revealed Beta")).toBeInTheDocument();
    expect(screen.queryByText("Revealed Alpha")).not.toBeInTheDocument();

    // Reconnect: the page is torn down with the session cleared, then a fresh
    // mount receives the restored engine prompt. The identity is read again
    // from the restored payload, not from anything the first mount held.
    cleanup();
    clearSession();
    expect(screen.queryByText("Revealed Beta")).not.toBeInTheDocument();
    seedPrompt(paymentPrompt(2, CARD_B), 2);
    render(gamePageTree());
    expect(await screen.findByText("Revealed Beta")).toBeInTheDocument();
    expect(screen.queryByText("Revealed Alpha")).not.toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: /^Pay/ }));
    expect(dispatchMock).toHaveBeenCalledWith({
      type: "PayUnlessCost",
      data: { pay: true },
    });
  });

  it("names the card in the disjunctive cost-choice prompt", async () => {
    seedPrompt(chooseCostPrompt(1, CARD_B), 1);
    render(gamePageTree());
    expect(await screen.findByText("Revealed Beta")).toBeInTheDocument();
    expect(screen.queryByText("Revealed Alpha")).not.toBeInTheDocument();

    act(() => {
      useMultiplayerStore.setState({ activePlayerId: 2 });
      useGameStore.setState({ waitingFor: chooseCostPrompt(2, CARD_A) as never });
    });
    expect(await screen.findByText("Revealed Alpha")).toBeInTheDocument();
    expect(screen.queryByText("Revealed Beta")).not.toBeInTheDocument();
  });

  it("displays earlier public sacrifice choices from the scoped unless poll to later payers", async () => {
    const bear = gameObjectFactory.creature(2, 2).named("Grizzly Bears").withId(CARD_A).build();
    const state = gameStateFactory.withPlayers(0, 1, 2).withObjects(bear).build();
    state.pending_player_scope_unless_punishment = {
      declining_players: [1],
      payments: [
        {
          player: 0,
          action: "Sacrifice",
          objects: [{ object_id: CARD_A, incarnation: 0 }],
        },
      ],
    };
    act(() => {
      useGameStore.setState({
        gameId: "unless-poll-choices",
        gameMode: "online",
        gameState: state,
        waitingFor: paymentPrompt(2, null) as never,
      });
      useMultiplayerStore.setState({ activePlayerId: 2, isSpectator: false });
    });
    render(gamePageTree());

    expect(await screen.findByText("Player 1: Grizzly Bears")).toBeInTheDocument();
    expect(screen.getByText(/Player 2:/)).toBeInTheDocument();
  });
});
