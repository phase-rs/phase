import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import {
  HIDDEN_CARD_NAME,
  type GameAction,
  type GameObject,
} from "../../../adapter/types.ts";
import { useAnimationStore } from "../../../stores/animationStore.ts";
import { useGameStore } from "../../../stores/gameStore.ts";
import { useMultiplayerStore } from "../../../stores/multiplayerStore.ts";
import { useUiStore } from "../../../stores/uiStore.ts";
import { buildGameObjectWithCoreTypes, buildObjectMap } from "../../../test/factories/gameObjectFactory.ts";
import { buildGameState, buildPlayers, buildPriorityWaitingFor } from "../../../test/factories/gameStateFactory.ts";
import { LibraryPile } from "../LibraryPile.tsx";

vi.mock("../../../hooks/useCardImage", () => ({
  useCardBackImage: () => ({ src: "/test-card-back.png", isLoading: false }),
  useCardImage: () => ({ src: null, isLoading: false }),
}));

const dispatchMock = vi.fn(async () => undefined);

vi.mock("../../../hooks/useGameDispatch.ts", () => ({
  useGameDispatch: () => dispatchMock,
}));

function makeObject(id: number, name: string): GameObject {
  return buildGameObjectWithCoreTypes(["Artifact"], {
    id,
    card_id: id,
    zone: "Library",
    name,
    mana_cost: { type: "Cost", shards: [], generic: 1 },
    timestamp: 1,
    entered_battlefield_turn: null,
  });
}

function setStore({
  topCardId = 42,
  topCardName = "Sol Ring",
  canPeek,
  actions,
}: {
  topCardId?: number;
  topCardName?: string;
  canPeek: boolean;
  actions: GameAction[];
}) {
  const top = makeObject(topCardId, topCardName);
  top.display_visible_to_viewer = canPeek;
  const gameState = buildGameState({
    active_player: 0,
    objects: buildObjectMap(top),
    players: buildPlayers([
      {
        id: 0,
        library: [topCardId],
        can_look_at_top_of_library: canPeek,
      },
      {
        id: 1,
        can_look_at_top_of_library: false,
      },
    ]),
    battlefield: [],
    exile: [],
    stack: [],
    revealed_cards: [],
    waiting_for: buildPriorityWaitingFor(),
  });

  useGameStore.setState({
    gameState,
    waitingFor: gameState.waiting_for,
    legalActions: actions,
    legalActionsByObject: actions.length > 0 ? { [String(topCardId)]: actions } : {},
    spellCosts: {},
    gameMode: "ai",
  });
  useUiStore.setState({
    inspectedObjectId: null,
    previewSticky: false,
    pendingAbilityChoice: null,
  });
}

function castAction(objectId: number): GameAction {
  return {
    type: "CastSpell",
    data: { object_id: objectId, card_id: objectId, targets: [] },
  };
}

function playLandAction(objectId: number): GameAction {
  return {
    type: "PlayLand",
    data: { object_id: objectId, card_id: objectId },
  };
}

// CR 702.170f: the plot special action Fblthp, Lost on the Range grants to the
// top card — a runtime-granted ability, so it is absent from `abilities`.
function plotAction(objectId: number): GameAction {
  return {
    type: "ActivateAbility",
    data: { source_id: objectId, ability_index: 0 },
  };
}

function setOpponentLibraryTop(
  topCardName: string,
  reveal: {
    revealedCards?: number[];
    displayVisible?: boolean;
  } = {},
) {
  const topCardId = 77;
  const top = makeObject(topCardId, topCardName);
  top.display_visible_to_viewer = reveal.displayVisible ?? false;
  const gameState = buildGameState({
    active_player: 0,
    objects: buildObjectMap(top),
    players: buildPlayers([
      {
        id: 0,
        can_look_at_top_of_library: false,
      },
      {
        id: 1,
        library: [topCardId],
        can_look_at_top_of_library: false,
      },
    ]),
    battlefield: [],
    exile: [],
    stack: [],
    revealed_cards: reveal.revealedCards ?? [],
    waiting_for: buildPriorityWaitingFor(),
  });

  useGameStore.setState({
    gameState,
    waitingFor: gameState.waiting_for,
    legalActions: [],
    legalActionsByObject: {},
    spellCosts: {},
    gameMode: "ai",
  });
}

