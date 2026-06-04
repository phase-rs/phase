import { cleanup, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import type { GameObject, GameState } from "../../../adapter/types.ts";
import { useGameStore } from "../../../stores/gameStore.ts";
import { useUiStore } from "../../../stores/uiStore.ts";
import { CardPreview } from "../CardPreview.tsx";

vi.mock("../../../hooks/useCardImage.ts", () => ({
  useCardImage: () => ({
    src: "card.png",
    isLoading: false,
    isRotated: false,
    isFlip: false,
  }),
}));

vi.mock("../../../hooks/useEngineCardData.ts", () => ({
  useEngineCardData: () => null,
  useCardParseDetails: () => null,
  useCardRulings: () => [],
}));

function battlefieldObject(overrides: Partial<GameObject> = {}): GameObject {
  return {
    id: 101,
    card_id: 1,
    owner: 0,
    controller: 0,
    zone: "Battlefield",
    tapped: false,
    face_down: false,
    flipped: false,
    transformed: false,
    damage_marked: 0,
    dealt_deathtouch_damage: false,
    attached_to: null,
    attachments: [],
    counters: {},
    name: "Pithing Needle",
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
    entered_battlefield_turn: 1,
    ...overrides,
  };
}

function gameStateWithObject(object: GameObject): GameState {
  return {
    turn_number: 1,
    active_player: 0,
    phase: "PreCombatMain",
    players: [],
    priority_player: 0,
    objects: { [String(object.id)]: object },
    next_object_id: 102,
    battlefield: [object.id],
    stack: [],
    exile: [],
    rng_seed: 1,
    combat: null,
    waiting_for: { type: "Priority", data: { player: 0 } },
    has_pending_cast: false,
    lands_played_this_turn: 0,
    max_lands_per_turn: 1,
    priority_pass_count: 0,
    pending_replacement: null,
    layers_dirty: false,
    next_timestamp: 2,
  } as GameState;
}

afterEach(() => {
  cleanup();
  useGameStore.setState({ gameState: null, spellCosts: {} });
  useUiStore.setState({ inspectedObjectId: null, altHeld: false });
});

describe("CardPreview chosen attributes", () => {
  it("shows a persisted chosen card name for a battlefield permanent", () => {
    const object = battlefieldObject({
      chosen_attributes: [{ type: "CardName", value: "Lightning Bolt" }],
    });
    useGameStore.setState({ gameState: gameStateWithObject(object), spellCosts: {} });
    useUiStore.setState({ inspectedObjectId: object.id, altHeld: false });

    render(<CardPreview cardName="Pithing Needle" position={{ x: 20, y: 20 }} />);

    expect(screen.getByText("Chosen")).toBeInTheDocument();
    expect(screen.getByText("Card name: Lightning Bolt")).toBeInTheDocument();
  });

  it("renders keyword reminder tooltips for battlefield permanents", () => {
    const object = battlefieldObject({
      keywords: ["Flying", { Ward: { type: "Mana", data: { Cost: { shards: [], generic: 2 } } } }],
      base_keywords: ["Flying", { Ward: { type: "Mana", data: { Cost: { shards: [], generic: 2 } } } }],
    });
    useGameStore.setState({ gameState: gameStateWithObject(object), spellCosts: {} });
    useUiStore.setState({ inspectedObjectId: object.id, altHeld: false });

    render(<CardPreview cardName="Pithing Needle" position={{ x: 20, y: 20 }} />);

    expect(screen.getByText("Flying")).toBeInTheDocument();
    expect(screen.getByText("Ward {2}")).toHaveAttribute("aria-describedby");
    expect(screen.getByText(/creatures with flying or reach/)).toBeInTheDocument();
    expect(screen.getByText(/ward cost/)).toBeInTheDocument();
  });
});
