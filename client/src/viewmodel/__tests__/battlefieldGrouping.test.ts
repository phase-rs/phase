import { describe, expect, it } from "vitest";

import type { GameObject } from "../../adapter/types";
import { groupByName, partitionByType } from "../battlefieldProps";

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
    name: "Test Card",
    power: null,
    toughness: null,
    loyalty: null,
    card_types: { supertypes: [], core_types: ["Artifact"], subtypes: [] },
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

describe("partitionByType", () => {
  it("separates creatures, lands, and other", () => {
    const objects = [
      makeGameObject({ id: 1, card_types: { supertypes: [], core_types: ["Creature"], subtypes: [] } }),
      makeGameObject({ id: 2, card_types: { supertypes: ["Basic"], core_types: ["Land"], subtypes: ["Forest"] } }),
      makeGameObject({ id: 3, card_types: { supertypes: [], core_types: ["Artifact"], subtypes: [] } }),
      makeGameObject({ id: 4, card_types: { supertypes: [], core_types: ["Enchantment"], subtypes: [] } }),
      makeGameObject({ id: 5, card_types: { supertypes: [], core_types: ["Creature"], subtypes: ["Elf"] } }),
    ];

    const result = partitionByType(objects);

    expect(result.creatures).toEqual([1, 5]);
    expect(result.lands).toEqual([2]);
    expect(result.other).toEqual([3, 4]);
  });

  it("returns empty arrays for no objects", () => {
    const result = partitionByType([]);

    expect(result.creatures).toEqual([]);
    expect(result.lands).toEqual([]);
    expect(result.other).toEqual([]);
  });

  it("classifies land-creatures as lands", () => {
    const objects = [
      makeGameObject({ id: 1, card_types: { supertypes: [], core_types: ["Creature", "Land"], subtypes: [] } }),
    ];

    const result = partitionByType(objects);
    // Lands take priority since they check first
    expect(result.lands).toEqual([1]);
    expect(result.creatures).toEqual([]);
  });
});

describe("groupByName", () => {
  it("produces one group per permanent (no stacking)", () => {
    const objects = [
      makeGameObject({ id: 1, name: "Forest" }),
      makeGameObject({ id: 2, name: "Forest" }),
      makeGameObject({ id: 3, name: "Mountain" }),
    ];

    const groups = groupByName(objects);

    expect(groups).toHaveLength(3);
    expect(groups.every((g) => g.count === 1)).toBe(true);
    expect(groups.map((g) => g.ids[0])).toEqual([1, 2, 3]);
  });

  it("preserves name and representative for each permanent", () => {
    const objects = [
      makeGameObject({ id: 5, name: "Forest" }),
      makeGameObject({ id: 9, name: "Mountain" }),
    ];

    const groups = groupByName(objects);

    expect(groups[0].name).toBe("Forest");
    expect(groups[0].representative.id).toBe(5);
    expect(groups[1].name).toBe("Mountain");
    expect(groups[1].representative.id).toBe(9);
  });
});