function setSharedLibrary({ shared }: { shared: boolean }) {
  const top = makeObject(11, "Sol Ring");
  top.display_visible_to_viewer = true;
  const action = castAction(11);
  const gameState = buildGameState({
    active_player: 0,
    priority_player: 1,
    objects: buildObjectMap(top),
    players: buildPlayers([
      { id: 0, library: [11, 12, 13] },
      { id: 1, library: [] },
    ]),
    battlefield: [],
    exile: [],
    stack: [],
    revealed_cards: [],
    waiting_for: buildPriorityWaitingFor({ data: { player: 1 } }),
    ...(shared ? { derived: { shared_piles: { library: 0, graveyard: 0 } } } : {}),
  });
  useGameStore.setState({
    gameState,
    waitingFor: gameState.waiting_for,
    legalActions: [action],
    legalActionsByObject: { "11": [action] },
    spellCosts: {},
    gameMode: "online",
  });
  useMultiplayerStore.setState({ activePlayerId: 1 });
}

describe("LibraryPile shared library", () => {
  beforeEach(() => {
    dispatchMock.mockClear();
  });

  afterEach(() => {
    cleanup();
    useMultiplayerStore.setState({ activePlayerId: null });
  });

  it("renders the holder's pile for the non-holder seat's request", () => {
    setSharedLibrary({ shared: true });
    const { container } = render(<LibraryPile playerId={1} />);
    expect(container.querySelector('[data-library-pile="0"]')).not.toBeNull();
    expect(container.querySelector('[data-library-pile="1"]')).toBeNull();
    expect(container.querySelector("button[data-grouped-ids]")).toHaveAttribute(
      "data-grouped-ids",
      "11",
    );
  });

  it("lets the non-holder seat play from the shared top the engine offers", () => {
    setSharedLibrary({ shared: true });
    render(<LibraryPile playerId={0} />);
    const button = screen.getByRole("button", { name: /play sol ring from top of library/i });
    expect(button).not.toBeDisabled();
    fireEvent.click(button);
    expect(dispatchMock).toHaveBeenCalledWith(expect.objectContaining({ type: "CastSpell" }));
  });

  it("still refuses the opponent's own library when nothing is shared", () => {
    setSharedLibrary({ shared: false });
    render(<LibraryPile playerId={0} />);
    const button = screen.getByRole("button", { name: /library \(3 cards\)/i });
    expect(button).toHaveAttribute("data-library-top-cast", "false");
    fireEvent.click(button);
    expect(dispatchMock).not.toHaveBeenCalled();
  });
});

