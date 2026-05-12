import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { GameAction, GameObject, GameState } from "../../../adapter/types.ts";
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

describe("LibraryPile play/cast surfacing (#297)", () => {
  beforeEach(() => {
    dispatchMock.mockClear();
  });

  afterEach(() => {
    cleanup();
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

  it("does not dispatch when there is no play action", () => {
    setStore({ canPeek: true, actions: [] });
    render(<LibraryPile playerId={0} />);
    const button = screen.getByRole("button", { name: /library \(1 cards\)/i });
    expect(button).toBeDisabled();
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
