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
  it("separates creatures, lands, support permanents, planeswalkers, and other", () => {
    const objects = [
      makeGameObject({ id: 1, card_types: { supertypes: [], core_types: ["Creature"], subtypes: [] } }),
      makeGameObject({ id: 2, card_types: { supertypes: ["Basic"], core_types: ["Land"], subtypes: ["Forest"] } }),
      makeGameObject({ id: 3, card_types: { supertypes: [], core_types: ["Artifact"], subtypes: [] } }),
      makeGameObject({ id: 4, card_types: { supertypes: [], core_types: ["Enchantment"], subtypes: [] } }),
      makeGameObject({ id: 5, card_types: { supertypes: [], core_types: ["Creature"], subtypes: ["Elf"] } }),
      makeGameObject({ id: 6, card_types: { supertypes: [], core_types: ["Planeswalker"], subtypes: [] } }),
      makeGameObject({ id: 7, card_id: 0, card_types: { supertypes: [], core_types: ["Artifact"], subtypes: ["Treasure"] } }),
      makeGameObject({ id: 8, card_types: { supertypes: [], core_types: ["Battle"], subtypes: [] } }),
    ];

    const result = partitionByType(objects);

    expect(result.creatures).toEqual([1, 5]);
    expect(result.lands).toEqual([2]);
    expect(result.support).toEqual([3, 4, 7]);
    expect(result.planeswalkers).toEqual([6]);
    expect(result.other).toEqual([8]);
  });

  it("returns empty arrays for no objects", () => {
    const result = partitionByType([]);

    expect(result.creatures).toEqual([]);
    expect(result.lands).toEqual([]);
    expect(result.support).toEqual([]);
    expect(result.planeswalkers).toEqual([]);
    expect(result.other).toEqual([]);
  });

  it("classifies land-creatures as creatures", () => {
    const objects = [
      makeGameObject({ id: 1, card_types: { supertypes: [], core_types: ["Creature", "Land"], subtypes: [] } }),
    ];

    const result = partitionByType(objects);
    // Creatures take priority — animated lands should display in the creature zone
    expect(result.creatures).toEqual([1]);
    expect(result.lands).toEqual([]);
    expect(result.support).toEqual([]);
    expect(result.planeswalkers).toEqual([]);
    expect(result.other).toEqual([]);
  });

  it("keeps creature tokens in the creature zone", () => {
    const objects = [
      makeGameObject({
        id: 1,
        card_id: 0,
        card_types: { supertypes: [], core_types: ["Artifact", "Creature"], subtypes: ["Construct"] },
      }),
    ];

    const result = partitionByType(objects);

    expect(result.creatures).toEqual([1]);
    expect(result.support).toEqual([]);
  });

  it("excludes attached Equipment from all partition rows", () => {
    const objects = [
      makeGameObject({ id: 1, card_types: { supertypes: [], core_types: ["Creature"], subtypes: [] } }),
      makeGameObject({
        id: 99,
        attached_to: 1,
        card_types: { supertypes: [], core_types: ["Artifact"], subtypes: ["Equipment"] },
      }),
    ];

    const result = partitionByType(objects);

    expect(result.creatures).toEqual([1]);
    expect(result.support).toEqual([]);
    expect(result.lands).toEqual([]);
    expect(result.planeswalkers).toEqual([]);
    expect(result.other).toEqual([]);
  });

  it("excludes attached Aura from all partition rows", () => {
    const objects = [
      makeGameObject({ id: 1, card_types: { supertypes: [], core_types: ["Creature"], subtypes: [] } }),
      makeGameObject({
        id: 99,
        attached_to: 1,
        card_types: { supertypes: [], core_types: ["Enchantment"], subtypes: ["Aura"] },
      }),
    ];

    const result = partitionByType(objects);

    expect(result.creatures).toEqual([1]);
    expect(result.support).toEqual([]);
  });

  it("excludes attached token (card_id === 0) from all partition rows", () => {
    const objects = [
      makeGameObject({ id: 1, card_types: { supertypes: [], core_types: ["Creature"], subtypes: [] } }),
      makeGameObject({
        id: 99,
        card_id: 0,
        attached_to: 1,
        card_types: { supertypes: [], core_types: ["Artifact"], subtypes: ["Equipment"] },
      }),
    ];

    const result = partitionByType(objects);

    expect(result.creatures).toEqual([1]);
    expect(result.support).toEqual([]);
  });

  it("excludes bestowed Aura-creature (Creature + Enchantment core types) when attached", () => {
    // CR 702.103: Bestowed creatures are Auras as long as attached. Their
    // core_types still includes "Creature" — without the attached_to filter
    // running first, they'd land in the creatures row AND the chip row.
    const objects = [
      makeGameObject({ id: 1, card_types: { supertypes: [], core_types: ["Creature"], subtypes: [] } }),
      makeGameObject({
        id: 99,
        attached_to: 1,
        card_types: {
          supertypes: [],
          core_types: ["Creature", "Enchantment"],
          subtypes: ["Satyr", "Aura"],
        },
      }),
    ];

    const result = partitionByType(objects);

    expect(result.creatures).toEqual([1]);
    expect(result.support).toEqual([]);
  });

  it("preserves player-attached Auras (status quo) in the support row", () => {
    // AttachTarget::Player(PlayerId) is currently lossy at the WASM boundary —
    // attached_to flattens to null for Curses. Until that's fixed, they must
    // still render somewhere visible. Documented limitation.
    const objects = [
      makeGameObject({
        id: 99,
        attached_to: null,
        card_types: { supertypes: [], core_types: ["Enchantment"], subtypes: ["Aura", "Curse"] },
      }),
    ];

    const result = partitionByType(objects);

    expect(result.support).toEqual([99]);
  });
});

