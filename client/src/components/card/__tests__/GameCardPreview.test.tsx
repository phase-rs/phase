import { cleanup, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import type { GameObject, GameState } from "../../../adapter/types.ts";
import { useGameStore } from "../../../stores/gameStore.ts";
import { usePreferencesStore } from "../../../stores/preferencesStore.ts";
import { useUiStore } from "../../../stores/uiStore.ts";
import { GameCardPreview } from "../GameCardPreview.tsx";

// CardPreview renders <img alt={cardName} …>; mocking the image hook lets us
// assert the forwarded name without loading Scryfall assets. Mirrors the mocks
// in CardPreview.test.tsx.
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

function inspect(object: GameObject, faceIndex = 0): void {
  useGameStore.setState({ gameState: gameStateWithObject(object), spellCosts: {} });
  useUiStore.setState({ inspectedObjectId: object.id, inspectedFaceIndex: faceIndex });
}

afterEach(() => {
  cleanup();
  useGameStore.setState({ gameState: null, spellCosts: {} });
  useUiStore.setState({
    inspectedObjectId: null,
    inspectedFaceIndex: 0,
    isDragging: false,
    shiftHeld: false,
    altHeld: false,
  });
  // GameCardPreview adds a third store; reset it so "shift" mode doesn't leak.
  usePreferencesStore.setState({ cardPreviewMode: "follow" });
});

describe("GameCardPreview", () => {
  it("forwards the inspected object's name to the preview", () => {
    inspect(battlefieldObject());

    render(<GameCardPreview />);

    expect(screen.getAllByAltText("Pithing Needle").length).toBeGreaterThan(0);
  });

  it("renders no preview while a card is being dragged", () => {
    inspect(battlefieldObject());
    useUiStore.setState({ isDragging: true });

    const { container } = render(<GameCardPreview />);

    expect(container.firstChild).toBeNull();
    expect(screen.queryByAltText("Pithing Needle")).toBeNull();
  });

  it("suppresses the preview in shift mode when Shift is not held", () => {
    inspect(battlefieldObject());
    usePreferencesStore.setState({ cardPreviewMode: "shift" });
    useUiStore.setState({ shiftHeld: false });

    const { container } = render(<GameCardPreview />);

    expect(container.firstChild).toBeNull();

    // Holding Shift reveals it.
    cleanup();
    useUiStore.setState({ shiftHeld: true });
    render(<GameCardPreview />);
    expect(screen.getAllByAltText("Pithing Needle").length).toBeGreaterThan(0);
  });

  it("shows the back-face name when inspecting face index 1", () => {
    const dfc = battlefieldObject({
      name: "Delver of Secrets",
      back_face: {
        name: "Insectile Aberration",
        power: 3,
        toughness: 2,
        card_types: { supertypes: [], core_types: ["Creature"], subtypes: ["Human", "Insect"] },
        mana_cost: { type: "Cost", shards: [], generic: 0 },
        keywords: ["Flying"],
        abilities: [],
        color: ["Blue"],
      },
    });
    inspect(dfc, 1);

    render(<GameCardPreview />);

    expect(screen.getAllByAltText("Insectile Aberration").length).toBeGreaterThan(0);
  });

  it("never previews a face-down permanent (hidden information)", () => {
    inspect(battlefieldObject({ face_down: true }));

    const { container } = render(<GameCardPreview />);

    expect(container.firstChild).toBeNull();
  });
});
