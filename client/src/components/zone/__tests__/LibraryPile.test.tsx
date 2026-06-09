import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import {
  HIDDEN_CARD_NAME,
  type GameAction,
  type GameObject,
  type GameState,
} from "../../../adapter/types.ts";
import { useGameStore } from "../../../stores/gameStore.ts";
import { useUiStore } from "../../../stores/uiStore.ts";
import { LibraryPile } from "../LibraryPile.tsx";

vi.mock("../../../hooks/useCardImage", () => ({
  useCardImage: () => ({ src: null, isLoading: false }),
}));

const dispatchMock = vi.fn(async () => undefined);

vi.mock("../../../hooks/useGameDispatch.ts", () => ({
  useGameDispatch: () => dispatchMock,
}));

function makeObject(id: number, name: string): GameObject {
  return {
    id,
    card_id: id,
    owner: 0,
    controller: 0,
    zone: "Library",
    tapped: false,
    face_down: false,
    flipped: false,
    transformed: false,
    damage_marked: 0,
    dealt_deathtouch_damage: false,
    attached_to: null,
    attachments: [],
    counters: {},
    name,
    power: null,
    toughness: null,
    loyalty: null,
    card_types: { supertypes: [], core_types: ["Artifact"], subtypes: [] },
    mana_cost: { type: "Cost", shards: [], generic: 1 },
    keywords: [],
    abilities: [],
    trigger_definitions: [],
    replacement_definitions: [],
    static_definitions: [],
    color: [],
    base_power: null,
    base_toughness: null,
    base_keywords: [],
    base_color: [],
    timestamp: 1,
    entered_battlefield_turn: null,
  };
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
  const gameState = {
    active_player: 0,
    objects: { [topCardId]: top },
    players: [
      {
        id: 0,
        life: 20,
        poison_counters: 0,
        mana_pool: { mana: [] },
        library: [topCardId],
        hand: [],
        graveyard: [],
        has_drawn_this_turn: false,
        lands_played_this_turn: 0,
        turns_taken: 0,
        can_look_at_top_of_library: canPeek,
      },
      {
        id: 1,
        life: 20,
        poison_counters: 0,
        mana_pool: { mana: [] },
        library: [],
        hand: [],
        graveyard: [],
        has_drawn_this_turn: false,
        lands_played_this_turn: 0,
        turns_taken: 0,
        can_look_at_top_of_library: false,
      },
    ],
    battlefield: [],
    exile: [],
    stack: [],
    combat: null,
    revealed_cards: [],
    waiting_for: { type: "Priority", data: { player: 0 } },
  } as unknown as GameState;

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
  } as unknown as GameAction;
}

function playLandAction(objectId: number): GameAction {
  return {
    type: "PlayLand",
    data: { object_id: objectId, card_id: objectId },
  } as unknown as GameAction;
}

function setOpponentLibraryTop(
  topCardName: string,
  reveal: {
    revealedCards?: number[];
    privateLookPlayer?: number;
    privateLookIds?: number[];
  } = {},
) {
  const topCardId = 77;
  const top = makeObject(topCardId, topCardName);
  const gameState = {
    active_player: 0,
    objects: { [topCardId]: top },
    players: [
      {
        id: 0,
        life: 20,
        poison_counters: 0,
        mana_pool: { mana: [] },
        library: [],
        hand: [],
        graveyard: [],
        has_drawn_this_turn: false,
        lands_played_this_turn: 0,
        turns_taken: 0,
        can_look_at_top_of_library: false,
      },
      {
        id: 1,
        life: 20,
        poison_counters: 0,
        mana_pool: { mana: [] },
        library: [topCardId],
        hand: [],
        graveyard: [],
        has_drawn_this_turn: false,
        lands_played_this_turn: 0,
        turns_taken: 0,
        can_look_at_top_of_library: false,
      },
    ],
    battlefield: [],
    exile: [],
    stack: [],
    combat: null,
    revealed_cards: reveal.revealedCards ?? [],
    private_look_player: reveal.privateLookPlayer,
    private_look_ids: reveal.privateLookIds ?? [],
    waiting_for: { type: "Priority", data: { player: 0 } },
  } as unknown as GameState;

  useGameStore.setState({
    gameState,
    waitingFor: gameState.waiting_for,
    legalActions: [],
    legalActionsByObject: {},
    spellCosts: {},
    gameMode: "ai",
  });
}

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

  it("shows opponent library top after a private look peek (Mishra's Bauble)", () => {
    // CR 701.20e: I (player 0) privately look at the opponent's (player 1) top.
    // The engine records the look in private_look_player/ids; the pile shows it.
    setOpponentLibraryTop("Lightning Bolt", { privateLookPlayer: 0, privateLookIds: [77] });
    render(<LibraryPile playerId={1} />);
    const button = screen.getByRole("button", { name: /library \(1 card\)/i });
    expect(button).toBeInTheDocument();
    // Peeked tops use the cyan border; card-back alt text is hidden.
    expect(button.className).toContain("border-cyan-600");
    expect(screen.queryByAltText("Library")).not.toBeInTheDocument();
  });

  it("keeps an opponent library top hidden when nothing reveals it (no leak)", () => {
    // Regression guard (#2631): single-player renders the raw, unredacted state,
    // so the opponent's top carries a real name. With NO reveal set membership it
    // must stay a card-back — never inferred visible from the name.
    setOpponentLibraryTop("Lightning Bolt");
    render(<LibraryPile playerId={1} />);
    const button = screen.getByRole("button", { name: /library \(1 card\)/i });
    expect(button.className).toContain("border-gray-600");
    expect(screen.getByAltText("Library")).toBeInTheDocument();
  });

  it("shows an opponent library top that is publicly revealed (revealed_cards)", () => {
    // CR 701.20b: opponent's own public reveal (Oracle of Mul Daya) — visible to
    // all players via revealed_cards, so the pile shows it with the amber border.
    setOpponentLibraryTop("Lightning Bolt", { revealedCards: [77] });
    render(<LibraryPile playerId={1} />);
    const button = screen.getByRole("button", { name: /library \(1 card\)/i });
    expect(button.className).toContain("border-amber-500");
    expect(screen.queryByAltText("Library")).not.toBeInTheDocument();
  });

  it("keeps a masked opponent library top hidden", () => {
    setOpponentLibraryTop(HIDDEN_CARD_NAME);
    render(<LibraryPile playerId={1} />);
    const button = screen.getByRole("button", { name: /library \(1 card\)/i });
    expect(button.className).toContain("border-gray-600");
    expect(screen.getByAltText("Library")).toBeInTheDocument();
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

    fireEvent.touchStart(button, {
      touches: [{ clientX: 10, clientY: 10 }],
    });
    vi.advanceTimersByTime(500);
    fireEvent.touchEnd(button);
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