describe("LibraryPile play/cast surfacing (#297)", () => {
  beforeEach(() => {
    dispatchMock.mockClear();
  });

  afterEach(() => {
    cleanup();
    vi.useRealTimers();
  });

  it("dispatches the CastSpell action when the top card is castable (Mystic Forge)", () => {
    setStore({ canPeek: true, actions: [castAction(42)] });
    render(<LibraryPile playerId={0} />);
    const button = screen.getByRole("button", { name: /play sol ring from top of library/i });
    expect(button).not.toBeDisabled();
    fireEvent.click(button);
    expect(dispatchMock).toHaveBeenCalledTimes(1);
    expect(dispatchMock).toHaveBeenCalledWith(
      expect.objectContaining({ type: "CastSpell" }),
    );
  });

  it("dispatches the PlayLand action when the top card is a playable land (Future Sight)", () => {
    setStore({
      topCardName: "Forest",
      canPeek: true,
      actions: [playLandAction(42)],
    });
    render(<LibraryPile playerId={0} />);
    const button = screen.getByRole("button", { name: /play forest from top of library/i });
    expect(button).not.toBeDisabled();
    fireEvent.click(button);
    expect(dispatchMock).toHaveBeenCalledTimes(1);
    expect(dispatchMock).toHaveBeenCalledWith(
      expect.objectContaining({ type: "PlayLand" }),
    );
  });

  it("dispatches an engine-surfaced plot activation on the top card (Fblthp)", () => {
    setStore({ canPeek: true, actions: [plotAction(42)] });
    render(<LibraryPile playerId={0} />);
    const button = screen.getByRole("button", { name: /activate sol ring from top of library/i });
    expect(button).toHaveAttribute("data-library-top-cast", "true");
    fireEvent.click(button);
    expect(dispatchMock).toHaveBeenCalledTimes(1);
    expect(dispatchMock).toHaveBeenCalledWith(plotAction(42));
  });

  it("routes multi-action top cards to the ability-choice modal", () => {
    // E.g. Bolas's Citadel: cast normally + cast via PayLife alt-cost would
    // both appear when both options are legal at once.
    const actions = [castAction(42), castAction(42)];
    setStore({ canPeek: true, actions });
    render(<LibraryPile playerId={0} />);
    fireEvent.click(screen.getByRole("button", { name: /play sol ring from top of library/i }));
    // Multi-action path delegates to the shared ability-choice modal — no
    // direct dispatch.
    expect(dispatchMock).not.toHaveBeenCalled();
    expect(useUiStore.getState().pendingAbilityChoice).toEqual({
      objectId: 42,
      actions,
    });
  });

  it("shows an opponent library top when Rust projects it as visible", () => {
    setOpponentLibraryTop("Lightning Bolt", { displayVisible: true });
    render(<LibraryPile playerId={1} />);
    const button = screen.getByRole("button", { name: /library \(1 card\)/i });
    expect(button).toBeInTheDocument();
    expect(button).toHaveAttribute("data-grouped-ids", "77");
    // Peeked tops use the cyan border; card-back alt text is hidden.
    expect(button.className).toContain("border-cyan-600");
    expect(screen.queryByAltText("Card back")).not.toBeInTheDocument();
  });

  it("keeps an opponent library top hidden when nothing reveals it (no leak)", () => {
    // Regression guard (#2631): single-player renders the raw, unredacted state,
    // so the opponent's top carries a real name. With NO reveal set membership it
    // must stay a card-back — never inferred visible from the name.
    setOpponentLibraryTop("Lightning Bolt");
    render(<LibraryPile playerId={1} />);
    const button = screen.getByRole("button", { name: /library \(1 card\)/i });
    expect(button.className).toContain("border-gray-600");
    expect(button).not.toHaveAttribute("data-grouped-ids");
    expect(screen.getByAltText("Card back")).toBeInTheDocument();
  });

  it("uses reveal state only for the public-reveal treatment, not visibility", () => {
    setOpponentLibraryTop("Lightning Bolt", { revealedCards: [77], displayVisible: true });
    render(<LibraryPile playerId={1} />);
    const button = screen.getByRole("button", { name: /library \(1 card\)/i });
    expect(button.className).toContain("border-amber-500");
    expect(screen.queryByAltText("Card back")).not.toBeInTheDocument();
  });

  it("keeps a masked opponent library top hidden", () => {
    setOpponentLibraryTop(HIDDEN_CARD_NAME);
    render(<LibraryPile playerId={1} />);
    const button = screen.getByRole("button", { name: /library \(1 card\)/i });
    expect(button.className).toContain("border-gray-600");
    expect(screen.getByAltText("Card back")).toBeInTheDocument();
  });

  it("does not dispatch when there is no play action", () => {
    setStore({ canPeek: true, actions: [] });
    render(<LibraryPile playerId={0} />);
    const button = screen.getByRole("button", { name: /library \(1 card\)/i });
    expect(button).not.toBeDisabled();
    fireEvent.click(button);
    expect(dispatchMock).not.toHaveBeenCalled();
  });

  it("pluralizes the hidden-library aria label", () => {
    setStore({ canPeek: false, actions: [] });
    render(<LibraryPile playerId={0} />);
    expect(screen.getByRole("button", { name: /library \(1 card\)/i })).toBeInTheDocument();

    cleanup();

    setStore({ topCardId: 43, canPeek: false, actions: [] });
    useGameStore.setState((state) => ({
      gameState: state.gameState
        ? {
            ...state.gameState,
            objects: {
              ...state.gameState.objects,
              44: makeObject(44, "Mox Pearl"),
            },
            players: state.gameState.players.map((player, index) =>
              index === 0 ? { ...player, library: [43, 44] } : player,
            ),
          }
        : state.gameState,
    }));
    render(<LibraryPile playerId={0} />);
    expect(screen.getByRole("button", { name: /library \(2 cards\)/i })).toBeInTheDocument();
  });

  it("long-press previews the visible top card without dispatching", () => {
    vi.useFakeTimers();
    setStore({ canPeek: true, actions: [] });
    render(<LibraryPile playerId={0} />);
    const button = screen.getByRole("button", { name: /library \(1 card\)/i });

    fireEvent.pointerDown(button, {
      button: 0,
      clientX: 10,
      clientY: 10,
      isPrimary: true,
      pointerId: 1,
      pointerType: "touch",
    });
    vi.advanceTimersByTime(500);
    fireEvent.pointerUp(button, {
      button: 0,
      clientX: 10,
      clientY: 10,
      isPrimary: true,
      pointerId: 1,
      pointerType: "touch",
    });
    fireEvent.click(button);

    expect(dispatchMock).not.toHaveBeenCalled();
    expect(useUiStore.getState().inspectedObjectId).toBe(42);
    expect(useUiStore.getState().previewSticky).toBe(true);
  });

  it("opens the library viewer instead of casting when onView is wired and the top is visible", () => {
    // With a viewer available, a visible top routes the click to the modal
    // (where play-from-top lives), mirroring graveyard/exile. canView wins over
    // the direct-cast fast path.
    const onView = vi.fn();
    setStore({ canPeek: true, actions: [castAction(42)] });
    render(<LibraryPile playerId={0} onView={onView} />);
    const button = screen.getByRole("button", { name: /play sol ring from top of library/i });
    expect(button).not.toBeDisabled();
    fireEvent.click(button);
    expect(onView).toHaveBeenCalledTimes(1);
    expect(dispatchMock).not.toHaveBeenCalled();
  });

  it("opens the library viewer for a revealed top with no play action", () => {
    const onView = vi.fn();
    setStore({ canPeek: true, actions: [] });
    render(<LibraryPile playerId={0} onView={onView} />);
    fireEvent.click(screen.getByRole("button", { name: /library \(1 card\)/i }));
    expect(onView).toHaveBeenCalledTimes(1);
  });

  it("does not open the viewer when the top is hidden (nothing revealed)", () => {
    // A masked top (no peek, no reveal) has nothing to show — clicking must not
    // open the modal even though onView is wired.
    const onView = vi.fn();
    setStore({ canPeek: false, actions: [] });
    render(<LibraryPile playerId={0} onView={onView} />);
    fireEvent.click(screen.getByRole("button", { name: /library \(1 card\)/i }));
    expect(onView).not.toHaveBeenCalled();
  });

  it("surfaces engine-reported play action even without peek (engine is authoritative)", () => {
    setStore({ canPeek: false, actions: [castAction(42)] });
    render(<LibraryPile playerId={0} />);
    // Without peek the top name is hidden, so aria-label falls back to the
    // generic "from top of library" phrasing.
    const button = screen.getByRole("button", { name: /play top of library from top of library/i });
    expect(button).not.toBeDisabled();
  });
});

