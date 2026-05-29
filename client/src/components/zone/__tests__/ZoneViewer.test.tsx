import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { GameAction, GameObject, GameState } from "../../../adapter/types.ts";
import { useGameStore } from "../../../stores/gameStore.ts";
import { useUiStore } from "../../../stores/uiStore.ts";
import { ZoneViewer } from "../ZoneViewer.tsx";

vi.mock("../../card/CardImage.tsx", () => ({
  CardImage: ({ cardName }: { cardName: string }) => (
    <div aria-label={cardName} data-testid="card-image" />
  ),
}));

const targetDispatch = vi.fn();

vi.mock("../../../hooks/useGameDispatch.ts", () => ({
  useGameDispatch: () => targetDispatch,
}));

function makeObject(overrides: Partial<GameObject> = {}): GameObject {
  return {
    id: 7,
    card_id: 700,
    owner: 0,
    controller: 0,
    zone: "Graveyard",
    tapped: false,
    face_down: false,
    flipped: false,
    transformed: false,
    damage_marked: 0,
    dealt_deathtouch_damage: false,
    attached_to: null,
    attachments: [],
    counters: {},
    name: "Flame Jab",
    power: null,
    toughness: null,
    loyalty: null,
    card_types: { supertypes: [], core_types: ["Sorcery"], subtypes: [] },
    mana_cost: { type: "Cost", shards: ["Red"], generic: 0 },
    keywords: ["Retrace"],
    abilities: [],
    trigger_definitions: [],
    replacement_definitions: [],
    static_definitions: [],
    color: ["Red"],
    base_power: null,
    base_toughness: null,
    base_keywords: ["Retrace"],
    base_color: ["Red"],
    timestamp: 1,
    entered_battlefield_turn: null,
    ...overrides,
  };
}

function makeCastAction(objectId: number): GameAction {
  return {
    type: "CastSpell",
    data: { object_id: objectId, card_id: 700, targets: [] },
  };
}

function makeState(object: GameObject): GameState {
  return {
    active_player: 0,
    priority_player: 0,
    players: [
      {
        id: 0,
        life: 20,
        poison_counters: 0,
        mana_pool: { mana: [] },
        library: [],
        hand: [],
        graveyard: [object.id],
        has_drawn_this_turn: false,
        lands_played_this_turn: 0,
        turns_taken: 0,
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
      },
    ],
    objects: { [object.id]: object },
    battlefield: [],
    exile: [],
    stack: [],
    combat: null,
    waiting_for: { type: "Priority", data: { player: 0 } },
  } as unknown as GameState;
}

describe("ZoneViewer", () => {
  const dispatch = vi.fn(async () => []);

  beforeEach(() => {
    const object = makeObject();
    const action = makeCastAction(object.id);
    const gameState = makeState(object);
    targetDispatch.mockClear();
    dispatch.mockClear();
    useGameStore.setState({
      gameState,
      waitingFor: gameState.waiting_for,
      legalActions: [action],
      legalActionsByObject: { [String(object.id)]: [action] },
      spellCosts: {},
      dispatch,
      gameMode: "ai",
    });
    useUiStore.setState({
      inspectedObjectId: null,
      previewSticky: false,
      pendingAbilityChoice: null,
      debugInteractionMode: false,
    });
  });

  afterEach(() => {
    cleanup();
  });

  it("dispatches an engine-provided graveyard CastSpell action", () => {
    render(<ZoneViewer zone="graveyard" playerId={0} onClose={vi.fn()} />);

    // The castable card carries the purple "playable" affordance instead of a
    // labeled button; clicking the card itself routes through handleCast and
    // auto-dispatches the lone CastSpell action.
    fireEvent.click(screen.getByTestId("card-image"));

    expect(dispatch).toHaveBeenCalledTimes(1);
    expect(dispatch).toHaveBeenCalledWith(
      expect.objectContaining({ type: "CastSpell" }),
    );
  });
});
