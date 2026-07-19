import { cleanup, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import type { GameObject } from "../../../adapter/types.ts";
import { useCardImage } from "../../../hooks/useCardImage.ts";
import { useGameStore } from "../../../stores/gameStore.ts";
import { useUiStore } from "../../../stores/uiStore.ts";
import { buildGameObject, buildObjectMap } from "../../../test/factories/gameObjectFactory.ts";
import { buildGameState } from "../../../test/factories/gameStateFactory.ts";
import { CardPreview } from "../CardPreview.tsx";

vi.mock("../../../hooks/useCardImage.ts", () => ({
  useCardImage: vi.fn(() => ({
    src: "card.png",
    isLoading: false,
    isRotated: false,
    isFlip: false,
  })),
}));

vi.mock("../../../hooks/useEngineCardData.ts", () => ({
  useEngineCardData: () => null,
  useCardParseDetails: () => null,
  useCardRulings: () => [],
}));

function battlefieldObject(overrides: Partial<GameObject> = {}): GameObject {
  return buildGameObject({
    id: 101,
    card_id: 1,
    zone: "Battlefield",
    name: "Pithing Needle",
    mana_cost: { type: "Cost", shards: [], generic: 1 },
    ...overrides,
  });
}

function gameStateWithObject(object: GameObject) {
  return buildGameState({
    objects: buildObjectMap(object),
    next_object_id: 102,
    battlefield: [object.id],
    next_timestamp: 2,
  });
}

afterEach(() => {
  cleanup();
  vi.clearAllMocks();
  Object.defineProperty(window, "innerWidth", { configurable: true, writable: true, value: 1280 });
  Object.defineProperty(window, "innerHeight", { configurable: true, writable: true, value: 768 });
  useGameStore.setState({ gameState: null, spellCosts: {}, legalActionsByObject: {} });
  useUiStore.setState({ inspectedObjectId: null, altHeld: false });
});

describe("CardPreview chosen attributes", () => {
  it("clamps an explicit preview position into the viewport", () => {
    Object.defineProperty(window, "innerHeight", { configurable: true, writable: true, value: 768 });
    const { container } = render(<CardPreview cardName="Pithing Needle" position={{ x: 20, y: 20 }} />);

    const preview = container.querySelector<HTMLElement>("[data-card-preview]");
    expect(preview).not.toBeNull();
    expect(preview?.style.left).toBe("40px");
    expect(preview?.style.top).toBe("16px");
    expect(screen.getAllByAltText("Pithing Needle").length).toBeGreaterThan(0);
  });

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
    expect(screen.getByText("Ward").closest("[aria-describedby]")).not.toBeNull();
    expect(screen.getAllByAltText("2").length).toBeGreaterThan(0);
    expect(screen.getByText(/creatures with flying or reach/)).toBeInTheDocument();
    expect(screen.getByText(/ward cost/)).toBeInTheDocument();
  });

  it("renders mana symbols in battlefield preview ability text", () => {
    const object = battlefieldObject({
      abilities: [
        {
          description: "{G}, {T}: Add {G}.",
          effects: [],
          targets: [],
          cost: { type: "Tap" },
          timing: "AnyTime",
          kind: "Activated",
        },
      ],
    });
    useGameStore.setState({
      gameState: gameStateWithObject(object),
      legalActionsByObject: {
        [String(object.id)]: [
          {
            type: "ActivateAbility",
            data: { source_id: object.id, ability_index: 0 },
          },
        ],
      },
      spellCosts: {},
    });
    useUiStore.setState({ inspectedObjectId: object.id, altHeld: false });

    render(<CardPreview cardName="Pithing Needle" position={{ x: 20, y: 20 }} />);

    expect(screen.getByText(/Activate/)).toBeInTheDocument();
    expect(screen.getAllByAltText("T").length).toBeGreaterThan(0);
    expect(screen.getAllByAltText("G").length).toBeGreaterThan(0);
  });

  it("passes token lookup metadata to the mobile preview image hook", () => {
    Object.defineProperty(window, "innerWidth", { configurable: true, writable: true, value: 500 });
    const object = battlefieldObject({
      display_source: "Token",
      name: "Elf Warrior",
      power: 2,
      toughness: 2,
      color: ["Green"],
      card_types: { supertypes: [], core_types: ["Creature"], subtypes: ["Elf", "Warrior"] },
      token_image_ref: {
        scryfall_id: "token-printing-id",
        scryfall_oracle_id: "token-oracle-id",
        face_name: "Elf Warrior",
        preset_id: "elf-warrior-token",
      },
    });
    useGameStore.setState({ gameState: gameStateWithObject(object), spellCosts: {} });
    useUiStore.setState({ inspectedObjectId: object.id, altHeld: false });

    render(<CardPreview cardName="Elf Warrior" />);

    expect(useCardImage).toHaveBeenCalledWith("Elf Warrior", expect.objectContaining({
      isToken: true,
      tokenFilters: expect.objectContaining({
        colors: ["Green"],
        power: 2,
        subtypes: ["Elf", "Warrior"],
        toughness: 2,
      }),
      tokenImageRef: object.token_image_ref,
    }));
  });
});

// MAJOR-1 (CR 602.5): CardPreview is the SECOND `blocked_abilities` consumer and
// had no coverage before this change. It renders every prohibiting source name via
// preview.fromSource, joined, dropping ids absent from `objects`.
describe("CardPreview blocked abilities", () => {
  function inspectWith(object: GameObject, sources: GameObject[] = []) {
    const gameState = buildGameState({
      objects: buildObjectMap(object, ...sources),
      next_object_id: 999,
      battlefield: [object.id],
      next_timestamp: 2,
    });
    useGameStore.setState({ gameState, spellCosts: {} });
    useUiStore.setState({ inspectedObjectId: object.id, altHeld: false });
    render(<CardPreview cardName="Grim Monolith" position={{ x: 20, y: 20 }} />);
  }

  it("renders both prohibiting source names when two sources block one ability", () => {
    const object = battlefieldObject({
      id: 101,
      name: "Grim Monolith",
      abilities: [
        {
          description: "{T}: draw",
          effects: [],
          targets: [],
          cost: { type: "Tap" },
          timing: "AnyTime",
          kind: "Activated",
        },
      ],
      blocked_abilities: [
        { ability_index: 0, sources: [201, 202], type: "CantBeActivated" },
      ],
    });
    inspectWith(object, [
      buildGameObject({ id: 201, name: "Needle A" }),
      buildGameObject({ id: 202, name: "Needle B" }),
    ]);

    expect(screen.getByText(/\(from Needle A, Needle B\)/)).toBeInTheDocument();
  });

  it("renders a single prohibiting source name", () => {
    const object = battlefieldObject({
      id: 101,
      name: "Grim Monolith",
      abilities: [],
      blocked_abilities: [
        { ability_index: 0, sources: [201], type: "CantBeActivated" },
      ],
    });
    inspectWith(object, [buildGameObject({ id: 201, name: "Needle A" })]);

    expect(screen.getByText(/\(from Needle A\)/)).toBeInTheDocument();
  });

  it("drops a departed source id and renders no fromSource span", () => {
    const object = battlefieldObject({
      id: 101,
      name: "Grim Monolith",
      abilities: [],
      // source 999 is absent from `objects` — the departed-source guard drops it.
      blocked_abilities: [
        { ability_index: 0, sources: [999], type: "Prohibited" },
      ],
    });
    inspectWith(object);

    expect(screen.queryByText(/\(from/)).not.toBeInTheDocument();
  });
});