describe("groupByName", () => {
  it("stacks matching permanents by name and tapped state", () => {
    const objects = [
      makeGameObject({ id: 1, name: "Forest" }),
      makeGameObject({ id: 2, name: "Forest" }),
      makeGameObject({ id: 3, name: "Mountain" }),
    ];

    const groups = groupByName(objects);

    expect(groups).toHaveLength(2);
    expect(groups[0]).toMatchObject({ name: "Forest", ids: [1, 2], count: 2 });
    expect(groups[1]).toMatchObject({ name: "Mountain", ids: [3], count: 1 });
  });

  it("separates tapped and untapped copies into different groups", () => {
    const objects = [
      makeGameObject({ id: 1, name: "Forest", tapped: false }),
      makeGameObject({ id: 2, name: "Forest", tapped: true }),
      makeGameObject({ id: 3, name: "Forest", tapped: false }),
    ];

    const groups = groupByName(objects);

    expect(groups).toHaveLength(2);
    const untapped = groups.find((g) => !g.representative.tapped);
    const tapped = groups.find((g) => g.representative.tapped);
    expect(untapped).toMatchObject({ count: 2, ids: [1, 3] });
    expect(tapped).toMatchObject({ count: 1, ids: [2] });
  });

  it("only attachments prevent grouping — identical-counter copies stack together", () => {
    const objects = [
      makeGameObject({ id: 1, name: "Grizzly Bears", counters: { Plus1Plus1: 1 } }),
      makeGameObject({ id: 2, name: "Grizzly Bears", counters: { Plus1Plus1: 1 } }),
      makeGameObject({ id: 3, name: "Grizzly Bears", attachments: [99] }),
    ];

    const groups = groupByName(objects);

    // Two copies with identical counters stack; the one with an attachment is solo
    expect(groups).toHaveLength(2);
    expect(groups.find((g) => g.count === 2)?.ids).toEqual([1, 2]);
    expect(groups.find((g) => g.count === 1)?.ids).toEqual([3]);
  });

  it("separates copies with different counter amounts", () => {
    const objects = [
      makeGameObject({ id: 1, name: "Grizzly Bears", counters: { Plus1Plus1: 1 } }),
      makeGameObject({ id: 2, name: "Grizzly Bears", counters: { Plus1Plus1: 2 } }),
      makeGameObject({ id: 3, name: "Grizzly Bears", counters: { Plus1Plus1: 1 } }),
    ];

    const groups = groupByName(objects);

    expect(groups).toHaveLength(2);
    expect(groups.find((g) => g.count === 2)?.ids).toEqual([1, 3]);
    expect(groups.find((g) => g.count === 1)?.ids).toEqual([2]);
  });

  it("separates copies with different damage marked", () => {
    const objects = [
      makeGameObject({ id: 1, name: "Grizzly Bears", power: 2, toughness: 2 }),
      makeGameObject({ id: 2, name: "Grizzly Bears", power: 2, toughness: 2, damage_marked: 1 }),
      makeGameObject({ id: 3, name: "Grizzly Bears", power: 2, toughness: 2 }),
    ];

    const groups = groupByName(objects);

    expect(groups).toHaveLength(2);
    expect(groups.find((g) => g.count === 2)?.ids).toEqual([1, 3]);
    expect(groups.find((g) => g.count === 1)?.ids).toEqual([2]);
  });

  it("separates copies with different power/toughness (pump effects)", () => {
    const objects = [
      makeGameObject({ id: 1, name: "Grizzly Bears", power: 2, toughness: 2 }),
      makeGameObject({ id: 2, name: "Grizzly Bears", power: 4, toughness: 4 }),
      makeGameObject({ id: 3, name: "Grizzly Bears", power: 2, toughness: 2 }),
    ];

    const groups = groupByName(objects);

    expect(groups).toHaveLength(2);
    expect(groups.find((g) => g.count === 2)?.ids).toEqual([1, 3]);
    expect(groups.find((g) => g.count === 1)?.ids).toEqual([2]);
  });

  it("separates copies with different keywords (temporary keyword grants)", () => {
    const objects = [
      makeGameObject({ id: 1, name: "Llanowar Elves", keywords: [] }),
      makeGameObject({ id: 2, name: "Llanowar Elves", keywords: ["Flying"] }),
      makeGameObject({ id: 3, name: "Llanowar Elves", keywords: [] }),
    ];

    const groups = groupByName(objects);

    expect(groups).toHaveLength(2);
    expect(groups.find((g) => g.count === 2)?.ids).toEqual([1, 3]);
    expect(groups.find((g) => g.count === 1)?.ids).toEqual([2]);
  });

  it("separates copies with different parameterized keywords (e.g. ward {2} vs ward {3})", () => {
    const ward2 = { type: "Ward", data: { type: "Generic", amount: 2 } };
    const ward3 = { type: "Ward", data: { type: "Generic", amount: 3 } };
    const objects = [
      makeGameObject({ id: 1, name: "Spectral Sailor", keywords: [ward2] }),
      makeGameObject({ id: 2, name: "Spectral Sailor", keywords: [ward3] }),
      makeGameObject({ id: 3, name: "Spectral Sailor", keywords: [ward2] }),
    ];

    const groups = groupByName(objects);

    expect(groups).toHaveLength(2);
    expect(groups.find((g) => g.count === 2)?.ids).toEqual([1, 3]);
    expect(groups.find((g) => g.count === 1)?.ids).toEqual([2]);
  });

  it("separates copies with different colors (color-changing effects)", () => {
    const objects = [
      makeGameObject({ id: 1, name: "Grizzly Bears", color: ["Green"] }),
      makeGameObject({ id: 2, name: "Grizzly Bears", color: ["Blue"] }),
      makeGameObject({ id: 3, name: "Grizzly Bears", color: ["Green"] }),
    ];

    const groups = groupByName(objects);

    expect(groups).toHaveLength(2);
    expect(groups.find((g) => g.count === 2)?.ids).toEqual([1, 3]);
    expect(groups.find((g) => g.count === 1)?.ids).toEqual([2]);
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