describe("LibraryPile flight veil", () => {
  beforeEach(() => {
    useAnimationStore.getState().clearQueue();
  });

  afterEach(() => {
    cleanup();
    useAnimationStore.getState().clearQueue();
  });

  it("shows a back in place of a visible top card that is in flight, keeping the count", () => {
    setStore({ canPeek: true, actions: [] });
    useAnimationStore.getState().veilFlight(42);

    render(<LibraryPile playerId={0} />);

    expect(screen.queryByRole("img", { name: "Sol Ring" })).toBeNull();
    expect(screen.getByAltText("Card back")).toBeInTheDocument();
    expect(screen.getByText("1")).toBeInTheDocument();
  });

  it("shows the visible top card again once the flight releases it", () => {
    setStore({ canPeek: true, actions: [] });
    useAnimationStore.getState().veilFlight(42);
    render(<LibraryPile playerId={0} />);

    act(() => useAnimationStore.getState().unveilFlight(42));

    expect(screen.getByRole("img", { name: "Sol Ring" })).toBeInTheDocument();
  });

  it("keeps a hidden top card anonymous while it is in flight", () => {
    setStore({ canPeek: false, actions: [] });
    useAnimationStore.getState().veilFlight(42);

    render(<LibraryPile playerId={0} />);

    expect(screen.getByAltText("Card back")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: /library \(1 card\)/i })).not.toHaveAttribute(
      "data-grouped-ids",
    );
  });

  it("ignores a flight veil on a card below the top", () => {
    setStore({ canPeek: true, actions: [] });
    useGameStore.setState((state) => ({
      gameState: state.gameState
        ? {
            ...state.gameState,
            objects: { ...state.gameState.objects, 43: makeObject(43, "Mox Pearl") },
            players: state.gameState.players.map((player, index) =>
              index === 0 ? { ...player, library: [42, 43] } : player,
            ),
          }
        : state.gameState,
    }));
    useAnimationStore.getState().veilFlight(43);

    render(<LibraryPile playerId={0} />);

    expect(screen.getByText("2")).toBeInTheDocument();
    expect(screen.getByRole("img", { name: "Sol Ring" })).toBeInTheDocument();
  });
});
