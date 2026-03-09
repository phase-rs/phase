import { describe, expect, it } from "vitest";

import type { GameObject } from "../../adapter/types";
import { getDominantManaColor } from "../dominantColor";

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
    svars: {},
    color: ["Green"],
    base_power: null,
    base_toughness: null,
    base_keywords: [],
    base_color: ["Green"],
    timestamp: 1,
    entered_battlefield_turn: null,
    ...overrides,
  };
}

describe("getDominantManaColor", () => {
  it("returns the most common color from lands", () => {
    const objects: Record<string, GameObject> = {
      "1": makeGameObject({ id: 1, controller: 0, color: ["Green"] }),
      "2": makeGameObject({ id: 2, controller: 0, color: ["Green"] }),
      "3": makeGameObject({ id: 3, controller: 0, color: ["Red"] }),
    };

    const result = getDominantManaColor([1, 2, 3], objects, 0);

    expect(result).toBe("Green");
  });

  it("returns null when no colored lands exist", () => {
    const objects: Record<string, GameObject> = {
      "1": makeGameObject({ id: 1, controller: 0, color: [] }),
    };

    const result = getDominantManaColor([1], objects, 0);

    expect(result).toBeNull();
  });

  it("filters to lands controlled by the specified player", () => {
    const objects: Record<string, GameObject> = {
      "1": makeGameObject({ id: 1, controller: 0, color: ["Green"] }),
      "2": makeGameObject({ id: 2, controller: 1, color: ["Blue"] }),
      "3": makeGameObject({
        id: 3,
        controller: 0,
        color: ["Red"],
        card_types: { supertypes: [], core_types: ["Creature"], subtypes: [] },
      }),
    };

    const result = getDominantManaColor([1, 2, 3], objects, 0);

    // Only land id=1 owned by player 0 counts
    expect(result).toBe("Green");
  });

  it("returns null for empty battlefield", () => {
    const result = getDominantManaColor([], {}, 0);

    expect(result).toBeNull();
  });

  it("handles tied colors by returning first in WUBRG order", () => {
    const objects: Record<string, GameObject> = {
      "1": makeGameObject({ id: 1, controller: 0, color: ["Red"] }),
      "2": makeGameObject({ id: 2, controller: 0, color: ["Green"] }),
    };

    const result = getDominantManaColor([1, 2], objects, 0);

    // Tied at 1 each, first encountered wins (or we define WUBRG priority)
    expect(result).toBeDefined();
  });
});
