import { describe, expect, it } from "vitest";

import type { GameObject } from "../../adapter/types";
import { getDeckDominantColor, getDominantManaColor } from "../dominantColor";

function makeGameObject(overrides: Partial<GameObject> = {}): GameObject {
  return {
    id: 1,
    card_id: 100,
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
    name: "Test Land",
    power: null,
    toughness: null,
    loyalty: null,
    card_types: { supertypes: ["Basic"], core_types: ["Land"], subtypes: ["Forest"] },
    mana_cost: { type: "NoCost" },
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
    ...overrides,
  };
}

describe("getDominantManaColor", () => {
  it("returns the most common color from land subtypes", () => {
    const objects: Record<string, GameObject> = {
      "1": makeGameObject({ id: 1, card_types: { supertypes: ["Basic"], core_types: ["Land"], subtypes: ["Forest"] } }),
      "2": makeGameObject({ id: 2, card_types: { supertypes: ["Basic"], core_types: ["Land"], subtypes: ["Forest"] } }),
      "3": makeGameObject({ id: 3, card_types: { supertypes: ["Basic"], core_types: ["Land"], subtypes: ["Mountain"] } }),
    };

    const result = getDominantManaColor([1, 2, 3], objects, 0);

    expect(result).toBe("Green");
  });

  it("returns null when no colored lands or spells exist", () => {
    const objects: Record<string, GameObject> = {
      "1": makeGameObject({
        id: 1,
        card_types: { supertypes: [], core_types: ["Land"], subtypes: [] },
      }),
    };

    const result = getDominantManaColor([1], objects, 0);

    expect(result).toBeNull();
  });

  it("filters to permanents owned by the specified player", () => {
    const objects: Record<string, GameObject> = {
      "1": makeGameObject({ id: 1, owner: 0, card_types: { supertypes: ["Basic"], core_types: ["Land"], subtypes: ["Forest"] } }),
      "2": makeGameObject({ id: 2, owner: 1, card_types: { supertypes: ["Basic"], core_types: ["Land"], subtypes: ["Island"] } }),
    };

    const result = getDominantManaColor([1, 2], objects, 0);

    expect(result).toBe("Green");
  });

  it("returns null for empty battlefield", () => {
    const result = getDominantManaColor([], {}, 0);

    expect(result).toBeNull();
  });

  it("uses owner (not controller) for filtering", () => {
    const objects: Record<string, GameObject> = {
      "1": makeGameObject({ id: 1, owner: 0, controller: 1, card_types: { supertypes: ["Basic"], core_types: ["Land"], subtypes: ["Forest"] } }),
      "2": makeGameObject({ id: 2, owner: 0, controller: 0, card_types: { supertypes: ["Basic"], core_types: ["Land"], subtypes: ["Island"] } }),
    };

    // Object 1 is owned by player 0 but controlled by player 1 — still counts for player 0's deck color
    const result = getDominantManaColor([1, 2], objects, 0);

    expect(result).toBe("Green"); // Would be Blue if controller-based
  });

  it("counts mana cost shards of non-land permanents", () => {
    const objects: Record<string, GameObject> = {
      "1": makeGameObject({
        id: 1,
        name: "Serra Angel",
        card_types: { supertypes: [], core_types: ["Creature"], subtypes: ["Angel"] },
        mana_cost: { type: "Cost", shards: ["W", "W"], generic: 3 },
      }),
      "2": makeGameObject({
        id: 2,
        name: "Lightning Bolt",
        card_types: { supertypes: [], core_types: ["Creature"], subtypes: [] },
        mana_cost: { type: "Cost", shards: ["R"], generic: 0 },
      }),
    };

    const result = getDominantManaColor([1, 2], objects, 0);

    expect(result).toBe("White");
  });

  it("combines land subtypes and spell mana costs", () => {
    const objects: Record<string, GameObject> = {
      "1": makeGameObject({ id: 1, card_types: { supertypes: ["Basic"], core_types: ["Land"], subtypes: ["Island"] } }),
      "2": makeGameObject({ id: 2, card_types: { supertypes: ["Basic"], core_types: ["Land"], subtypes: ["Island"] } }),
      "3": makeGameObject({
        id: 3,
        name: "Red Creature",
        card_types: { supertypes: [], core_types: ["Creature"], subtypes: [] },
        mana_cost: { type: "Cost", shards: ["R"], generic: 1 },
      }),
    };

    const result = getDominantManaColor([1, 2, 3], objects, 0);

    expect(result).toBe("Blue");
  });
});

describe("getDeckDominantColor", () => {
  it("determines color from library cards when battlefield is empty", () => {
    const objects: Record<string, GameObject> = {
      "1": makeGameObject({ id: 1, card_types: { supertypes: ["Basic"], core_types: ["Land"], subtypes: ["Mountain"] } }),
      "2": makeGameObject({ id: 2, card_types: { supertypes: ["Basic"], core_types: ["Land"], subtypes: ["Mountain"] } }),
      "3": makeGameObject({ id: 3, card_types: { supertypes: ["Basic"], core_types: ["Land"], subtypes: ["Forest"] } }),
    };

    const result = getDeckDominantColor([1, 2, 3], [], [], objects, 0);

    expect(result).toBe("Red");
  });

  it("combines library, hand, and battlefield for color detection", () => {
    const objects: Record<string, GameObject> = {
      "1": makeGameObject({ id: 1, card_types: { supertypes: ["Basic"], core_types: ["Land"], subtypes: ["Swamp"] } }),
      "2": makeGameObject({ id: 2, card_types: { supertypes: ["Basic"], core_types: ["Land"], subtypes: ["Swamp"] } }),
      "3": makeGameObject({ id: 3, card_types: { supertypes: ["Basic"], core_types: ["Land"], subtypes: ["Island"] } }),
    };

    // 1 in library, 2 in hand, 3 on battlefield
    const result = getDeckDominantColor([1], [2], [3], objects, 0);

    expect(result).toBe("Black");
  });

  it("returns null when deck has no colored cards", () => {
    const result = getDeckDominantColor([], [], [], {}, 0);

    expect(result).toBeNull();
  });
});
